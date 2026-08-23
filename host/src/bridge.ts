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
  /** 断连一次性回调(adapter 在此把在飞调用按 bridgeDisconnected 落定)。 */
  onDead(handler: () => void): void
  /** 在飞请求期间把通道 pin 为 ref(one-shot 宿主可自然退出的关键):
   * 桥通道默认 unref,不让它独自撑住宿主事件循环;流/rpc 在飞时
   * hold,全部落定后 release——空闲的桥等价于不存在。 */
  hold(): void
  release(): void
  close(): void
}

export function connectBridge(port: number): BridgeConnection {
  const sock = net.connect(port, '127.0.0.1')
  const frameHandlers: Array<(frame: Frame) => void> = []
  const deadHandlers: Array<() => void> = []
  let resolveHello: () => void = () => {}
  const helloDone = new Promise<void>(resolve => { resolveHello = resolve })
  let disconnected = false

  // 通道断连绝不能变成进程崩溃(§十.4:桥侧死亡不拖垮 TS 栈)——
  // 否则桥 socket 的 ECONNRESET 在 dsh 里是 unhandled error,整个宿主崩。
  // 断连后:在飞与后续模型调用按 LlmError 拒绝,宿主其余功能照常。
  const markDisconnected = (why: string) => {
    if (disconnected) return
    disconnected = true
    pendingHolds = 0
    try { sock.unref() } catch { /* 已销毁 */ }
    console.error(`[rutis-bridge] bridge channel lost (${why}); model calls via the bridge will fail until restart`)
    for (const handler of deadHandlers) handler()
  }
  sock.on('error', (e: Error) => markDisconnected(e.message))
  sock.on('close', () => markDisconnected('closed'))

  // 空闲 unref:连接与握手期间保持 ref(连接本身要完成);hello 应答后
  // 若无在飞请求即 unref。此后 hold/release 按在飞数翻转。
  let pendingHolds = 0
  const hold = () => { if (++pendingHolds === 1) sock.ref() }
  const release = () => { if (pendingHolds > 0 && --pendingHolds === 0) sock.unref() }
  const idleAfterHello = () => { if (pendingHolds === 0) sock.unref() }

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
        idleAfterHello()
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
    onDead(handler) { deadHandlers.push(handler) },
    hold,
    release,
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

/** 一条 aimux 路由的 settings profile(llm-aimux: providers.<route>)。 */
export interface AimuxProviderProfile {
  /** Credential reference(env 名),per-request 经 ctx.credentials 解析。 */
  apiKeyEnv?: string
  /** 配置面显示名;默认路由名。 */
  displayName?: string
  /** 后端 aimux provider 名;缺省即路由名。路由名是 dsh 侧标签
   * (须避开官方 pi-ai 已声明的 provider 名,如 deepseek),后端名是
   * aimux 侧的真名——两者解耦后,一条 aimux provider 可以挂多条路由。 */
  provider?: string
}

/** 桥 adapter 的装配面。 */
export interface BridgeAdapterOptions {
  /** 当前生效的 profiles(settings 驱动,memoized)。 */
  profiles: () => ReadonlyMap<string, AimuxProviderProfile>
  /** 按 credential ref 解析 key(dsh credentials 体系)。 */
  resolveKey: (ref: string) => Promise<string | undefined>
  /** 无 profile 路由的兜底 key(验收宿主形态);缺省即无兜底。 */
  fallbackKey?: () => Promise<string | undefined>
}

/**
 * 桥 adapter(§四.1 的多路由聚合形态,对标 llm-pi-ai):路由集由 settings
 * 的 providers dict 驱动;每条路由 = 一个 aimux provider。stream 与
 * listModels 均过线,凭据 per-request 解析后随请求携带。注册面同步成员
 * 是注册期快照(F6 复核结论)。
 */
export class BridgeAdapter extends LlmAdapter {
  private nextId = 100
  private readonly pending = new Map<number, StreamCall>()
  private readonly rpcPending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>()

  constructor(conn: BridgeConnection, private readonly options: BridgeAdapterOptions) {
    super()
    conn.onFrame(frame => {
      if (frame.type === 'ntf' && frame.method === 'svc/part') {
        const params = frame.params as Record<string, unknown>
        this.pending.get(params.dispatchId as number)?.mapPart(params.part as Record<string, unknown>)
      } else if (frame.type === 'res') {
        const id = frame.id as number
        const waiter = this.rpcPending.get(id)
        if (waiter !== undefined) {
          this.rpcPending.delete(id)
          if (frame.ok === true) waiter.resolve(frame.result ?? null)
          else waiter.reject(new LlmError(String((frame.error as { message?: string })?.message ?? 'rpc failed'), String((frame.error as { code?: string })?.code ?? 'bridgeError')))
          return
        }
        this.pending.get(id)?.settle(frame.ok === true, frame.error ?? frame.result)
      }
    })
    // §十.4:断连时在飞调用不能永远挂起——全部按 bridgeDisconnected 落定。
    conn.onDead(() => {
      for (const call of this.pending.values()) {
        call.settle(false, { code: 'bridgeDisconnected', message: 'bridge channel lost during stream' })
      }
      this.pending.clear()
      for (const waiter of this.rpcPending.values()) {
        waiter.reject(new LlmError('bridge channel lost during rpc', 'bridgeDisconnected'))
      }
      this.rpcPending.clear()
    })
    this.send = conn.send
    this.isDead = conn.dead
    this.connHold = conn.hold
    this.connRelease = conn.release
  }

  private readonly send: (frame: Frame) => void
  private readonly isDead: () => boolean
  private readonly connHold: () => void
  private readonly connRelease: () => void

  providerInfo(provider: string): { id: string; name: string } {
    return { id: provider, name: this.options.profiles().get(provider)?.displayName ?? provider }
  }

  /** 路由名 → 后端 aimux provider 名(profile.provider,缺省路由名)。 */
  private backendFor(route: string): string {
    return this.options.profiles().get(route)?.provider ?? route
  }

  /** 通用 req/res 过线(listModels 等非流方法)。 */
  private rpc<T>(method: string, params: Record<string, unknown>): Promise<T> {
    const id = this.nextId++
    this.connHold()
    return new Promise<T>((resolve, reject) => {
      this.rpcPending.set(id, { resolve: resolve as (v: unknown) => void, reject })
      this.send({ type: 'req', id, method: 'svc/call', params: { service: 'llm', method, params } })
    }).finally(() => this.connRelease()) as Promise<T>
  }

  /** 该路由的 key:profile 的 apiKeyEnv 解析;无 profile 用兜底。 */
  private async keyFor(provider: string): Promise<string | undefined> {
    const profile = this.options.profiles().get(provider)
    if (profile === undefined) return this.options.fallbackKey?.()
    if (profile.apiKeyEnv === undefined) return undefined
    return this.options.resolveKey(profile.apiKeyEnv)
  }

  async listModels(provider: string): Promise<Array<{ provider: string; id: string; name: string }>> {
    if (this.isDead()) return []
    const apiKey = await this.keyFor(provider).catch(() => undefined)
    const result = await this.rpc<{ models: Array<{ id: string }> }>('listModels', {
      provider: this.backendFor(provider),
      ...(apiKey !== undefined ? { apiKey } : {}),
    })
    return (result.models ?? []).map(m => ({ provider, id: m.id, name: m.id }))
  }

  /** dsh `GenerateOptions` → aimux-llm 的中性 prompt 形状(本插件是
   * 唯一懂 dsh 的地方:翻译只发生在这里,Rust 侧零 dsh 知识)。 */
  private promptSpec(options: GenerateOptions): Record<string, unknown> {
    const messages = (options.messages ?? []).map(message => ({
      role: message.role,
      text: (Array.isArray(message.content) ? message.content : [])
        .map(block => (block !== null && typeof block === 'object' && 'text' in block && typeof (block as { text?: unknown }).text === 'string' ? (block as { text: string }).text : ''))
        .join(''),
    }))
    const tools = (options.tools ?? []).map(tool => ({
      name: tool.name,
      ...(tool.description !== undefined ? { description: tool.description } : {}),
      ...(tool.parameters !== undefined ? { parameters: tool.parameters } : {}),
    }))
    return {
      ...(options.system !== undefined ? { system: options.system } : {}),
      messages,
      tools,
    }
  }

  async *stream(options: GenerateOptions): AsyncIterable<StreamChunk> {
    if (this.isDead()) {
      throw new LlmError('bridge channel is disconnected — restart with rutis-dsh up to restore model calls', 'bridgeDisconnected')
    }
    const id = this.nextId++
    const call = new StreamCall()
    this.pending.set(id, call)
    this.connHold()
    const apiKey = await this.keyFor(options.provider).catch(() => undefined)
    // 过线 = aimux-llm 声明的中性 DTO(路由名已在 TS 侧解析为后端
    // provider);dsh 形状不出本插件。
    this.send({
      type: 'req', id, method: 'svc/call',
      params: {
        service: 'llm', method: 'stream',
        params: {
          provider: this.backendFor(options.provider),
          model: options.model,
          ...(apiKey !== undefined ? { apiKey } : {}),
          options: this.promptSpec(options),
        },
      },
    })
    try {
      yield* call.drain()
    } finally {
      this.pending.delete(id)
      this.connRelease()
    }
  }
}
