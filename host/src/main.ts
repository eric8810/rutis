/**
 * 独立验收宿主(M2 e2e 用):不经过 profile,直接消费 npm 精确版本的
 * dsh-llm,跑 M2-4 的完整验收(两轮 turn + usage 逐字段 + console.log
 * 注入)。协议层/adapter 共享自 bridge.ts(与 plugin.ts 同一实现)。
 * 由 Rust 侧测试经 `node --import tsx src/main.ts` 拉起;退出码即结论。
 */
import assert from 'node:assert/strict'
import { Context } from '@deepseek-ai/cordis'
import LlmRuntime, { createUserMessage } from '@deepseek-ai/dsh-llm'
import type { GenerateOptions, StreamChunk } from '@deepseek-ai/dsh-llm'
import { BridgeAdapter, connectBridge } from './bridge.ts'

const port = Number(process.env.BRIDGE_PORT)

async function collect(ctx: Context, options: GenerateOptions): Promise<StreamChunk[]> {
  const chunks: StreamChunk[] = []
  for await (const chunk of ctx.llm.stream(options)) chunks.push(chunk)
  return chunks
}

async function main() {
  const conn = connectBridge(port)
  const ctx = new Context()
  await ctx.plugin(LlmRuntime)
  const adapter = new BridgeAdapter(conn, {
    profiles: () => new Map([['aimux-bridge', {}]]),
    resolveKey: async () => undefined,
  })
  ctx.llm.registerAdapter(['aimux-bridge'], adapter)
  await conn.helloDone

  assert.ok(ctx.llm.listProviders().some(p => p.id === 'aimux-bridge'), 'adapter registered')

  // 注入验收前置:装载狂写 stdout 的插件,流在噪声中必须完好。
  await ctx.plugin({
    name: 'noisy-logger',
    apply() {
      for (let i = 0; i < 300; i++) console.log(`[noisy-logger] ${i} ${'x'.repeat(40)}`)
    },
  })

  const options: GenerateOptions = {
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

  // 第一轮:tool-call 过线。
  const round1 = await collect(ctx, options)
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
  const round2 = await collect(ctx, options)
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
  conn.close()
  process.exit(0)
}

main().catch(e => {
  console.error('[host] FAIL:', e)
  process.exit(1)
})
