//! cordis 原版 spec 对拍测试(docs/cordis-spec-parity-2026-08-18.md:
//! §二 27 全对拍 + §三 21 内核对拍)。测试名沿用原 spec `it('...')` 标题
//! (snake_case),每条注释标注原文件:行号,便于追溯。
//!
//! 载体改写(语义内核保持,断言按 Rust 通道表达):
//! - fake timers → 确定性同步点(Notify 门 + 状态观察),不做 sleep 计时断言;
//! - 字符串事件名 → 类型化事件(Ping);generator effect → Effect::Many /
//!   分段装配(Staged);Proxy 属性断言 → 状态/get 断言;
//! - 执行错误走 await 通道、不重复进 ErrorSink(TS 同时记 logger);
//! - fiber 级 dispose 返回清理错误(TS resolve + 记 logger;Rust 按 D6/D20
//!   经 dispose 任务通道返回,错误仍恰好观察一次)。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rutis::{
    BoxFuture, CordisError, Ctx, Effect, Event, FiberState, FiberStatusChanged, FiberView,
    Listener, Next, Plugin, PluginId, Terminal, TypeKey, WaterfallListener,
};
use tokio::runtime::Handle;
use tokio::sync::Notify;

// ── 测试助手 ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Ping {
    value: u32,
}

impl Event for Ping {
    const NAME: &'static str = "test::Ping";
    type Value = u32;
}

/// 对拍用服务类型(isolate/service/inertia 系列共用)。
#[derive(Debug)]
struct Foo {
    bar: u32,
}

#[derive(Debug)]
struct Qux;

type Gate = Arc<Notify>;
type Seq = Arc<Mutex<Vec<u32>>>;
type StrSeq = Arc<Mutex<Vec<&'static str>>>;

fn gate() -> Gate {
    Arc::new(Notify::new())
}

fn seq() -> Seq {
    Arc::new(Mutex::new(Vec::new()))
}

fn str_seq() -> StrSeq {
    Arc::new(Mutex::new(Vec::new()))
}

fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

fn view_slot() -> Arc<Mutex<Option<FiberView>>> {
    Arc::new(Mutex::new(None))
}

/// 计数监听器(每命中一次可通知一次)。
struct Rec {
    hits: Arc<AtomicUsize>,
    notify: Option<Gate>,
}

impl Listener<Ping> for Rec {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a Ping,
    ) -> BoxFuture<'a, Result<Option<u32>, CordisError>> {
        let hits = self.hits.clone();
        let notify = self.notify.clone();
        Box::pin(async move {
            hits.fetch_add(1, Ordering::SeqCst);
            if let Some(n) = notify {
                n.notify_one();
            }
            Ok(None)
        })
    }
}

/// 闭包插件助手(与 contract.rs 同形态)。
struct Simple<F> {
    name: &'static str,
    injects: Vec<TypeKey>,
    apply: F,
}

impl<F> Plugin for Simple<F>
where
    F: for<'a> Fn(&'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        self.name
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        (self.apply)(ctx)
    }
}

fn simple<F>(name: &'static str, apply: F) -> Simple<F>
where
    F: for<'a> Fn(&'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> + Send + Sync + 'static,
{
    Simple {
        name,
        injects: vec![],
        apply,
    }
}

fn simple_dep<F>(name: &'static str, injects: Vec<TypeKey>, apply: F) -> Simple<F>
where
    F: for<'a> Fn(&'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> + Send + Sync + 'static,
{
    Simple {
        name,
        injects,
        apply,
    }
}

fn counting_effect(counter: Arc<AtomicUsize>) -> Effect {
    Effect::Disposer(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }))
}

fn failing_effect(msg: &'static str) -> Effect {
    Effect::Disposer(Box::new(move || Err(err(msg))))
}

fn err(msg: &'static str) -> CordisError {
    CordisError::ServiceNotFound(msg.to_string())
}

fn sink_ctx() -> (Ctx, Arc<Mutex<Vec<Arc<CordisError>>>>) {
    let collected: Arc<Mutex<Vec<Arc<CordisError>>>> = Arc::new(Mutex::new(Vec::new()));
    let to = collected.clone();
    let ctx = Ctx::root_with_sink(
        Handle::current(),
        Arc::new(move |e: Arc<CordisError>| to.lock().unwrap().push(e)),
    );
    (ctx, collected)
}

async fn soon<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .expect("timed out")
}

/// 等待 fiber 进入目标状态(watch last-value)。
async fn wait_state(view: &FiberView, want: FiberState) {
    soon(async {
        let mut rx = view.watch();
        loop {
            if rx.borrow().state == want {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await;
}

/// 让出调度若干轮:被门挡住的任务必然无法完成,用于"未落定"断言。
async fn yields() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}

/// 状态迁移日志((plugin_id, from, to) 列表)。
type TransitionLog = Arc<Mutex<Vec<(PluginId, FiberState, FiberState)>>>;

/// 收集状态迁移事件(D24:锁内 FIFO 入队、锁外分发)。
struct StatusLog {
    seen: Arc<Mutex<Vec<(PluginId, FiberState, FiberState)>>>,
}

impl Listener<FiberStatusChanged> for StatusLog {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a FiberStatusChanged,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        let seen = self.seen.clone();
        let ev = e.clone();
        Box::pin(async move {
            seen.lock().unwrap().push((ev.plugin_id, ev.from, ev.to));
            Ok(None)
        })
    }
}

fn transitions_of(log: &TransitionLog, id: PluginId) -> Vec<(FiberState, FiberState)> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|(pid, _, _)| *pid == id)
        .map(|(_, from, to)| (*from, *to))
        .collect()
}

