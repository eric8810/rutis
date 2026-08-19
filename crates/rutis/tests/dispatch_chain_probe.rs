//! 派发尾链并发回归测试(D31):两个任务并发 emit 同一事件类型,
//! 链不得分叉——监听器调用永不重叠(max_in_flight 恒 1)。
//!
//! 背景:emit 的"取 prev → spawn → 存尾"必须单次持锁原子完成;
//! 若 remove/insert 分两段锁,并发同类型 emit 会各自拿到 prev,
//! 派发任务并发执行,顺序保证失效(修复前实测 max_in_flight=4)。

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use rutis::{Ctx, Event, Listener};

#[derive(Debug)]
struct ProbeEvent {}

impl Event for ProbeEvent {
    const NAME: &'static str = "probe/fork";
    type Value = ();
}

/// 监听器持 Arc 原子句柄(注册移走监听器,断言侧仍可读)。
struct ChainProbe {
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
}

impl Listener<ProbeEvent> for ChainProbe {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a ProbeEvent,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        let in_flight = self.in_flight.clone();
        let max_in_flight = self.max_in_flight.clone();
        let completed = self.completed.clone();
        Box::pin(async move {
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(now, Ordering::SeqCst);
            // 一点工作量,放大重叠窗口:若链分叉,并发派发在此明显重叠
            std::thread::sleep(std::time::Duration::from_micros(50));
            in_flight.fetch_sub(1, Ordering::SeqCst);
            completed.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    }
}

const EMITS_PER_EMITTER: usize = 2_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_chain_does_not_fork_under_concurrent_emit() {
    let root = Ctx::root().unwrap();
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    root.events()
        .on(
            &root,
            ChainProbe {
                in_flight: in_flight.clone(),
                max_in_flight: max_in_flight.clone(),
                completed: completed.clone(),
            },
        )
        .unwrap();

    let a = {
        let root = root.clone();
        tokio::spawn(async move {
            for _ in 0..EMITS_PER_EMITTER {
                root.events().emit(&root, Arc::new(ProbeEvent {}));
            }
        })
    };
    let b = {
        let root = root.clone();
        tokio::spawn(async move {
            for _ in 0..EMITS_PER_EMITTER {
                root.events().emit(&root, Arc::new(ProbeEvent {}));
            }
        })
    };
    let (_, _) = tokio::join!(a, b);

    // 确定性排干:全部派发完成(completed == 总 emit 数)后读终值
    let total = EMITS_PER_EMITTER * 2;
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while completed.load(Ordering::SeqCst) < total {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("chain drains");

    assert_eq!(
        max_in_flight.load(Ordering::SeqCst),
        1,
        "同事件类型的派发任务必须串行:链分叉了"
    );
}
