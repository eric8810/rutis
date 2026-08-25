# 工作文档:记忆指针(memory pointer)——让模型感知自己在继续历史

> 开始:2026-08-25。来源:用户敏锐指出"session 恢复(历史进 prompt)≠ 模型
> 自动引用历史"——机制生效、认知不生效。本实现直接回应此痛点,也是
> 使命"迭代能力最强 / 环境适应性最佳"的核心一环。

## 目的
恢复的 session(generation > 1,跨进程/代际)在 system prompt 尾部附加
一段**记忆指针**,显式告知模型:
- 你在继续一段历史对话(第 N 代,identity=X)
- 以上是之前的完整消息,视为你的记忆
- 回答时引用它,不要重新询问、不要假装失忆

## 实现
`crates/rutis-agent/src/driver.rs::run_loop` prompt 组装处:
- 仅在 `session.id().generation() > 1`(恢复的 session)时附加。
- 全新 session(generation = 1)不加——连续对话自然连续,无需提示。
- 附加在 system prompt 尾部(与 persona 并存,不覆盖)。

## 为什么 generation > 1 而不是"有历史"
- 首次 followup 时 session 已含刚 push 的 user 消息,非空;
  "有历史" 无法区分"全新对话"与"恢复对话"。
- generation > 1 精确表示"跨进程/代际恢复"——这才是需要提示的场景。

## 测试(session_persist.rs,12 个全绿)
- `restored_session_carries_memory_pointer`:第二代恢复,第一轮 prompt
  的 system 含"记忆指针" + "第 2 代" + "identity="。
- `fresh_session_never_has_memory_pointer`:全新 session 两轮均无指针。

## 验收
- cargo test -p rutis-agent 全绿(新增 2 测试 + 原 10 测试)。
- 行为:冷启动恢复后,模型被显式告知"你在继续历史"。

## 边界
- 指针是 system prompt 静态注入,不随 turn 变化(同一代内稳定)。
- 未做:指针随历史长度动态调整(超长历史需压缩/摘要,后续)。
- 未做:模型实际"使用"记忆指针的效果度量(需真实 LLM 冒烟,后续)。
