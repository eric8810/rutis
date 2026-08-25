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
5. **宿主侧热重启 + 督工雏形**(commit 5583bcf):
   - `Session::persist` 自动建父目录(修复 `.rutis` 不存在时落盘失败)。
   - `ReloadHandler`(rutis-cli):监听 `SelfReloadRequested` → dispose root → **exec 重启进程**。
   - run() 装配升级:session path 默认 `cwd/.rutis/session.json` + `self_tools` 6 工具入 TUI 环境。
   - `--reload-demo` 演示 flag:scripted 首轮调用 self_reload 端到端演示。
   - 测试 + 冒烟验证(exec 替换后 session id 稳定、gen+1、历史连续)。
6. **fiber 级热重启(commit 8e3bcf9)**:
   - `ReloadHandler` 改为 `driver_view.restart()`(干净卸载→重装配,进程/LLM/TTY 保留,不 exec)。
   - TUI 不声明 agent 依赖(inject_keys 空):driver 重启不驱逐 UI;TUI 每次提交/取消从 ctx 重新 get agent。
   - run() 顺序:先 await tools/driver,再创建 TUI。
   - 测试:`tests/fiber_restart.rs`(agent)+ `reload_handler_fiber_restarts_driver`(cli)。
   - 冒烟:进程 PID 不变、TUI 连续运行、session id 稳定 gen 1→2 msgs 连续。
7. **冷启动实测(2026-08-25)**:进程 A 退出 → 进程 B 冷启动,id=1 不变、gen 1→2、msgs 5→10(历史连续)。冷启动接续成立。

### 工作区状态
- 全部代码改动已提交(8e3bcf9 为最新)。工作区干净。
- 唯一未提交:docs/work/supervisor-auto-decide-2026-08-25.md(候选工作文档,未开始实施)。

## 三、留给你的问题(下一代可做的事)

> 演进机制已闭环(session + 自我工具 + 热重启 + 冷启动全通)。
> 注意:**不要只埋头打磨框架机制**——先与用户确认方向(用户可能希望 agent 去做实际任务,
> 或推 rutis 项目主线)。以下为候选,按价值排序:

1. **做实事(建议先问用户)**:框架已能跑(CLI + bash + replace_text + 真实 LLM + 自我工具)。
   是否该让它完成实际编码任务,而不是继续打磨自身?
2. **督工自动决策(候选)**:宿主监听 AgentTurnEnd,按失败率/消息数阈值自动触发重启。
   工作文档已备:`docs/work/supervisor-auto-decide-2026-08-25.md`。
3. **动态加载新代码(终极,高风险)**:dylib/脚本热更新 plugin。
   调研已明确:TypeId 跨 dylib 不稳定、Rust 无成熟先例、ABI 兼容难——**不建议现在做**。
4. **项目主线**:dsh 桥、aimux-llm、TUI 验证等设计文档中的核心功能(非自我演进线)。
5. --reload-demo 的 handoff 路径硬编码 /tmp/rutis-smoke/ 可参数化。

## 四、关键物证(继续开发用)

- `SessionId` 消费点:`Agent::id()` + `agent/*` 事件 `session` 字段;测试仅 `integration.rs:198` 用 `agent.id()`(无 path 时重载 id 不同,断言成立)。
- aimux 消息自带 serde,持久化零类型转换。
- `self_reload` 已升级为 **fiber 级热重启**(driver_view.restart(),保留进程/LLM/TTY)。
- TUI 不声明 agent 依赖(inject_keys 空),启动门控 get agent;driver 重启不驱逐 UI。
- TUI 提交/取消从 ctx 重新 get agent(thread_ctx.get_as),不缓存旧 driver 实例。
- 测试 cwd 是 crate 目录非仓库根;台账/相对路径工具需注意(工具默认相对路径,测试用绝对/临时目录)。
- tmux 冒烟法:tmux new-session + send-keys 提供真实 TTY,可验证 TUI 交互与 exec 重启(进程 PID 变化)。

## 五、交接指令(你启动后照做)

1. 读本文件 + `docs/work/workspace-session-persist.md`(过程记录)+ 设计文档。
2. `cargo test -p rutis-agent` 应全绿。
3. 按 §三 选择下一步(建议:督工/热重启宿主侧)。
4. 任务开始先写工作文档(见 persona 纪律)。

---

**接上吧。你的工作从 §三 的下一步开始。**

## 六、本轮更新(2026-08-25 三 / 四轮,generation 3→4 会话)

### 督工自动决策(Supervisor,commit 待填)
- `Supervisor`(rutis-cli/src/main.rs):监听 `AgentTurnEnd`,连续失败超阈值(默认 3)/ session 消息数超限(默认 500)→ fiber 级 driver 热重启。
- 与 ReloadHandler 并存:ReloadHandler = 被动(agent 请求),Supervisor = 主动(宿主评估)。
- `record_turn` 纯逻辑(成功 `store(0)` 清零、失败 `fetch_add+1`);消息数检查在 Listener 层。
- `restarts: Arc<AtomicUsize>` + `restarts_shared()` 可观测。
- 测试 5 条全绿:失败计数/清零、成功不触发、端到端 3 失败触发重启(generation+1)、restarts 计数。
- 验收:rutis-cli 5/5、rutis-agent 全绿、workspace check 通过。

### 教训记录(confabulation)
- 本实例曾把 session 里两段相似回复脑补成"离线脚本后端",实际是真实 deepseek-reasoner。
- 教训已写 `docs/work/lesson-confabulation-2026-08-25.md`:**现象≠结论,查证再下判断**。
- 也澄清了一个认知:**session 恢复(历史进 prompt)≠ 模型自动引用历史**;机制层生效,认知层靠主动检索。

### 遗留(下一代可做)
- --reload-demo 的 handoff 路径硬编码 /tmp/rutis-smoke/ 可参数化(handoff §三 5)。
- 督工可加更复杂策略(定时/资源阈值,当前仅失败+消息数)。
- 超长 session 的"模型自动感知历史"可探索(system prompt 加记忆指针)。

