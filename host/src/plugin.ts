/**
 * rutis-bridge 插件本体:插进官方 dsh profile 的 cordis 插件,以
 * llm-pi-ai 同构的**多路由聚合 adapter**形态存在(aimux = provider 聚合
 * 平台,路由集由 settings 驱动):
 *
 * - settings section `llm-aimux:` 的 `providers` dict 声明路由,key 即
 *   路由名(= GenerateOptions.provider = aimux provider 名);增删路由
 *   热生效(settings 热重载,pi-ai 同款)。
 * - 每条 profile 的 `apiKeyEnv` 是 credential ref,per-request 经
 *   ctx.credentials 解析(web Models 页写的 key 就在这里),随调用过线,
 *   Rust 侧按 (provider, key, model) 构造并缓存 aimux provider。
 * - `registerConfigurableProviders` 声明配置面:web Models 页枚举到桥的
 *   路由,页面原生可配。
 * - 模型目录经 adapter.listModels 过线(aimux 真拉 /models)。
 * - `forwardEvents` 声明的事件经 `evt/emit` 转发回流(事件缝 v1)。
 *
 * 由 runner 拉起的 dsh 经 `RUTIS_BRIDGE_PORT` env 找到 Rust 侧;端口未设
 * 即显式报错——桥插件只在 runner 语境里有意义。
 */
import type { Context } from '@deepseek-ai/cordis'
import { LlmError } from '@deepseek-ai/dsh-llm'
import type { LlmConfigurableProvider } from '@deepseek-ai/dsh-llm'
import { installSettingsSection, settingsNamespace } from '@deepseek-ai/dsh-settings'
import z from '@deepseek-ai/schemastery'
import { BridgeAdapter, connectBridge } from './bridge.ts'
import type { AimuxProviderProfile } from './bridge.ts'

export const name = 'rutis-bridge'
export const inject = ['llm']

const NS = settingsNamespace('llm-aimux')

/** 一条路由的 settings profile(llm-aimux: providers.<route>)。 */
const ProviderProfile: z<AimuxProviderProfile> = z.object({
  apiKeyEnv: z.string().role('credential-ref'),
  displayName: z.string(),
})

/** 插件与 settings section 共用的 schema:providers dict,key 即路由。 */
export const Config: z<Config> = z.object({
  providers: z.dict(ProviderProfile).default({}),
})
export interface Config {
  providers?: Record<string, AimuxProviderProfile>
}

export interface PluginConfig {
  portEnv?: string
  /** 经事件缝转发的事件名(v1:声明式清单)。 */
  forwardEvents?: string[]
}

interface CredentialResolver {
  resolve(ref: string): Promise<{ value: string } | undefined>
}

function deepEqualJson(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

export function apply(ctx: Context, pluginConfig: PluginConfig = {}): void {
  const port = Number(process.env[pluginConfig.portEnv ?? 'RUTIS_BRIDGE_PORT'])
  if (!Number.isFinite(port) || port <= 0) {
    throw new Error('rutis-bridge: RUTIS_BRIDGE_PORT is not set — start dsh via the rutis runner (`rutis-dsh up`)')
  }
  const conn = connectBridge(port)

  // 事件缝:声明的事件 → evt/emit 过线(订阅先建,装载期事件也能过线)。
  const on = ctx.on as unknown as (name: string, listener: (...args: unknown[]) => void) => void
  for (const eventName of pluginConfig.forwardEvents ?? []) {
    on(eventName, (...args: unknown[]) => {
      const payload = args.length <= 1 ? args[0] : args
      conn.send({ type: 'ntf', method: 'evt/emit', params: { event: eventName, params: safeJson(payload) } })
    })
  }

  // ── settings 驱动的 profiles(pi-ai 同款:memoized by 快照 identity)──
  let current: () => Config = () => ({})
  let lastRaw: Config | undefined
  let memoized: ReadonlyMap<string, AimuxProviderProfile> | undefined
  const profiles = (): ReadonlyMap<string, AimuxProviderProfile> => {
    const raw = current()
    if (raw === lastRaw && memoized !== undefined) return memoized
    const next = new Map<string, AimuxProviderProfile>(Object.entries(raw.providers ?? {}))
    lastRaw = raw
    memoized = next
    return next
  }
  profiles()

  // 凭据:credentials 服务是可选组合,软查询;per-request 解析。
  const credentials = (ctx as unknown as { get?: (name: string) => unknown }).get?.('credentials') as CredentialResolver | undefined
  const resolveKey = async (ref: string): Promise<string | undefined> => {
    if (credentials === undefined) return process.env[ref]
    return (await credentials.resolve(ref))?.value
  }

  const adapter = new BridgeAdapter(conn, { profiles, resolveKey })

  // ── 配置面:让 web Models 页枚举到桥的路由 ──
  let directory: { replace(entries: LlmConfigurableProvider[]): void } | undefined
  let directoryFacts: unknown
  const ensureDirectory = (): void => {
    const entries: LlmConfigurableProvider[] = [...profiles().entries()]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([route, profile]) => ({
        provider: route,
        displayName: profile.displayName ?? route,
        settingsNs: NS,
        settingsPath: ['providers', route],
        declared: true,
      }))
    if (entries.length === 0) return
    if (deepEqualJson(entries, directoryFacts)) return
    if (directory === undefined) {
      directory = ctx.llm.registerConfigurableProviders(entries)
    } else {
      directory.replace(entries)
    }
    directoryFacts = entries
  }
  ensureDirectory()

  // ── 路由注册:profiles 的路由集 → adapter 路由,原子替换 ──
  let registration: { replace(routes: string[]): void } | undefined
  let registeredFacts: unknown
  const ensureRegistration = (): void => {
    const facts = [...profiles().keys()].sort()
    if (deepEqualJson(facts, registeredFacts)) return
    if (registration === undefined) {
      if (facts.length === 0) {
        registeredFacts = facts
        return
      }
      registration = ctx.llm.registerAdapter(facts, adapter)
    } else {
      registration.replace(facts)
    }
    registeredFacts = facts
  }
  ensureRegistration()

  // ── settings section:组合基底 + 用户层合并,热重载 ──
  installSettingsSection(ctx, NS, Config, {}, {
    setSource: (source: () => Config) => {
      const base = current
      current = () => ({ ...(base() as Config), ...source() })
    },
    onChange: () => {
      ensureDirectory()
      ensureRegistration()
    },
  })
}

function safeJson(value: unknown): unknown {
  try {
    return JSON.parse(JSON.stringify(value ?? null))
  } catch {
    return { unserializable: true }
  }
}

export default { name, inject, apply }
