//! `Agent` 接口——多轮、可观察、可取消、流式(设计 §三.3)。

use std::sync::atomic::{AtomicUsize, Ordering};

use aimux_core::message::ModelMessage;
use futures::stream::BoxStream;
use rutis::TypeKey;
use serde_json::Value;

use crate::session::SessionId;

/// agent 服务键(`AgentDriverPlugin` 提供,`get_as::<dyn Agent>` 取回)。
pub fn agent_key() -> TypeKey {
    TypeKey::of::<dyn Agent>()
}

/// driver 生命周期态(设计 §三.3;原子存 0=idle / 1=running)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Running,
}

impl AgentStatus {
    pub(crate) fn as_usize(self) -> usize {
        match self {
            AgentStatus::Idle => 0,
            AgentStatus::Running => 1,
        }
    }

    pub(crate) fn from_usize(v: usize) -> Self {
        match v {
            1 => AgentStatus::Running,
            _ => AgentStatus::Idle,
        }
    }
}

/// 原子状态单元(driver 内部)。
pub(crate) struct StatusCell(AtomicUsize);

impl StatusCell {
    pub(crate) fn idle() -> Self {
        Self(AtomicUsize::new(AgentStatus::Idle.as_usize()))
    }

    pub(crate) fn set(&self, s: AgentStatus) {
        self.0.store(s.as_usize(), Ordering::SeqCst);
    }

    pub(crate) fn get(&self) -> AgentStatus {
        AgentStatus::from_usize(self.0.load(Ordering::SeqCst))
    }
}

/// 一个 turn 的流式输出:文本增量 + 工具调用边界 + 终态(设计 §三.3)。
/// TUI/前端逐块消费;session 仍由 driver 回写——流是视图,不是事实源。
#[derive(Debug)]
pub enum TurnEvent {
    /// 模型文本增量(流式)。
    TextDelta(String),
    /// 工具调用开始。
    ToolCall { name: String, args: Value },
    /// 工具结果(ok=false 时 output 为 `error: ...` 回喂文本)。
    ToolResult {
        name: String,
        ok: bool,
        output: String,
    },
    /// turn 终态:终答全文或错误。
    Done(Result<String, AgentError>),
}

/// agent 循环错误。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent turn stopped (cancelled)")]
    Stopped,
    #[error("agent turn exceeded max_steps={0}")]
    MaxSteps(usize),
    #[error("llm failed: {0}")]
    Llm(String),
}

/// 多轮 agent 接口(dsh Agent 的多轮内核裁剪:保留 id/status/session/
/// followup/cancel,裁掉 inbox 排队 / steer / fork / resume / 维护调度)。
///
/// `followup` 返回 `BoxStream<TurnEvent>` 而非 `Result<String>`——流式是
/// 第一性需求(TUI 逐字输出);非流式调用方收集 `TextDelta` 拼合即可,
/// 终答从 `TurnEvent::Done` 取。
pub trait Agent: Send + Sync + 'static {
    /// 与 session 共享的身份。
    fn id(&self) -> SessionId;
    /// idle | running。
    fn status(&self) -> AgentStatus;
    /// session 快照(driver 内部持锁拷贝;`messages()` 只读)。
    fn session(&self) -> SessionSnapshot;
    /// 提交一条用户消息:push 进 session,驱动一个 turn。
    /// 返回该 turn 的事件流(懒执行:消费才推进)。
    fn followup<'a>(&'a self, input: &'a str) -> BoxStream<'a, TurnEvent>;
    /// 中断当前 turn;session(history)保留,下次 followup 继续。
    fn cancel(&self);
}

/// session 只读快照(session 由 driver 独占,接口层只能给拷贝)。
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    id: SessionId,
    messages: Vec<ModelMessage>,
}

impl SessionSnapshot {
    pub(crate) fn new(id: SessionId, messages: &[ModelMessage]) -> Self {
        Self {
            id,
            messages: messages.to_vec(),
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn messages(&self) -> &[ModelMessage] {
        &self.messages
    }
}
