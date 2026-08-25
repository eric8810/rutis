//! rutis-agent:基于 rutis 五支柱的最小 agent 框架(M3)。
//!
//! 设计:`docs/design-min-agent-2026-08-18.md`(要素与形态)+
//! `docs/design-agent-verification-tui-2026-08-18.md`(验证与 TUI)。
//! 一句话定义:**一个 aimux [`LanguageModel`](aimux_core::language_model) 服务
//! + 一个 [`ToolRegistry`] 插件 + 一个实现 [`Agent`] 接口的 driver 插件
//! + 一个 [`Session`](连续 loop 的事实源,可选持久化:重启恢复历史)。**
//!
//! - **LLM 后端**是服务不是插件:aimux 已实现 seam(329 provider),
//!   直接 `root.provide_as(llm_key(), model)`——不写 `LlmPlugin` 空壳。
//! - **[`ToolsPlugin`]** 提供 `ToolRegistry` 服务:schema 直接产 aimux
//!   `FunctionTool` 进 `CallOptions.tools`;runner 失败转 `error: ...`
//!   回喂模型,panic 任务边界,不崩循环。
//! - **[`AgentDriverPlugin`]** 双门控(`injects = [llm, tools]`),就绪后
//!   提供 `dyn Agent`;fiber 卸载经 `ctx.cancelled()` 级联取消当前 turn。
//! - **[`Session`]** 只存模型可见消息(一层,无事件→投影两层):多轮
//!   `followup` 之间 history 连续;session 是事实源。可选持久化
//!   (`AgentDriverPlugin::with_session_path`):重启后 history 恢复,
//!   identity 稳定、generation 递增;默认关闭(纯内存,现状不变)。
//! - **turn 过程经 EventBus 广播**:`followup` 返回终态,文本增量 /
//!   工具调用 / 工具结果 / turn 终态 emit 到 [`events`] 的 `agent/*`
//!   事件(`AgentTextDelta` 等)——任何观察方订阅事件,不独占 stream;
//!   监听器随注册方 fiber 卸载(D28)。
//! - **loop 关键节点走 waterfall**(设计 §四.1):`agent/pre-step`
//!   (改写/拒绝进入这步的 messages)+ 工具三段 `tools/pre-execute`
//!   (门控)/ 执行 / `tools/post-execute`(结果决策,失败也到这);
//!   默认行为即原行为,插件挂 `on_waterfall` 中间件可改写、可 veto
//!   ——框架自己吃狗粮。
//! - **[`TuiPlugin`]**:ratatui+crossterm 前端,`agent/*` 事件监听器,
//!   不是 loop 的一部分。
//!
//! 验证三层(验证文档 §一):单元([`ScriptedLlm`] 实现真
//! `LanguageModel`,按序弹出)+ 集成(aimux `MockReplayModel` 录制回放,
//! 双门控 / 依赖驱动驱逐 / fiber 卸载取消)+ 真实端到端
//! (`examples/demo.rs`、`examples/tui.rs`,`#[ignore]` 测试手动触发)。
//!
//! ```no_run
//! # async fn doc() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use rutis::Ctx;
//! use rutis_agent::{agent_key, llm_key, Agent, AgentDriverPlugin, AgentTextDelta, ToolsPlugin};
//!
//! let root = Ctx::root()?;
//! let llm: Arc<dyn aimux_core::LanguageModel> = /* aimux provider 或 ScriptedLlm */ unimplemented!();
//! root.provide_as(llm_key(), llm)?;
//! root.plugin(ToolsPlugin::new(vec![]));
//! let driver_view = root.plugin(AgentDriverPlugin::new(16));
//! (&driver_view).await.expect("driver loads (gated on llm+tools)");
//!
//! // 观察方:订阅 agent/* 事件,过程增量逐块到达
//! # struct L;
//! # impl rutis::Listener<AgentTextDelta> for L {
//! #     fn call<'a>(&'a self, _c: &'a rutis::Ctx, e: &'a AgentTextDelta)
//! #         -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
//! #         print!("{}", e.delta); Box::pin(async { Ok(None) })
//! #     }
//! # }
//! root.events().on(&root, L)?;
//!
//! let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
//! let answer = agent.followup("weather in Oslo?").await?; // 终态
//! # Ok(())
//! # }
//! ```
#![allow(clippy::type_complexity)]

mod agent;
mod driver;
mod events;
mod minimal;
mod scripted;
mod session;
mod tools;
mod tui;

pub use agent::{agent_key, Agent, AgentError, AgentStatus, SessionSnapshot};
pub use driver::{llm_key, session_path_key, AgentDriver, AgentDriverPlugin};
pub use events::{
    AgentPreStep, AgentStepEvent, AgentTextDelta, AgentToolCall, AgentToolResult, AgentTurnEnd,
    SelfReloadRequested, ToolPostExecute, ToolPreExecute,
};
pub use minimal::{minimal_persona, minimal_tools};
pub use scripted::{into_service, tool_call, LlmResponse, ScriptedCall, ScriptedLlm};
pub use session::{Session, SessionId};
pub use tools::bash::bash_tool;
pub use tools::replace_text::replace_text_tool;
pub use tools::self_tools::{
    self_build, self_check, self_persist, self_reload, self_rollback_tool, self_status, self_tools,
    VersionLedger, VERSION_LEDGER_PATH,
};
pub use tools::{tools_key, ToolDef, ToolOutput, ToolRegistry, ToolsPlugin};
pub use tui::TuiPlugin;
