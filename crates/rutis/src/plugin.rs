use crate::ctx::Ctx;
use crate::error::CordisError;
use crate::key::TypeKey;
use crate::{BoxFuture, Effect};

/// 插件 = 装配单元(支柱 1)。config 烘进实例(D19):
/// 具体插件在 `new(config)` 时持有配置,`validate` 校验自持那份。
pub trait Plugin: Send + Sync + 'static {
    /// 显示名(日志/诊断)。
    fn name(&self) -> &str;

    /// 依赖门控声明(支柱 2):全部就绪(存在 + provider Active + `check()` 通过)才启动。
    fn injects(&self) -> &[TypeKey] {
        &[]
    }

    /// 校验自持有 config(D12:validate-before-store,注册/装载期调用)。
    fn validate(&self) -> Result<(), CordisError> {
        Ok(())
    }

    /// 装配体:提供 0..n 服务、注册 0..n 监听、交回清理。
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>>;
}
