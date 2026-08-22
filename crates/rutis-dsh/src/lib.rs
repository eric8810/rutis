//! rutis-dsh:dsh 桥的 Rust 侧(设计:docs/design-dsh-bridge-2026-08-21.md)。
//!
//! 层次纪律(设计 §一):本 crate 是 dsh 关系的唯一所在,依赖 rutis(+后续
//! 波次的 aimux),**不依赖 rutis-agent**——两者是内核上的兄弟。
//!
//! M1 范围 = 协议层 [`proto`](proto):传输无关的帧模型 + 内存 wire 上的
//! 会话状态机。llm 缝 / 事件缝 / 事件类型映射是后续波次;fd3 与 unix
//! socket 传输是 M2 对 [`proto::Wire`] 的另一个实现。

pub mod proto;

pub use proto::{
    Bridge, BridgeConfig, BridgeStats, CancelPrefix, CancelTarget, EvtDeclaration, EvtMode,
    ExpectedHost, Frame, HelloCaps, HostGoneRecord, InboundHooks, MemoryWire, Outcome, PeerCaps,
    PluginLedger, ProtoError, RemoteError, Wire,
};
