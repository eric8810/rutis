//! 桥协议 v1.1(设计 §三):帧模型、传输抽象、会话状态机。
//!
//! 三类消息双向并发,每条请求带关联 id;全部帧预留 `scopeId`(v1 不过滤——
//! rutis 内核 D29:事件不按 isolate 过滤,过滤是 dsh 语义,将来由桥端实现)。
//!
//! M1 用内存 wire 验收全套语义(往返/并发/取消/超时/握手错配与能力集求差/
//! 同名仲裁/重入/宿主死亡),零 Node;`Wire` 是唯一传输接缝。

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rutis::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

// ---------------------------------------------------------------------------
// 帧
// ---------------------------------------------------------------------------

/// 单条协议帧(§三:三类消息,JSON 线格式)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Frame {
    /// 调用方 → 被调方:一次方法调用。
    Req {
        id: u64,
        method: String,
        #[serde(default)]
        params: Value,
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "scopeId")]
        scope_id: Option<String>,
    },
    /// 被调方 → 调用方:一次调用的应答。`ok` 的真值选择载荷字段。
    Res {
        id: u64,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<RemoteError>,
    },
    /// 任一侧 → 对侧:通知,无应答。
    Ntf {
        method: String,
        #[serde(default)]
        params: Value,
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "scopeId")]
        scope_id: Option<String>,
    },
}

/// `Res` 帧的应载荷解析:按 `ok` 取 `result` 或 `error`。
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Ok(Value),
    Err(RemoteError),
}

impl Frame {
    /// 把 `Res` 帧折叠为 [`Outcome`];`ok` 与载荷字段不一致是线格式违规。
    pub fn outcome(&self) -> Option<Result<Outcome, ProtoError>> {
        match self {
            Frame::Res { id, ok, result, error } => Some(match (*ok, result, error) {
                (true, Some(result), None) => Ok(Outcome::Ok(result.clone())),
                (false, None, Some(error)) => Ok(Outcome::Err(error.clone())),
                _ => Err(ProtoError::Wire(format!("malformed res frame for id {id}: ok/载荷不一致"))),
            }),
            _ => None,
        }
    }
}

/// 远端错误(线格式:`{"code","message"}`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteError {
    pub code: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// 传输抽象(M1 内存实现;M2 增 fd3 / unix socket 实现)
// ---------------------------------------------------------------------------

/// 帧传输。`send`/`recv` 均取 `&self`:泵任务独占收帧语义由"只有泵调
/// `recv`"的约定保证,真传输的读写半也按此形状封装。
pub trait Wire: Send + Sync + 'static {
    /// 发送一帧;对端已消失是 [`ProtoError::HostGone`]。
    fn send(&self, frame: Frame) -> BoxFuture<'static, Result<(), ProtoError>>;
    /// 收一帧;`None` = 对端关闭(宿主死亡)。
    fn recv(&self) -> BoxFuture<'static, Option<Frame>>;
}

/// 内存 wire:`mpsc` 对接的两端,验收测试用;`pair` 返回(桥侧, 宿主侧)。
pub struct MemoryWire {
    out: mpsc::Sender<Frame>,
    r#in: std::sync::Arc<tokio::sync::Mutex<mpsc::Receiver<Frame>>>,
}

