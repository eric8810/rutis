/**
 * rutis-bridge 插件本体:插进官方 dsh profile 的 cordis 插件。
 * 以生态自有机制注册 llm adapter(§四.1,不劫持 ctx.llm),把模型调用
 * 过线到 Rust(aimux);`forwardEvents` 声明的事件经 `evt/emit` 转发回流
 * (事件缝 v1:装载请求声明的事件集)。
 *
 * 由 runner 拉起的 dsh 经 `RUTIS_BRIDGE_PORT` env 找到 Rust 侧;
 * 端口未设即显式报错——桥插件只在 runner 语境里有意义。
 */
import type { Context } from '@deepseek-ai/cordis'
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
}

export function apply(ctx: Context, config: Config = {}): void {
  const port = Number(process.env[config.portEnv ?? 'RUTIS_BRIDGE_PORT'])
  if (!Number.isFinite(port) || port <= 0) {
    throw new Error('rutis-bridge: RUTIS_BRIDGE_PORT is not set — start dsh via the rutis runner (`rutis-dsh up`)')
  }
  const route = config.route ?? 'aimux-bridge'
  const conn = connectBridge(port)

  // 事件缝:声明的事件 → evt/emit 过线。订阅先建(装载期事件也能过线),
  // 载荷以 JSON 快照过境(活对象替身是 L4 范围)。
  for (const eventName of config.forwardEvents ?? []) {
    ctx.on(eventName, (...args: unknown[]) => {
      const payload = args.length <= 1 ? args[0] : args
      conn.send({ type: 'ntf', method: 'evt/emit', params: { event: eventName, params: safeJson(payload) } })
    })
  }

  // llm 缝:桥 adapter 以 adapter 身份进入官方 LlmRuntime。
  ctx.llm.registerAdapter([route], new BridgeAdapter(conn))

  ctx.on('dispose', () => conn.close())
}

function safeJson(value: unknown): unknown {
  try {
    return JSON.parse(JSON.stringify(value ?? null))
  } catch {
    return { unserializable: true }
  }
}

export default { name, inject, apply }
