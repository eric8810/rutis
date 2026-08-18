//! Session——连续 loop 的事实源(设计 §三.2)。

use std::sync::atomic::{AtomicU64, Ordering};

use aimux_core::message::ModelMessage;

/// 全局 session 计数器:driver 创建 session 时分配。
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// session 身份;与 `Agent::id()` 共享(设计 §三.3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

impl SessionId {
    pub fn next() -> Self {
        Self(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

/// 连续对话的载体:有序消息记录,直接存模型可见消息,一层。
/// 不做持久化 / 回放 / compaction(dsh 两层结构为持久化服务,内存版不需要)。
///
/// 防外部改用只读快照(`&[ModelMessage]`);多轮 `followup` 之间,
/// history 就活在这里——第二轮思考感知到的即 `messages()` 全量。
#[derive(Debug, Clone)]
pub struct Session {
    id: SessionId,
    messages: Vec<ModelMessage>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            id: SessionId::next(),
            messages: Vec::new(),
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    /// 追加一条消息(user / assistant / tool 结果)。
    pub fn push(&mut self, msg: ModelMessage) {
        self.messages.push(msg);
    }

    /// 只读快照:下一轮思考的感知输入。
    pub fn messages(&self) -> &[ModelMessage] {
        &self.messages
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
