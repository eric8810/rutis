use std::any::Any;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::ctx::Ctx;
use crate::error::CordisError;
use crate::BoxFuture;

/// 类型化事件(D16)。`NAME` 仅日志诊断,不参与唯一性与分发;
/// `Value` 是 serial 短路值类型(裁决开放问题 2)。
pub trait Event: Send + Sync + 'static {
    const NAME: &'static str;
    type Value: Send + 'static;
}

/// 监听器统一形状(D16:helper trait 解决 alias 的 E0106)。
///
/// 返回 `Ok(Some(v))` = serial 短路值;`Ok(None)` = 放行;`Err` = 错误。
pub trait Listener<E: Event>: Send + Sync + 'static {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a E,
    ) -> BoxFuture<'a, Result<Option<E::Value>, CordisError>>;
}

impl<E, F> Listener<E> for F
where
    E: Event,
    F: for<'a> Fn(&'a Ctx, &'a E) -> BoxFuture<'a, Result<Option<E::Value>, CordisError>>
        + Send
        + Sync
        + 'static,
{
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a E,
    ) -> BoxFuture<'a, Result<Option<E::Value>, CordisError>> {
        self(ctx, e)
    }
}

/// waterfall 监听器(D17:独立注册面,显式接收 `Next` 续延)。
///
/// 不调用 `next` 即 veto 剩余链与终态续延。
pub trait WaterfallListener<E: Event>: Send + Sync + 'static {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a E,
        next: Next<'a, E>,
    ) -> BoxFuture<'a, Result<E::Value, CordisError>>;
}

/// waterfall 终态续延(D17):由调用方提供的兜底行为,不与事件并列。
pub trait Terminal<E: Event>: Send + 'static {
    fn call<'a>(&'a self, ctx: &'a Ctx, e: &'a E) -> BoxFuture<'a, Result<E::Value, CordisError>>;
}

impl<E, F> Terminal<E> for F
where
    E: Event,
    F: for<'a> Fn(&'a Ctx, &'a E) -> BoxFuture<'a, Result<E::Value, CordisError>> + Send + 'static,
{
    fn call<'a>(&'a self, ctx: &'a Ctx, e: &'a E) -> BoxFuture<'a, Result<E::Value, CordisError>> {
        self(ctx, e)
    }
}

/// 类型化续延句柄,交给 [`WaterfallListener`]。零参调用(TS waterfall 语义:
/// 事件载荷固定,值经返回值向上流动),M1 定稿(见 §八)。
pub struct Next<'a, E: Event> {
    pub(crate) inner: crate::bus::ErasedNext<'a>,
    pub(crate) _marker: PhantomData<fn() -> E>,
}

impl<'a, E: Event> Next<'a, E> {
    /// 调用链上的下一个 waterfall 监听器(最终是终态续延)。
    pub fn call(self) -> BoxFuture<'a, Result<E::Value, CordisError>> {
        Box::pin(async move {
            let boxed = self.inner.invoke().await?;
            match boxed.downcast::<E::Value>() {
                Ok(v) => Ok(*v),
                Err(_) => Err(CordisError::PluginFailed(
                    "waterfall value type mismatch".into(),
                )),
            }
        })
    }
}

/// `on()`/`on_waterfall()` 注册选项(§四偏差清单:prepend 保留,影响 serial/waterfall 结果)。
#[derive(Debug, Clone, Copy, Default)]
pub struct EventOptions {
    /// 插到现有监听器之前(默认追加在后)。
    pub prepend: bool,
}

// ── 类型擦除适配层(内部) ────────────────────────────────────────

pub(crate) type ErasedValue = Box<dyn Any + Send>;
pub(crate) type DynEvent = dyn Any + Send + Sync;

pub(crate) struct ListenerAdapter<L, E: Event>(pub L, pub PhantomData<fn() -> E>);

impl<E: Event, L: Listener<E>> crate::bus::ErasedCall for ListenerAdapter<L, E> {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a DynEvent,
    ) -> BoxFuture<'a, Result<Option<ErasedValue>, CordisError>> {
        match e.downcast_ref::<E>() {
            Some(e) => Box::pin(async move {
                self.0
                    .call(ctx, e)
                    .await
                    .map(|opt| opt.map(|v| Box::new(v) as ErasedValue))
            }),
            None => mismatch(),
        }
    }
}

pub(crate) struct WaterfallAdapter<L, E: Event>(pub L, pub PhantomData<fn() -> E>);

impl<E: Event, L: WaterfallListener<E>> crate::bus::ErasedWaterfallCall for WaterfallAdapter<L, E> {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a DynEvent,
        next: crate::bus::ErasedNext<'a>,
    ) -> BoxFuture<'a, Result<ErasedValue, CordisError>> {
        match e.downcast_ref::<E>() {
            Some(e) => {
                let next = Next {
                    inner: next,
                    _marker: PhantomData,
                };
                Box::pin(async move {
                    let v = self.0.call(ctx, e, next).await?;
                    Ok(Box::new(v) as ErasedValue)
                })
            }
            None => mismatch(),
        }
    }
}

pub(crate) struct TerminalAdapter<E: Event, T: Terminal<E>>(pub T, pub PhantomData<fn() -> E>);

impl<E: Event, T: Terminal<E>> crate::bus::ErasedTerminal for TerminalAdapter<E, T> {
    fn call<'a>(
        &'a mut self,
        ctx: &'a Ctx,
        e: &'a DynEvent,
    ) -> BoxFuture<'a, Result<ErasedValue, CordisError>> {
        match e.downcast_ref::<E>() {
            Some(e) => Box::pin(async move {
                self.0
                    .call(ctx, e)
                    .await
                    .map(|v| Box::new(v) as ErasedValue)
            }),
            None => mismatch(),
        }
    }
}

fn mismatch<'a, T>() -> BoxFuture<'a, Result<T, CordisError>> {
    Box::pin(async { Err(CordisError::PluginFailed("event type mismatch".into())) })
}

/// 内联 poll panic 捕获(apply 在驱动任务内联执行,无法经 JoinError 边界)。
pub(crate) struct CatchUnwind<F>(Pin<Box<F>>);

impl<F> CatchUnwind<F> {
    pub(crate) fn new(fut: F) -> Self {
        Self(Box::pin(fut))
    }
}

impl<F: Future> Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn Any + Send>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let inner =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| this.0.as_mut().poll(cx)));
        match inner {
            Ok(poll) => poll.map(Ok),
            Err(p) => Poll::Ready(Err(p)),
        }
    }
}
