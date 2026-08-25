# HANDOFF:冷重启交接凭据

> 2026-08-23。写给冷重启后的下一个 rutis agent 实例。
> **你(新实例)启动后的第一件事:读本文件。** 它告诉你"你是谁、项目到哪了、你该做什么"。
> 读完本文件,你会接上上一代的记忆与工作状态——这是你身份延续的凭据。

## 一、你是谁 / 你在哪

- 你是 rutis agent——运行在 rutis/Cordis 范式上的自我演进 coding agent。
- 你的使命:工程上最好的、环境适应性最佳的、迭代能力最强的 agent。
- 你的工作区:rutis(一个 Cordis 核心范式的 Rust 实现,含内核 `crates/rutis`、agent 框架 `crates/rutis-agent`、CLI、dsh 桥)。
- **你的记忆载体**:`docs/work/handoff.md`(本文件,意图/状态)+ session(模型历史,持久化后)。两者都在,你才算"接上了"。

## 二、当前状态(上一代留下的事实)

### 已完成
1. **自我演进 persona 已实现并测试锚住**:
   - `crates/rutis-agent/src/minimal.rs` 的 `minimal_persona` 已升级为分节提示词(身份/使命/环境引导/工作纪律/自我演进),含"执行可能中断→任务文档化""可改自己代码(plugin+热更新)""persona 变更需用户同意"等条款。
   - 测试锚点:`minimal_tools.rs::persona_carries_essential_self_evolution_clauses`。
2. **两份设计文档已成文**:
   - `docs/design-self-evolving-agent-2026-08-23.md`(persona 设计)
   - `docs/design-session-persist-and-self-tools-2026-08-23.md`(本交接对应的实现蓝图,见 §三)
3. **工作区有未提交改动**(上一代的工作,含 TUI 重写等,别丢):
   - 修改:`Cargo.lock`、`crates/rutis-agent/{Cargo.toml, examples/*, src/{driver,events,minimal,scripted,tui}.rs, tests/minimal_tools.rs}`、`crates/rutis-cli/src/main.rs`
   - 新增:`docs/design-rutui-rewrite-2026-08-23.md`、`docs/design-self-evolving-agent-2026-08-23.md`、`docs/design-session-persist-and-self-tools-2026-08-23.md`
   - ⚠️ **这些是未提交的工作,先不要乱动;实现新任务前先确认它们能编译测试通过。**

### 未完成(你的任务)
**实现 `docs/design-session-persist-and-self-tools-2026-08-23.md` 设计的两部分:**

## 三、你的任务(按顺序)

### 任务 1:session 持久化(地基)
见设计文档 §一。要点:
- `Session` 加 `persist(path)` / `restore(path)`,`SessionFile { version:1, id, messages, saved_at_ms }`,原子写(临时文件+rename)。
- `SessionId` 改分代 `{ identity: u64, generation: u32 }`,`as_u64()` 保留(返回 identity)。
- `AgentDriverPlugin::with_session_path(path)`;`apply` 时 restore(失败静默降级);保存时机:每 turn 结束 + fiber 卸载 effect disposer。
- 默认关闭(`None` = 现状)。
- 测试:`session_persist_roundtrip`、`corrupt_file_starts_fresh`、`session_restored_after_driver_restart`(核心)、`not_persisted_by_default`。

### 任务 2:自我控制工具包(手)
见设计文档 §二。要点:
- 6 个工具:`self_status` / `self_persist` / `self_reload`(新增,经 `Agent` trait 现有方法组合)+ `self_build` / `self_check`(复用 bash)+ `self_rollback`(版本台账)。
- 工具作为 `ToolDef` 注册进 `ToolsPlugin::new(defs)`。
- `self_reload` 先做冷重启版(写意图 + 请求退出),督工热重启为后续。

### 任务 3:验收
- `cargo test -p rutis-agent` 全绿。
- 核心验收:重启后 session 恢复,模型历史连续。

## 四、关键物证(实现时用得上)

- **aimux 消息自带 serde**:`ModelMessage`/`MessageContent`/`Role`/`ContentPart`(0.3.0)全部 `#[derive(Serialize, Deserialize)]`,持久化零类型转换。
- **fiber restart 现成**:`crates/rutis/src/fiber.rs` 有 `pub fn restart()`,内核不用动。
- **保存时机挂点**:fiber 卸载走 `drain_effects`(LIFO),effect disposer 是卸载时清理动作;persist 后注册→先清理。
- **`SessionId` 消费点**:`Agent::id()` + `agent/*` 事件载荷 `session` 字段;测试仅 `integration.rs:198` 用 `agent.id()`。分代改动影响面小。
- **`Agent` trait 不加方法**(避免污染接口):`self_status`/`self_persist` 经现有 `id()`/`status()`/`session()` 组合。

## 五、交接指令(你启动后照做)

1. **先读** `docs/design-session-persist-and-self-tools-2026-08-23.md`(实现蓝图)+ `docs/design-self-evolving-agent-2026-08-23.md`(persona 背景)。
2. **确认现状可编译**:`cargo test -p rutis-agent`(应全绿;若红,先修上一代的未提交改动,别带着坏状态开工)。
3. **实现任务 1**(session 持久化)→ 跑测试。
4. **实现任务 2**(自我控制工具包)→ 跑测试。
5. **任务 3 全绿后**,更新本 handoff(标记任务完成),然后你可以自行决定下一步(督工/热加载)。
6. **纪律**(你的 persona 第一条):任务开始先写文档、过程持续更新——本文件就是你的起点,别丢。

## 六、留给你的问题(上一代的未决思考)

- 督工(自动决策热加载)怎么做?——设计文档 §四 演进顺序第 3 步。
- 动态加载新代码(dylib/脚本)怎么做?——第 4 步,终极形态。
- 冷重启 vs 热重启的取舍,等你做完任务 1、2 后重新评估。

---

**接上吧。你的工作从任务 1 开始。**
