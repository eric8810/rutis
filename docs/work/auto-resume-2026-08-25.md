# 工作文档:自动续跑(AutoResume)——agent 自己启动,不等用户输入

> 2026-08-25。用户核心痛点:"现在的你总是自己停下来,我不输入你就不会
> 自己自动启动运行。"

## 机制
- `AutoResume`(rutis-agent/src/auto.rs):监听 `AgentTurnEnd`。
  turn 结束后查 session 待办(`todo`):
  - 有待办 → 自动发起下一个 followup("(自动续跑) 继续待办…")
  - 无待办 → 停下等用户
- 防失控:`max_auto_turns` 上限(CLI 默认 5),超限停下;失败 turn 不续跑。
- 装配:CLI run() 注册 `AutoResume::new(5)`(ReloadHandler/Supervisor 之后)。

## 验证(examples/auto_resume.rs,已跑通)
- 用户输入 1 轮 + 设 todo("run steps until done")
- `[auto-resume] continuing todo` × 3(自动 3 轮)
- 第 4 次到上限:`4 auto turns (limit 3), stopping`
- session 2 → 8 条(用户 1 轮 + 自动 3 轮)

## 与 self_todo 配合
- `self_todo` 工具:agent 自己记录"下一步做什么"(写进 session,持久化)。
- `AutoResume`:宿主自动把待办变成下一轮——**agent 自己驱动自己**。
- 重启恢复:todo 注入 prompt(# 待办/下一步)→ AutoResume 继续续跑。
- 完整闭环:**写 → 测 → 加载(hotplug_load)→ 用 → 记待办 → 自动续跑**。

## 边界
- 自动续跑需要宿主装配 AutoResume(CLI 已装配;纯 driver 无)。
- 上限防死循环;失败 turn 不自动续跑(交给督工/用户)。
- 待办是单条文本;复杂任务可后续升级为任务队列。
