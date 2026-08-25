//! `agent/*` 事件(设计 §三.3/§四.1)——turn 过程增量的唯一观察通道。
//!
//! 输出是广播:一次 turn 可被多方观察(TUI + 日志 + 未来前端),
//! 观察方晚订阅、只看不动都行;监听器归注册方 fiber 所有(D28),
//! 随 fiber 卸载。driver 侧每个增量 `emit`:
//!
//! - [`AgentTextDelta`][]:模型文本增量(流式逐块)
//! - [`AgentReasoning`][]:模型推理/reasoning 增量(流式逐块,渲染为 Thinking 块)
//! - [`AgentToolCall`][]:工具调用开始
//! - [`AgentToolResult`][]:工具结果(失败为 `error: ...` 回喂文本)
//! - [`AgentTurnEnd`][]:turn 终态(`ok=false` 时 `error` 为错误摘要)
//! - [`AgentStepEvent`][]:每步模型响应后的摘要(步号/内容/工具调用数)

use aimux_core::language_model_message::LanguageModelPromptMessage;
use aimux_core::options::Tool;
use aimux_core::tool::ToolCall;
use rutis::Event;
use serde_json::Value;

use crate::session::SessionId;
use crate::tools::ToolOutput;

/// 自我控制工具请求重启(`self_reload` 工具广播;宿主监听后
/// 优雅退出并重启进程——冷重启版,督工热重启为后续)。
#[derive(Debug, Clone)]
pub struct SelfReloadRequested {
    pub session: SessionId,
    pub reason: String,
    pub intent_path: String,
}

impl Event for SelfReloadRequested {
    const NAME: &'static str = "self/reload-requested";
    type Value = ();
}

/// 模型文本增量(流式逐块,经 EventBus 广播)。
#[derive(Debug, Clone)]
pub struct AgentTextDelta {
    pub session: SessionId,
    pub step: usize,
    pub delta: String,
}

impl Event for AgentTextDelta {
    const NAME: &'static str = "agent/text-delta";
    type Value = ();
}

/// 模型推理/思考(reasoning)增量(流式逐块,经 EventBus 广播)。
///
/// 由 driver 收到 `StreamPart::ReasoningDelta` 时发出;TUI 侧渲染为
/// rutui 的 `ThinkingBlock`(reasoning 折叠块),与正文 `AgentTextDelta`
/// 区分开。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AgentReasoning {
    pub session: SessionId,
    pub step: usize,
    pub delta: String,
}

impl Event for AgentReasoning {
    const NAME: &'static str = "agent/reasoning";
    type Value = ();
}

/// 工具调用开始。
#[derive(Debug, Clone)]
pub struct AgentToolCall {
    pub session: SessionId,
    pub name: String,
    pub args: Value,
}

impl Event for AgentToolCall {
    const NAME: &'static str = "agent/tool-call";
    type Value = ();
}

/// 工具结果(ok=false 时 output 为 `error: ...` 回喂文本)。
#[derive(Debug, Clone)]
pub struct AgentToolResult {
    pub session: SessionId,
    pub name: String,
    pub ok: bool,
    pub output: String,
}

impl Event for AgentToolResult {
    const NAME: &'static str = "agent/tool-result";
    type Value = ();
}

/// turn 终态(result 不 Clone,走 Err 摘要;Ok 记空串)。
#[derive(Debug, Clone)]
pub struct AgentTurnEnd {
    pub session: SessionId,
    pub ok: bool,
    pub error: String,
}

impl Event for AgentTurnEnd {
    const NAME: &'static str = "agent/turn-end";
    type Value = ();
}

/// 每步模型响应后的摘要:步号、内容、工具调用数。
#[derive(Debug, Clone)]
pub struct AgentStepEvent {
    pub session: SessionId,
    pub step: usize,
    pub content: Option<String>,
    pub tool_calls: usize,
}

impl Event for AgentStepEvent {
    const NAME: &'static str = "agent/step";
    type Value = ();
}

// ── waterfall 节点(设计 §四.1:关键节点可拦截,插件挂中间件改写/veto)──
// 载荷 owned(Event: 'static),driver 按需 clone——单次调用开销远小于
// 工具执行本身。

/// `agent/pre-step`:进入这步前的消息与工具集(可改写/拒绝;默认原样)。
///
/// waterfall 值:`Err(reason)` = 拒绝本步(turn 以 `Pipeline(reason)`
/// 失败收尾),`Ok((prompt, tools))` = 放行(可被中间件改写)。
#[derive(Debug, Clone)]
pub struct AgentPreStep {
    pub session: SessionId,
    pub step: usize,
    /// 本步将发给模型的 prompt(含 system 前置)。
    pub prompt: Vec<LanguageModelPromptMessage>,
    /// 本步提供的工具 schema。
    pub tools: Vec<Tool>,
}

impl Event for AgentPreStep {
    const NAME: &'static str = "agent/pre-step";
    type Value = PreStepDecision;
}

/// `agent/pre-step` 的 waterfall 值。
pub type PreStepDecision = Result<(Vec<LanguageModelPromptMessage>, Vec<Tool>), String>;

/// `tools/pre-execute`:工具执行前门控(拒绝或放行;默认放行)。
///
/// waterfall 值:`Some(reason)` = 拒绝执行(reason 转为模型可见的
/// `error: ...` 结果回喂),`None` = 放行。
#[derive(Debug, Clone)]
pub struct ToolPreExecute {
    pub session: SessionId,
    pub call: ToolCall,
}

impl Event for ToolPreExecute {
    const NAME: &'static str = "tools/pre-execute";
    type Value = Option<String>;
}

/// `tools/post-execute`:执行后的结果决策(accept/replace/重试由
/// 中间件语义决定;失败也到这——`ok=false` 时同样可改写)。
///
/// waterfall 值:替换后的结果(默认原样 accept)。
#[derive(Debug, Clone)]
pub struct ToolPostExecute {
    pub session: SessionId,
    pub call: ToolCall,
    pub ok: bool,
    pub output: String,
}

impl Event for ToolPostExecute {
    const NAME: &'static str = "tools/post-execute";
    type Value = ToolOutput;
}
