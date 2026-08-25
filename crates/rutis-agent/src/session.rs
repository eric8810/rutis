//! Session——连续 loop 的事实源(设计 §三.2)+ 可选持久化。
//!
//! 持久化目标:进程重启 / 依赖重载后,模型可见历史(session)恢复,
//! `agent.id()` 不变。格式单 JSON 文件(原子写:临时文件 + rename);
//! aimux 消息自带 serde,零类型转换。
//! 默认关闭:`None` = 不持久化,现状不变。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aimux_core::message::ModelMessage;
use serde::{Deserialize, Serialize};

/// 全局 session 计数器:driver 创建 session 时分配(仅首次分配用;
/// 恢复时直接用文件里的 identity,不再自增——跨重启 identity 稳定)。
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// session 身份;与 `Agent::id()` 共享(设计 §三.3)。
///
/// 分代设计:`identity` 首次分配后稳定(落盘跨重启不变 = 对齐凭据),
/// `generation` 标识"第几代"(重启 +1)。`as_u64()` 保留(返回
/// identity),兼容现有消费点(`Agent::id()`、`agent/*` 事件载荷)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId {
    identity: u64,
    generation: u32,
}

impl SessionId {
    /// 首次分配:identity 全局自增,generation 从 1 起。
    pub fn next() -> Self {
        Self {
            identity: NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
            generation: 1,
        }
    }

    /// 从持久化文件恢复:identity 沿用落盘值,generation +1(新代)。
    pub(crate) fn restored(identity: u64, generation: u32) -> Self {
        Self {
            identity,
            generation: generation.saturating_add(1),
        }
    }

    pub fn as_u64(&self) -> u64 {
        self.identity
    }

    pub fn identity(&self) -> u64 {
        self.identity
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }
}

/// 持久化文件格式(version 1)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFile {
    /// 文件格式版本(当前 1)。
    pub version: u32,
    /// 稳定身份(跨重启不变)。
    pub id: u64,
    /// 分代(重启 +1)。
    pub generation: u32,
    pub messages: Vec<ModelMessage>,
    /// 记忆摘要(可选):长会话压缩后,被裁剪消息的摘要。serde default
    /// 保证旧文件(无该字段)兼容加载。
    #[serde(default)]
    pub summary: Option<String>,
    /// 待办/下一步(可选):agent 中断后自动接续的工作指引。serde default
    /// 兼容旧文件。
    #[serde(default)]
    pub todo: Option<String>,
    pub saved_at_ms: u64,
}

const SESSION_FILE_VERSION: u32 = 1;

/// 连续对话的载体:有序消息记录,直接存模型可见消息,一层。
/// 可选持久化:恢复/落盘经 [`Session::persist`] / [`Session::restore`];
/// 不传路径 = 纯内存,现状不变。
///
/// 防外部改用只读快照(`&[ModelMessage]`);多轮 `followup` 之间,
/// history 就活在这里——第二轮思考感知到的即 `messages()` 全量。
#[derive(Debug, Clone)]
pub struct Session {
    id: SessionId,
    messages: Vec<ModelMessage>,
    /// 记忆摘要:长会话压缩后,被裁剪消息的摘要;None = 无摘要。
    summary: Option<String>,
    /// 待办/下一步:中断后自动接续的工作指引;None = 无待办。
    todo: Option<String>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            id: SessionId::next(),
            messages: Vec::new(),
            summary: None,
            todo: None,
        }
    }

    /// 从持久化文件恢复;失败/缺文件静默降级为新 Session(不阻断启动)。
    pub fn restore(path: &Path) -> Self {
        match Self::try_restore(path) {
            Ok(Some(s)) => s,
            _ => Self::new(),
        }
    }

    fn try_restore(path: &Path) -> Result<Option<Self>, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let file: SessionFile = serde_json::from_str(&raw)
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
        if file.version != SESSION_FILE_VERSION {
            return Err(format!(
                "unsupported session file version {} (expected {})",
                file.version, SESSION_FILE_VERSION
            ));
        }
        Ok(Some(Self {
            id: SessionId::restored(file.id, file.generation),
            messages: file.messages,
            summary: file.summary,
            todo: file.todo,
        }))
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

    /// 记忆摘要(长会话压缩后);None = 无摘要。
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// 待办/下一步(中断后自动接续);None = 无待办。
    pub fn todo(&self) -> Option<&str> {
        self.todo.as_deref()
    }

    /// 设置/更新待办(agent 记录下一步工作)。
    pub fn set_todo(&mut self, todo: String) {
        self.todo = Some(todo);
    }

    /// 压缩:保存摘要,裁剪 messages 到最近 `keep` 条。
    /// 返回 (压缩前消息数, 压缩后消息数)。
    pub fn compact(&mut self, summary: String, keep: usize) -> (usize, usize) {
        let before = self.messages.len();
        self.summary = Some(summary);
        if self.messages.len() > keep {
            let cut = self.messages.len() - keep;
            self.messages.drain(..cut);
        }
        (before, self.messages.len())
    }

    /// 原子落盘:写临时文件 + rename(同目录,避免跨设备)。
    /// 父目录不存在时自动创建(默认路径 `.rutis/session.json` 的
    /// `.rutis` 目录可能尚未存在)。
    /// 错误上抛,由调用方决定(落盘失败不阻断 turn,但可观测)。
    pub fn persist(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
        }
        let file = SessionFile {
            version: SESSION_FILE_VERSION,
            id: self.id.identity(),
            generation: self.id.generation(),
            messages: self.messages.clone(),
            summary: self.summary.clone(),
            todo: self.todo.clone(),
            saved_at_ms: now_ms(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| format!("serialize session: {e}"))?;

        let tmp = tmp_path(path);
        std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))?;
        Ok(())
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// 毫秒时间戳(落盘审计用)。
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 同目录临时文件:`.rutis.session.json.tmp` → rename 覆盖。
pub(crate) fn tmp_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(".tmp");
    PathBuf::from(os)
}
