//! rutis: Cordis 核心范式的 Rust 惯用实现。
//!
//! 五支柱(设计文档 §一):
//! 1. 插件 = 装配单元(一次 `apply`,提供服务/监听/清理)
//! 2. fiber = 生命周期容器(六态状态机 + 依赖门控 + 级联卸载 + 恰好一次清理)
//! 3. 服务 = 类型键注册表 + isolate 作用域
//! 4. 事件总线 = 四分发语义(emit/parallel/serial/waterfall)
//! 5. 依赖驱动重载(provider 卸载 → 消费者驱逐并自动重载)

#![allow(clippy::type_complexity)]

mod bus;
mod ctx;
mod effect;
mod error;
mod event;
mod fiber;
mod key;
mod plugin;
mod registry;

pub use bus::EventBus;
pub use ctx::Ctx;
pub use effect::{Disposer, Effect};
pub use error::{CordisError, ErrorSink};
pub use event::{Event, EventOptions, Listener, Next, Terminal, WaterfallListener};
pub use fiber::{FiberState, FiberStatusChanged, FiberView, PluginId, Snapshot};
pub use key::{Key, ServiceKey, TypeKey};
pub use plugin::Plugin;

/// dyn 兼容的 future 别名(与 `futures::future::BoxFuture` 同一定义,D1)。
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
