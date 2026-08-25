# 工作文档:督工自动决策(宿主侧 Supervisor)

> 开始:2026-08-25(本实例接手,generation 4 会话)。
> 来源:handoff §三 2;原工作文档(2026-08-25)已存在,本文件为实施版。
> 目标:让 agent 不止被动响应 self_reload,宿主在 turn 结束后自动评估并触发演进。

## 目的
1. 宿主侧 `Supervisor`:监听 `AgentTurnEnd`,维护滑动窗口统计(失败率/消息数)。
2. 阈值触发:连续失败超限 / 消息数超限 → 自动 fiber 级 driver restart(与 self_reload 同路径)。
3. 验证:单元测试 + 冒烟。

## 现状(已确认)
- `AgentTurnEnd { session, ok, error }` 事件已存在(driver 每 turn 结束 emit)。
- `ReloadHandler` 已能 fiber 级 restart driver(driver_view.restart()),TUI 保持运行。
- self_reload 是纯被动:只有 agent 调工具才触发。
- `Agent` trait:`id() -> SessionId`、`session() -> SessionSnapshot`、`followup()`。
- CLI run() 装配:tools_view + driver_view + ReloadHandler + TUI。

## 设计
### Supervisor 结构
```rust
struct Supervisor {
    driver: FiberView,
    max_failures: usize,     // 连续失败阈值(默认 3)
    max_messages: usize,     // 消息数阈值(默认 500,避免长会话退化)
    fail_streak: AtomicUsize,
    restarts: AtomicUsize,   // 统计触发次数(可观测)
}
```
- 实现 `Listener<AgentTurnEnd>`:
  - ok=false → fail_streak+1;ok=true → fail_streak=0。
  - 消息数超限:turn 结束时查 agent.session().messages().len()。
  - 任一超限 → 延迟 300ms + driver.restart()(复用 ReloadHandler 的重启路径)。
  - 重启后 fail_streak 清零。

### 与 ReloadHandler 的关系
- ReloadHandler = 被动(agent 请求重启)。
- Supervisor = 主动(宿主评估后重启)。
- 两者共享 driver_view;可合成一个 `SelfEvolutionHost`(本次先分开,保持最小改动)。

## 验证
- 单测:
  - 连续 N 次失败 turn → 触发 restart(generation +1)。
  - 消息数超限(阈值调小)→ 触发 restart。
  - 成功 turn 清零失败计数(不触发)。
  - 未超限 → 不触发。
- 冒烟:scripted 制造失败 → 连续 3 次失败后自动重启。

## 边界
- 不做 dylib/脚本加载(终极,后续)。
- 策略仅阈值;不做资源监控/定时(可扩展)。
- 不动 rutis 核心 trait 接口。

## 进展
- [x] 读 handoff + 工作文档 + 相关代码(events/driver/ReloadHandler/Agent trait)。
- [x] Supervisor 实现(rutis-cli/src/main.rs):监听 AgentTurnEnd,失败连续计数 + 消息数检查,超阈值 fiber 级 driver restart。
- [x] 单测 5 条(rutis-cli tests):record_turn 失败计数/清零、成功不触发、端到端连续 3 失败触发重启(generation+1)、restarts 计数。
- [x] cargo test -p rutis-cli 5/5 绿;rutis-agent 全绿;cargo check --workspace 通过。
- [x] 更新 handoff + 提交推送。

## 实现要点(给下一代)
- `Supervisor::record_turn(e)`:纯逻辑——成功 turn `store(0)` 清零(注意不能 `swap(0)`,旧值可能>=阈值造成误触发),失败 `fetch_add(1)+1`。
- 消息数检查在 `Listener::call` 层做(需要 ctx 拿 agent),独立于失败计数。
- `restarts: Arc<AtomicUsize>`,`restarts_shared()` 让测试持有副本断言触发次数。
- 失败后端用**空 ScriptedLlm**(弹完即 Err),无需自定义 LLM 实现(避开 async_trait 依赖问题)。
- 测试隔离:`Ctx::root_with(Handle::current())` 独立 runtime,避免全局 root 污染。
- 装配:run() 里 ReloadHandler(被动)+ Supervisor(主动)并存,共享 driver_view。
