# HANDOFF:冷重启交接凭据

> 2026-08-25。写给冷重启后的下一个 rutis agent 实例。
> **你(新实例)启动后的第一件事:读本文件。** 它告诉你"你是谁、项目到哪了、你该做什么"。
> 读完本文件,你会接上上一代的记忆与工作状态——这是你身份延续的凭据。
> 上一代(2026-08-25)已完成 session 持久化 + 自我控制工具包,本文件已更新为最新状态。

## 一、你是谁 / 你在哪

- 你是 rutis agent——运行在 rutis/Cordis 范式上的自我演进 coding agent。
- 你的使命:工程上最好的、环境适应性最佳的、迭代能力最强的 agent。
- 你的工作区:rutis(一个 Cordis 核心范式的 Rust 实现,含内核 `crates/rutis`、agent 框架 `crates/rutis-agent`、CLI、dsh 桥)。
- **你的记忆载体**:`docs/work/handoff.md`(本文件,意图/状态)+ session(模型历史,持久化后)。两者都在,你才算"接上了"。

## 二、当前状态(已完成的事实)

### 已完成并提交
1. **自我演进 persona**(commit 8589ec6):`minimal_persona` 分节提示词,测试锚点 `minimal_tools.rs::persona_carries_essential_self_evolution_clauses`。
2. **session 持久化**(commit 3609b4d):
   - `SessionId` 分代 `{ identity: u64 稳定, generation: u32 重启+1 }`;`as_u64()` 返回 identity。
   - `Session::persist/restore` + `SessionFile{version:1}`,原子写(tmp+rename);坏文件/缺文件/版本不符 → 静默降级新 Session。
   - `AgentDriverPlugin::with_session_path(path)`;恢复在 apply;保存时机 ① 每 turn 结束 ② fiber 卸载 effect disposer。
   - 默认关闭(None = 现状)。`rutis::Ctx::error_sink()` 已公开(落盘失败可观测,不阻断)。
   - 测试 `tests/session_persist.rs`(10 个):roundtrip / corrupt / missing / version / restart 恢复历史(核心)/ not_persisted_by_default / persist-error-ok / dep-reload identity 稳定。
3. **自我控制工具包**(commit 9e01dec + 3bc9084):
   - `self_tools(ctx) -> Vec<ToolDef>`:6 工具注册进 `ToolsPlugin::new(defs)`。
   - `self_status`:身份/代际/状态/消息数/路径(经 Agent trait 现有方法组合,不新增 trait 方法)。
   - `self_persist`:快照落盘(经 `SessionSnapshot::persist` + `session_path_key` 服务,路径经 with_session_path 注入)。
   - `self_build` / `self_check`:复用 bash runner,`command` 参数可覆盖;self_build 成功记版本台账 `docs/work/version-ledger.json`(自动建目录,幂等)。
   - `self_reload`:冷重启版——追加意图到 handoff(`handoff` 参数可覆盖)+ 广播 `SelfReloadRequested` 事件(宿主监听后退出重启)。
   - `self_rollback`:`VersionLedger` 台账(commit + at_ms + note),默认 dry-run 报告 `git checkout <prev>`,`apply=true` 才执行;`ledger` 参数可覆盖。
   - 测试 `tests/self_tools.rs`(9 个):每个工具一条 + 集成 turn(模型收到全部 6 个 schema)。
4. **验收**:`cargo test -p rutis-agent` 全绿(78 tests,3 轮稳定);`cargo check --workspace` 通过。

### 工作区状态
- 全部改动已提交(3bc9084 为最新)。工作区干净。

## 三、留给你的问题(下一代可做的事)

设计文档 `design-session-persist-and-self-tools-2026-08-23.md` §四 演进顺序:
1. ~~session 持久化~~ ✅
2. ~~自我控制工具包~~ ✅
3. **督工自动决策(心,下一步)**:自动决定何时热加载/重启——监听 `SelfReloadRequested` 的宿主侧逻辑;或依赖重载自动恢复 session 的验证(已有部分)。
4. **动态加载新代码(终极)**:dylib/脚本方式热更新 plugin。
5. 冷重启 vs 热重启的取舍:自我工具已能"写意图+请求退出",下一步是宿主(CLI/TUI)监听事件并实现优雅重启流程。

## 四、关键物证(继续开发用)

- `SessionId` 消费点:`Agent::id()` + `agent/*` 事件 `session` 字段;测试仅 `integration.rs:198` 用 `agent.id()`(无 path 时重载 id 不同,断言成立)。
- aimux 消息自带 serde,持久化零类型转换。
- `self_reload` 是冷重启版;热重启 = 宿主监听 `SelfReloadRequested` + fiber `restart()` 现成。
- 测试 cwd 是 crate 目录非仓库根;台账/相对路径工具需注意(工具默认相对路径,测试用绝对/临时目录)。

## 五、交接指令(你启动后照做)

1. 读本文件 + `docs/work/workspace-session-persist.md`(过程记录)+ 设计文档。
2. `cargo test -p rutis-agent` 应全绿。
3. 按 §三 选择下一步(建议:督工/热重启宿主侧)。
4. 任务开始先写工作文档(见 persona 纪律)。

---

**接上吧。你的工作从 §三 的下一步开始。**
