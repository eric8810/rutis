# Rust 实现简化评估(定稿)

> 2026-08-17。目的:在不影响功能的前提下降低维护与认知负担,收敛"机制比语义重"的地方。
> 方法:读完全部 10 个模块(lib/bus/ctx/effect/error/event/fiber/key/plugin/registry)+ contract.rs 测试模式,逐条论证"砍了会不会丢功能/破测试"。

## 一条方法论约束

这份代码的复杂度大多是**修出来的**(评审→修 bug→加机制的循环),每处几乎都对应一个真实修过的竞态。所以简化不是"删多余的",而是**判断哪些竞态是某个结构选择自找的**——换个根结构,整类补丁连同竞态一起消失。

## 核心结论:唯一值得动的是 fiber 驱动任务模型

### 现状:为"状态机跑在独立任务里"打了六个补丁

fiber 状态机当前跑在一个常驻 mpsc 驱动任务里([fiber.rs:453](../rust/crates/min-cordis/src/fiber.rs#L453) `drive`)。读完 `drive`/`post`/`drain_stale`/`settle_inner`/`evict_and_finalize` 全文后确认,围绕它有六样**纯结构性、无业务语义**的机制:

| 机制 | 位置 | 存在的原因 |
|---|---|---|
| `Intent` 四态枚举 + `mpsc::UnboundedSender` | [fiber.rs:142](../rust/crates/min-cordis/src/fiber.rs#L142) | 意图投递给驱动任务 |
| `intents_inflight` 原子计数 | [fiber.rs:145](../rust/crates/min-cordis/src/fiber.rs#L145) | settle 要区分"resolved 但还有没处理的意图" |
| `alive` 标志 | [fiber.rs:148](../rust/crates/min-cordis/src/fiber.rs#L148) | 驱动退出后拒投,防 join 永等 |
| `drain_stale` 排干协议 | [fiber.rs:529-543](../rust/crates/min-cordis/src/fiber.rs#L529) | 驱动退出前排干残留,带"让出等迟到 send"的 yield 循环 |
| `resolved` 标志 + settle 双重借阅确认 | [fiber.rs:714-719](../rust/crates/min-cordis/src/fiber.rs#L714) | 收窄"已 resolved Pending + 未处理再装载通知"的 TOCTOU |
| 预取消旁路(dispose/restart 入队前 `cancel_current`) | [fiber.rs:652](../rust/crates/min-cordis/src/fiber.rs#L652)、:678 | 驱动串行,不预取消则 apply 永远等不到 |

**这六样没有一样是业务语义,全是为了让"状态机跑在独立任务里"这个结构选择不出事而打的补丁。**

### 为什么驱动任务模型自找这些竞态

状态机迁移的本质需求只有一条:**同一时刻一个 fiber 只有一个迁移在跑**。

驱动任务用"单任务串行消费意图队列"来表达这条,但代价是引入了"调用方 → 队列 → 驱动"这层间接。所有补丁都在补这层间接捅出的洞:

- 队列会丢(驱动退出后)→ `alive` + `drain_stale` + `post` 返 false(bug 2/3 的修法);
- 意图在途与状态不同拍 → `intents_inflight` + `resolved` + 双重确认;
- apply 和 dispose 都要经同一驱动 → 预取消旁路绕过串行。

### 替代:每 fiber 一把 `tokio::Mutex`

"同一时刻一个迁移在跑"用一把 `tokio::Mutex` 直接表达,不需要任务、队列、计数、标志、排干协议。改动后:

- dispose/restart/refresh 各自 `lock().await` 获取迁移权,执行完整迁移,释放;
- **bug 2/3 从根上消失**——没有队列,就没有"排队意图无人处理";
- `intents_inflight`/`alive`/`drain_stale`/`resolved`/预取消旁路**全部删掉**;
- settle 不再需要"等 inflight==0",**持锁本身就是"无迁移在途"的直接证据**。

### 为什么 tokio::Mutex 够(两条关键推演)

**推演 1:dispose 介入 apply 的语义等价。** 担心"apply 持锁,dispose 拿不到锁"是多余的——驱动模型里 dispose 同样要排在 apply 后面等 apply 退出(都是协作取消,都不是抢占)。两者在"等 apply 退出"这点上完全等价。当前测试里的 dispose-during-loading 场景,靠 cancel token 让 apply 协作返回,锁模型下同样成立:dispose 先 `cancel_current()`(锁外),再 `lock().await` 等 apply 释放。

**推演 2:并发 notify 不依赖驱动任务。** provider 驱逐时 `notify_key_changed` 给 N 个消费者各发通知。当前是 fire-and-forget `post(RefreshDeps)`,N 个消费者的驱动任务天然并行。锁模型下 notify 方仍是 fire-and-forget——对每个消费者 spawn 一个"获取锁并 refresh"的任务即可,N 个消费者照样并行。**并发性来自 notify 方不等待,不来自驱动任务。**

### 唯一要重新锚定的:恰好一次

当前 dispose 的恰好一次靠"锁内 `terminal_task` 单 `Arc<TransitionTask>` + join watch"([fiber.rs:647-660](../rust/crates/min-cordis/src/fiber.rs#L647))。锁模型下 dispose 调用方自己持锁跑卸载,恰好一次要重新表达为:

- 锁内检查是否已有终态(`Disposed` + 缓存的 `Arc<CordisError>`);
- 首个进入者执行卸载并写终态,后续/并发调用者 join 同一个 `Arc<CordisError>`。

机制更少:`tokio::Mutex` + 一个 `watch<Option<Arc<CordisError>>>` 终态信号,替代现在的 `TransitionTask` + `TaskDone` + `join_task` 专用循环 + `complete_task`/`complete_intent` 两处完成逻辑。

## 不该动的(读完全部代码后确认承重)

上一轮我误判了两处,读完测试后纠正:

- **`status_queue` FIFO + `FiberStatusChanged` 事件:不能砍。** `state_transitions` 测试([contract.rs:250-264](../rust/crates/min-cordis/tests/contract.rs#L250))直接断言完整迁移路径 `[Pending→Loading→Active→Unloading→Disposed]`,靠 `e.seq` 排序。watch 是 last-value 语义,慢订阅者跳过中间态,给不出这条路径。`FiberStatusChanged` 是公开 API(lib.rs:27 导出),承担"全量状态迁移历史"这个 watch 给不了的职责。

- **两个 watch channel(TransitionTask.done_tx + snapshot_tx):保持分开。** 前者承载"一次转换的完成 + 错误 identity"(恰好一次 + `Arc<CordisError>`),后者承载"连续状态流"(可能被跳过)。关注点不同,硬合并会把 `Snapshot` 塞满转换字段,认知负担反升。

- 其余确认承重的:`StoredValue` 双重包装(支持 `T: ?Sized`)、`CatchUnwind`(用户回调 panic 边界)、反向索引三元组(粒度必需)、`removing` 标志(清理期自访问)、`EffectRecord` 的 spawn 独立任务(取消安全,bug 4 的修法)。

## 工作量与风险(诚实边界)

这是一次小重构,不是白捡:

1. 重新锚定恰好一次(见上);
2. 改 `dispose`/`restart`/`settle_inner` 三个公开入口的内部实现;
3. `evict_and_finalize`([ctx.rs:333-362](../rust/crates/min-cordis/src/ctx.rs#L333))的 `RefreshDepsJoin` 意图要改成"获取消费者锁并 refresh,join 其完成";
4. 重跑 69 个测试 + 保留 bug 2/3 的对抗用例验证。

**收益**:删掉一整个抽象层(意图队列)和它的六个补丁机制,把 fiber 生命周期的认知负担从"理解一个 actor 模型 + 它的补丁"降到"一把锁"。这是本 crate 里唯一一处"删掉一层抽象"而非"挪动代码"的机会。

**风险**:恰好一次的重新锚定如果做错,会回归 bug 2/3/8;必须靠现有并发测试(`concurrent_dispose_join`、`dispose_during_dispose`、`cancel_wakes_awaiters`)+ bug 2/3 对抗用例兜底。

## 结论

值得做的只有 #1(fiber 驱动任务 → tokio::Mutex),它是唯一"删掉一层抽象"的简化,且能连根消除 bug 2/3 那一整类并发问题。其余候选(status_queue、合并 watch、Disposer 简化)读完代码后确认都是承重或挪动而非删减,不动。

建议:单独一个 commit 做 #1,做完用全套测试 + 对抗用例验证;不夹带其它改动。
