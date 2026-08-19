use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ctx::Ctx;
use crate::error::{join_panic_error, panic_error, CordisError};
use crate::event::{
    CatchUnwind, DynEvent, ErasedValue, Event, EventOptions, Listener, ListenerAdapter, Terminal,
    TerminalAdapter, WaterfallAdapter, WaterfallListener,
};
use crate::{BoxFuture, Disposer, Effect};

/// waterfall 链上的擦除续延:调用下一个监听器,最终落到终态续延。
pub(crate) struct ErasedNext<'a> {
    chain: &'a [Arc<dyn ErasedWaterfallCall>],
    index: usize,
    ctx: &'a Ctx,
    event: &'a DynEvent,
    terminal: &'a mut (dyn ErasedTerminal + 'a),
}

impl<'a> ErasedNext<'a> {
    pub(crate) fn invoke(self) -> BoxFuture<'a, Result<ErasedValue, CordisError>> {
        if self.index < self.chain.len() {
            let ErasedNext {
                chain,
                index,
                ctx,
                event,
                terminal,
            } = self;
            chain[index].call(
                ctx,
                event,
                ErasedNext {
                    chain,
                    index: index + 1,
                    ctx,
                    event,
                    terminal,
                },
            )
        } else {
            let ErasedNext {
                ctx,
                event,
                terminal,
                ..
            } = self;
            terminal.call(ctx, event)
        }
    }
}

pub(crate) trait ErasedCall: Send + Sync + 'static {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a DynEvent,
    ) -> BoxFuture<'a, Result<Option<ErasedValue>, CordisError>>;
}

pub(crate) trait ErasedWaterfallCall: Send + Sync + 'static {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a DynEvent,
        next: ErasedNext<'a>,
    ) -> BoxFuture<'a, Result<ErasedValue, CordisError>>;
}

pub(crate) trait ErasedTerminal: Send {
    fn call<'a>(
        &'a mut self,
        ctx: &'a Ctx,
        e: &'a DynEvent,
    ) -> BoxFuture<'a, Result<ErasedValue, CordisError>>;
}

/// 注册的监听器条目(泛型合一,简化 S4):`C` 为擦除后的调用句柄。
struct Hook<C> {
    call: C,
    once: bool,
}

fn insert_hook<C>(list: &mut Vec<Arc<Hook<C>>>, hook: Arc<Hook<C>>, prepend: bool) {
    if prepend {
        list.insert(0, hook);
    } else {
        list.push(hook);
    }
}

fn retain_hook<C>(list: &mut Vec<Arc<Hook<C>>>, hook: &Arc<Hook<C>>) {
    list.retain(|h| !Arc::ptr_eq(h, hook));
}

/// 快照(保位)并从注册表取出 once 条目:恰好一次由调用方持有的总线锁
/// 互斥直接保证——锁外无需任何第二套同步(简化)。
fn claim_once<C>(list: &mut Vec<Arc<Hook<C>>>) -> Vec<Arc<Hook<C>>> {
    let snapshot = list.clone();
    list.retain(|h| !h.once);
    snapshot
}

#[derive(Default)]
struct BusInner {
    hooks: HashMap<TypeId, Vec<Arc<Hook<Arc<dyn ErasedCall>>>>>,
    wf_hooks: HashMap<TypeId, Vec<Arc<Hook<Arc<dyn ErasedWaterfallCall>>>>>,
    /// 同事件类型的派发尾链(D31):每次 emit 的派发任务 await 上一个,
    /// 保证同事件多次 emit 按发射序执行(修 spawn 调度乱序)。
    dispatch_tail: HashMap<TypeId, tokio::task::JoinHandle<()>>,
}

/// 类型化事件总线(D3:回调注册表;D16:四分发,无同步 bail)。
///
/// 监听器经 `Ctx` 注册,自动归该 fiber 所有(D28)。
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Mutex<BusInner>>,
}