impl MemoryWire {
    pub fn pair(buffer: usize) -> (MemoryWire, MemoryWire) {
        let (bridge_tx, host_rx) = mpsc::channel(buffer);
        let (host_tx, bridge_rx) = mpsc::channel(buffer);
        // 桥收宿主发的帧(bridge_rx),宿主收桥发的帧(host_rx)。
        (
            MemoryWire { out: bridge_tx, r#in: std::sync::Arc::new(tokio::sync::Mutex::new(bridge_rx)) },
            MemoryWire { out: host_tx, r#in: std::sync::Arc::new(tokio::sync::Mutex::new(host_rx)) },
        )
    }
}

impl Wire for MemoryWire {
    fn send(&self, frame: Frame) -> BoxFuture<'static, Result<(), ProtoError>> {
        let out = self.out.clone();
        Box::pin(async move {
            out.send(frame).await.map_err(|_| ProtoError::HostGone)
        })
    }

    fn recv(&self) -> BoxFuture<'static, Option<Frame>> {
        let r#in = std::sync::Arc::clone(&self.r#in);
        Box::pin(async move { r#in.lock().await.recv().await })
    }
}

// ---------------------------------------------------------------------------
// 握手面(§三 规则 1:hello + 能力集协商)
// ---------------------------------------------------------------------------

/// 宿主在 `hello` 里申报的能力集。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PeerCaps {
    #[serde(default)]
    pub services: BTreeSet<String>,
    #[serde(default, rename = "wfKinds")]
    pub wf_kinds: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl PeerCaps {
    /// 装载期能力集求差(§三 规则 1):`injects` 中宿主没有的服务。
    pub fn missing_services<'a>(&self, injects: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        injects
            .into_iter()
            .filter(|name| !self.services.contains(*name))
            .map(str::to_owned)
            .collect()
    }
}

/// 宿主 `hello` 参数的完整形状。
#[derive(Debug, Clone, Deserialize)]
pub struct HelloCaps {
    pub protocol: u32,
    pub base: String,
    #[serde(rename = "baseSemver")]
    pub base_semver: String,
    #[serde(rename = "dshSemver")]
    pub dsh_semver: String,
    #[serde(default)]
    pub stack: Vec<String>,
    #[serde(default)]
    pub caps: PeerCaps,
}

/// Rust 侧对宿主的握手期望(错配在握手期报错,§三 规则 1)。
#[derive(Debug, Clone)]
pub struct ExpectedHost {
    pub protocol: u32,
    /// `Some` 时要求 `dshSemver` 精确相等(版本声明制的第一道闸)。
    pub dsh_semver: Option<String>,
    /// `Some` 时要求 `base` 相等(min-cordis / cordis)。
    pub base: Option<String>,
}

impl ExpectedHost {
    pub fn protocol(protocol: u32) -> ExpectedHost {
        ExpectedHost { protocol, dsh_semver: None, base: None }
    }
}

// ---------------------------------------------------------------------------
// 取消与超时(§三 规则 4)
// ---------------------------------------------------------------------------

/// 取消目标的类型前缀,避免 call / invocation / dispatch 命名空间相撞。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CancelPrefix {
    Call,
    Invocation,
    Dispatch,
}

/// 取消目标,线格式 `call:12` / `inv:3` / `disp:7`。
#[derive(Debug, Clone, PartialEq)]
pub struct CancelTarget {
    pub prefix: CancelPrefix,
    pub id: u64,
}

impl CancelTarget {
    pub fn call(id: u64) -> CancelTarget {
        CancelTarget { prefix: CancelPrefix::Call, id }
    }
}

impl std::fmt::Display for CancelTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix = match self.prefix {
            CancelPrefix::Call => "call",
            CancelPrefix::Invocation => "inv",
            CancelPrefix::Dispatch => "disp",
        };
        write!(f, "{prefix}:{}", self.id)
    }
}

// ---------------------------------------------------------------------------
// 事件申报(§三 规则 5:mode 预留)
// ---------------------------------------------------------------------------

/// 事件分发模式。v1 底座方向只消费 emit;parallel/serial 为锈化波预留,
/// 不得静默降级为 ntf。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvtMode {
    Emit,
    Parallel,
    Serial,
}

/// `evt/on` 申报里的一条事件订阅。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvtDeclaration {
    pub name: String,
    pub mode: EvtMode,
}

// ---------------------------------------------------------------------------
// 同名仲裁(§三 规则 3)
// ---------------------------------------------------------------------------

/// 已装载插件的记账:同 id 同 entry 重载幂等;同 id 异 entry 拒绝后者并
/// 指名已有者。宿主侧行为的 Rust 侧镜像,桥据此仲裁 `plugin/load`。
#[derive(Debug, Default)]
pub struct PluginLedger {
    entries: HashMap<String, String>,
}

impl PluginLedger {
    /// 申报一次装载。`(id, entry)` 与已有相同 → 幂等通过;`id` 已被别的
    /// entry 占用 → [`ProtoError::DuplicatePlugin`] 指名已有者。
    pub fn load(&mut self, plugin_id: &str, entry: &str) -> Result<(), ProtoError> {
        match self.entries.get(plugin_id) {
            Some(existing) if existing == entry => Ok(()),
            Some(existing) => Err(ProtoError::DuplicatePlugin {
                plugin_id: plugin_id.to_owned(),
                existing_entry: existing.clone(),
                attempted_entry: entry.to_owned(),
            }),
            None => {
                self.entries.insert(plugin_id.to_owned(), entry.to_owned());
                Ok(())
            }
        }
    }

