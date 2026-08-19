//! emit 派发序回归测试(D31 尾链):同一事件类型的多次 emit,
//! 监听器按发射序收到(单发射者);emit 间隔 1ms 的慢流变体同样有序。
//!
//! 历史:尾链修复前,多线程 runtime 下背靠背 emit 约 30% 事件错位
//! (最大位移 52);单线程与 ≥1ms 间隔零乱序——即真实远程 LLM 流无感,
//! 本地快后端(ScriptedLlm / ollama)高发。测试默认 current_thread
//! 单线程,故顺序性必须在 multi_thread flavor 下钉死。

use std::sync::{Arc, Mutex};

use rutis::{Ctx, Event, Listener};

#[derive(Debug)]
struct ProbeEvent {
    seq: usize,
}

impl Event for ProbeEvent {
    const NAME: &'static str = "probe/seq";
    type Value = ();
}

/// TUI 式监听器:逐条记录到达序。
struct Recorder(Arc<Mutex<Vec<usize>>>);

impl Listener<ProbeEvent> for Recorder {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a ProbeEvent,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        let log = self.0.clone();
        let seq = e.seq;
        Box::pin(async move {
            log.lock().unwrap().push(seq);
            Ok(None)
        })
    }
}

const N: usize = 200;
const ROUNDS: usize = 20;

async fn emit_rounds(root: &Ctx, log: &Arc<Mutex<Vec<usize>>>, gap_ms: u64) -> usize {
    let mut misplaced = 0usize;
    for _ in 0..ROUNDS {
        log.lock().unwrap().clear();
        for seq in 0..N {
            root.events().emit(root, Arc::new(ProbeEvent { seq }));
            if gap_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(gap_ms)).await;
            }
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while log.lock().unwrap().len() < N {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("all events delivered");
        let recorded = log.lock().unwrap().clone();
        misplaced += recorded
            .iter()
            .enumerate()
            .filter(|(pos, &seq)| seq != *pos)
            .count();
    }
    misplaced
}

/// 背靠背 emit(模拟 driver 流消费循环,stream.next() 即时就绪):
/// 多线程 runtime 下尾链必须保证零错位。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn emit_delivers_in_order_back_to_back_multi_thread() {
    let root = Ctx::root().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    root.events().on(&root, Recorder(log.clone())).unwrap();
    let misplaced = emit_rounds(&root, &log, 0).await;
    assert_eq!(misplaced, 0, "同事件类型 emit 必须按发射序投递");
}

/// emit 间隔 1ms(模拟真实网络流):同样有序。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn emit_delivers_in_order_with_network_gap() {
    let root = Ctx::root().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    root.events().on(&root, Recorder(log.clone())).unwrap();
    let misplaced = emit_rounds(&root, &log, 1).await;
    assert_eq!(misplaced, 0, "同事件类型 emit 必须按发射序投递");
}

/// 单线程 runtime(测试默认形态):基线,同样必须有序。
#[tokio::test]
async fn emit_delivers_in_order_single_thread() {
    let root = Ctx::root().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    root.events().on(&root, Recorder(log.clone())).unwrap();
    let misplaced = emit_rounds(&root, &log, 0).await;
    assert_eq!(misplaced, 0, "同事件类型 emit 必须按发射序投递");
}
