/**
 * 桥端共享协议层:TCP 连接 + 行帧 JSON + StreamCall(aimux StreamPart →
 * dsh StreamChunk 映射)+ BridgeAdapter。被两种形态共用:
 * - `plugin.ts`(rutis-bridge 插件,插进官方 dsh profile)
 * - `main.ts`(独立验收宿主,M2 e2e 用)
 */
import net from 'node:net'
import { CallId, LlmAdapter, LlmError } from '@deepseek-ai/dsh-llm'
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
  /** 通道是否已断(断连后 dsh 必须继续活着——§十.4)。 */
  dead(): boolean
  close(): void
}

export function connectBridge(port: number): BridgeConnection {
  const sock = net.connect(port, '127.0.0.1')
  const frameHandlers: Array<(frame: Frame) => void> = []
  let resolveHello: () => void = () => {}
  const helloDone = new Promise<void>(resolve => { resolveHello = resolve })
  let disconnected = false

  // 通道断连绝不能变成进程崩溃(§十.4:桥侧死亡不拖垮 TS 栈)——
  // 否则桥 socket 的 ECONNRESET 在 dsh 里是 unhandled error,整个宿主崩。
  // 断连后:后续模型调用按 LlmError 拒绝,宿主其余功能照常。
  const markDisconnected = (why: string) => {
    if (disconnected) return
    disconnected = true
    console.error(`[rutis-bridge] bridge channel lost (${why}); model calls via the bridge will fail until restart`)
  }
  sock.on('error', (e: Error) => markDisconnected(e.message))
  sock.on('close', () => markDisconnected('closed'))

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
    dead: () => disconnected,
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
    if (!ok) {
      // dsh 官方约定:adapter 失败抛带 code 的 LlmError(webserver 的错误面
      // 按此接住);普通 Error 会漏出为未分类故障。
      const remote = payload as { code?: string; message?: string } | undefined
      this.error = new LlmError(remote?.message ?? `bridge stream failed: ${JSON.stringify(payload)}`, remote?.code ?? 'bridgeError')
    }
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
/** 桥 adapter 的可选装配面。 */
export interface BridgeAdapterOptions {
  /** 每次调用解析凭据(dsh credentials 体系);返回值随请求过线,
   * Rust 侧用它构造 provider。 */
  resolveKey?: () => Promise<string | undefined>
}

/** 桥 adapter(§四.1):stream 过线,chunk 流回流;注册面同步成员是注册期
 * 快照(F6 复核结论),继承 LlmAdapter 默认实现。 */
export class BridgeAdapter extends LlmAdapter {
  private nextId = 100
  private readonly pending = new Map<number, StreamCall>()

  constructor(conn: BridgeConnection, options: BridgeAdapterOptions = {}) {
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
    this.isDead = conn.dead
    this.resolveKey = options.resolveKey
  }

  private readonly send: (frame: Frame) => void
  private readonly isDead: () => boolean
  private readonly resolveKey: (() => Promise<string | undefined>) | undefined

  async *stream(options: GenerateOptions): AsyncIterable<StreamChunk> {
    if (this.isDead()) {
      throw new LlmError('bridge channel is disconnected — restart with rutis-dsh up to restore model calls', 'bridgeDisconnected')
    }
    const id = this.nextId++
    const call = new StreamCall()
    this.pending.set(id, call)
    // dsh 侧凭据(per-request 解析)随请求过线;解析失败不阻塞调用——
    // Rust 侧还有 env 兜底。
    const apiKey = await this.resolveKey?.().catch(() => undefined)
    this.send({
      type: 'req', id, method: 'svc/call',
      params: {
        service: 'llm', method: 'stream',
        params: {
          options: JSON.parse(JSON.stringify(options ?? {})),
          ...(apiKey !== undefined ? { credentials: { apiKey } } : {}),
        },
      },
    })
    try {
      yield* call.drain()
    } finally {
      this.pending.delete(id)
    }
  }
}