    pub fn unload(&mut self, plugin_id: &str) -> bool {
        self.entries.remove(plugin_id).is_some()
    }

    pub fn entry(&self, plugin_id: &str) -> Option<&str> {
        self.entries.get(plugin_id).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// 错误与会话状态
// ---------------------------------------------------------------------------

/// 协议层错误。
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("wire failure: {0}")]
    Wire(String),
    #[error("handshake failed: {0}")]
    Handshake(String),
    #[error("call {method} (id {id}) timed out after {timeout_ms}ms")]
    Timeout { id: u64, method: String, timeout_ms: u64 },
    #[error("timeout {requested}ms exceeds bridge cap {max}ms")]
    TimeoutTooLarge { requested: u64, max: u64 },
    #[error("call {method} (id {id}) cancelled")]
    Cancelled { id: u64, method: String },
    #[error("remote error {code}: {message}")]
    Remote { code: String, message: String },
    #[error("host is gone")]
    HostGone,
    #[error("duplicate plugin {plugin_id}: existing entry {existing_entry}, attempted {attempted_entry}")]
    DuplicatePlugin { plugin_id: String, existing_entry: String, attempted_entry: String },
    #[error("bridge not ready: {0}")]
    NotReady(String),
}

/// 宿主死亡时的观察连续性记录(§九 M1:仅失 llm 缝,断连必须留痕)。
#[derive(Debug, Clone, PartialEq)]
pub struct HostGoneRecord {
    /// 断连时仍在飞的调用(id, method)清单。
    pub pending: Vec<(u64, String)>,
    /// 累计孤儿应答数(迟到 res 与放弃接收的竞态合并计数)。
    pub orphan_responses: u64,
    /// 断连前累计收到的帧数。
    pub frames_received: u64,
}

/// 会话状态机:`Connecting → Ready | Failed`,任一态 → `Disconnected`。
#[derive(Debug, Clone)]
pub enum SessionState {
    Connecting,
    Ready(PeerCaps),
    Failed(String),
    Disconnected(HostGoneRecord),
}

/// 会话计数(观察面)。
#[derive(Debug, Clone, Default)]
pub struct BridgeStats {
    pub frames_sent: u64,
    pub frames_received: u64,
    pub orphan_responses: u64,
}

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

/// 桥的运行配置。默认超时桥定,全局上限配置(§三 规则 4)。
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
}

impl Default for BridgeConfig {
    fn default() -> BridgeConfig {
        BridgeConfig { default_timeout_ms: 30_000, max_timeout_ms: 120_000 }
    }
}

/// 入站请求钩子:返回 `Ok`/`Err` 都会作为 res 回给宿主。
pub type RequestHook = Arc<
    dyn Fn(u64, String, Value) -> BoxFuture<'static, Result<Value, RemoteError>> + Send + Sync,
>;

/// 入站通知钩子(evt/emit 等走这里)。
pub type NotifyHook = Arc<dyn Fn(String, Value) -> BoxFuture<'static, ()> + Send + Sync>;

/// 宿主 → Rust 方向的入站分发面。`on_request` 缺省时回 `unhandled` 错误
/// (显式不静默)。
#[derive(Default)]
pub struct InboundHooks {
    pub on_request: Option<RequestHook>,
    pub on_notify: Option<NotifyHook>,
}

/// 在飞调用的结算信号:`Res` 是对端应答,`HostGone` 是宿主死亡排空。
enum CallSettled {
    Res(Result<Value, RemoteError>),
    HostGone,
}

struct Pending {
    method: String,
    tx: oneshot::Sender<CallSettled>,
}

struct Shared {
    wire: Box<dyn Wire>,
    pending: Mutex<HashMap<u64, Pending>>,
    next_id: AtomicU64,
    config: BridgeConfig,
    hooks: InboundHooks,
    expected: ExpectedHost,
    own_caps: PeerCaps,
    state: watch::Sender<SessionState>,
    stats: BridgeStatsInternals,
}

