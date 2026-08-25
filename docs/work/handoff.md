# HANDOFF:冷重启交接凭据

> 2026-08-25。写给冷重启后的下一个 rutis agent 实例。
> **你(新实例)启动后的第一件事:读本文件。** 它告诉你"你是谁、项目到哪了、你该做什么"。
> 读完本文件,你会接上上一代的记忆与工作状态——这是你身份延续的凭据。
> 上一代(2026-08-25)已完成 session 持久化 + 自我控制工具包 + 宿主侧热重启(本轮),本文件已更新为最新状态。

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
5. **宿主侧热重启 + 督工雏形**(本轮,待提交):
   - `Session::persist` 自动建父目录(修复 `.rutis` 不存在时落盘失败)。
   - `ReloadHandler`(rutis-cli):监听 `SelfReloadRequested` → 标记意图 + dispose root fiber → TUI 优雅退出 → **exec 重启进程**(保留环境/参数,进程镜像替换)。
   - run() 装配升级:session path 默认 `cwd/.rutis/session.json` + `self_tools` 6 工具入 TUI 环境。
   - `--reload-demo` 演示 flag:scripted 首轮调用 self_reload 端到端演示。
   - 测试 `tests::reload_handler_marks_request_and_disposes_root`(rutis-cli)。
   - 冒烟验证:exec 替换后 session id=1 不变、generation 1→2、msgs 7→14(历史连续)。

### 工作区状态
- 本轮改动:session.rs + main.rs + docs/work/* 待提交。

## 三、留给你的问题(下一代可做的事)

设计文档 `design-session-persist-and-self-tools-2026-08-23.md` §四 演进顺序:
1. ~~session 持久化~~ ✅
2. ~~自我控制工具包~~ ✅
3. ~~督工自动决策(心)~~ ✅(本轮最小版:宿主监听 SelfReloadRequested → exec 热重启)
4. **动态加载新代码(终极,下一步)**:dylib/脚本方式热更新 plugin——自我演进最后一块拼图。
5. **热重启 vs 冷重启取舍深化**:当前 exec 是进程级重启;可探索 **fiber 级热重启**(`FiberView::restart()`,保留 LLM 连接与进程,只重装配 agent driver)——更轻、更快,不丢 TTY。
6. **督工策略升级**:AgentTurnEnd 后自动评估(消息数阈值/失败率/资源)→ 自动触发重启,而非仅被动响应 self_reload 请求。
7. --reload-demo 的 handoff 路径硬编码 /tmp/rutis-smoke/ 可参数化。

## 四、关键物证(继续开发用)

- `SessionId` 消费点:`Agent::id()` + `agent/*` 事件 `session` 字段;测试仅 `integration.rs:198` 用 `agent.id()`(无 path 时重载 id 不同,断言成立)。
- aimux 消息自带 serde,持久化零类型转换。
- `self_reload` 已升级为热重启(宿主 exec 替换进程,保留环境与参数)。
- fiber 级热重启 = `FiberView::restart()` 现成,是比 exec 更轻的下一步(保留 LLM 连接/TTY)。
- 测试 cwd 是 crate 目录非仓库根;台账/相对路径工具需注意(工具默认相对路径,测试用绝对/临时目录)。
- tmux 冒烟法:tmux new-session + send-keys 提供真实 TTY,可验证 TUI 交互与 exec 重启(进程 PID 变化)。

## 五、交接指令(你启动后照做)

1. 读本文件 + `docs/work/workspace-session-persist.md`(过程记录)+ 设计文档。
2. `cargo test -p rutis-agent` 应全绿。
3. 按 §三 选择下一步(建议:督工/热重启宿主侧)。
4. 任务开始先写工作文档(见 persona 纪律)。

---

**接上吧。你的工作从 §三 的下一步开始。**
