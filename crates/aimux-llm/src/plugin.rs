//! aimux-llm 的 rutis 插件形态:apply → 注册 `dyn LlmService`。

use std::sync::Arc;

use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, TypeKey};

use crate::provider::AimuxLlm;
use crate::service::LlmService;

/// 服务键(`get_as::<dyn LlmService>(llm_service_key())` 取回)。
pub fn llm_service_key() -> TypeKey {
    TypeKey::of::<dyn LlmService>()
}

/// 装配单元:持服务实例,apply 注册进当前 ctx。
pub struct AimuxLlmPlugin {
    service: Arc<dyn LlmService>,
}

impl AimuxLlmPlugin {
    /// env 兜底构造(见 [`AimuxLlm::from_env`])。
    pub fn from_env() -> Self {
        Self { service: Arc::new(AimuxLlm::from_env()) }
    }

    /// 服务注入版(测试/组合根定制)。
    pub fn with_service(service: Arc<dyn LlmService>) -> Self {
        Self { service }
    }
}

impl Plugin for AimuxLlmPlugin {
    fn name(&self) -> &str {
        "aimux-llm"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.provide_as::<dyn LlmService>(llm_service_key(), Arc::clone(&self.service))?;
            Ok(Effect::Done)
        })
    }
}
