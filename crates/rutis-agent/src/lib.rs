//! rutis-agent:基于 rutis 五支柱的最小 agent 框架(M3)。
//!
//! 设计:`docs/design-min-agent-2026-08-18.md`(要素与形态)+
//! `docs/design-agent-verification-tui-2026-08-18.md`(验证与 TUI)。
//! 一句话定义:**一个 aimux [`LanguageModel`](aimux_core::language_model) 服务
//! + 一个 [`ToolRegistry`] 插件 + 一个实现 [`Agent`] 接口的 driver 插件
//! + 一个内存 [`Session`](连续 loop 的事实源)。**
//!
//! - **LLM 后端**是服务不是插件:aimux 已实现 seam(329 provider),
//!   直接 `root.provide_as(llm_key(), model)`——不写 `LlmPlugin` 空壳。
//! - **[`ToolsPlugin`]** 提供 `ToolRegistry` 服务:schema 直接产 aimux
//!   `FunctionTool` 进 `CallOptions.tools`;runner 失败转 `error: ...`
//!   回喂模型,panic 任务边界,不崩循环。
//! - **[`AgentDriverPlugin`]** 双门控(`injects = [llm, tools]`),就绪后
//!   提供 `dyn Agent`;fiber 卸载经 `ctx.cancelled()` 级联取消当前 turn。
//! - **[`Session`]** 只存模型可见消息(一层,无事件→投影两层):多轮
//!   `followup` 之间 history 连续;流是视图,session 是事实源。
//! - **流式是第一性需求**:`followup` 返回 `BoxStream<TurnEvent>`
//!   (`TextDelta` 逐块 / 工具调用边界 / `Done` 终态),TUI 逐字消费。
//! - **[`TuiPlugin`]**:ratatui+crossterm 前端,消费 `Agent` 服务,
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
//! use futures::StreamExt;
//! use rutis::Ctx;
//! use rutis_agent::{agent_key, llm_key, Agent, AgentDriverPlugin, ToolsPlugin};
//!
//! let root = Ctx::root()?;
//! let llm: Arc<dyn aimux_core::LanguageModel> = /* aimux provider 或 ScriptedLlm */ unimplemented!();
//! root.provide_as(llm_key(), llm)?;
//! root.plugin(ToolsPlugin::new(vec![]));
//! let driver_view = root.plugin(AgentDriverPlugin::new(16));
//! (&driver_view).await.expect("driver loads (gated on llm+tools)");
//!
//! let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
//! let mut stream = agent.followup("weather in Oslo?");
//! while let Some(ev) = stream.next().await { /* TextDelta / ToolCall / ToolResult / Done */ }
//! # Ok(())
//! # }
//! ```

#![allow(clippy::type_complexity)]

mod agent;
mod driver;
mod scripted;
mod session;
mod tools;
mod tui;

pub use agent::{agent_key, Agent, AgentError, AgentStatus, SessionSnapshot, TurnEvent};
pub use driver::{llm_key, AgentDriver, AgentDriverPlugin, AgentStepEvent, AgentToolEvent};
pub use scripted::{into_service, tool_call, LlmResponse, ScriptedCall, ScriptedLlm};
pub use session::{Session, SessionId};
pub use tools::{tools_key, ToolDef, ToolOutput, ToolRegistry, ToolsPlugin};
pub use tui::TuiPlugin;
