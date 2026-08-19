//! `Agent` 接口——多轮、可观察、可取消(设计 §三.3)。
//!
//! turn 的过程输出走 EventBus,不走独占 stream:`followup` 返回终态,
//! 文本增量 / 工具调用 / 工具结果经 [`crate::events`] 的 `agent/*`
//! 事件广播——输出是广播,任何观察方(TUI / 日志 / 未来前端)订阅
//! 事件即可,晚订阅、只看不动都行;监听器随注册方 fiber 卸载(D28)。

use std::sync::atomic::{AtomicUsize, Ordering};

use aimux_core::message::ModelMessage;
use rutis::{BoxFuture, TypeKey};

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

/// agent 循环错误。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent turn stopped (cancelled)")]
    Stopped,
    #[error("agent turn exceeded max_steps={0}")]
    MaxSteps(usize),
    #[error("llm failed: {0}")]
    Llm(String),
    #[error("tool pipeline failed: {0}")]
    Pipeline(String),
}

/// 多轮 agent 接口(dsh Agent 的多轮内核裁剪:保留 id/status/session/
/// followup/cancel,裁掉 inbox 排队 / steer / fork / resume / 维护调度)。
///
/// `followup` 只负责"触发 turn + 回传终态";过程增量(text/tool/状态)
/// 经 EventBus 的 `agent/*` 事件广播,不独占返回——观察方(TUI/日志/
/// 其他前端)订阅事件,不调 followup 拿流。
pub trait Agent: Send + Sync + 'static {
    /// 与 session 共享的身份。
    fn id(&self) -> SessionId;
    /// idle | running。
    fn status(&self) -> AgentStatus;
    /// session 快照(driver 内部持锁拷贝;`messages()` 只读)。
    fn session(&self) -> SessionSnapshot;
    /// 提交一条用户消息:push 进 session,驱动一个 turn,返回终态。
    fn followup<'a>(&'a self, input: &'a str) -> BoxFuture<'a, Result<String, AgentError>>;
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
