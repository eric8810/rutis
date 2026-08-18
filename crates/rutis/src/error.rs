use std::sync::Arc;

/// 框架错误。不 `Clone`(D11);跨任务共享走 `Arc<CordisError>`。
///
/// 变体分层(D25):`PluginFailed` 仅包装 apply/清理边界外来的非 Cordis 错误;
/// apply 自身返回的 `CordisError` 直接传播,不再递归包一层。
#[derive(Debug, thiserror::Error)]
pub enum CordisError {
    #[error("service {0:?} not found in scope")]
    ServiceNotFound(String),
    #[error("plugin failed")]
    PluginFailed(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("multiple errors: {errors:?}")]
    Aggregate {
        /// 聚合成员,不压平(D18/§五 dispose 契约)。`Arc` 成员是 D11(不
        /// Clone)与 D25(错误 identity 缓存 `Arc`)的推论,见 §八 实现记录。
        errors: Vec<Arc<CordisError>>,
    },
    #[error("fiber disposed")]
    InactiveEffect,
    #[error("config validation failed: {issues:?}")]
    Validation { issues: Vec<String> },
    #[error("dependency unsatisfied: {0:?}")]
    InjectUnsatisfied(Vec<String>),
    /// 同一 (key, scope) 重复注册(实现新增变体,见 §八 实现记录)。
    #[error("service {0:?} already registered in scope")]
    ServiceExists(String),
}

/// 错误汇聚点。默认实现输出到 stderr(设计 §二)。
///
/// 成员为 `Arc<CordisError>`:dispose 类错误经 `TransitionTask` 缓存 identity
/// (D25),而 `CordisError` 刻意不 Clone(D11),见 §八 实现记录。
pub type ErrorSink = Arc<dyn Fn(Arc<CordisError>) + Send + Sync>;

pub(crate) fn default_sink() -> ErrorSink {
    Arc::new(|e: Arc<CordisError>| eprintln!("[rutis] {e}"))
}

/// 单错原样、多错聚合不压平;返回 `None` 表示无错。
pub(crate) fn aggregate_errors(errors: Vec<CordisError>) -> Option<CordisError> {
    match errors.len() {
        0 => None,
        1 => errors.into_iter().next(),
        _ => Some(CordisError::Aggregate {
            errors: errors.into_iter().map(Arc::new).collect(),
        }),
    }
}

/// 同 [`aggregate_errors`],成员已是共享 identity 的 `Arc`。
pub(crate) fn aggregate_arcs(errors: Vec<Arc<CordisError>>) -> Option<Arc<CordisError>> {
    match errors.len() {
        0 => None,
        1 => errors.into_iter().next(),
        _ => Some(Arc::new(CordisError::Aggregate { errors })),
    }
}

/// 把任务边界捕获的 panic 转成 `PluginFailed`(D30)。
pub(crate) fn panic_error(p: Box<dyn std::any::Any + Send>) -> CordisError {
    let msg = if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic in task".to_string()
    };
    CordisError::PluginFailed(msg.into())
}

/// JoinError → 错误:先区分 panic 与取消——`into_panic()` 在取消场景会
/// 二次 panic(评审 P2),取消转明确的任务取消错误。
pub(crate) fn join_panic_error(join_err: tokio::task::JoinError) -> CordisError {
    if join_err.is_panic() {
        panic_error(join_err.into_panic())
    } else {
        CordisError::PluginFailed("task cancelled before completion".into())
    }
}
