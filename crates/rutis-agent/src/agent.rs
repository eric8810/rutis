//! `Agent` 接口——多轮、可观察、可取消(设计 §三.3)。
//!
//! turn 的过程输出走 EventBus,不走独占 stream:`followup` 返回终态,
//! 文本增量 / 工具调用 / 工具结果经 [`crate::events`] 的 `agent/*`
//! 事件广播——输出是广播,任何观察方(TUI / 日志 / 未来前端)订阅
//! 事件即可,晚订阅、只看不动都行;监听器随注册方 fiber 卸载(D28)。

use std::path::Path;
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
    /// 压缩 session 记忆:保存摘要,裁剪 messages 到最近 `keep` 条。
    /// 返回 (压缩前消息数, 压缩后消息数)。供 `self_compact` 工具调用。
    fn compact(&self, summary: String, keep: usize) -> (usize, usize);
    /// 记录/更新待办(中断后自动接续的工作指引)。供 `self_todo` 工具调用。
    fn set_todo(&self, todo: String);
    /// 运行中更新 system prompt(persona):自我改善真正闭环——
    /// agent 修改自己的 persona 后立即生效,无需重启。
    fn update_persona(&self, persona: String);
}

/// session 只读快照(session 由 driver 独占,接口层只能给拷贝)。
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    id: SessionId,
    messages: Vec<ModelMessage>,
    summary: Option<String>,
    todo: Option<String>,
}

impl SessionSnapshot {
    pub(crate) fn new(
        id: SessionId,
        messages: &[ModelMessage],
        summary: Option<String>,
        todo: Option<String>,
    ) -> Self {
        Self {
            id,
            messages: messages.to_vec(),
            summary,
            todo,
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn messages(&self) -> &[ModelMessage] {
        &self.messages
    }

    /// 记忆摘要(长会话压缩后);None = 无摘要。
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// 待办/下一步(中断后自动接续);None = 无待办。
    pub fn todo(&self) -> Option<&str> {
        self.todo.as_deref()
    }

    /// 快照落盘(供 `self_persist` 工具经 `Agent::session()` 组合调用;
    /// 不新增 `Agent` trait 方法)。原子写,错误上抛。
    pub fn persist(&self, path: &Path) -> Result<(), String> {
        let file = crate::session::SessionFile {
            version: 1,
            id: self.id.identity(),
            generation: self.id.generation(),
            messages: self.messages.clone(),
            summary: self.summary.clone(),
            todo: self.todo.clone(),
            saved_at_ms: crate::session::now_ms(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| format!("serialize session: {e}"))?;
        let tmp = crate::session::tmp_path(path);
        std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))?;
        Ok(())
    }
}
