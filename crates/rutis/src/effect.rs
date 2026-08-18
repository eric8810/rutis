use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::error::{aggregate_arcs, join_panic_error, panic_error, CordisError};
use crate::BoxFuture;

/// 插件装配交回的清理(D18:清理可报错;闭包捕获 owned `Ctx`,不传 `&Ctx`)。
pub enum Effect {
    /// 无清理。
    Done,
    /// 同步清理。
    Disposer(Box<dyn FnOnce() -> Result<(), CordisError> + Send>),
    /// 异步清理(在任务边界执行,panic 经 JoinError 包 `PluginFailed`,D30)。
    AsyncDisposer(Box<dyn FnOnce() -> BoxFuture<'static, Result<(), CordisError>> + Send>),
    /// 多个清理,按声明序逆序(LIFO)执行。
    Many(Vec<Effect>),
}

impl Effect {
    fn into_cleanups(self, out: &mut Vec<Cleanup>) {
        match self {
            Effect::Done => {}
            Effect::Disposer(f) => out.push(Cleanup::Sync(f)),
            Effect::AsyncDisposer(f) => out.push(Cleanup::Async(f)),
            Effect::Many(v) => {
                for e in v {
                    e.into_cleanups(out);
                }
            }
        }
    }
}

enum Cleanup {
    Sync(Box<dyn FnOnce() -> Result<(), CordisError> + Send>),
    Async(Box<dyn FnOnce() -> BoxFuture<'static, Result<(), CordisError>> + Send>),
}

/// EffectRecord(§四):执行与清理分离;清理恰好一次,重复调用 join 同一结果;
/// 严格 LIFO 串行;单错原样、多错聚合不压平;一个清理失败不阻止其余清理。
pub(crate) struct EffectRecord {
    st: Mutex<EffectState>,
    notify: Notify,
}

enum EffectState {
    Live(Vec<Cleanup>),
    Draining,
    Done(Option<Arc<CordisError>>),
}

impl EffectRecord {
    pub(crate) fn new(effect: Effect) -> Arc<Self> {
        let mut cleanups = Vec::new();
        effect.into_cleanups(&mut cleanups);
        Arc::new(Self {
            st: Mutex::new(EffectState::Live(cleanups)),
            notify: Notify::new(),
        })
    }

    /// 排干清理。首个调用者把清理移入独立任务执行,所有调用者(含被
    /// drop 后重来的)join 同一终态——取消安全:本 future 被 drop 不影响
    /// 清理进度,不会卡死在 Draining(评审 #4)。重复调用 join 同一结果
    /// (`exactly_once_same_error` 断言 `Arc` identity)。
    pub(crate) async fn drain(
        self: &Arc<Self>,
        handle: &tokio::runtime::Handle,
    ) -> Result<(), Arc<CordisError>> {
        let claimed = {
            let mut st = self.st.lock().unwrap();
            match &mut *st {
                EffectState::Live(_) => {
                    let live = std::mem::replace(&mut *st, EffectState::Draining);
                    match live {
                        EffectState::Live(cleanups) => Some(cleanups),
                        _ => unreachable!(),
                    }
                }
                EffectState::Draining | EffectState::Done(_) => None,
            }
        };

        if let Some(mut cleanups) = claimed {
            let this = self.clone();
            let runner = handle.clone();
            handle.spawn(async move {
                // 清理全程 panic 兜底(评审 #7/#11):任何 panic 转为
                // PluginFailed 错误,任务必定写回 Done,不卡 Draining
                let err = run_cleanups(&mut cleanups, &runner).await;
                *this.st.lock().unwrap() = EffectState::Done(err.clone());
                this.notify.notify_waiters();
            });
        }
        self.join().await
    }

    async fn join(&self) -> Result<(), Arc<CordisError>> {
        loop {
            let notified = self.notify.notified();
            {
                let st = self.st.lock().unwrap();
                if let EffectState::Done(e) = &*st {
                    return match e.clone() {
                        Some(e) => Err(e),
                        None => Ok(()),
                    };
                }
            }
            notified.await;
        }
    }
}

/// 严格 LIFO 串行执行清理;闭包调用(`f()`)与 Future poll 两处 panic
/// 边界都捕获为 `PluginFailed`(评审 #7),一个清理失败不阻止其余清理。
async fn run_cleanups(
    cleanups: &mut Vec<Cleanup>,
    handle: &tokio::runtime::Handle,
) -> Option<Arc<CordisError>> {
    let mut errors: Vec<CordisError> = Vec::new();
    while let Some(cleanup) = cleanups.pop() {
        let result = match cleanup {
            Cleanup::Sync(f) => match std::panic::catch_unwind(AssertUnwindSafe(f)) {
                Ok(r) => r,
                Err(p) => Err(panic_error(p)),
            },
            Cleanup::Async(f) => {
                // 闭包调用本身也在任务边界内(评审 #7)
                let fut = match std::panic::catch_unwind(AssertUnwindSafe(f)) {
                    Ok(fut) => fut,
                    Err(p) => {
                        errors.push(panic_error(p));
                        continue;
                    }
                };
                let joined = handle.spawn(fut);
                match joined.await {
                    Ok(r) => r,
                    // 取消不是 panic:into_panic 在取消场景会二次 panic(评审 P2);
                    // 无论哪种,EffectRecord 都最终进 Done,不卡 Draining
                    Err(join_err) => Err(join_panic_error(join_err)),
                }
            }
        };
        if let Err(e) = result {
            errors.push(e);
        }
    }
    aggregate_arcs(errors.into_iter().map(Arc::new).collect())
}

/// 清理句柄(D28):仅表示提前释放,不需要也不应再放回 `Effect`;
/// drop 不触发清理(fiber 卸载仍兜底)。
pub struct Disposer {
    run: Option<Box<dyn FnOnce() -> BoxFuture<'static, Result<(), Arc<CordisError>>> + Send>>,
}

impl Disposer {
    pub(crate) fn new(
        run: Box<dyn FnOnce() -> BoxFuture<'static, Result<(), Arc<CordisError>>> + Send>,
    ) -> Self {
        Self { run: Some(run) }
    }

    /// 提前释放并等待清理完成(幂等:重复调用 join 同一终态)。
    pub fn dispose(mut self) -> BoxFuture<'static, Result<(), Arc<CordisError>>> {
        match self.run.take() {
            Some(run) => run(),
            None => Box::pin(async { Ok(()) }),
        }
    }
}

impl std::fmt::Debug for Disposer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Disposer")
    }
}
