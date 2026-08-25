# 工作文档:session 持久化 + 自我控制工具包

> 开始:2026-08-25(实例接手)。来源:docs/work/handoff.md 任务 1→2→3。

## 目的
实现 `docs/design-session-persist-and-self-tools-2026-08-23.md`:
1. session 持久化(重启不丢记忆,agent.id() 稳定)
2. 自我控制工具包(6 工具:self_status/self_persist/self_reload/self_build/self_check/self_rollback)
3. 验收:cargo test -p rutis-agent 全绿 + 核心验收(重启后 session 恢复)

## 现状(已确认)
- 上一代未提交改动全绿:59 tests 通过(7 套件)。
- Session = { id: u64 全局自增, messages: Vec<ModelMessage> },纯内存。
- SessionId 消费点:Agent::id()、agent/* 事件 session 字段、integration.rs:198。
- aimux 消息全部 derive serde(物证已读 message.rs/content.rs)。
- rutis Effect::AsyncDisposer 是 fiber 卸载清理挂点;FiberView::restart() 现成。
- AgentDriver::new 已收 system_prompt 参数;需要加 session_path。

## 计划
### 任务 1:session 持久化
- [x] 读设计文档 + 现状代码
- [x] session.rs:SessionId 分代 {identity, generation} + persist/restore + SessionFile
- [x] driver.rs:with_session_path + apply restore + turn 结束 persist + effect disposer persist
- [x] 测试:session_persist_roundtrip / corrupt_file_starts_fresh / session_restored_after_driver_restart / not_persisted_by_default + missing/version/error-break
### 任务 2:自我控制工具包
- [ ] tools/self.rs:self_status/self_persist/self_reload + self_build/self_check(复用 bash)+ self_rollback
- [ ] 挂载:minimal_tools 或新 defs 入口;每个工具一条 scripted 测试
### 任务 3:验收
- [ ] cargo test -p rutis-agent 全绿
- [ ] 核心验收:重启后 session 恢复,模型历史连续
- [ ] 更新 handoff.md

## 关键决策
- 保存时机:① followup 返回前(每 turn 结束)② fiber 卸载 effect disposer(LIFO 后注册先清理)
- 恢复失败静默降级(新 Session),不阻断启动
- SessionId::as_u64 保留返回 identity,兼容现有消费点
- self_reload 先做冷重启版(写意图+退出),督工热重启后续

## 进展
- [2026-08-25] 接手,读设计文档×2、handoff、现状代码,确认全绿。开始写本工作文档。

## 进展(任务 1 完成)
- [2026-08-25] 任务 1 完成:9 个 session 持久化测试全绿,全量 68 tests 通过。
  - SessionId 分代 {identity, generation},as_u64 返回 identity,restored() generation+1。
  - Session::persist 原子写(tmp+rename)/ restore 失败静默降级。
  - AgentDriverPlugin::with_session_path;保存时机 ① followup 末尾 ② fiber 卸载 AsyncDisposer。
  - rutis Ctx::error_sink() 由 pub(crate) 改为 pub(落盘失败可观测,不静默)。
  - 注意:依赖重载无 path 时 identity 仍全局自增(integration.rs:198 assert_ne 成立)。
