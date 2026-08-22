/**
 * rutis-bridge 插件本体:插进官方 dsh profile 的 cordis 插件。
 * 以生态自有机制注册 llm adapter(§四.1,不劫持 ctx.llm),把模型调用
 * 过线到 Rust(aimux);`forwardEvents` 声明的事件经 `evt/emit` 转发回流
 * (事件缝 v1:装载请求声明的事件集)。
 *
 * 凭据对齐 dsh 产品体验:key 在 web Models 页/dsh 侧配置(credentials
 * 体系,env → 托管文件层叠,per-request 解析)——每次调用解析后随请求
 * 过线,Rust 侧用它构造 provider。shell 里 export key 只是 Rust 侧的
 * 兜底,不再是主路径。
 *
 * 由 runner 拉起的 dsh 经 `RUTIS_BRIDGE_PORT` env 找到 Rust 侧;
 * 端口未设即显式报错——桥插件只在 runner 语境里有意义。
 */
import type { Context } from '@deepseek-ai/cordis'
import { credentialRef } from '@deepseek-ai/dsh-credentials'
import { BridgeAdapter, connectBridge } from './bridge.ts'

export const name = 'rutis-bridge'
export const inject = ['llm']

export interface Config {
  /** 读取端口的 env 名(默认 RUTIS_BRIDGE_PORT)。 */
  portEnv?: string
  /** 经事件缝转发的事件名(v1:声明式清单;通配订阅是后续波次)。 */
  forwardEvents?: string[]
  /** 注册的 provider 路由名(默认 aimux-bridge)。 */
  route?: string
  /** 凭据引用(与官方 llm-deepseek 的 apiKeyEnv 同一体系,默认
   * DEEPSEEK_API_KEY——web Models 页写的 key 落在这里)。 */
  credentialRef?: string
}

interface CredentialResolver {
  resolve(ref: { readonly brand: unique symbol } | string): Promise<{ value: string } | undefined>
}

export function apply(ctx: Context, config: Config = {}): void {
  const port = Number(process.env[config.portEnv ?? 'RUTIS_BRIDGE_PORT'])
  if (!Number.isFinite(port) || port <= 0) {
    throw new Error('rutis-bridge: RUTIS_BRIDGE_PORT is not set — start dsh via the rutis runner (`rutis-dsh up`)')
  }
  const route = config.route ?? 'aimux-bridge'
  const conn = connectBridge(port)

  // 事件缝:声明的事件 → evt/emit 过线。订阅先建(装载期事件也能过线),
  // 载荷以 JSON 快照过境(活对象替身是 L4 范围)。名字来自配置,cordis 的
  // Events 键约束在装载边界放行。
  const on = ctx.on as unknown as (name: string, listener: (...args: unknown[]) => void) => void
  for (const eventName of config.forwardEvents ?? []) {
    on(eventName, (...args: unknown[]) => {
      const payload = args.length <= 1 ? args[0] : args
      conn.send({ type: 'ntf', method: 'evt/emit', params: { event: eventName, params: safeJson(payload) } })
    })
  }

  // 凭据:credentials 服务是可选组合(最小组合没有),软查询;per-request
  // 解析,页面改 key 下一次调用即生效,与官方 adapter 同语义。
  const credentials = (ctx as unknown as { get?: (name: string) => unknown }).get?.('credentials') as CredentialResolver | undefined
  const ref = credentialRef(config.credentialRef ?? 'DEEPSEEK_API_KEY')
  const resolveKey = credentials
    ? async (): Promise<string | undefined> => (await credentials.resolve(ref))?.value
    : undefined

  // llm 缝:桥 adapter 以 adapter 身份进入官方 LlmRuntime。
  ctx.llm.registerAdapter([route], new BridgeAdapter(conn, { resolveKey }))
}

function safeJson(value: unknown): unknown {
  try {
    return JSON.parse(JSON.stringify(value ?? null))
  } catch {
    return { unserializable: true }
  }
}

export default { name, inject, apply }
