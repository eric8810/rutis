/**
 * TS 侧桥端宿主(npm 直装形态):dsh 包经精确 npm 版本消费
 * (`@deepseek-ai/dsh-llm@0.1.1-rc.2` + `@deepseek-ai/cordis@4.0.1`),
 * 不依赖本地 deepseek-harness 检出。由 Rust 侧测试经
 * `node --import tsx src/main.ts` 拉起(env:BRIDGE_PORT);进程退出码即
 * 验收结论。
 *
 * 验收内容(同 M2-4):真 dsh LlmRuntime + 桥 adapter 的两轮完整 turn
 * (tool-call 过线 → 本宿主执行 → 回喂 → 终轮文本 + usage 逐字段)
 * + console.log 注入(帧流不损坏)。
 */
import assert from 'node:assert/strict'
import net from 'node:net'
import { Context } from '@deepseek-ai/cordis'
import LlmRuntime, { CallId, createUserMessage, LlmAdapter } from '@deepseek-ai/dsh-llm'
import type { GenerateOptions, StreamChunk } from '@deepseek-ai/dsh-llm'

const port = Number(process.env.BRIDGE_PORT)

type Frame = Record<string, unknown>

const sock = net.connect(port, '127.0.0.1')
sock.on('connect', () => {
  send({
    type: 'req', id: 1, method: 'hello',
    params: {
      protocol: 1, base: 'cordis', baseSemver: '4.0.1', stack: ['node', 'npm'],
      caps: { services: ['llm'], wfKinds: [], scopes: [] },
    },
  })
})

function send(frame: Frame) {
  sock.write(JSON.stringify(frame) + '\n')
}

let resolveHello: () => void = () => {}
const helloDone = new Promise<void>(resolve => { resolveHello = resolve })

function mapFinishReason(finish: Record<string, unknown> | undefined): { kind: string } {
  switch (finish?.unified) {
    case 'tool-calls': return { kind: 'tool-calls' }
    case 'length': return { kind: 'max-tokens' }
    default: return { kind: 'stop' }
  }
}

function mapUsage(usage: Record<string, unknown> | undefined) {
  const input = usage?.input_tokens as Record<string, unknown> | undefined
  const output = usage?.output_tokens as Record<string, unknown> | undefined
  return {
    inputTokens: Number(input?.no_cache ?? input?.total ?? 0),
    outputTokens: Number(output?.total ?? 0),
    ...(input?.cache_read !== undefined ? { cacheReadTokens: Number(input.cache_read) } : {}),
  }
}

class StreamCall {
  private buffered: StreamChunk[] = []
  private closed = false
  private error: Error | undefined
  private wake: (() => void) | undefined
  private text = ''
  private blockOpen = false

  mapPart(part: Record<string, unknown>) {
    const kind = Object.keys(part)[0]!
    const value = part[kind] as Record<string, unknown>
    if (kind === 'TextDelta') {
      if (!this.blockOpen) {
        this.blockOpen = true
        this.push({ type: 'block-start', index: 0, blockType: 'text' })
      }
      const delta = String(value.delta)
      this.text += delta
      this.push({ type: 'text-delta', index: 0, text: delta })
    } else if (kind === 'ToolCall') {
      this.push({
        type: 'tool-call-delta',
        index: 0,
        id: CallId(String(value.tool_call_id)),
        name: String(value.tool_name),
        argumentsDelta: JSON.stringify(value.input),
      })
    } else if (kind === 'Finish') {
      if (this.blockOpen) {
        this.push({ type: 'block-end', index: 0, block: { type: 'text', text: this.text } })
      }
      this.push({ type: 'usage', usage: mapUsage(value.usage as Record<string, unknown>) })
      this.push({ type: 'finish', reason: mapFinishReason(value.finish_reason as Record<string, unknown>) })
    }
  }

  settle(ok: boolean, payload: unknown) {
    if (!ok) this.error = new Error(`bridge stream failed: ${JSON.stringify(payload)}`)
    this.closed = true
    this.wake?.()
  }

  private push(chunk: StreamChunk) {
    this.buffered.push(chunk)
    this.wake?.()
  }

  async *drain(): AsyncIterable<StreamChunk> {
    while (true) {
      if (this.buffered.length > 0) {
        yield this.buffered.shift()!
      } else if (this.error !== undefined) {
        throw this.error
      } else if (this.closed) {
        return
      } else {
        await new Promise<void>(resolve => { this.wake = resolve })
        this.wake = undefined
      }
    }
  }
}

class BridgeAdapter extends LlmAdapter {
  private nextId = 100
  private readonly pending = new Map<number, StreamCall>()