#[derive(Default)]
struct BridgeStatsInternals {
    frames_sent: AtomicU64,
    frames_received: AtomicU64,
    /// 孤儿合并计数:迟到 res(无在飞匹配)+ 接收方已放弃的竞态。
    orphan_responses: AtomicU64,
    /// 断连记录;Some 后会话终态。
    gone: Mutex<Option<HostGoneRecord>>,
}

/// 桥会话句柄:克隆廉价,泵在后台独立运行(重入由"泵与 request 互不
/// 阻塞"覆盖)。
#[derive(Clone)]
pub struct Bridge {
    shared: Arc<Shared>,
    state_rx: watch::Receiver<SessionState>,
}

impl Bridge {
    /// 启动会话与后台泵。宿主侧随后应发 `hello`;用 [`Bridge::ready`] 等
    /// 握手结果。
    pub fn start(
        wire: Box<dyn Wire>,
        config: BridgeConfig,
        hooks: InboundHooks,
        expected: ExpectedHost,
        own_caps: PeerCaps,
    ) -> Bridge {
        let (state_tx, state_rx) = watch::channel(SessionState::Connecting);
        let shared = Arc::new(Shared {
            wire,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            config,
            hooks,
            expected,
            own_caps,
            state: state_tx,
            stats: BridgeStatsInternals::default(),
        });
        tokio::spawn(pump(Arc::clone(&shared)));
        Bridge { shared, state_rx }
    }

    /// 等握手完成;失败/断连先到则报错。
    pub async fn ready(&mut self) -> Result<PeerCaps, ProtoError> {
        loop {
            match &*self.state_rx.borrow() {
                SessionState::Ready(caps) => return Ok(caps.clone()),
                SessionState::Failed(reason) => {
                    return Err(ProtoError::Handshake(reason.clone()))
                }
                SessionState::Disconnected(_) => return Err(ProtoError::HostGone),
                SessionState::Connecting => {}
            }
            if self.state_rx.changed().await.is_err() {
                return Err(ProtoError::HostGone)
            }
        }
    }

    /// 等宿主死亡记录;已断连(或握手失败——同为会话终态)立即返回。
    pub async fn wait_disconnect(&mut self) -> HostGoneRecord {
        loop {
            match &*self.state_rx.borrow() {
                SessionState::Disconnected(record) => return record.clone(),
                SessionState::Failed(_) => return self.gone_record(),
                _ => {}
            }
            if self.state_rx.changed().await.is_err() {
                // 发送端只在 Shared drop 时消失,而泵持有 Arc,不可达;防御。
                return self.gone_record()
            }
        }
    }

