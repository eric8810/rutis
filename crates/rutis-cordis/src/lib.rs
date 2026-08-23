//! rutis-cordis:cordis 基座桥(设计:docs/design-dsh-bridge-2026-08-21.md v3.2)。
//!
//! 层次纪律(v3.2 三条腿):本 crate 是**基座桥**——cordis 词汇(装载仲裁/
//! 服务注册/事件总线四分发/isolate 作用域),**零 dsh 知识**;dsh 面(llm
//! 缝/事件映射/替身表/dshSemver)在 `rutis-dsh`,骑在本 crate 上。
//! `rutis-agent` 与本 crate 互不依赖,是内核上的兄弟。
//!
//! - [`rpc`]:机制层——帧信封(含三预留字段)、`Wire` 传输接缝、会话状态机、
//!   在飞表、超时、取消、孤儿应答计数。任何 JSON 帧对端可用。
//! - [`proto`]:cordis 协议词汇——hello 基座面、能力集、evt mode、
//!   wf kind、装载仲裁。

pub mod proto;
pub mod rpc;
pub mod services;
pub mod tcp;

pub use proto::{
    EvtDeclaration, EvtMode, ExpectedHost, HelloCaps, HelloVerify, PeerCaps, PluginLedger,
    WfDeclaration, WfKind,
};
pub use rpc::{
    Bridge, BridgeConfig, BridgeStats, CancelPrefix, CancelTarget, Frame, HostGoneRecord,
    InboundHooks, MemoryWire, Outcome, ProtoError, RemoteError, SessionState, Wire,
};
pub use services::{CordisService, ServiceDispatch, ServiceReply};
pub use tcp::TcpWire;