impl EventBus {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BusInner::default())),
        }
    }

    /// 注册监听器(默认追加在后)。
    pub fn on<E: Event>(&self, ctx: &Ctx, l: impl Listener<E>) -> Result<Disposer, CordisError> {
        self.add_hook(ctx, l, EventOptions::default(), false)
    }

    /// 注册监听器(带选项)。
    pub fn on_opt<E: Event>(
        &self,
        ctx: &Ctx,
        l: impl Listener<E>,
        opts: EventOptions,
    ) -> Result<Disposer, CordisError> {
        self.add_hook(ctx, l, opts, false)
    }

    /// 注册一次性监听器:至多调用一次。
    pub fn once<E: Event>(&self, ctx: &Ctx, l: impl Listener<E>) -> Result<Disposer, CordisError> {
        self.add_hook(ctx, l, EventOptions::default(), true)
    }

    /// 注册 waterfall 监听器(D17:独立注册面)。
    pub fn on_waterfall<E: Event>(
        &self,
        ctx: &Ctx,
        l: impl WaterfallListener<E>,
    ) -> Result<Disposer, CordisError> {
        self.add_wf_hook(ctx, l, EventOptions::default(), false)
    }

    /// 注册 waterfall 监听器(带选项)。
    pub fn on_waterfall_opt<E: Event>(
        &self,
        ctx: &Ctx,
        l: impl WaterfallListener<E>,
        opts: EventOptions,
    ) -> Result<Disposer, CordisError> {
        self.add_wf_hook(ctx, l, opts, false)
    }

    fn add_hook<E: Event>(
        &self,
        ctx: &Ctx,
        l: impl Listener<E>,
        opts: EventOptions,
        once: bool,
    ) -> Result<Disposer, CordisError> {
        let hook: Arc<Hook<Arc<dyn ErasedCall>>> = Arc::new(Hook {
            call: Arc::new(ListenerAdapter(l, std::marker::PhantomData)),
            once,
        });
        let bus = self.clone();
        ctx.effect(move || {
            {
                let mut inner = bus.inner.lock().unwrap();
                let list = inner.hooks.entry(TypeId::of::<E>()).or_default();
                insert_hook(list, hook.clone(), opts.prepend);
            }
            Effect::Disposer(Box::new(move || {
                let mut inner = bus.inner.lock().unwrap();
                if let Some(list) = inner.hooks.get_mut(&TypeId::of::<E>()) {
                    retain_hook(list, &hook);
                }
                Ok(())
            }))
        })
    }

    fn add_wf_hook<E: Event>(
        &self,
        ctx: &Ctx,
        l: impl WaterfallListener<E>,
        opts: EventOptions,
        once: bool,
    ) -> Result<Disposer, CordisError> {
        let hook: Arc<Hook<Arc<dyn ErasedWaterfallCall>>> = Arc::new(Hook {
            call: Arc::new(WaterfallAdapter(l, std::marker::PhantomData)),
            once,
        });
        let bus = self.clone();
        ctx.effect(move || {
            {
                let mut inner = bus.inner.lock().unwrap();
                let list = inner.wf_hooks.entry(TypeId::of::<E>()).or_default();
                insert_hook(list, hook.clone(), opts.prepend);
            }
            Effect::Disposer(Box::new(move || {
                let mut inner = bus.inner.lock().unwrap();
                if let Some(list) = inner.wf_hooks.get_mut(&TypeId::of::<E>()) {
                    retain_hook(list, &hook);
                }
                Ok(())
            }))
        })
    }

    /// 快照监听器并取出 once 条目(简化:恰好一次由总线锁的互斥直接保证,
    /// 无需第二套原子认领)。**快照保持注册序**(§四:顺序控制影响
    /// serial/waterfall 结果);once 从注册表删除后,Disposer/卸载的
    /// 移除自然变 no-op。
    fn take_hooks<E: Event>(&self, _waterfall: bool) -> Vec<Arc<Hook<Arc<dyn ErasedCall>>>> {
        let mut inner = self.inner.lock().unwrap();
        let Some(list) = inner.hooks.get_mut(&TypeId::of::<E>()) else {
            return Vec::new();
        };
        claim_once(list)
    }

    fn take_wf_hooks<E: Event>(&self) -> Vec<Arc<dyn ErasedWaterfallCall>> {
        let mut inner = self.inner.lock().unwrap();
        let Some(list) = inner.wf_hooks.get_mut(&TypeId::of::<E>()) else {
            return Vec::new();
        };
        claim_once(list)
            .into_iter()
            .map(|h| h.call.clone())
            .collect()
    }

    /// emit:触发即忘(D16/D30)。**同事件类型按发射序串行派发**(D31):
    /// 单次持锁内"取上一派发任务句柄 → spawn 新任务 → 存为尾"(原子,
    /// 防 remove/insert 两段锁在并发同类型 emit 下分叉链);任务内先
    /// await 上一个,再按注册序逐个 await 监听器。监听器 panic 经
    /// CatchUnwind 捕获路由 ErrorSink,`prev.await` 正常返回,链不断;
    /// 监听器内重入 emit 同类事件仅排到链尾,不死锁。跨事件类型不保证
    /// 顺序(已知边界,见 D31)。spawn 在临界区内只入队不同步执行,
    /// std Mutex 无重入,故 `take_hooks` 的锁必须已释放。
    pub fn emit<E: Event>(&self, ctx: &Ctx, e: Arc<E>) {
        let hooks = self.take_hooks::<E>(false);
        if hooks.is_empty() {
            return; // 不进链:无监听器不产生派发任务
        }
        let ctx2 = ctx.clone();
        let sink = ctx.error_sink();
        let handle = ctx.handle().clone();
        let mut inner = self.inner.lock().unwrap();
        let prev = inner.dispatch_tail.remove(&TypeId::of::<E>());
        let tail = handle.spawn(async move {
            // 等同事件上一次派发完成(链式保序)
            if let Some(prev) = prev {
                let _ = prev.await;
            }
            // 按注册序逐个 await(不并发 spawn,否则退回乱序)
            for hook in hooks {
                let out = CatchUnwind::new(hook.call.call(&ctx2, &*e as &DynEvent)).await;
                match out {
                    Ok(Ok(_)) => {}
                    Ok(Err(err)) => sink(Arc::new(err)),
                    Err(p) => sink(Arc::new(panic_error(p))),
                }
            }
        });
        inner.dispatch_tail.insert(TypeId::of::<E>(), tail);
    }

    /// parallel:并发全等,聚合全部错误(JoinSet,D16)。
    pub async fn parallel<E: Event>(&self, ctx: &Ctx, e: Arc<E>) -> Result<(), CordisError> {
        let hooks = self.take_hooks::<E>(false);
        if hooks.is_empty() {
            return Ok(());
        }
        let mut set = tokio::task::JoinSet::new();
        for hook in hooks {
            let ctx2 = ctx.clone();
            let e2 = e.clone();
            set.spawn_on(
                async move { hook.call.call(&ctx2, &*e2 as &DynEvent).await },
                ctx.handle(),
            );
        }
        let mut errors: Vec<CordisError> = Vec::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => errors.push(err),
                // 取消不是 panic:into_panic 会二次 panic(评审 P2)
                Err(join_err) => errors.push(join_panic_error(join_err)),
            }
        }
        match crate::error::aggregate_errors(errors) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// serial:顺序调用至首个短路值 `Ok(Some(v))`(JoinSet 顺序 await,任务 `'static`,D16)。
    /// serial:顺序调用至首个短路值 `Ok(Some(v))`(TS serial 语义:
    /// 上一个监听器完成才调用下一个,按注册序短路)。
    /// 内联顺序 await,不 spawn(载荷可借用,`&E` 对齐 §二 草案);
    /// panic 经 CatchUnwind 边界转 `PluginFailed`(D30 精神)。
    pub async fn serial<E: Event>(
        &self,
        ctx: &Ctx,
        e: &E,
    ) -> Result<Option<E::Value>, CordisError> {
        for hook in self.take_hooks::<E>(false) {
            let outcome = CatchUnwind::new(hook.call.call(ctx, e as &DynEvent)).await;
            match outcome {
                Ok(Ok(Some(boxed))) => {
                    return match boxed.downcast::<E::Value>() {
                        Ok(v) => Ok(Some(*v)),
                        Err(_) => Err(CordisError::PluginFailed(
                            "serial value type mismatch".into(),
                        )),
                    };
                }
                Ok(Ok(None)) => continue,
                Ok(Err(err)) => return Err(err),
                Err(p) => return Err(panic_error(p)),
            }
        }
        Ok(None)
    }

    /// waterfall:中间件续延(D17)。`terminal` 为调用方兜底续延;
    /// 监听器不调用 `next` 即 veto。内联 CPS 递归(见 §八:panic 向分发者传播)。
    pub fn waterfall<'a, E: Event, T: Terminal<E> + 'a>(
        &self,
        ctx: &'a Ctx,
        e: &'a E,
        terminal: T,
    ) -> BoxFuture<'a, Result<E::Value, CordisError>> {
        let bus = self.clone();
        Box::pin(async move {
            let chain = bus.take_wf_hooks::<E>();
            let mut terminal: Box<dyn ErasedTerminal + 'a> =
                Box::new(TerminalAdapter(terminal, std::marker::PhantomData));
            let next = ErasedNext {
                chain: &chain,
                index: 0,
                ctx,
                event: e as &DynEvent,
                terminal: terminal.as_mut(),
            };
            let boxed = next.invoke().await?;
            match boxed.downcast::<E::Value>() {
                Ok(v) => Ok(*v),
                Err(_) => Err(CordisError::PluginFailed(
                    "waterfall value type mismatch".into(),
                )),
            }
        })
    }
}
