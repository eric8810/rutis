/**
 * 桥端共享协议层:TCP 连接 + 行帧 JSON + StreamCall(aimux StreamPart →
 * dsh StreamChunk 映射)+ BridgeAdapter。被两种形态共用:
 * - `plugin.ts`(rutis-bridge 插件,插进官方 dsh profile)
 * - `main.ts`(独立验收宿主,M2 e2e 用)
 */
import net from 'node:net'
import { CallId, LlmAdapter } from '@deepseek-ai/dsh-llm'
import type { GenerateOptions, StreamChunk } from '@deepseek-ai/dsh-llm'

type Frame = Record<string, unknown>

export interface BridgeConnection {
  send(frame: Frame): void
  /** 全帧订阅(adapter 的 res/llm-chunk 接线面)。 */
  onFrame(handler: (frame: Frame) => void): void
  /** ntf 订阅(事件缝回流面)。 */
  onNtf(handler: (method: string, params: Record<string, unknown>) => void): void
  /** hello 完成信号。 */
  helloDone: Promise<void>
  close(): void
}

export function connectBridge(port: number): BridgeConnection {
  const sock = net.connect(port, '127.0.0.1')
  const frameHandlers: Array<(frame: Frame) => void> = []
  let resolveHello: () => void = () => {}
  const helloDone = new Promise<void>(resolve => { resolveHello = resolve })

  function send(frame: Frame) {
    sock.write(JSON.stringify(frame) + '\n')
  }

  sock.on('connect', () => {
    send({
      type: 'req', id: 1, method: 'hello',
      params: {
        protocol: 1, base: 'cordis', baseSemver: '4.0.1', stack: ['node'],
        caps: { services: ['llm'], wfKinds: [], scopes: [] },
      },
    })
  })

  let buf = ''
  sock.setEncoding('utf8')
  sock.on('data', (chunk: string) => {
    buf += chunk
    let nl: number
    while ((nl = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, nl)
      buf = buf.slice(nl + 1)
      if (line.length === 0) continue
      let frame: Frame
      try {
        frame = JSON.parse(line) as Frame
      } catch {
        continue
      }
      if (frame.type === 'res' && frame.id === 1) {
        resolveHello()
        continue
      }
      for (const handler of frameHandlers) handler(frame)
    }
  })

  return {
    send,
    onFrame(handler) { frameHandlers.push(handler) },
    onNtf(handler) {
      frameHandlers.push(frame => {
        if (frame.type === 'ntf') handler(frame.method as string, frame.params as Record<string, unknown>)
      })
    },
    helloDone,
    close() { sock.destroy() },
  }
}

function mapFinishReason(finish: Record<string, unknown> | undefined): import('@deepseek-ai/dsh-llm').FinishReason {
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

/** 桥 adapter(§四.1):stream 过线,chunk 流回流;注册面同步成员是注册期
 * 快照(F6 复核结论),继承 LlmAdapter 默认实现。 */
export class BridgeAdapter extends LlmAdapter {
  private nextId = 100
  private readonly pending = new Map<number, StreamCall>()

  constructor(conn: BridgeConnection) {
    super()
    conn.onFrame(frame => {
      if (frame.type === 'ntf' && frame.method === 'llm/chunk') {
        const params = frame.params as Record<string, unknown>
        this.pending.get(params.dispatchId as number)?.mapPart(params.part as Record<string, unknown>)
      } else if (frame.type === 'res') {
        this.pending.get(frame.id as number)?.settle(frame.ok === true, frame.error ?? frame.result)
      }
    })
    this.send = conn.send
  }

  private readonly send: (frame: Frame) => void

  async *stream(options: GenerateOptions): AsyncIterable<StreamChunk> {
    const id = this.nextId++
    const call = new StreamCall()
    this.pending.set(id, call)
    this.send({ type: 'req', id, method: 'svc/call', params: { service: 'llm', method: 'stream', params: { options: JSON.parse(JSON.stringify(options ?? {})) } } })
    try {
      yield* call.drain()
    } finally {
      this.pending.delete(id)
    }
  }
}