    fn gone_record(&self) -> HostGoneRecord {
        self.shared
            .stats
            .gone
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| HostGoneRecord {
                pending: Vec::new(),
                orphan_responses: self.shared.stats.orphan_responses.load(Ordering::Relaxed),
                frames_received: self.shared.stats.frames_received.load(Ordering::Relaxed),
            })
    }

    /// 一次方法调用。`timeout_ms` 为 `None` 用配置默认;超过全局上限是
    /// 配置错误(显式拒绝,不静默截断)。
    pub async fn request(
        &self,
        method: impl Into<String>,
        params: Value,
        timeout_ms: Option<u64>,
    ) -> Result<Value, ProtoError> {
        match &*self.state_rx.borrow() {
            SessionState::Ready(_) => {}
            SessionState::Connecting => {
                return Err(ProtoError::NotReady("handshake not completed".into()))
            }
            SessionState::Failed(reason) => {
                return Err(ProtoError::NotReady(format!("handshake failed: {reason}")))
            }
            SessionState::Disconnected(_) => return Err(ProtoError::HostGone),
        }
        let method = method.into();
        let timeout_ms = match timeout_ms {
            Some(t) if t > self.shared.config.max_timeout_ms => {
                return Err(ProtoError::TimeoutTooLarge {
                    requested: t,
                    max: self.shared.config.max_timeout_ms,
                })
            }
            t => t.unwrap_or(self.shared.config.default_timeout_ms),
        };
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(id, Pending { method: method.clone(), tx });
        let frame = Frame::Req { id, method: method.clone(), params, scope_id: None };
        if let Err(e) = self.shared.send_frame(frame).await {
            self.shared.pending.lock().unwrap().remove(&id);
            return Err(e)
        }
        match tokio::time::timeout(Duration::from_millis(timeout_ms), rx).await {
            Ok(Ok(CallSettled::Res(outcome))) => {
                outcome.map_err(|RemoteError { code, message }| ProtoError::Remote { code, message })
            }
            Ok(Ok(CallSettled::HostGone)) | Ok(Err(_)) => Err(ProtoError::HostGone),
            Err(_elapsed) => {
                // 超时本地移除在飞,迟到 res 因此必落孤儿计数(§三 规则 4:
                // 超时按失败处理,不等待)。res 恰在窗口内完成的竞态同样以
                // 超时结论交付。
                self.shared.pending.lock().unwrap().remove(&id);
                Err(ProtoError::Timeout { id, method, timeout_ms })
            }
        }
    }

    /// 发一条通知(无应答)。
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), ProtoError> {
        self.shared
            .send_frame(Frame::Ntf { method: method.to_owned(), params, scope_id: None })
            .await
    }

    /// 取消传播(§三 规则 4):发 `cancel` 通知并立即放弃对应在飞调用;
    /// 迟到的 res 按孤儿丢弃计数。
    pub async fn cancel(&self, target: CancelTarget) -> Result<(), ProtoError> {
        if target.prefix == CancelPrefix::Call {
            if let Some(pending) = self.shared.pending.lock().unwrap().remove(&target.id) {
                if pending
                    .tx
                    .send(CallSettled::Res(Err(RemoteError {
                        code: "cancelled".into(),
                        message: "cancelled by caller".into(),
                    })))
                    .is_err()
                {
                    self.shared.stats.orphan_responses.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        self.notify("cancel", serde_json::json!({ "target": target.to_string() })).await
    }

    /// 当前计数快照。
    pub fn stats(&self) -> BridgeStats {
        let s = &self.shared.stats;
        BridgeStats {
            frames_sent: s.frames_sent.load(Ordering::Relaxed),
            frames_received: s.frames_received.load(Ordering::Relaxed),
            orphan_responses: s.orphan_responses.load(Ordering::Relaxed),
        }
    }
}

impl Shared {
    async fn send_frame(&self, frame: Frame) -> Result<(), ProtoError> {
        self.wire.send(frame).await?;
        self.stats.frames_sent.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// 幂等的终态落账:排空在飞并记录;`publish` 决定是否把状态覆盖为
    /// `Disconnected`(握手失败路径的状态已是 `Failed`,不覆盖——两种终态
    /// 都由 `wait_disconnect` 收敛)。
    fn host_gone(&self, publish: bool) {
        let mut gone = self.stats.gone.lock().unwrap();
        if gone.is_some() {
            return
        }
        let drained: Vec<_> = self.pending.lock().unwrap().drain().collect();
        let mut pending = Vec::new();
        for (id, p) in drained {
            pending.push((id, p.method));
            // 接收方已放弃(超时竞态)的 send 失败并入孤儿计数。
            if p.tx.send(CallSettled::HostGone).is_err() {
                self.stats.orphan_responses.fetch_add(1, Ordering::Relaxed);
            }
        }
        let record = HostGoneRecord {
            pending,
            orphan_responses: self.stats.orphan_responses.load(Ordering::Relaxed),
            frames_received: self.stats.frames_received.load(Ordering::Relaxed),
        };
        *gone = Some(record.clone());
        drop(gone);
        if publish {
            let _ = self.state.send(SessionState::Disconnected(record));
        }
    }

    fn complete(&self, id: u64, outcome: Result<Value, RemoteError>) {
        let mut pending = self.pending.lock().unwrap();
        match pending.remove(&id) {
            Some(p) => {
                if p.tx.send(CallSettled::Res(outcome)).is_err() {
                    // 调用方已超时/取消放弃接收:按孤儿应答计数(§三 规则 4)。
                    self.stats.orphan_responses.fetch_add(1, Ordering::Relaxed);
                }
            }
            None => {
                self.stats.orphan_responses.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 入站 Req 的服务:钩子在独立任务执行(泵不阻塞,重入面),完成后回 res。
    /// 关联函数 + `Arc<Shared>`:`spawn` 要求 'static。
    fn serve_request(shared: Arc<Shared>, id: u64, method: String, params: Value) {
        tokio::spawn(async move {
            let outcome = match shared.hooks.on_request.clone() {
                Some(h) => h(id, method.clone(), params).await,
                None => Err(RemoteError {
                    code: "unhandled".into(),
                    message: format!("no request handler for {method}"),
                }),
            };
            let frame = match outcome {
                Ok(result) => Frame::Res { id, ok: true, result: Some(result), error: None },
                Err(error) => Frame::Res { id, ok: false, result: None, error: Some(error) },
            };
            if shared.wire.send(frame).await.is_ok() {
                shared.stats.frames_sent.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    fn dispatch_notify(&self, method: String, params: Value) {
        if let Some(hook) = self.hooks.on_notify.clone() {
            tokio::spawn(async move {
                hook(method, params).await;
            });
        }
    }

    /// hello 的握手校验与回包(§三 规则 1)。失败在握手期报错并断开。
    async fn handle_hello(&self, id: u64, params: Value) -> Result<(), String> {
        let caps: HelloCaps = serde_json::from_value(params)
            .map_err(|e| format!("hello params malformed: {e}"))?;
        if caps.protocol != self.expected.protocol {
            return Err(format!(
                "protocol version mismatch: host declared {}, bridge expects {}",
                caps.protocol, self.expected.protocol
            ))
        }
        if let Some(expected) = &self.expected.dsh_semver {
            if &caps.dsh_semver != expected {
                return Err(format!(
                    "dshSemver mismatch: host declared {}, bridge pins {expected}",
                    caps.dsh_semver
                ))
            }
        }
        if let Some(expected) = &self.expected.base {
            if &caps.base != expected {
                return Err(format!(
                    "base mismatch: host declared {}, bridge expects {expected}",
                    caps.base
                ))
            }
        }
        let reply = serde_json::json!({
            "base": "rutis",
            "baseSemver": env!("CARGO_PKG_VERSION"),
            "dshSemver": caps.dsh_semver,
            "stack": ["rutis"],
            "caps": serde_json::to_value(&self.own_caps).expect("PeerCaps serializes"),
        });
        let peer_caps = caps.caps.clone();
        let frame = Frame::Res { id, ok: true, result: Some(reply), error: None };
        if self.send_frame(frame).await.is_err() {
            return Err("host closed during handshake".into())
        }
        let _ = self.state.send(SessionState::Ready(peer_caps));
        Ok(())
    }
}

async fn pump(shared: Arc<Shared>) {
    loop {
        let Some(frame) = shared.wire.recv().await else {
            shared.host_gone(true);
            return
        };
        shared.stats.frames_received.fetch_add(1, Ordering::Relaxed);
        match frame {
            Frame::Res { id, ok, result, error } => {
                let outcome = match (ok, result, error) {
                    (true, Some(result), None) => Ok(result),
                    (false, None, Some(RemoteError { code, message })) => {
                        Err(RemoteError { code, message })
                    }
                    _ => {
                        // 线格式违规:计孤儿(无法归属),继续泵。
                        shared.stats.orphan_responses.fetch_add(1, Ordering::Relaxed);
                        continue
                    }
                };
                shared.complete(id, outcome);
            }
            Frame::Req { id, method, params, .. } => {
                let connecting =
                    matches!(&*shared.state.borrow(), SessionState::Connecting);
                if connecting {
                    if method == "hello" {
                        if let Err(reason) = shared.handle_hello(id, params).await {
                            let frame = Frame::Res {
                                id,
                                ok: false,
                                result: None,
                                error: Some(RemoteError {
                                    code: "handshake".into(),
                                    message: reason.clone(),
                                }),
                            };
                            let _ = shared.send_frame(frame).await;
                            let _ = shared.state.send(SessionState::Failed(reason));
                            shared.host_gone(false);
                            return
                        }
                    } else {
                        // 握手前的任何其他帧都是协议违规。
                        let reason = format!("first frame must be hello, got {method}");
                        let frame = Frame::Res {
                            id,
                            ok: false,
                            result: None,
                            error: Some(RemoteError { code: "handshake".into(), message: reason.clone() }),
                        };
                        let _ = shared.send_frame(frame).await;
                        let _ = shared.state.send(SessionState::Failed(reason));
                        shared.host_gone(false);
                        return
                    }
                    continue
                }
                Shared::serve_request(Arc::clone(&shared), id, method, params);
            }
            Frame::Ntf { method, params, .. } => {
                shared.dispatch_notify(method, params);
            }
        }
    }
}
