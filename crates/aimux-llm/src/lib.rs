//! aimux-llm:以独立 rutis 插件形态存在的 llm 服务
//! (决策:docs/decision-aimux-llm-plugin-2026-08-23.md v2 对象 A)。
//!
//! 层次纪律:依赖 rutis 内核 + aimux;**零桥知识、零宿主形态知识**——
//! 不知道 dsh,不知道 wire,不依赖 rutis-cordis。任何宿主(rutis 运行时
//! 内的消费者,或经业务无关桥过线的远端消费者)都以同一服务面使用它。
//!
//! 服务面(即中性协议的 schema 所在,DTO 见 [`service`]):
//! - `stream(StreamRequest) → StreamPart 流`;
//! - `list_models(provider, key) → 模型表`。
//! 服务实现 [`provider::AimuxLlm`]:per-(provider,key,model) 工厂缓存、
//! listModels 缓存、无 key 回落(构造失败不阻塞宿主,调用时报错)。

pub mod plugin;
pub mod provider;
pub mod service;

pub use plugin::{llm_service_key, AimuxLlmPlugin};
pub use provider::{AimuxLlm, ProviderFactory};
pub use service::{LlmService, LlmServiceError, ModelBrief, StreamRequest};