  feedNtf(method: string, params: Record<string, unknown>) {
    if (method !== 'llm/chunk') return
    this.pending.get(params.dispatchId as number)?.mapPart(params.part as Record<string, unknown>)
  }

  feedRes(id: number, ok: boolean, payload: unknown) {
    this.pending.get(id)?.settle(ok, payload)
  }

  async *stream(options: GenerateOptions): AsyncIterable<StreamChunk> {
    const id = this.nextId++
    const call = new StreamCall()
    this.pending.set(id, call)
    send({
      type: 'req', id, method: 'svc/call',
      params: { service: 'llm', method: 'stream', params: { options: JSON.parse(JSON.stringify(options ?? {})) } },
    })
    try {
      yield* call.drain()
    } finally {
      this.pending.delete(id)
    }
  }
}

const adapter = new BridgeAdapter()

let buf = ''
sock.setEncoding('utf8')
sock.on('data', (chunk: string) => {
  buf += chunk
  let nl: number
  while ((nl = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, nl)
    buf = buf.slice(nl + 1)
    if (line.length === 0) continue
    const frame = JSON.parse(line) as Frame
    if (frame.type === 'res') {
      if (frame.id === 1) {
        resolveHello()
      } else {
        adapter.feedRes(frame.id as number, frame.ok === true, frame.error ?? frame.result)
      }
    } else if (frame.type === 'ntf') {
      adapter.feedNtf(frame.method as string, frame.params as Record<string, unknown>)
    }
  }
})

function baseOptions(): GenerateOptions {
  return {
    provider: 'aimux-bridge',
    model: 'scripted',
    system: 'You are the m2 acceptance host.',
    messages: [createUserMessage({ content: [{ type: 'text', text: 'run the acceptance turn' }] })],
    tools: [{
      name: 'echo_tool',
      description: 'Echoes its text argument back into the turn.',
      parameters: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] },
    }],
  }
}

let ctx: Context

async function collect(options: GenerateOptions): Promise<StreamChunk[]> {
  const chunks: StreamChunk[] = []
  for await (const chunk of ctx.llm.stream(options)) chunks.push(chunk)
  return chunks
}

async function main() {
  ctx = new Context()
  await ctx.plugin(LlmRuntime)
  ctx.llm.registerAdapter(['aimux-bridge'], adapter)
  await helloDone

  // 注册面(F6:同步注册表留本地)。
  assert.ok(ctx.llm.listProviders().some(p => p.id === 'aimux-bridge'), 'adapter registered')

  // 注入验收前置:装载狂写 stdout 的插件,流在噪声中必须完好。
  await ctx.plugin({
    name: 'noisy-logger',
    apply() {
      for (let i = 0; i < 300; i++) console.log(`[noisy-logger] ${i} ${'x'.repeat(40)}`)
    },
  })

  // 第一轮:tool-call 过线。
  const options = baseOptions()
  const round1 = await collect(options)
  const toolCall = round1.find((c): c is Extract<StreamChunk, { type: 'tool-call-delta' }> => c.type === 'tool-call-delta')
  assert.ok(toolCall, 'tool-call chunk arrived')
  assert.equal(toolCall!.name, 'echo_tool')
  const args = JSON.parse(toolCall!.argumentsDelta) as { text: string }
  assert.equal(args.text, 'm2')
  const finish1 = round1.find((c): c is Extract<StreamChunk, { type: 'finish' }> => c.type === 'finish')
  assert.deepEqual(finish1?.reason, { kind: 'tool-calls' })

  // 宿主执行工具(v3 语义:工具执行留 TS),结果回喂。
  options.messages = [...options.messages, createUserMessage({ content: [{ type: 'text', text: `TOOL_RESULT: ${args.text}` }] })]

  // 终轮:文本 + finish + usage 逐字段。
  const round2 = await collect(options)
  const text = round2
    .filter((c): c is Extract<StreamChunk, { type: 'text-delta' }> => c.type === 'text-delta')
    .map(c => c.text)
    .join('')
  assert.equal(text, 'all done')
  const finish2 = round2.find((c): c is Extract<StreamChunk, { type: 'finish' }> => c.type === 'finish')
  assert.deepEqual(finish2?.reason, { kind: 'stop' })
  const usage = round2.find((c): c is Extract<StreamChunk, { type: 'usage' }> => c.type === 'usage')
  assert.deepEqual(usage?.usage, { inputTokens: 9, outputTokens: 7, cacheReadTokens: 2 })

  console.log('[host] PASS: two-round turn + tool backfeed + usage fidelity + console.log injection')
  await ctx.fiber.dispose()
  sock.destroy()
  process.exit(0)
}

main().catch(e => {
  console.error('[host] FAIL:', e)
  process.exit(1)
})