async fn wait_transitions(log: &TransitionLog, id: PluginId, at_least: usize) {
    soon(async {
        loop {
            if transitions_of(log, id).len() >= at_least {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
}

/// 分段装配(dispose.spec async yield 系列的 Rust 载体):每段先等放行门,
/// 段体落地奇数值,注册偶数值清理;段间检查取消——当前段惯性完成后才中止,
/// 后续段不再执行(对齐 TS async generator 的 abort 语义)。
struct Staged {
    name: &'static str,
    gates: Vec<Gate>,
    signals: Vec<Gate>,
    seq: Seq,
}

impl Plugin for Staged {
    fn name(&self) -> &str {
        self.name
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        let gates = self.gates.clone();
        let signals = self.signals.clone();
        let seq = self.seq.clone();
        Box::pin(async move {
            for (i, gate) in gates.iter().enumerate() {
                gate.notified().await;
                let odd = i as u32 * 2 + 1;
                seq.lock().unwrap().push(odd);
                let seq2 = seq.clone();
                let even = i as u32 * 2 + 2;
                ctx.effect(move || {
                    Effect::Disposer(Box::new(move || {
                        seq2.lock().unwrap().push(even);
                        Ok(())
                    }))
                })?;
                signals[i].notify_one();
                if ctx.cancellation_token().is_cancelled() {
                    return Ok(Effect::Done);
                }
            }
            Ok(Effect::Done)
        })
    }
}

// ── reentrant.spec.ts — Fiber adversarial lifecycle ──────────────

// tests/reentrant.spec.ts:29
// 执行错误走 await 通道,回滚清理错误走 ErrorSink,终态 FAILED。
// 偏差:TS 把执行错误也记一次 logger;Rust 只经 await 通道返回(设计 §八)。
#[tokio::test]
async fn keeps_plugin_execution_failure_separate_from_rollback_cleanup_failure() {
    let (ctx, sink) = sink_ctx();
    let view = ctx.plugin(simple("fail-and-dirty", move |ctx: &Ctx| {
        Box::pin(async move {
            ctx.effect(move || Effect::Disposer(Box::new(move || Err(err("cleanup failed")))))?;
            Err(err("execution failed"))
        })
    }));
    let e = (&view)
        .await
        .expect_err("execution failure via await channel");
    assert!(matches!(*e, CordisError::ServiceNotFound(ref m) if m == "execution failed"));
    assert_eq!(view.state().state, FiberState::Failed);
    // 回滚清理错误恰好一条进 sink
    assert_eq!(sink.lock().unwrap().len(), 1);
    assert!(
        matches!(*sink.lock().unwrap()[0], CordisError::ServiceNotFound(ref m) if m == "cleanup failed")
    );
}

// tests/reentrant.spec.ts:158
// 无迁移时重复依赖通知合并:apply 只跑一次。
#[tokio::test]
async fn coalesces_duplicate_dependency_notifications_without_a_transition() {
    let ctx = Ctx::root().unwrap();
    ctx.provide(Foo { bar: 1 }).unwrap();
    let calls = counter();
    let calls_in = calls.clone();
    let view = ctx.plugin(simple_dep(
        "consumer",
        vec![TypeKey::of::<Foo>()],
        move |_ctx: &Ctx| {
            let calls = calls_in.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Effect::Done)
            })
        },
    ));
    (&view).await.expect("load");
    ctx.refresh(); // 重复依赖通知(reflect.notify 的 Rust 面)
    (&view).await.expect("settle");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(view.state().state, FiberState::Active);
}

// tests/reentrant.spec.ts:172
// 两 root 的 provide/inject 互不影响;重 provide 只重载自己作用域的消费者。
#[tokio::test]
async fn distinguishes_provider_incarnations_without_a_global_counter() {
    let root1 = Ctx::root().unwrap();
    let root2 = Ctx::root().unwrap();
    let key = TypeKey::of::<Foo>();
    let values1: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let values2: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let v1 = values1.clone();
    let fiber1 = root1.plugin(simple_dep("c1", vec![key], move |ctx: &Ctx| {
        let v1 = v1.clone();
        Box::pin(async move {
            // 记录本代实际看到的 provider 值( incarnation )
            v1.lock().unwrap().push(ctx.get::<Foo>().expect("foo").bar);
            Ok(Effect::Done)
        })
    }));
    let v2 = values2.clone();
    let fiber2 = root2.plugin(simple_dep("c2", vec![key], move |ctx: &Ctx| {
        let v2 = v2.clone();
        Box::pin(async move {
            v2.lock().unwrap().push(ctx.get::<Foo>().expect("foo").bar);
            Ok(Effect::Done)
        })
    }));
    let dispose1 = root1.provide(Foo { bar: 1 }).unwrap();
    root2.provide(Foo { bar: 10 }).unwrap();
    (&fiber1).await.expect("f1");
    (&fiber2).await.expect("f2");

    dispose1.dispose().await.unwrap();
    wait_state(&fiber1, FiberState::Pending).await;
    root1.provide(Foo { bar: 2 }).unwrap();
    (&fiber1).await.expect("f1 reloads");

    assert_eq!(*values1.lock().unwrap(), vec![1, 2]); // 旧 incarnation → 新值
    assert_eq!(*values2.lock().unwrap(), vec![10]); // 另一 root 不受影响
    assert_eq!(fiber2.state().state, FiberState::Active);
}

// ── reentrant.spec.ts — Fiber publication ownership ──────────────

// tests/reentrant.spec.ts:222
// 观察者(TS internal/plugin → Rust FiberStatusChanged 监听器)抛错只进
// ErrorSink,dispose 仍正常完成,终态 DISPOSED。
#[tokio::test]
async fn logs_disposal_observer_failures_without_rejecting_disposal() {
    struct ErrOnDisposed {
        id: PluginId,
    }
    impl Listener<FiberStatusChanged> for ErrOnDisposed {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a FiberStatusChanged,
        ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
            let hit = e.plugin_id == self.id && e.to == FiberState::Unloading;
            Box::pin(async move {
                if hit {
                    Err(err("observer failed"))
                } else {
                    Ok(None)
                }
            })
        }
    }
    struct MarkDisposed {
        id: PluginId,
        seen: StrSeq,
    }
    impl Listener<FiberStatusChanged> for MarkDisposed {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a FiberStatusChanged,
        ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
            let seen = self.seen.clone();
            let hit = e.plugin_id == self.id && e.to == FiberState::Unloading;
            Box::pin(async move {
                if hit {
                    seen.lock().unwrap().push("disposed");
                }
                Ok(None)
            })
        }
    }

    let (ctx, sink) = sink_ctx();
    let view = ctx.plugin(simple("observed", move |_ctx: &Ctx| {
        Box::pin(async { Ok(Effect::Done) })
    }));
    (&view).await.expect("load");
    ctx.events()
        .on(&ctx, ErrOnDisposed { id: view.id })
        .unwrap();
    let seen = str_seq();
    ctx.events()
        .on(
            &ctx,
            MarkDisposed {
                id: view.id,
                seen: seen.clone(),
            },
        )
        .unwrap();

    view.dispose()
        .await
        .expect("observer failure must not reject disposal");
    assert_eq!(*seen.lock().unwrap(), vec!["disposed"]);
    soon(async {
        while sink.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    assert_eq!(sink.lock().unwrap().len(), 1);
    assert!(
        matches!(*sink.lock().unwrap()[0], CordisError::ServiceNotFound(ref m) if m == "observer failed")
    );
    assert_eq!(view.state().state, FiberState::Disposed);
}

// tests/reentrant.spec.ts:242
// dispose 不等异步观察者;观察者错误事后落 ErrorSink。
// 偏差:清理错误经 dispose 返回(TS resolve + logger),不经 sink 重复记录。
#[tokio::test]
async fn does_not_await_async_disposal_observers_but_still_observes_rejections() {
    struct SlowObserver {
        id: PluginId,
        started: Gate,
        gate: Gate,
    }
    impl Listener<FiberStatusChanged> for SlowObserver {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a FiberStatusChanged,
        ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
            let started = self.started.clone();
            let gate = self.gate.clone();
            let hit = e.plugin_id == self.id && e.to == FiberState::Unloading;
            Box::pin(async move {
                if !hit {
                    return Ok(None);
                }
                started.notify_one();
                gate.notified().await;
                Err(err("observer"))
            })
        }
    }

    let (ctx, sink) = sink_ctx();
    let cleanup_started = gate();
    let cleanup_gate = gate();
    let observer_started = gate();
    let observer_gate = gate();
    let cs_in = cleanup_started.clone();
    let cg_in = cleanup_gate.clone();
    let view = ctx.plugin(simple("async-disposal", move |_ctx: &Ctx| {
        let started = cs_in.clone();
        let gate = cg_in.clone();
        Box::pin(async {
            Ok(Effect::AsyncDisposer(Box::new(move || {
                let started = started.clone();
                let gate = gate.clone();
                Box::pin(async move {
                    started.notify_one();
                    gate.notified().await;
                    Err(err("cleanup"))
                })
            })))
        })
    }));
    (&view).await.expect("load");
    ctx.events()
        .on(
            &ctx,
            SlowObserver {
                id: view.id,
                started: observer_started.clone(),
                gate: observer_gate.clone(),
            },
        )
        .unwrap();

    let v = view.clone();
    let disposal = tokio::spawn(async move { v.dispose().await });
    soon(observer_started.notified()).await; // 观察者已启动
    soon(cleanup_started.notified()).await; // 清理已启动
    cleanup_gate.notify_one();
    let result = disposal.await.unwrap();
    assert!(result.is_err()); // Rust 通道:清理错误经 dispose 返回
    assert_eq!(view.state().state, FiberState::Disposed);
    assert!(sink.lock().unwrap().is_empty()); // 观察者还挂着,尚未报错

    observer_gate.notify_one();
    soon(async {
        while sink.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    assert!(
        matches!(*sink.lock().unwrap()[0], CordisError::ServiceNotFound(ref m) if m == "observer")
    );
}

// tests/reentrant.spec.ts:288
// 父 dispose 排干未激活子的资源:child 从未 apply 即 DISPOSED。
// 载体:internal/plugin 钩子 → 装载窗口内的协作退出 + 级联效应。
#[tokio::test]
async fn lets_parent_disposal_during_publication_drain_pending_child_effects() {
    let ctx = Ctx::root().unwrap();
    let child_calls = counter();
    let child_slot = view_slot();
    let cc_in = child_calls.clone();
    let slot_in = child_slot.clone();
    let owner = ctx.plugin(simple("owner", move |ctx: &Ctx| {
        let child_calls = cc_in.clone();
        let slot = slot_in.clone();
        Box::pin(async move {
            let child = ctx.plugin(simple_dep(
                "child",
                vec![TypeKey::of::<Qux>()], // 依赖永不就绪 → 子保持 Pending
                move |_ctx: &Ctx| {
                    let child_calls = child_calls.clone();
                    Box::pin(async move {
                        child_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(Effect::Done)
                    })
                },
            ));
            *slot.lock().unwrap() = Some(child);
            // 装载(publication)窗口内父被卸载:协作退出(apply 等待取消信号)
            ctx.cancelled().await;
            Ok(Effect::Done)
        })
    }));
    // owner 尚在 LOADING(apply 等待取消信号):dispose 与发布竞争
    wait_state(&owner, FiberState::Loading).await;
    let o = owner.clone();
    let disposal = tokio::spawn(async move { o.dispose().await });
    disposal.await.unwrap().expect("parent disposal completes");

    let child = child_slot.lock().unwrap().clone().expect("child spawned");
    assert_eq!(child_calls.load(Ordering::SeqCst), 0); // child 从未 apply
    soon(async {
        let mut rx = child.watch();
        loop {
            if rx.borrow().state == FiberState::Disposed {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await;
}

// tests/reentrant.spec.ts:330
// 父(装载中)与子的 dispose 汇合到同一进行中的清理。
#[tokio::test]
async fn makes_a_loading_parent_join_child_cleanup_already_in_progress() {
    let ctx = Ctx::root().unwrap();
    let hold = gate(); // 拖住 owner 的 apply(publication 窗口)
    let go = gate(); // 等 test 填充 owner view 槽
    let cleanup_started = gate();
    let cleanup_hold = gate();
    let cleanup_count = counter();
    let owner_slot = view_slot();
    let child_slot = view_slot();

    let hold_in = hold.clone();
    let go_in = go.clone();
    let cs_in = cleanup_started.clone();
    let ch_in = cleanup_hold.clone();
    let cc_in = cleanup_count.clone();
    let os_in = owner_slot.clone();
    let cslot_in = child_slot.clone();
    let owner = ctx.plugin(simple("loading-owner", move |ctx: &Ctx| {
        let hold = hold_in.clone();
        let go = go_in.clone();
        let cleanup_started = cs_in.clone();
        let cleanup_hold = ch_in.clone();
        let cleanup_count = cc_in.clone();
        let owner_slot = os_in.clone();
        let child_slot = cslot_in.clone();
        Box::pin(async move {
            let child_slot_for_child = child_slot.clone();
            let child = ctx.plugin(simple("loading-child", move |ctx: &Ctx| {
                let go = go.clone();
                let started = cleanup_started.clone();
                let hold = cleanup_hold.clone();
                let count = cleanup_count.clone();
                let owner_slot = owner_slot.clone();
                let child_slot = child_slot_for_child.clone();
                Box::pin(async move {
                    go.notified().await; // 等 test 填好 owner view 槽
                    ctx.effect(move || {
                        Effect::AsyncDisposer(Box::new(move || {
                            let started = started.clone();
                            let hold = hold.clone();
                            let count = count.clone();
                            Box::pin(async move {
                                started.notify_one();
                                hold.notified().await;
                                count.fetch_add(1, Ordering::SeqCst);
                                Ok(())
                            })
                        }))
                    })?;
                    let owner = owner_slot.lock().unwrap().clone().unwrap();
                    tokio::spawn(async move {
                        let _ = owner.dispose().await;
                    });
                    let me = child_slot.lock().unwrap().clone().unwrap();
                    tokio::spawn(async move {
                        let _ = me.dispose().await;
                    });
                    Ok(Effect::Done)
                })
            }));
            *child_slot.lock().unwrap() = Some(child);
            hold.notified().await; // owner 保持 LOADING
            Ok(Effect::Done)
        })
    }));
    // owner 的 apply 已创建 child 并填槽(publication 进行中)
    soon(async {
        while child_slot.lock().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    *owner_slot.lock().unwrap() = Some(owner.clone());
    go.notify_one();
    soon(cleanup_started.notified()).await;

    // 两个 dispose 都汇合到进行中的清理:门未放行前必然无法完成
    yields().await;
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 0);

    hold.notify_one(); // owner 的 apply 落地(惯性完成)
    cleanup_hold.notify_one(); // 清理放行
    soon(async {
        let mut rx = owner.watch();
        loop {
            if rx.borrow().state == FiberState::Disposed {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await;
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1); // 恰好一次
}

// ── reentrant.spec.ts — Effect adversarial disposal ──────────────

// tests/reentrant.spec.ts:372
// 重入销毁 join 同一进行中的清理;restart 等清理落地;清理恰好一次。
#[tokio::test]
async fn returns_one_disposal_promise_and_joins_cleanup_already_in_progress() {
    let ctx = Ctx::root().unwrap();
    let started = gate();
    let hold = gate();
    let count = counter();
    let c = count.clone();
    let started_in = started.clone();
    let hold_in = hold.clone();
    let d = ctx
        .effect(move || {
            let started = started_in.clone();
            let hold = hold_in.clone();
            let c = c.clone();
            Effect::AsyncDisposer(Box::new(move || {
                let started = started.clone();
                let hold = hold.clone();
                let c = c.clone();
                Box::pin(async move {
                    started.notify_one();
                    hold.notified().await;
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }))
        })
        .unwrap();

    let first = tokio::spawn(async move { d.dispose().await }); // 提前释放
    soon(started.notified()).await; // 清理进行中

    let done = Arc::new(AtomicBool::new(false));
    let d2 = done.clone();
    let root = ctx.root_view();
    let restarting = tokio::spawn(async move {
        let _ = root.restart().await;
        d2.store(true, Ordering::SeqCst);
    });
    yields().await;
    assert!(!done.load(Ordering::SeqCst)); // restart 等待进行中的清理

    hold.notify_one();
    first.await.unwrap().expect("early release completes");
    restarting.await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // 后续销毁 join 缓存终态,不重跑
    ctx.root_view().dispose().await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

// tests/reentrant.spec.ts:397
// 清理按 LIFO 全量执行;失败确定性聚合(顺序 = LIFO 执行序)。
#[tokio::test]
async fn attempts_every_cleanup_in_lifo_order_and_aggregates_failures_deterministically() {
    let ctx = Ctx::root().unwrap();
    let s = seq();
    let s_in = s.clone();
    let d = ctx
        .effect(move || {
            let s = s_in.clone();
            let s1 = s.clone();
            let s2 = s.clone();
            let s3 = s.clone();
            Effect::Many(vec![
                Effect::Disposer(Box::new(move || {
                    s1.lock().unwrap().push(1);
                    Err(err("first"))
                })),
                Effect::AsyncDisposer(Box::new(move || {
                    let s2 = s2.clone();
                    Box::pin(async move {
                        tokio::task::yield_now().await;
                        s2.lock().unwrap().push(2);
                        Ok(())
                    })
                })),
                Effect::Disposer(Box::new(move || {
                    s3.lock().unwrap().push(3);
                    Err(err("third"))
                })),
            ])
        })
        .unwrap();
    let e = d.dispose().await.expect_err("aggregate");
    assert_eq!(*s.lock().unwrap(), vec![3, 2, 1]); // LIFO
    match &*e {
        CordisError::Aggregate { errors } => {
            assert!(matches!(*errors[0], CordisError::ServiceNotFound(ref m) if m == "third"));
            assert!(matches!(*errors[1], CordisError::ServiceNotFound(ref m) if m == "first"));
        }
        other => panic!("expected aggregate, got {other:?}"),
    }
}

// tests/reentrant.spec.ts:423
// 用户自己的 Aggregate 整体作为单元素进聚合,不拆散(不压平)。
#[tokio::test]
async fn preserves_an_aggregate_error_thrown_by_user_cleanup_as_one_failure() {
    let ctx = Ctx::root().unwrap();
    let d = ctx
        .effect(move || {
            Effect::Many(vec![
                Effect::Disposer(Box::new(|| {
                    Err(CordisError::Aggregate {
                        errors: vec![Arc::new(err("a")), Arc::new(err("b"))],
                    })
                })),
                Effect::Disposer(Box::new(|| Err(err("other")))),
            ])
        })
        .unwrap();
    let e = d.dispose().await.expect_err("aggregate");
    match &*e {
        CordisError::Aggregate { errors } => {
            assert_eq!(errors.len(), 2);
            assert!(matches!(*errors[0], CordisError::ServiceNotFound(ref m) if m == "other"));
            match &*errors[1] {
                CordisError::Aggregate { errors: inner } => assert_eq!(inner.len(), 2),
                other => panic!("expected nested aggregate, got {other:?}"),
            }
        }
        other => panic!("expected aggregate, got {other:?}"),
    }
}

// tests/reentrant.spec.ts:437
// 直接清理失败对共享 promise 可见:所有等待者观察同一错误(Arc identity)。
#[tokio::test]
async fn keeps_a_direct_cleanup_failure_observable_through_the_shared_promise() {
    let ctx = Ctx::root().unwrap();
    let d = ctx
        .effect(move || Effect::Disposer(Box::new(move || Err(err("cleanup failed")))))
        .unwrap();
    let e1 = d.dispose().await.expect_err("first observation");
    // fiber 级卸载 join 同一记录:同一错误 identity
    let e2 = ctx
        .root_view()
        .dispose()
        .await
        .expect_err("second observation");
    assert!(Arc::ptr_eq(&e1, &e2));
}

// tests/reentrant.spec.ts:448
// restart 时清理抛错只记日志(ErrorSink),fiber 回 ACTIVE。
#[tokio::test]
async fn contains_cleanup_failure_at_structural_restart() {
    let (ctx, sink) = sink_ctx();
    let view = ctx.plugin(simple("dirty-restart", move |_ctx: &Ctx| {
        Box::pin(async { Ok(failing_effect("cleanup failed")) })
    }));
    (&view).await.expect("load");
    view.restart().await.expect("restart resolves");
    assert_eq!(view.state().state, FiberState::Active);
    assert_eq!(sink.lock().unwrap().len(), 1);
    assert!(
        matches!(*sink.lock().unwrap()[0], CordisError::ServiceNotFound(ref m) if m == "cleanup failed")
    );
}

// tests/reentrant.spec.ts:460
// 同步执行错误当场抛给调用方,回滚清理错误走 logger,restart 落地后 ACTIVE。
// 载体:generator 的同步 throw → apply 的 Err(settle 先于 restart 入队,
// 保证调用方能观察到执行错误)。
#[tokio::test]
async fn separates_synchronous_execution_and_rollback_cleanup_failures() {
    let (ctx, sink) = sink_ctx();
    let registered = gate();
    let hold = gate();
    let slot = view_slot();
    let attempts = counter();
    let reg_in = registered.clone();
    let hold_in = hold.clone();
    let slot_in = slot.clone();
    let att_in = attempts.clone();
    let view = ctx.plugin(simple("sync-exec-fail", move |ctx: &Ctx| {
        let registered = reg_in.clone();
        let hold = hold_in.clone();
        let slot = slot_in.clone();
        let attempts = att_in.clone();
        Box::pin(async move {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                ctx.effect(move || failing_effect("cleanup failed"))?;
                registered.notify_one();
                hold.notified().await;
                let v = slot.lock().unwrap().clone().unwrap();
                tokio::spawn(async move {
                    let _ = v.restart().await;
                });
                Err(err("execution failed"))
            } else {
                Ok(Effect::Done)
            }
        })
    }));
    *slot.lock().unwrap() = Some(view.clone());
    soon(registered.notified()).await;

    // settle 先入队:观察到执行错误(当场抛给调用方的 Rust 对应物)
    let settle = {
        let v = view.clone();
        tokio::spawn(async move { (&v).await })
    };
    hold.notify_one();
    let e = settle
        .await
        .unwrap()
        .expect_err("execution failure observed");
    assert!(matches!(*e, CordisError::ServiceNotFound(ref m) if m == "execution failed"));

    wait_state(&view, FiberState::Active).await; // restart 落地
    assert_eq!(sink.lock().unwrap().len(), 1); // 回滚清理错误恰好一条
    assert!(
        matches!(*sink.lock().unwrap()[0], CordisError::ServiceNotFound(ref m) if m == "cleanup failed")
    );
}

// tests/reentrant.spec.ts:479
// 失败 effect 回滚已收集清理恰好一次,之后不残留、不重放。
#[tokio::test]
async fn removes_a_synchronously_failed_effect_after_rolling_back_collected_cleanup() {
    let ctx = Ctx::root().unwrap();
    let cleanups = counter();
    let attempts = counter();
    let cu_in = cleanups.clone();
    let att_in = attempts.clone();
    let view = ctx.plugin(simple("rollback-once", move |ctx: &Ctx| {
        let cleanups = cu_in.clone();
        let attempts = att_in.clone();
        Box::pin(async move {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                let c = cleanups.clone();
                ctx.effect(move || counting_effect(c))?;
                Err(err("execution failed"))
            } else {
                Ok(Effect::Done)
            }
        })
    }));
    let e = (&view).await.expect_err("execution failed");
    assert!(matches!(*e, CordisError::ServiceNotFound(ref m) if m == "execution failed"));
    assert_eq!(cleanups.load(Ordering::SeqCst), 1); // 回滚恰好一次

    view.dispose().await.unwrap(); // 不重放
    assert_eq!(cleanups.load(Ordering::SeqCst), 1);
    assert_eq!(view.state().state, FiberState::Disposed);
}

// tests/reentrant.spec.ts:492
// 重入 restart 阻塞在异步回滚上;放行后 resolve,不重放执行错误。
#[tokio::test]
async fn makes_reentrant_restart_await_async_rollback_without_replaying_the_execution_failure() {
    let ctx = Ctx::root().unwrap();
    let registered = gate();
    let cleanup_started = gate();
    let cleanup_hold = gate();
    let cleanup_count = counter();
    let slot = view_slot();
    let attempts = counter();
    let reg_in = registered.clone();
    let cs_in = cleanup_started.clone();
    let ch_in = cleanup_hold.clone();
    let cc_in = cleanup_count.clone();
    let slot_in = slot.clone();
    let att_in = attempts.clone();
    let view = ctx.plugin(simple("async-rollback", move |ctx: &Ctx| {
        let registered = reg_in.clone();
        let started = cs_in.clone();
        let hold = ch_in.clone();
        let count = cc_in.clone();
        let slot = slot_in.clone();
        let attempts = att_in.clone();
        Box::pin(async move {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                let started = started.clone();
                let hold = hold.clone();
                let count = count.clone();
                ctx.effect(move || {
                    Effect::AsyncDisposer(Box::new(move || {
                        let started = started.clone();
                        let hold = hold.clone();
                        let count = count.clone();
                        Box::pin(async move {
                            started.notify_one();
                            hold.notified().await;
                            count.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        })
                    }))
                })?;
                registered.notify_one();
                let v = slot.lock().unwrap().clone().unwrap();
                tokio::spawn(async move {
                    let _ = v.restart().await;
                });
                Err(err("execution failed"))
            } else {
                Ok(Effect::Done)
            }
        })
    }));
    *slot.lock().unwrap() = Some(view.clone());
    soon(registered.notified()).await;

    soon(cleanup_started.notified()).await; // 回滚清理进行中
    yields().await;
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 0); // restart 尚未落地

    cleanup_hold.notify_one();
    wait_state(&view, FiberState::Active).await; // restart 放行后完成,新代 ACTIVE
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1); // 恰好一次
}

// tests/reentrant.spec.ts:519
// 异步执行错误与清理错误分走两通道。
// 偏差:TS 清理错误经 dispose() 拒绝;Rust 失败装载先回滚(错误进 sink),
// dispose 再调用时无残留 → Ok(TS fiber.ts:749-779 失败路径的 §八 17 落法)。
#[tokio::test]
async fn separates_asynchronous_execution_and_disposal_failures() {
    let (ctx, sink) = sink_ctx();
    let view = ctx.plugin(simple("two-channels", move |ctx: &Ctx| {
        Box::pin(async move {
            ctx.effect(move || failing_effect("cleanup failed"))?;
            Err(err("execution failed"))
        })
    }));
    let e = (&view).await.expect_err("execution channel");
    assert!(matches!(*e, CordisError::ServiceNotFound(ref m) if m == "execution failed"));
    assert_eq!(sink.lock().unwrap().len(), 1); // 清理错误走回滚通道
    assert!(
        matches!(*sink.lock().unwrap()[0], CordisError::ServiceNotFound(ref m) if m == "cleanup failed")
    );
    view.dispose().await.unwrap(); // 已回滚 → 无残留清理
    assert_eq!(view.state().state, FiberState::Disposed);
}

// tests/reentrant.spec.ts:532
// 自动回滚的清理错误只记一次(即使结构性重启也 join 同一回滚)。
#[tokio::test]
async fn logs_auto_rollback_cleanup_failure_once_when_a_structural_owner_joins() {
    let (ctx, sink) = sink_ctx();
    let registered = gate();
    let hold = gate();
    let slot = view_slot();
    let attempts = counter();
    let restart_done = Arc::new(AtomicBool::new(false));
    let reg_in = registered.clone();
    let hold_in = hold.clone();
    let slot_in = slot.clone();
    let att_in = attempts.clone();
    let flag_in = restart_done.clone();
    let view = ctx.plugin(simple("log-once", move |ctx: &Ctx| {
        let registered = reg_in.clone();
        let hold = hold_in.clone();
        let slot = slot_in.clone();
        let attempts = att_in.clone();
        let restart_done = flag_in.clone();
        Box::pin(async move {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                ctx.effect(move || failing_effect("cleanup failed"))?;
                registered.notify_one();
                hold.notified().await;
                let v = slot.lock().unwrap().clone().unwrap();
                let flag = restart_done.clone();
                tokio::spawn(async move {
                    let _ = v.restart().await;
                    flag.store(true, Ordering::SeqCst);
                });
                Err(err("execution failed"))
            } else {
                Ok(Effect::Done)
            }
        })
    }));
    *slot.lock().unwrap() = Some(view.clone());
    soon(registered.notified()).await;
    hold.notify_one();
    soon(async {
        while !restart_done.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    // 回滚 + 重启 join 同一回滚:错误只记录一次
    assert_eq!(sink.lock().unwrap().len(), 1);
    assert!(
        matches!(*sink.lock().unwrap()[0], CordisError::ServiceNotFound(ref m) if m == "cleanup failed")
    );
    assert_eq!(view.state().state, FiberState::Active);
}

// tests/reentrant.spec.ts:553
// 重入 restart 等待异步 setup 及其异步清理全部落地。
#[tokio::test]
async fn makes_reentrant_restart_await_async_execution_and_cleanup() {
    let ctx = Ctx::root().unwrap();
    let go = gate();
    let exec_hold = gate();
    let cleanup_started = gate();
    let cleanup_hold = gate();
    let cleanup_count = counter();
    let slot = view_slot();
    let attempts = counter();
    let go_in = go.clone();
    let eh_in = exec_hold.clone();
    let cs_in = cleanup_started.clone();
    let ch_in = cleanup_hold.clone();
    let cc_in = cleanup_count.clone();
    let slot_in = slot.clone();
    let att_in = attempts.clone();
    let view = ctx.plugin(simple("await-both", move |_ctx: &Ctx| {
        let go = go_in.clone();
        let exec_hold = eh_in.clone();
        let started = cs_in.clone();
        let hold = ch_in.clone();
        let count = cc_in.clone();
        let slot = slot_in.clone();
        let attempts = att_in.clone();
        Box::pin(async move {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                go.notified().await;
                let v = slot.lock().unwrap().clone().unwrap();
                tokio::spawn(async move {
                    let _ = v.restart().await;
                });
                exec_hold.notified().await; // 异步执行段
                let started = started.clone();
                let hold = hold.clone();
                let count = count.clone();
                Ok(Effect::AsyncDisposer(Box::new(move || {
                    let started = started.clone();
                    let hold = hold.clone();
                    let count = count.clone();
                    Box::pin(async move {
                        started.notify_one();
                        hold.notified().await;
                        count.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                })))
            } else {
                Ok(Effect::Done)
            }
        })
    }));
    *slot.lock().unwrap() = Some(view.clone());
    go.notify_one();
    exec_hold.notify_one(); // 执行段放行
    soon(cleanup_started.notified()).await; // 清理已开始(restart 的卸载)

    yields().await;
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 0); // restart 等待清理

    cleanup_hold.notify_one();
    wait_state(&view, FiberState::Active).await;
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
}

// tests/reentrant.spec.ts:581
// 卸载期注册 effect 报 INACTIVE_EFFECT,fiber 恢复 ACTIVE。
#[tokio::test]
async fn rejects_effect_registration_during_unload() {
    let ctx = Ctx::root().unwrap();
    let captured: Arc<Mutex<Option<CordisError>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();
    let view = ctx.plugin(simple("reject-during-unload", move |ctx: &Ctx| {
        let cap = cap.clone();
        let ctx2 = ctx.clone();
        Box::pin(async {
            Ok(Effect::Disposer(Box::new(move || {
                let cap = cap.clone();
                let ctx3 = ctx2.clone();
                *cap.lock().unwrap() = ctx3.effect(move || Effect::Done).err();
                Ok(())
            })))
        })
    }));
    (&view).await.expect("load");
    view.restart().await.expect("restart completes");
    assert!(matches!(
        captured.lock().unwrap().take(),
        Some(CordisError::InactiveEffect)
    ));
    assert_eq!(view.state().state, FiberState::Active);
}

// tests/reentrant.spec.ts:598
// LOADING 期注册 effect 合法,dispose 时执行。
// 偏差:PENDING 探测点借 internal/plugin 钩子(Rust 无此扩展面,刻意不保真);
// 此处对拍装载期半段 + Pending 期不启动的语义。
#[tokio::test]
async fn accepts_effects_while_a_child_is_pending_or_loading() {
    let ctx = Ctx::root().unwrap();
    let cleaned: StrSeq = str_seq();
    let slot = view_slot();
    let cleaned_in = cleaned.clone();
    let slot_in = slot.clone();
    let view = ctx.plugin(simple_dep(
        "state-probe",
        vec![TypeKey::of::<Qux>()], // 先缺依赖 → PENDING
        move |ctx: &Ctx| {
            let cleaned = cleaned_in.clone();
            let slot = slot_in.clone();
            Box::pin(async move {
                let v = slot.lock().unwrap().clone().expect("slot filled");
                assert_eq!(v.state().state, FiberState::Loading); // apply 期 = LOADING
                let cleaned = cleaned.clone();
                ctx.effect(move || {
                    let cleaned = cleaned.clone();
                    Effect::Disposer(Box::new(move || {
                        cleaned.lock().unwrap().push("loading");
                        Ok(())
                    }))
                })?;
                Ok(Effect::Done)
            })
        },
    ));
    // PENDING 期:apply 未跑,注册不发生
    assert_eq!(view.state().state, FiberState::Pending);
    *slot.lock().unwrap() = Some(view.clone());
    ctx.provide(Qux).unwrap();
    (&view).await.expect("loads");
    view.dispose().await.unwrap();
    assert_eq!(*cleaned.lock().unwrap(), vec!["loading"]);
}

// tests/reentrant.spec.ts:134
// 过期代异步失败不污染当前代(stale-epoch)。
// 载体:update config → restart 换代;偏差:stale 错误不进 logger
// (Rust 经状态通道,最终 error 为 None)。
#[tokio::test]
async fn does_not_let_a_stale_execution_failure_poison_the_current_generation() {
    let ctx = Ctx::root().unwrap();
    let hold = gate();
    let attempts = counter();
    let values: StrSeq = str_seq();
    let hold_in = hold.clone();
    let att_in = attempts.clone();
    let val_in = values.clone();
    let view = ctx.plugin(simple("stale-gen", move |_ctx: &Ctx| {
        let hold = hold_in.clone();
        let attempts = att_in.clone();
        let values = val_in.clone();
        Box::pin(async move {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            values
                .lock()
                .unwrap()
                .push(if n == 0 { "old" } else { "new" });
            if n == 0 {
                hold.notified().await;
                Err(err("stale execution"))
            } else {
                Ok(Effect::Done)
            }
        })
    }));
    wait_state(&view, FiberState::Loading).await;
    let v2 = view.clone();
    let restarting = tokio::spawn(async move { v2.restart().await });
    hold.notify_one(); // 旧代失败落地
    restarting.await.unwrap().expect("restart resolves");
    assert_eq!(*values.lock().unwrap(), vec!["old", "new"]);
    assert_eq!(view.state().state, FiberState::Active);
    assert!(view.state().error.is_none()); // 当前代不被旧失败污染
}

// ── fiber.spec.ts ────────────────────────────────────────────────

// tests/fiber.spec.ts:7 — fake timers → 同步点
// LOADING 期依赖消失不立即卸载;apply 惯性完成后经 UNLOADING 回 PENDING;
// 重新 provide 后再 LOADING→ACTIVE。
#[tokio::test]
async fn inertia_lock_1() {
    let ctx = Ctx::root().unwrap();
    let log: TransitionLog = Arc::new(Mutex::new(Vec::new()));
    ctx.events()
        .on(&ctx, StatusLog { seen: log.clone() })
        .unwrap();
    let exec_hold = gate();
    let cleanup_count = counter();
    let applies = counter();
    let d0 = ctx.provide(Foo { bar: 1 }).unwrap();
    let eh_in = exec_hold.clone();
    let cc_in = cleanup_count.clone();
    let ap_in = applies.clone();
    let view = ctx.plugin(simple_dep(
        "inertia-1",
        vec![TypeKey::of::<Foo>()],
        move |_ctx: &Ctx| {
            let hold = eh_in.clone();
            let count = cc_in.clone();
            let applies = ap_in.clone();
            Box::pin(async move {
                // 仅首代装载受门控制(Notify 许可单次,再装载直接放行)
                if applies.fetch_add(1, Ordering::SeqCst) == 0 {
                    hold.notified().await;
                }
                Ok(counting_effect(count))
            })
        },
    ));

    wait_state(&view, FiberState::Loading).await;
    let dd = d0;
    let _evict = tokio::spawn(async move { dd.dispose().await });
    // 惯性:依赖已消失,装载中的 fiber 不立即卸载
    assert_eq!(view.state().state, FiberState::Loading);

    exec_hold.notify_one(); // apply 惯性完成
    wait_state(&view, FiberState::Pending).await; // 经 UNLOADING 回 PENDING
    _evict.await.unwrap().unwrap();

    ctx.provide(Foo { bar: 1 }).unwrap(); // 重新 provide
    wait_state(&view, FiberState::Active).await;

    wait_transitions(&log, view.id, 6).await;
    assert_eq!(
        transitions_of(&log, view.id),
        vec![
            (FiberState::Pending, FiberState::Loading),
            (FiberState::Loading, FiberState::Active),
            (FiberState::Active, FiberState::Unloading),
            (FiberState::Unloading, FiberState::Pending),
            (FiberState::Pending, FiberState::Loading),
            (FiberState::Loading, FiberState::Active),
        ]
    );
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
}

// tests/fiber.spec.ts:27
// LOADING 期同 fiber 被重新 provide:in-flight 加载直接完成进 ACTIVE,
// 不换代重载(apply 恰好一次)。
#[tokio::test]
async fn inertia_lock_2() {
    let ctx = Ctx::root().unwrap();
    let log: TransitionLog = Arc::new(Mutex::new(Vec::new()));
    ctx.events()
        .on(&ctx, StatusLog { seen: log.clone() })
        .unwrap();
    let exec_hold = gate();
    let applies = counter();
    let d0 = ctx.provide(Foo { bar: 1 }).unwrap();
    let eh_in = exec_hold.clone();
    let app_in = applies.clone();
    let view = ctx.plugin(simple_dep(
        "inertia-2",
        vec![TypeKey::of::<Foo>()],
        move |_ctx: &Ctx| {
            let hold = eh_in.clone();
            let applies = app_in.clone();
            Box::pin(async move {
                applies.fetch_add(1, Ordering::SeqCst);
                hold.notified().await;
                Ok(Effect::Done)
            })
        },
    ));
    wait_state(&view, FiberState::Loading).await;

    let dd = d0;
    let evict = tokio::spawn(async move { dd.dispose().await });
    assert_eq!(view.state().state, FiberState::Loading); // 惯性:仍 LOADING
                                                         // 让驱逐任务跑到标记摘除(标记在 evict 首个 await 前同步完成)
    yields().await;
    ctx.provide(Foo { bar: 2 }).unwrap(); // 同 fiber 重新 provide(替换摘除中的绑定)

    exec_hold.notify_one(); // in-flight 装载完成
    wait_state(&view, FiberState::Active).await;
    evict.await.unwrap().unwrap();
    (&view).await.expect("no further transition");

    assert_eq!(applies.load(Ordering::SeqCst), 1); // 不换代重载
    wait_transitions(&log, view.id, 2).await;
    assert_eq!(
        transitions_of(&log, view.id),
        vec![
            (FiberState::Pending, FiberState::Loading),
            (FiberState::Loading, FiberState::Active),
        ]
    );
}

// tests/fiber.spec.ts:43
// provider dispose 后消费者回 PENDING(驱逐 + 级联清理)。
#[tokio::test]
async fn inertia_lock_3() {
    let ctx = Ctx::root().unwrap();
    let cleanup_count = counter();
    let cc_in = cleanup_count.clone();
    let provider = ctx.plugin(simple("provider", move |ctx: &Ctx| {
        Box::pin(async move {
            ctx.provide(Foo { bar: 1 })?;
            Ok(Effect::Done)
        })
    }));
    (&provider).await.expect("P active");
    let view = ctx.plugin(simple_dep(
        "consumer",
        vec![TypeKey::of::<Foo>()],
        move |_ctx: &Ctx| {
            let count = cc_in.clone();
            Box::pin(async move { Ok(counting_effect(count)) })
        },
    ));
    (&view).await.expect("C active");

    provider.dispose().await.unwrap();
    wait_state(&view, FiberState::Pending).await;
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
}

// tests/fiber.spec.ts:65
// apply 抛错 → FAILED;失败 fiber 注册的监听器已回滚、不再触发。
// 偏差:执行错误不进 logger(Rust 走 await 通道)。
#[tokio::test]
async fn plugin_error() {
    let (ctx, sink) = sink_ctx();
    let hits = counter();
    let fail = {
        let hits = hits.clone();
        simple("apply-fail", move |ctx: &Ctx| {
            let hits = hits.clone();
            Box::pin(async move {
                ctx.events().on(ctx, Rec { hits, notify: None })?;
                Err(err("plugin error"))
            })
        })
    };
    let ok = {
        let hits = hits.clone();
        simple("apply-ok", move |ctx: &Ctx| {
            let hits = hits.clone();
            Box::pin(async move {
                ctx.events().on(ctx, Rec { hits, notify: None })?;
                Ok(Effect::Done)
            })
        })
    };
    let fiber1 = ctx.plugin(fail);
    let fiber2 = ctx.plugin(ok);
    let e = (&fiber1).await.expect_err("apply fails");
    assert!(matches!(*e, CordisError::ServiceNotFound(ref m) if m == "plugin error"));
    assert_eq!(fiber1.state().state, FiberState::Failed);
    (&fiber2).await.expect("fiber2 loads");
    assert_eq!(fiber2.state().state, FiberState::Active);
    assert!(sink.lock().unwrap().is_empty()); // 偏差:不记 logger

    ctx.events().serial(&ctx, &Ping { value: 1 }).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1); // 只有成功 fiber 的监听器
}

// tests/fiber.spec.ts:87
// dispose 抛错仍恰好一次。
// 偏差:TS fiber.dispose() resolve + 记 logger;Rust 经 dispose 任务通道
// 返回清理错误(D6/D20),错误仍恰好观察一次,终态 DISPOSED。
#[tokio::test]
async fn dispose_error() {
    let (ctx, sink) = sink_ctx();
    let calls = counter();
    let calls_in = calls.clone();
    let view = ctx.plugin(simple("dispose-fail", move |_ctx: &Ctx| {
        let calls = calls_in.clone();
        Box::pin(async {
            Ok(Effect::Disposer(Box::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(err("test"))
            })))
        })
    }));
    (&view).await.expect("load");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let e1 = view
        .dispose()
        .await
        .expect_err("cleanup error via dispose channel");
    assert!(matches!(*e1, CordisError::ServiceNotFound(ref m) if m == "test"));
    assert_eq!(calls.load(Ordering::SeqCst), 1); // 恰好一次
    let e2 = view.clone().dispose().await.expect_err("joins same error");
    assert!(Arc::ptr_eq(&e1, &e2));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(view.state().state, FiberState::Disposed);
    assert!(sink.lock().unwrap().is_empty()); // 不经 sink 重复记录
}

// tests/fiber.spec.ts:126(§三 部分对拍)
// 内核:restart 后重放 apply 回 ACTIVE(Proxy 外壳断言不做)。
#[tokio::test]
async fn restart_wrapped_fiber_replays_apply() {
    let ctx = Ctx::root().unwrap();
    let applies = counter();
    let att_in = applies.clone();
    let view = ctx.plugin(simple("replay", move |_ctx: &Ctx| {
        let applies = att_in.clone();
        Box::pin(async move {
            applies.fetch_add(1, Ordering::SeqCst);
            Ok(Effect::Done)
        })
    }));
    (&view).await.expect("first apply");
    view.restart().await.expect("restart");
    assert_eq!(applies.load(Ordering::SeqCst), 2);
    assert_eq!(view.state().state, FiberState::Active);
}

// tests/fiber.spec.ts:140(§三 部分对拍)
// 内核:provider 换代驱逐 consumer 按序重载,consumer 逐代看到 provider 值。
// 载体:update config(M4)→ provider.restart() 换代。
#[tokio::test]
async fn update_config_while_injected_service_reloads() {
    let ctx = Ctx::root().unwrap();
    let value = Arc::new(Mutex::new(1u32));
    let applied: Arc<Mutex<Vec<(u32, &'static str)>>> = Arc::new(Mutex::new(Vec::new()));
    let provider = {
        let value = value.clone();
        ctx.plugin(simple("provider", move |ctx: &Ctx| {
            let v = *value.lock().unwrap();
            Box::pin(async move {
                ctx.provide(Foo { bar: v })?;
                Ok(Effect::Done)
            })
        }))
    };
    let consumer = {
        let applied = applied.clone();
        ctx.plugin(simple_dep(
            "consumer",
            vec![TypeKey::of::<Foo>()],
            move |ctx: &Ctx| {
                let applied = applied.clone();
                Box::pin(async move {
                    let bar = ctx.get::<Foo>().expect("service present").bar;
                    applied.lock().unwrap().push((bar, "old"));
                    Ok(Effect::Done)
                })
            },
        ))
    };
    (&provider).await.expect("P");
    (&consumer).await.expect("C");
    assert_eq!(*applied.lock().unwrap(), vec![(1, "old")]);

    *value.lock().unwrap() = 2;
    provider.restart().await.expect("provider re-incarnates");
    (&provider).await.expect("P settles");
    (&consumer).await.expect("C reloaded");
    assert_eq!(*applied.lock().unwrap(), vec![(1, "old"), (2, "old")]); // 按序重载
    assert_eq!(consumer.state().state, FiberState::Active);
}

// ── dispose.spec.ts ──────────────────────────────────────────────

// tests/dispose.spec.ts:7(§三 内核)
// fiber.dispose 恰好一次执行清理,幂等(getEffects label 树为 JS 内省,不做)。
#[tokio::test]
async fn dispose_by_plugin() {
    let ctx = Ctx::root().unwrap();
    let calls = counter();
    let c_in = calls.clone();
    let view = ctx.plugin(simple("by-plugin", move |_ctx: &Ctx| {
        let calls = c_in.clone();
        Box::pin(async { Ok(counting_effect(calls)) })
    }));
    (&view).await.expect("load");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    view.dispose().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    view.dispose().await.unwrap(); // 幂等
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// tests/dispose.spec.ts:23(§三 内核)
// 手动 Disposer 提前释放恰好一次;fiber 卸载兜底不重跑。
// 载体:TS disposer 为可重复调用的函数;Rust Disposer 单次消费,
// "第二次调用 join 同一结果"由 fiber 卸载 join 表达。
#[tokio::test]
async fn dispose_manually() {
    let ctx = Ctx::root().unwrap();
    let calls = counter();
    let c_in = calls.clone();
    let d = ctx
        .effect(move || {
            let calls = c_in.clone();
            counting_effect(calls)
        })
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    d.dispose().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    ctx.root_view().dispose().await.unwrap(); // 兜底不重跑
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// tests/dispose.spec.ts:37(§三 内核)
// generator 多清理:LIFO [3,2,1] + 恰好一次 + 重入 join 同一结果
// (label 树为 JS 内省,不做;监听器注册穿插在序列中,其清理不可见)。
#[tokio::test]
async fn yield_dispose() {
    let ctx = Ctx::root().unwrap();
    let s = seq();
    let s1 = s.clone();
    ctx.effect(move || {
        let s = s1.clone();
        Effect::Disposer(Box::new(move || {
            s.lock().unwrap().push(1);
            Ok(())
        }))
    })
    .unwrap();
    let hits = counter();
    let h_in = hits.clone();
    ctx.events()
        .on(
            &ctx,
            Rec {
                hits: h_in,
                notify: None,
            },
        )
        .unwrap();
    let s2 = s.clone();
    ctx.effect(move || {
        let s = s2.clone();
        Effect::Disposer(Box::new(move || {
            s.lock().unwrap().push(2);
            Ok(())
        }))
    })
    .unwrap();
    let s3 = s.clone();
    ctx.effect(move || {
        let s = s3.clone();
        Effect::Disposer(Box::new(move || {
            s.lock().unwrap().push(3);
            Ok(())
        }))
    })
    .unwrap();

    assert_eq!(*s.lock().unwrap(), Vec::<u32>::new());
    let root = ctx.root_view();
    root.dispose().await.unwrap();
    assert_eq!(*s.lock().unwrap(), vec![3, 2, 1]); // LIFO,监听器清理不可见
    root.clone().dispose().await.unwrap(); // 重入 join:不重跑
    assert_eq!(*s.lock().unwrap(), vec![3, 2, 1]);
}

// tests/dispose.spec.ts:81
// 异步 setup 完成后注册清理,dispose 按序执行。
#[tokio::test]
async fn dispose_async_return_1() {
    let ctx = Ctx::root().unwrap();
    let s = seq();
    let hold = gate();
    let s_in = s.clone();
    let h_in = hold.clone();
    let view = ctx.plugin(simple("async-return-1", move |_ctx: &Ctx| {
        let s = s_in.clone();
        let hold = h_in.clone();
        Box::pin(async move {
            hold.notified().await; // sleep(100) 的确定性替身
            s.lock().unwrap().push(1);
            let s2 = s.clone();
            Ok(Effect::Disposer(Box::new(move || {
                s2.lock().unwrap().push(2);
                Ok(())
            })))
        })
    }));
    wait_state(&view, FiberState::Loading).await;
    assert_eq!(*s.lock().unwrap(), Vec::<u32>::new()); // setup 未完成
    hold.notify_one();
    (&view).await.expect("setup lands");
    assert_eq!(*s.lock().unwrap(), vec![1]);
    view.dispose().await.unwrap();
    assert_eq!(*s.lock().unwrap(), vec![1, 2]);
}

// tests/dispose.spec.ts:95
// setup 未完成时调 dispose:setup 惯性落地后仍清理(不丢已注册的清理)。
#[tokio::test]
async fn dispose_async_return_2() {
    let ctx = Ctx::root().unwrap();
    let s = seq();
    let hold = gate();
    let s_in = s.clone();
    let h_in = hold.clone();
    let view = ctx.plugin(simple("async-return-2", move |_ctx: &Ctx| {
        let s = s_in.clone();
        let hold = h_in.clone();
        Box::pin(async move {
            hold.notified().await;
            s.lock().unwrap().push(1);
            let s2 = s.clone();
            Ok(Effect::Disposer(Box::new(move || {
                s2.lock().unwrap().push(2);
                Ok(())
            })))
        })
    }));
    wait_state(&view, FiberState::Loading).await;
    let v = view.clone();
    let disposal = tokio::spawn(async move { v.dispose().await }); // setup 中 dispose
    assert_eq!(*s.lock().unwrap(), Vec::<u32>::new());
    hold.notify_one(); // setup 惯性完成
    disposal.await.unwrap().expect("disposal completes");
    assert_eq!(*s.lock().unwrap(), vec![1, 2]);
    assert_eq!(view.state().state, FiberState::Disposed);
}

// tests/dispose.spec.ts:108
// 全量分段落地后 dispose:清理 LIFO 逆序 [6,4,2]。
#[tokio::test]
async fn dispose_async_yield_1() {
    let ctx = Ctx::root().unwrap();
    let s = seq();
    let gates = vec![gate(), gate(), gate()];
    let signals = vec![gate(), gate(), gate()];
    let view = ctx.plugin(Staged {
        name: "async-yield-1",
        gates: gates.clone(),
        signals: signals.clone(),
        seq: s.clone(),
    });
    wait_state(&view, FiberState::Loading).await;
    for (g, sig) in gates.iter().zip(signals.iter()) {
        g.notify_one();
        soon(sig.notified()).await; // 每段确定性落地
    }
    (&view).await.expect("all stages landed");
    assert_eq!(*s.lock().unwrap(), vec![1, 3, 5]);
    view.dispose().await.unwrap();
    assert_eq!(*s.lock().unwrap(), vec![1, 3, 5, 6, 4, 2]);
}

// tests/dispose.spec.ts:128
// abort 落在首段等待:首段惯性完成即止,只落地已注册的清理 [1,2]。
#[tokio::test]
async fn dispose_async_yield_2_aborted() {
    let ctx = Ctx::root().unwrap();
    let s = seq();
    let gates = vec![gate(), gate(), gate()];
    let signals = vec![gate(), gate(), gate()];
    let view = ctx.plugin(Staged {
        name: "async-yield-2",
        gates: gates.clone(),
        signals: signals.clone(),
        seq: s.clone(),
    });
    wait_state(&view, FiberState::Loading).await;
    let v = view.clone();
    let disposal = tokio::spawn(async move { v.dispose().await }); // abort 信号
    yields().await; // dispose 已预取消当前代 token
    gates[0].notify_one(); // 首段惯性完成
    disposal.await.unwrap().expect("disposal completes");
    assert_eq!(*s.lock().unwrap(), vec![1, 2]); // 后续段未跑
    assert_eq!(view.state().state, FiberState::Disposed);
}

// tests/dispose.spec.ts:148
// abort 落在第二段等待:第二段惯性完成,第三段不跑 → [1,3,4,2]。
#[tokio::test]
async fn dispose_async_yield_3_aborted() {
    let ctx = Ctx::root().unwrap();
    let s = seq();
    let gates = vec![gate(), gate(), gate()];
    let signals = vec![gate(), gate(), gate()];
    let view = ctx.plugin(Staged {
        name: "async-yield-3",
        gates: gates.clone(),
        signals: signals.clone(),
        seq: s.clone(),
    });
    wait_state(&view, FiberState::Loading).await;
    gates[0].notify_one();
    soon(signals[0].notified()).await;
    assert_eq!(*s.lock().unwrap(), vec![1]);
    let v = view.clone();
    let disposal = tokio::spawn(async move { v.dispose().await }); // abort 落在第二段等待
    yields().await;
    gates[1].notify_one(); // 第二段惯性完成
    disposal.await.unwrap().expect("disposal completes");
    assert_eq!(*s.lock().unwrap(), vec![1, 3, 4, 2]);
    assert_eq!(view.state().state, FiberState::Disposed);
}

// tests/dispose.spec.ts:170
// await dispose:全量落地 + LIFO + 重入 join 同一结果(不重跑)。
#[tokio::test]
async fn dispose_async_yield_4_await_dispose() {
    let ctx = Ctx::root().unwrap();
    let s = seq();
    let gates = vec![gate(), gate(), gate()];
    let signals = vec![gate(), gate(), gate()];
    let view = ctx.plugin(Staged {
        name: "async-yield-4",
        gates: gates.clone(),
        signals: signals.clone(),
        seq: s.clone(),
    });
    wait_state(&view, FiberState::Loading).await;
    for (g, sig) in gates.iter().zip(signals.iter()) {
        g.notify_one();
        soon(sig.notified()).await;
    }
    (&view).await.expect("all stages landed");
    assert_eq!(*s.lock().unwrap(), vec![1, 3, 5]);
    view.dispose().await.unwrap();
    assert_eq!(*s.lock().unwrap(), vec![1, 3, 5, 6, 4, 2]);
    view.clone().dispose().await.unwrap(); // 重入 join
    assert_eq!(*s.lock().unwrap(), vec![1, 3, 5, 6, 4, 2]);
}

// tests/dispose.spec.ts:190
// 同步 setup 抛错即抛:不注册任何清理。
// 载体:effect factory 的同步 throw → apply 立即 Err。
#[tokio::test]
async fn dispose_return_with_error() {
    let ctx = Ctx::root().unwrap();
    let cleanups = counter();
    let view = ctx.plugin(simple("sync-throw", move |_ctx: &Ctx| {
        Box::pin(async { Err(err("test")) }) // 抛错先于任何注册
    }));
    let e = (&view).await.expect_err("throws immediately");
    assert!(matches!(*e, CordisError::ServiceNotFound(ref m) if m == "test"));
    assert_eq!(cleanups.load(Ordering::SeqCst), 0);
    view.dispose().await.unwrap(); // 无清理 → 正常完成
    assert_eq!(cleanups.load(Ordering::SeqCst), 0);
}

// tests/dispose.spec.ts:202
// 抛错前已 yield 的清理保留:失败时回滚执行恰好一次。
#[tokio::test]
async fn dispose_yield_with_error() {
    let ctx = Ctx::root().unwrap();
    let cleanups = counter();
    let attempts = counter();
    let cu_in = cleanups.clone();
    let att_in = attempts.clone();
    let view = ctx.plugin(simple("yield-then-throw", move |ctx: &Ctx| {
        let cleanups = cu_in.clone();
        let attempts = att_in.clone();
        Box::pin(async move {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                let c = cleanups.clone();
                ctx.effect(move || counting_effect(c))?; // 已注册的清理
                Err(err("test"))
            } else {
                Ok(Effect::Done)
            }
        })
    }));
    (&view).await.expect_err("execution fails");
    assert_eq!(cleanups.load(Ordering::SeqCst), 1); // 回滚恰好一次
}

// tests/dispose.spec.ts:215
// 异步 setup 抛错:错误经 await 通道拒绝,无清理残留。
#[tokio::test]
async fn dispose_async_return_with_error() {
    let ctx = Ctx::root().unwrap();
    let cleanups = counter();
    let hold = gate();
    let h_in = hold.clone();
    let view = ctx.plugin(simple("async-throw", move |_ctx: &Ctx| {
        let hold = h_in.clone();
        Box::pin(async move {
            hold.notified().await;
            Err(err("test"))
        })
    }));
    wait_state(&view, FiberState::Loading).await;
    hold.notify_one();
    let e = (&view).await.expect_err("rejects via await channel");
    assert!(matches!(*e, CordisError::ServiceNotFound(ref m) if m == "test"));
    assert_eq!(cleanups.load(Ordering::SeqCst), 0);
    assert_eq!(view.state().state, FiberState::Failed);
}

// tests/dispose.spec.ts:230
// 异步分段:首段清理注册后失败,回滚执行该清理恰好一次。
#[tokio::test]
async fn dispose_async_yield_with_error() {
    let ctx = Ctx::root().unwrap();
    let cleanups = counter();
    let hold = gate();
    let cu_in = cleanups.clone();
    let h_in = hold.clone();
    let view = ctx.plugin(simple("async-yield-throw", move |ctx: &Ctx| {
        let cleanups = cu_in.clone();
        let hold = h_in.clone();
        Box::pin(async move {
            hold.notified().await;
            let c = cleanups.clone();
            ctx.effect(move || counting_effect(c))?; // 首段已注册
            Err(err("test"))
        })
    }));
    wait_state(&view, FiberState::Loading).await;
    hold.notify_one();
    let e = (&view).await.expect_err("rejects");
    assert!(matches!(*e, CordisError::ServiceNotFound(ref m) if m == "test"));
    assert_eq!(cleanups.load(Ordering::SeqCst), 1); // 已注册的清理回滚一次
}

// ── plugin.spec.ts ───────────────────────────────────────────────

// tests/plugin.spec.ts:8
// 函数插件被调用一次且收到 options(config 烘进实例,D19)。
#[tokio::test]
async fn plugin_apply_functional_plugin() {
    let ctx = Ctx::root().unwrap();
    let seen: StrSeq = str_seq();
    let seen_in = seen.clone();
    let view = ctx.plugin(simple("fn-plugin", move |_ctx: &Ctx| {
        let seen = seen_in.clone();
        Box::pin(async move {
            seen.lock().unwrap().push("bar"); // 烘入的 options 值
            Ok(Effect::Done)
        })
    }));
    (&view).await.expect("applied once");
    assert_eq!(*seen.lock().unwrap(), vec!["bar"]);
}

// tests/plugin.spec.ts:36
// inactive context:失活后 plugin/effect/on 三面全部拒绝,回调不执行。
// 载体:TS 抛 'inactive context';Rust effect/on 返 InactiveEffect,
// plugin 无 Result 返回面 → 子 fiber 被即刻处置且永不 apply。
#[tokio::test]
async fn plugin_inactive_context() {
    let ctx = Ctx::root().unwrap();
    let hits = counter();
    let errs: Arc<Mutex<Vec<CordisError>>> = Arc::new(Mutex::new(Vec::new()));
    let late_slot = view_slot();
    let errs_in = errs.clone();
    let hits_in = hits.clone();
    let slot_in = late_slot.clone();
    let view = ctx.plugin(simple("inactive", move |ctx: &Ctx| {
        let errs = errs_in.clone();
        let hits = hits_in.clone();
        let slot = slot_in.clone();
        let ctx2 = ctx.clone();
        Box::pin(async {
            Ok(Effect::Disposer(Box::new(move || {
                let errs = errs.clone();
                let hits = hits.clone();
                let slot = slot.clone();
                let ctx3 = ctx2.clone();
                if let Some(e) = ctx3.effect(move || Effect::Done).err() {
                    errs.lock().unwrap().push(e);
                }
                if let Some(e) = ctx3.events().on(&ctx3, Rec { hits, notify: None }).err() {
                    errs.lock().unwrap().push(e);
                }
                let child = ctx3.plugin(simple("late", move |_c: &Ctx| {
                    Box::pin(async { Ok(Effect::Done) })
                }));
                *slot.lock().unwrap() = Some(child);
                Ok(())
            })))
        })
    }));
    (&view).await.expect("load");
    view.dispose().await.unwrap();

    {
        let captured = errs.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(
            captured
                .iter()
                .all(|e| matches!(e, CordisError::InactiveEffect)),
            "all registrations rejected: {captured:?}"
        );
    }
    // 失活 ctx 上注册的子插件被即刻处置,永不 apply
    let child = late_slot.lock().unwrap().clone().expect("child spawned");
    soon(async {
        let mut rx = child.watch();
        loop {
            if rx.borrow().state == FiberState::Disposed {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await;
    assert_eq!(hits.load(Ordering::SeqCst), 0); // 回调从未执行
}

/// 嵌套插件(plugin.spec 嵌套/快照用):每层注册一个计数监听器。
struct Nested {
    depth: u32,
    hits: Arc<AtomicUsize>,
}

impl Plugin for Nested {
    fn name(&self) -> &str {
        "nested"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        let depth = self.depth;
        let hits = self.hits.clone();
        Box::pin(async move {
            let h = hits.clone();
            ctx.events().on(
                ctx,
                Rec {
                    hits: h,
                    notify: None,
                },
            )?;
            if depth > 0 {
                ctx.plugin(Nested {
                    depth: depth - 1,
                    hits: hits.clone(),
                });
            }
            Ok(Effect::Done)
        })
    }
}

// tests/plugin.spec.ts:86
// 嵌套插件注册;dispose 级联清理全部子插件与监听;二次 dispose 幂等。
#[tokio::test]
async fn plugin_nested_plugins() {
    let ctx = Ctx::root().unwrap();
    let hits = counter();
    let root_hits = hits.clone();
    ctx.events()
        .on(
            &ctx,
            Rec {
                hits: root_hits,
                notify: None,
            },
        )
        .unwrap();
    let view = ctx.plugin(Nested {
        depth: 2,
        hits: hits.clone(),
    });
    (&view).await.expect("nested applied");

    ctx.events().serial(&ctx, &Ping { value: 1 }).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 4); // root + 三层嵌套

    view.dispose().await.unwrap();
    ctx.events().serial(&ctx, &Ping { value: 1 }).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 5); // 只剩 root 的 1 个

    view.dispose().await.unwrap(); // 二次 dispose 幂等
    ctx.events().serial(&ctx, &Ping { value: 1 }).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 6); // 仍然 1 个
}

// tests/plugin.spec.ts:145
// root dispose 级联清子 fiber;dispose 恰好一次且幂等
// (uid/_disposables 为 JS 内省,不做)。
#[tokio::test]
async fn plugin_root_dispose() {
    let ctx = Ctx::root().unwrap();
    let calls = counter();
    let c_in = calls.clone();
    let view = ctx.plugin(simple("child", move |_ctx: &Ctx| {
        let calls = c_in.clone();
        Box::pin(async { Ok(counting_effect(calls)) })
    }));
    (&view).await.expect("load");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let root = ctx.root_view();
    root.dispose().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1); // 级联清理恰好一次
    assert_eq!(view.state().state, FiberState::Disposed);
    root.clone().dispose().await.unwrap(); // 幂等
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// tests/plugin.spec.ts:165
// Service.init:装载时启动钩子调用,dispose 时返回的清理恰好一次。
// 载体:TS Service.init 钩子 → Rust apply + 返回清理(同一装配范式)。
#[tokio::test]
async fn plugin_service_init() {
    let ctx = Ctx::root().unwrap();
    let start = counter();
    let stop = counter();
    let s_in = start.clone();
    let t_in = stop.clone();
    let view = ctx.plugin(simple("init", move |_ctx: &Ctx| {
        let start = s_in.clone();
        let stop = t_in.clone();
        Box::pin(async move {
            start.fetch_add(1, Ordering::SeqCst);
            Ok(counting_effect(stop))
        })
    }));
    (&view).await.expect("load");
    assert_eq!(start.load(Ordering::SeqCst), 1);
    assert_eq!(stop.load(Ordering::SeqCst), 0);
    view.dispose().await.unwrap();
    assert_eq!(start.load(Ordering::SeqCst), 1);
    assert_eq!(stop.load(Ordering::SeqCst), 1);
}

// tests/plugin.spec.ts:123(§三 内核)
// 快照 = 监听器命中:卸载后恢复原状(恰好一次还原),重装一致。
#[tokio::test]
async fn plugin_compare_snapshot() {
    let ctx = Ctx::root().unwrap();
    let hits = counter();
    let root_hits = hits.clone();
    ctx.events()
        .on(
            &ctx,
            Rec {
                hits: root_hits,
                notify: None,
            },
        )
        .unwrap();
    ctx.events().serial(&ctx, &Ping { value: 1 }).await.unwrap();
    let baseline = hits.load(Ordering::SeqCst);

    let view = ctx.plugin(Nested {
        depth: 2,
        hits: hits.clone(),
    });
    (&view).await.expect("applied");
    ctx.events().serial(&ctx, &Ping { value: 1 }).await.unwrap();
    let with_plugin = hits.load(Ordering::SeqCst);
    assert_eq!(with_plugin - baseline, 4);

    view.dispose().await.unwrap();
    ctx.events().serial(&ctx, &Ping { value: 1 }).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst) - with_plugin, 1); // 恢复基线

    let view2 = ctx.plugin(Nested {
        depth: 2,
        hits: hits.clone(),
    });
    (&view2).await.expect("re-applied");
    ctx.events().serial(&ctx, &Ping { value: 1 }).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), with_plugin + 1 + 4); // 重装一致
}

// ── service.spec.ts ──────────────────────────────────────────────

// tests/service.spec.ts:7
// 依赖未就绪/provider init 未完成前 inject 回调阻塞,就绪后放行。
// 载体:Service.init 的异步初始化 → provider apply 内的门。
#[tokio::test]
async fn service_pending_inject() {
    let ctx = Ctx::root().unwrap();
    let init_hold = gate();
    let calls = counter();
    let ih_in = init_hold.clone();
    let provider = ctx.plugin(simple("init-gate", move |ctx: &Ctx| {
        let hold = ih_in.clone();
        Box::pin(async move {
            hold.notified().await; // Service.init 未完成
            ctx.provide(Foo { bar: 1 })?;
            Ok(Effect::Done)
        })
    }));
    let c_in = calls.clone();
    let consumer = ctx.plugin(simple_dep(
        "blocked",
        vec![TypeKey::of::<Foo>()],
        move |_ctx: &Ctx| {
            let calls = c_in.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Effect::Done)
            })
        },
    ));
    wait_state(&consumer, FiberState::Pending).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0); // inject 回调阻塞

    init_hold.notify_one(); // init 完成
    (&provider).await.expect("provider active");
    (&consumer).await.expect("consumer released");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// tests/service.spec.ts:128
// foo→qux、bar→foo+qux 拓扑门控:各 init 恰好一次。
#[tokio::test]
async fn service_multiple_injects() {
    let ctx = Ctx::root().unwrap();
    let foo_calls = counter();
    let bar_calls = counter();
    let qux_calls = counter();
    let f_in = foo_calls.clone();
    let foo = ctx.plugin(simple_dep(
        "foo",
        vec![TypeKey::of::<Qux>()],
        move |ctx: &Ctx| {
            let f = f_in.clone();
            Box::pin(async move {
                f.fetch_add(1, Ordering::SeqCst);
                ctx.provide(Foo { bar: 1 })?;
                Ok(Effect::Done)
            })
        },
    ));
    let b_in = bar_calls.clone();
    let bar = ctx.plugin(simple_dep(
        "bar",
        vec![TypeKey::of::<Foo>(), TypeKey::of::<Qux>()],
        move |_ctx: &Ctx| {
            let b = b_in.clone();
            Box::pin(async move {
                b.fetch_add(1, Ordering::SeqCst);
                Ok(Effect::Done)
            })
        },
    ));
    let q_in = qux_calls.clone();
    let qux = ctx.plugin(simple("qux", move |ctx: &Ctx| {
        let q = q_in.clone();
        Box::pin(async move {
            q.fetch_add(1, Ordering::SeqCst);
            ctx.provide(Qux)?;
            Ok(Effect::Done)
        })
    }));
    (&foo).await.expect("foo");
    (&bar).await.expect("bar");
    (&qux).await.expect("qux");
    assert_eq!(foo_calls.load(Ordering::SeqCst), 1);
    assert_eq!(bar_calls.load(Ordering::SeqCst), 1);
    assert_eq!(qux_calls.load(Ordering::SeqCst), 1);
}

// tests/service.spec.ts:109(§三 内核)
// 卸载/重装后快照还原:服务消失、消费者回 Pending;重装后恢复。
#[tokio::test]
async fn service_compare_snapshot() {
    let ctx = Ctx::root().unwrap();
    let calls = counter();
    let c_in = calls.clone();
    let provider = ctx.plugin(simple("svc", move |ctx: &Ctx| {
        Box::pin(async move {
            ctx.provide(Foo { bar: 1 })?;
            Ok(Effect::Done)
        })
    }));
    let consumer = ctx.plugin(simple_dep(
        "watcher",
        vec![TypeKey::of::<Foo>()],
        move |_ctx: &Ctx| {
            let calls = c_in.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Effect::Done)
            })
        },
    ));
    (&provider).await.expect("P");
    (&consumer).await.expect("C");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    provider.dispose().await.unwrap();
    assert!(ctx.get::<Foo>().is_none()); // 卸载还原
    wait_state(&consumer, FiberState::Pending).await;

    let p2 = ctx.plugin(simple("svc2", move |ctx: &Ctx| {
        Box::pin(async move {
            ctx.provide(Foo { bar: 2 })?;
            Ok(Effect::Done)
        })
    }));
    (&p2).await.expect("P2");
    soon(async {
        let mut rx = consumer.watch();
        loop {
            if rx.borrow().state == FiberState::Active {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await;
    assert!(ctx.get::<Foo>().is_some()); // 重装恢复
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

// ── isolate.spec.ts ──────────────────────────────────────────────

// tests/isolate.spec.ts:7
// isolate 切断父级可见性:各作用域独立 provide/inject;根 provider 摘除
// 只驱逐默认作用域消费者(其清理执行),不影响隔离作用域。
#[tokio::test]
async fn isolate_isolated_context() {
    let key = TypeKey::of::<Foo>();
    let ctx = Ctx::root().unwrap();
    let scope_a = ctx.isolate(key, "A");
    let scope_b = ctx.isolate(key, "B");
    let callbacks = counter();
    let disposes = counter();

    let mk_consumer = |_ctx: &Ctx| {
        let cb = callbacks.clone();
        let dp = disposes.clone();
        simple_dep("iso-consumer", vec![key], move |_c: &Ctx| {
            let cb = cb.clone();
            let dp = dp.clone();
            Box::pin(async move {
                cb.fetch_add(1, Ordering::SeqCst);
                Ok(counting_effect(dp))
            })
        })
    };
    let rc = ctx.plugin(mk_consumer(&ctx));
    let ca = scope_a.plugin(mk_consumer(&scope_a));
    let cb_ = scope_b.plugin(mk_consumer(&scope_b));

    let d0 = ctx.provide(Foo { bar: 100 }).unwrap();
    assert_eq!(ctx.get::<Foo>().unwrap().bar, 100);
    assert!(scope_a.get::<Foo>().is_none()); // 隔离作用域看不到根 provide
    assert!(scope_b.get::<Foo>().is_none());
    (&rc).await.expect("root consumer loads");
    assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    assert_eq!(disposes.load(Ordering::SeqCst), 0);

    let d1 = scope_a.provide(Foo { bar: 200 }).unwrap();
    assert_eq!(ctx.get::<Foo>().unwrap().bar, 100);
    assert_eq!(scope_a.get::<Foo>().unwrap().bar, 200);
    assert!(scope_b.get::<Foo>().is_none());
    (&ca).await.expect("scope A consumer loads");
    assert_eq!(callbacks.load(Ordering::SeqCst), 2);
    assert_eq!(disposes.load(Ordering::SeqCst), 0);

    d0.dispose().await.unwrap(); // 根 provider 摘除
    assert!(ctx.get::<Foo>().is_none());
    assert_eq!(scope_a.get::<Foo>().unwrap().bar, 200);
    assert_eq!(callbacks.load(Ordering::SeqCst), 2);
    assert_eq!(disposes.load(Ordering::SeqCst), 1); // 只有根消费者被驱逐清理

    let _d2 = scope_b.provide(Foo { bar: 300 }).unwrap();
    (&cb_).await.expect("scope B consumer loads");
    assert_eq!(callbacks.load(Ordering::SeqCst), 3);
    assert_eq!(disposes.load(Ordering::SeqCst), 1);
    drop(d1);
}

// tests/isolate.spec.ts:58
// 相同 label 共享同一份服务:一次 provide 两个作用域消费者都装载;
// 摘除时两个消费者都被驱逐清理;不同作用域(根)不受影响。
#[tokio::test]
async fn isolate_shared_label() {
    let key = TypeKey::of::<Foo>();
    let ctx = Ctx::root().unwrap();
    let scope_l1 = ctx.isolate(key, "L");
    let scope_l2 = ctx.isolate(key, "L"); // 同 label → 同作用域
    let callbacks = counter();
    let disposes = counter();

    let mk_consumer = |_ctx: &Ctx| {
        let cb = callbacks.clone();
        let dp = disposes.clone();
        simple_dep("shared-consumer", vec![key], move |_c: &Ctx| {
            let cb = cb.clone();
            let dp = dp.clone();
            Box::pin(async move {
                cb.fetch_add(1, Ordering::SeqCst);
                Ok(counting_effect(dp))
            })
        })
    };
    let rc = ctx.plugin(mk_consumer(&ctx));
    let c1 = scope_l1.plugin(mk_consumer(&scope_l1));
    let c2 = scope_l2.plugin(mk_consumer(&scope_l2));

    let d0 = ctx.provide(Foo { bar: 100 }).unwrap();
    (&rc).await.expect("root consumer loads");
    assert_eq!(callbacks.load(Ordering::SeqCst), 1);

    let d12 = scope_l1.provide(Foo { bar: 200 }).unwrap();
    assert_eq!(ctx.get::<Foo>().unwrap().bar, 100);
    assert_eq!(scope_l1.get::<Foo>().unwrap().bar, 200);
    assert_eq!(scope_l2.get::<Foo>().unwrap().bar, 200); // 同 label 共享
    (&c1).await.expect("L1 consumer loads");
    (&c2).await.expect("L2 consumer loads");
    assert_eq!(callbacks.load(Ordering::SeqCst), 3);
    assert_eq!(disposes.load(Ordering::SeqCst), 0);

    d12.dispose().await.unwrap(); // 共享 provider 摘除
    assert_eq!(ctx.get::<Foo>().unwrap().bar, 100); // 根不受影响
    assert!(scope_l1.get::<Foo>().is_none());
    assert!(scope_l2.get::<Foo>().is_none());
    assert_eq!(callbacks.load(Ordering::SeqCst), 3);
    assert_eq!(disposes.load(Ordering::SeqCst), 2); // 两个共享消费者都清理
    drop(d0);
    drop(rc);
}

// ── events.spec.ts(§三 内核:字符串事件名 → 类型化)──────────────

// tests/events.spec.ts:18
// on 注册 → emit 触发;Disposer 提前释放后不再触发。
#[tokio::test]
async fn events_ctx_on() {
    let ctx = Ctx::root().unwrap();
    let (hits, done) = (counter(), gate());
    let d = ctx
        .events()
        .on(
            &ctx,
            Rec {
                hits: hits.clone(),
                notify: Some(done.clone()),
            },
        )
        .unwrap();
    ctx.events().emit(&ctx, Arc::new(Ping { value: 1 }));
    soon(done.notified()).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    ctx.events().emit(&ctx, Arc::new(Ping { value: 1 }));
    soon(async {
        while hits.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    d.dispose().await.unwrap();
    ctx.events().emit(&ctx, Arc::new(Ping { value: 1 }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 2); // 卸载后不再分发
}

// tests/events.spec.ts:31
// once 只触发一次;提前释放后也不再触发。
#[tokio::test]
async fn events_ctx_once() {
    let ctx = Ctx::root().unwrap();
    let (hits, done) = (counter(), gate());
    let d = ctx
        .events()
        .once(
            &ctx,
            Rec {
                hits: hits.clone(),
                notify: Some(done.clone()),
            },
        )
        .unwrap();
    ctx.events().emit(&ctx, Arc::new(Ping { value: 1 }));
    soon(done.notified()).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    ctx.events().emit(&ctx, Arc::new(Ping { value: 1 }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1); // 至多一次
    d.dispose().await.unwrap();
    ctx.events().emit(&ctx, Arc::new(Ping { value: 1 }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

// tests/events.spec.ts:131
// waterfall:next 链传递值;不调 next(veto)则截断,后续监听器不执行。
#[tokio::test]
async fn events_ctx_waterfall() {
    struct Adder {
        called: Arc<AtomicUsize>,
    }
    impl WaterfallListener<Ping> for Adder {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a Ping,
            next: Next<'a, Ping>,
        ) -> BoxFuture<'a, Result<u32, CordisError>> {
            let called = self.called.clone();
            let base = e.value;
            Box::pin(async move {
                called.fetch_add(1, Ordering::SeqCst);
                let inner = next.call().await?;
                Ok(base + inner)
            })
        }
    }
    struct Veto {
        called: Arc<AtomicUsize>,
    }
    impl WaterfallListener<Ping> for Veto {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a Ping,
            _next: Next<'a, Ping>,
        ) -> BoxFuture<'a, Result<u32, CordisError>> {
            let called = self.called.clone();
            Box::pin(async move {
                called.fetch_add(1, Ordering::SeqCst);
                Ok(e.value) // veto:不调 next,直接返回
            })
        }
    }
    struct ConstTerminal(u32);
    impl Terminal<Ping> for ConstTerminal {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            _e: &'a Ping,
        ) -> BoxFuture<'a, Result<u32, CordisError>> {
            Box::pin(async { Ok(self.0) })
        }
    }

    let ctx = Ctx::root().unwrap();
    let a1_called = counter();
    let a2_called = counter();
    let veto_called = counter();
    let a4_called = counter();
    ctx.events()
        .on_waterfall(
            &ctx,
            Adder {
                called: a1_called.clone(),
            },
        )
        .unwrap();
    ctx.events()
        .on_waterfall(
            &ctx,
            Adder {
                called: a2_called.clone(),
            },
        )
        .unwrap();
    let out = ctx
        .events()
        .waterfall(&ctx, &Ping { value: 1 }, ConstTerminal(2))
        .await
        .unwrap();
    assert_eq!(out, 4); // 1 + (1 + terminal 2)
    assert_eq!(a1_called.load(Ordering::SeqCst), 1);
    assert_eq!(a2_called.load(Ordering::SeqCst), 1);

    ctx.events()
        .on_waterfall(
            &ctx,
            Veto {
                called: veto_called.clone(),
            },
        )
        .unwrap();
    ctx.events()
        .on_waterfall(
            &ctx,
            Adder {
                called: a4_called.clone(),
            },
        )
        .unwrap();
    let out2 = ctx
        .events()
        .waterfall(&ctx, &Ping { value: 1 }, ConstTerminal(2))
        .await
        .unwrap();
    assert_eq!(out2, 3); // veto 截断:1 + (1 + veto 1)
    assert_eq!(veto_called.load(Ordering::SeqCst), 1);
    assert_eq!(a4_called.load(Ordering::SeqCst), 0); // veto 后不再执行
}

// ── reflect.spec.ts(§三 内核)────────────────────────────────────

// tests/reflect.spec.ts:57
// fiber dispose 后访问其注入的服务 → 不可见(TS Proxy get 抛
// 'inactive context';Rust Option 失败语义 → None)。
#[tokio::test]
async fn reflect_service_inject_leak() {
    let ctx = Ctx::root().unwrap();
    let provider = ctx.plugin(simple("provider", move |ctx: &Ctx| {
        Box::pin(async move {
            ctx.provide(Foo { bar: 1 })?;
            Ok(Effect::Done)
        })
    }));
    let ctx_slot: Arc<Mutex<Option<Ctx>>> = Arc::new(Mutex::new(None));
    let slot_in = ctx_slot.clone();
    let consumer = ctx.plugin(simple_dep(
        "leak-probe",
        vec![TypeKey::of::<Foo>()],
        move |ctx: &Ctx| {
            let slot = slot_in.clone();
            Box::pin(async move {
                *slot.lock().unwrap() = Some(ctx.clone());
                Ok(Effect::Done)
            })
        },
    ));
    (&provider).await.expect("P");
    (&consumer).await.expect("C");
    let consumer_ctx = ctx_slot.lock().unwrap().clone().expect("ctx captured");
    assert!(consumer_ctx.get::<Foo>().is_some()); // 失活前可见

    consumer.dispose().await.unwrap();
    assert!(
        consumer_ctx.get::<Foo>().is_none(),
        "inactive fiber 的服务访问必须不可见"
    );
    assert!(ctx.get::<Foo>().is_some()); // provider 本身不受影响
}

// ── decorator.spec.ts(§三 内核)──────────────────────────────────

// tests/decorator.spec.ts:6
// @Inject 语义:依赖注册后方法才被调;依赖卸载后方法交回的清理执行。
#[tokio::test]
async fn decorator_inject_on_class_method() {
    let ctx = Ctx::root().unwrap();
    let method_calls = counter();
    let dispose_calls = counter();
    let m_in = method_calls.clone();
    let d_in = dispose_calls.clone();
    let bar = ctx.plugin(simple_dep(
        "bar-method",
        vec![TypeKey::of::<Foo>()],
        move |_ctx: &Ctx| {
            let calls = m_in.clone();
            let disposes = d_in.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst); // 依赖就绪后方法才被调
                Ok(counting_effect(disposes))
            })
        },
    ));
    assert_eq!(bar.state().state, FiberState::Pending); // Foo 未注册
    assert_eq!(method_calls.load(Ordering::SeqCst), 0);
    assert_eq!(dispose_calls.load(Ordering::SeqCst), 0);

    let provider = ctx.plugin(simple("foo-provider", move |ctx: &Ctx| {
        Box::pin(async move {
            ctx.provide(Foo { bar: 1 })?;
            Ok(Effect::Done)
        })
    }));
    (&provider).await.expect("P");
    (&bar).await.expect("method called once");
    assert_eq!(method_calls.load(Ordering::SeqCst), 1);
    assert_eq!(dispose_calls.load(Ordering::SeqCst), 0);

    provider.dispose().await.unwrap(); // 依赖卸载 → 方法 fiber 被驱逐清理
    wait_state(&bar, FiberState::Pending).await;
    assert_eq!(method_calls.load(Ordering::SeqCst), 1);
    assert_eq!(dispose_calls.load(Ordering::SeqCst), 1);
}
