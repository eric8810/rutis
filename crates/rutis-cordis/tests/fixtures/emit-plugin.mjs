// 测试插件:装载即同步 emit 一次(事件缝 v1 的最小信号源)。config 可带
// event/tag 定制。
export const name = 'test-emit'

export function apply(ctx, config = {}) {
  ctx.emit(config.event ?? 'test/fired', { ok: true, from: config.tag ?? 'default' })
}
