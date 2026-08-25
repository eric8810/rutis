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
- [x] tools/self_tools.rs:6 工具(self_status/self_persist/self_reload/self_build/self_check/self_rollback)+ VersionLedger
- [x] 挂载:self_tools(ctx) 返回 Vec<ToolDef>;events 加 SelfReloadRequested;9 条测试
### 任务 3:验收
- [x] cargo test -p rutis-agent 全绿(78 tests,3 轮稳定)
- [x] 核心验收:重启后 session 恢复,模型历史连续(session_restored_after_driver_restart + dependency_reload_keeps_identity_when_persisted)
- [x] 更新 handoff.md(见下一步)

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

## 进展(任务 2 完成)
- [2026-08-25] 任务 2 完成:9 个 self 工具测试全绿,全量 77 tests 通过(0.01s self 套件,不再真跑 cargo)。
  - 6 工具:self_status(身份/代际/状态/消息数/路径)、self_persist(快照落盘,无路径报错)、
    self_build/self_check(复用 bash runner,command 可覆盖,self_build 成功记台账)、
    self_reload(写 handoff 意图 + 广播 SelfReloadRequested,handoff 路径可参数化)、
    self_rollback(VersionLedger 台账,dry-run 默认,apply=true 执行 git checkout)。
  - SessionSnapshot::persist 新增(供 self_persist 经 Agent::session() 组合,不污染 Agent trait)。
  - driver provide session_path_key 服务(路径经 with_session_path 注入)。
  - VersionLedger.save 自动建父目录;台账 docs/work/version-ledger.json。
- 卡点/解决:测试 cwd 是 crate 目录非仓库根 → 台账相对路径写 crate 目录,测试用 crate 相对路径+建目录;
  self_build/self_check 真跑 cargo 递归测试 172s+并行不稳 → command 参数覆盖,轻命令锚"复用 bash+台账"逻辑。

## 进展(任务 3 完成)
- [2026-08-25] 任务 3 验收完成:全量 78 tests 全绿 ×3 轮稳定,workspace check 通过。
  - 补充测试:dependency_reload_keeps_identity_when_persisted(有 path 时依赖重载 identity 稳定)。
  - self_rollback 支持 ledger 参数(路径可注入,测试用临时目录,消除与 self_build 共享台账的竞争)。
  - lib.rs 文档同步(Session 可选持久化)。
- 待办:更新 handoff.md 标记任务完成。
