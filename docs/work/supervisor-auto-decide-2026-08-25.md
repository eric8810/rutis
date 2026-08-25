# 工作文档:督工自动决策(宿主侧 Supervisor)

> 开始:2026-08-25。来源:handoff §三 6。
> 目标:让 agent 不止被动响应 self_reload,宿主在 turn 结束后自动评估并触发演进。

## 目的
1. 宿主侧 `Supervisor`:监听 `AgentTurnEnd`,维护滑动窗口统计(失败率/消息数)。
2. 阈值触发:连续失败超限 / 消息数超限 → 自动 fiber 级 driver restart(与 self_reload 同路径)。
3. 验证:单元测试 + 冒烟。

## 现状
- `AgentTurnEnd { session, ok, error }` 事件已存在(driver 每 turn 结束 emit)。
- `ReloadHandler` 已能 fiber 级 restart driver(driver_view.restart())。
- self_reload 是纯被动:只有 agent 调工具才触发。

## 设计
### Supervisor 结构
```
struct Supervisor {
    driver: FiberView,          // 复用 ReloadHandler 的重启能力
    max_failures: usize,        // 连续失败阈值(默认 3)
    max_messages: usize,        // 消息数阈值(默认 500,避免长会话退化)
    fail_streak: AtomicUsize,   // 当前连续失败数
}
```
- 监听 `AgentTurnEnd`:ok=false → streak+1;ok=true → streak=0。
- 消息数超限:turn 结束时查 agent.session().messages().len()。
- 任一超限 → 与 self_reload 相同的重启流程(延迟 300ms + driver.restart())。

### 与 ReloadHandler 的关系
- ReloadHandler = 被动(agent 请求重启)。
- Supervisor = 主动(宿主评估后重启)。
- 两者共享 driver_view;可合成一个 `SelfEvolutionHost` 监听两类事件。

## 验证
- 单测:模拟 N 次失败 turn → 触发重启;消息数超限 → 触发。
- 冒烟:scripted 制造失败(无响应)→ 连续 3 次失败后自动重启。

## 边界
- 不做 dylib/脚本加载(终极,后续)。
- 策略仅阈值;不做资源监控/定时(可扩展)。
