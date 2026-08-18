# Rust 实现简化:三方评审综合结论

> 2026-08-18。综合三方:Dim 独立分析、[simplification-rust-impl-2026-08-18.md](simplification-rust-impl-2026-08-18.md)(下称"A 稿",偏删冗余)、[review-rust-maintainability-simplification-2026-08-18.md](review-rust-maintainability-simplification-2026-08-18.md)(下称"B 稿",偏修正确性 + 收敛)。
> 对象:[rust/](../rust/) 实现(核心 ~2100 行,70 测试全绿,两轮评审修复完毕)。
> 标尺:降低维护与认知负担——不数结构体数量,只问"这个机制挣不挣得回它的复杂度"。

## 一句话总结

这份代码没有过度设计,但有三处"同一事实说了两遍"是真该删的;另有四个正确性 bug 必须先修。复杂度大多是修 bug 修出来的诚实复杂度,简化的正确姿势不是找多余对象删,而是把说两遍的事实收敛成说一遍。

## 先纠正:两条"看着像重复其实不是"的误判

Dim 前两轮提过两条简化,经三份核对比对证明是错的,记录在案避免回退:

- **完整、有序的状态事件必须保留;`status_queue` 的结论要收窄(codex 第 6 条)。** `state_transitions` 测试([contract.rs:250-264](../rust/crates/min-cordis/tests/contract.rs#L250))证明的是 `FiberStatusChanged` 事件不能由 watch 替代(watch last-value,慢订阅者跳过中间态,给不出完整迁移路径),并不能证明 queue 是唯一实现方式。准确表述:必须保留完整、有序的状态事件;当前 status queue 已清楚且无实际问题,没必要改写。
- **两个 watch channel 不能合并**。`TransitionTask.done_tx`(一次转换的完成 + 错误 identity)与 `snapshot_tx`(连续状态流,可跳中间态)是两个关注点,硬合并会把 `Snapshot` 塞满转换字段,认知负担反升。

教训:**"看着像重复"不等于"是重复"**。判断标准是机制是否挣得回复杂度,不是结构体数量。

## 三处真冗余(三方独立收敛,可信)

### 1. `Binding` 的身份被复制了(优先做,零风险)

`lookup()` 返回克隆体,克隆体里的 `removing: AtomicBool` 是拍照那一刻的值——服务被标记删除后,旧副本还觉得自己可见。这不只是冗余,是潜伏的正确性 bug(移除中的服务短暂可见)。

**修法**:Registry 存 `Arc<Binding>`,lookup 返回同一身份的 Arc,删手写 Clone([registry.rs:36-49](../rust/crates/min-cordis/src/registry.rs#L36))。建立一条不变量:Registry、lookup、依赖解析、disposer 观察的是同一个 Binding 身份。

### 2. once 监听器的 `fired` 原子量是第二套锁

监听器集合本就被 EventBus 的 mutex 串行管着,又给 once 单独加了 `fired: AtomicBool`([bus.rs](../rust/crates/min-cordis/src/bus.rs))。一套状态两套同步;且这段微妙逻辑(`fired.swap(true)` 抢夺)在 `take_hooks`/`take_wf_hooks` 写了两遍——修一处漏一处的温床。

**修法**:锁内选中 once 监听器时"取出即删除"。并发 emit 仍只有一个拿到;disposer 后调自然变 no-op;`fired` 及相关原子顺序推理删掉。

### 3. "插件稳定了吗"用了五个信号拼(最重的认知负担)

判断一次 await 为什么不会提前返回或永久等死,维护者要同时记住五样的更新顺序:

- `Snapshot.resolved` 标志
- `intents_inflight` 原子计数
- `notify_inflight_drained`
- settle 的 watch 双重读取
- `drain_stale` 的 yield 循环

**修法(B 稿 Settle 方案,Dim 判断成立)**:新增 `Intent::Settle(TransitionTask)` barrier。"稳定"的本质是"我排进 mailbox 的活儿都干完了",这本该由 mailbox 先进先出的顺序直接回答,而不是在外面搭五个旁路信号反推。Settle 由驱动处理时,它之前入队的控制操作已完成,按当时状态完成 TransitionTask。

**加一个 Intent 变体,换掉五个分散机制**——这是唯一一处"加概念换删多概念"还划算的买卖。删掉:`resolved`、`intents_inflight`、`notify_inflight_drained`、双检、`drain_stale` 的 yield 协议。

## 一处存储冗余 + 与 Settle 的耦合(A 稿独立发现)

**反向依赖索引与 `last_deps` 存了同一份依赖集合(A 稿独立发现)。** `load()` 里 `add_consumer_edges(this, &deps)` 与 `last_deps = Some(deps)` 用的是同一个 `deps`([fiber.rs:342-343](../rust/crates/min-cordis/src/fiber.rs#L342))。方向有价值:可能消掉"reverse 边必须与绑定生命周期严格同步"这条跨结构不变式——跨结构同步正是历次 bug 产地。

**但这是候选,不是已确认的结论(codex 修正)。** 删除会带来:provider 摘除时扫描 `inject_index`;Registry 驱逐逻辑需读 Fiber 的 `last_deps`;必须证明 load、fail、unload 全路径下 `last_deps` 与当前消费关系严格一致。**应独立验证后决定,若证明成立倾向删除。**

**与 Settle 无必须捆绑的耦合(codex 修正,Dim 收回此前的捆绑主张)。** Settle 改的是"如何等 mailbox 前序完成",删 reverse map 改的是"如何发现要驱逐的消费者";`RefreshDepsJoin` 意图可原样保留,两者语义上无必须同时改的耦合。**拆成两个独立提交**,不捆绑,避免扩大回归面、难以归因。

## 正确性问题(B 稿独立发现,先于一切简化)

**共五项**:Binding 身份问题(见真冗余 #1,它本质是正确性 bug),加以下四项。

| # | 位置 | 问题 | 修法 |
|---|---|---|---|
| P1 | [ctx.rs effect()](../rust/crates/min-cordis/src/ctx.rs) | 注册有时间差:检查状态→释放锁→跑 factory→登记,中间 fiber 进 Unloading 则新 effect 被本轮卸载漏掉,监听器/服务泄漏 | "状态检查 + 登记进 effects"在同一临界区原子完成;factory 锁外跑;生命周期已变则立即清理新建的 record 并返失败 |
| P2 | [fiber.rs unload()](../rust/crates/min-cordis/src/fiber.rs) | 非终止 unload(restart/依赖刷新)把清理错误静默吞掉 | 聚合错误路由 ErrorSink,不改变下一代状态 |
| P2 | bus/effect/agent 多处 | `JoinError::into_panic()` 没区分 panic 与 cancellation——取消会再 panic 一次,破坏错误收敛 | 区分:panic→panic error;cancellation→明确的任务取消错误;EffectRecord 无论哪种都最终进 Done |
| P2 | [ctx.rs cancellation_token()](../rust/crates/min-cordis/src/ctx.rs) | fiber 已不存在时返回未取消 token,`cancelled().await` 永等 | 返回已取消 token,使"fiber 不存在"等价于"代际已结束" |

这四个是正确性问题,简化做得再漂亮也盖不住,必须先修。

## 边角清理(随时可做,低风险)

- emit 双层 spawn → 单层:任务内 `CatchUnwind` 包住调用、尾部自路由 ErrorSink,一次 spawn,D30 语义不丢。
- `Hook`/`WfHook` 两对复制 → 泛型合一(配合第 2 条 once 简化;共享"取出并消费 once"的辅助逻辑,但不把两种调用签名包装成通用枚举)。

**不做(codex 修正,Dim 收回 A 稿的两条激进建议):**

- **不删 `ServiceNotFound` 和 `Key<T>`。** 它们是公开 API:用户插件可构造 `ServiceNotFound`;`Key<T>` 提供类型化、可声明为 const 的限定键,`TypeKey::keyed::<T>()` 并不完全等价。删除影响功能与兼容性,对核心认知模型几乎无帮助。
- **不做 Adapter 毯式实现(codex 第 1 条,Dim 采纳错了 A 稿 S3,收回)。** `impl<E, L: Listener<E>> ErasedCall for L` 触发 unconstrained type parameter:同一 `L` 可实现多个 `Listener<E>`,`E` 无法唯一确定。现有 `ListenerAdapter<L, E>` 的 `PhantomData<fn() -> E>` 正是在 self type 中固定 `E`,是受 Rust 类型系统约束的必要形态,列入承重墙。

## 不该动的(三方确认承重墙)

| 机制 | 为什么不可省 |
|---|---|
| `StoredValue` 的 `Box<Arc<T>>` 双层 | 不改的理由是"局部存储细节,改掉不降认知负担",而非技术上不能改(codex 修正:外层 `Arc<dyn Any>` 本可把 `Arc<T>` 直接作被擦除值保存,Box 非唯一解) |
| 常驻 `snapshot_rx` | tokio watch 无 receiver 即视为关闭、send 静默失败,实踩过的坑 |
| `status_queue` + `FiberStatusChanged` | 见"误判"节:必须保留完整、有序的状态事件;当前 queue 已清楚且无实际问题,没必要改写(论证收窄,codex 修正) |
| 两个 watch channel 分开 | 见"误判"节,关注点不同 |
| CancellationToken 独立 | 状态转换与取消信号是两个职责,并入锁更难点 |
| 四种分发模式分开 | 调用方式/执行顺序/错误传播有真实差异,共享小辅助函数可以,不压成统一执行器 |
| `EffectRecord` 的 `Mutex<EffectState> + Notify` | 认领(Live→Draining 只许一次)本质是 CAS,watch 无条件覆盖表达不了 |
| 三个 Adapter 带 `PhantomData<E>` | 受 Rust 类型系统约束的必要形态(self type 固定 `E`,否则 unconstrained);codex 第 1 条 |
| 不引入通用 Completion 抽象 | EffectRecord 与 TransitionTask 所有权和状态转换不同,硬抽象只会得到参数很多、只有实现者懂的基础设施 |

## 执行顺序(按风险,每步独立提交 + 全量测试;codex 修正版)

1. **修正确性**:五个问题(`Arc<Binding>` 共享身份 + effect 生命周期原子边界 + cleanup 错误进 ErrorSink + into_panic 区分 cancellation + 失效 Ctx 返已取消 token)。让代码先"对",是简化的地基。
2. **删 once 的 `fired`**:零风险,立刻少一套要推理的同步。
3. **独立实现并验证 Settle barrier**:加一个 Intent 变体换删五个稳定性信号。单独成步,便于归因。
4. **独立评估 reverse map 能否由 `last_deps` 替代**:验证 load/fail/unload 全路径 `last_deps` 与消费关系严格一致后再决定;若成立倾向删。**与第 3 步拆成两个独立提交,不捆绑。**
5. **可选:emit 单层 spawn**。

做完第 3 步,fiber 的并发所有权能压成三句话:mailbox 管顺序,一把锁管状态+effects,Registry 里每个 Binding 一个身份。

## 配套修订

- 设计文档 D21 措辞:仅当第 4 步(reverse map 由 `last_deps` 替代)验证通过并实施时,才从"反向边三元组索引"改为"摘除时按 `last_deps` 含三元组判定消费者"。
- §八 追加本轮简化批次记录。
- (codex 修正)不删 `Key<T>`,§七.2 裁决不改判。

## 验收

每步跑:

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

第 3 步(Settle barrier)需补测试:

- Settle 排在初始 RefreshDeps 之后时不提前完成;
- Settle 与 provider notification 并发时遵循 mailbox 顺序;
- Dispose 与 Settle/Restart 并发投递时所有 TransitionTask 都完成。

第 4 步(删 reverse map,独立于第 3 步)若实施,需验证 + 回归:

- load/fail/unload 全路径下 `last_deps` 与当前消费关系严格一致;
- 驱逐精度:`eviction_order`/`isolate_no_cross_evict`/`single_service_evict` 钉着回归。

## 最终判断

第 3 步(Settle)做完,这份代码"必须记住的概念数"才真正降一个台阶;1、2、5 是顺手清干净;第 4 步(删 reverse map)独立验证后决定。其余都是挪动而非删减,不值得做。这份代码的复杂度是诚实的——每个机制对应一个真踩过的坑,所以简化的全部内容就是:**找到那几处"同一事实说了两遍"的地方,让它只说一遍。** 但要守住 codex 划的线:不删公开 API、不改受类型系统约束的 Adapter、不把可独立的改动捆成大动作——否则就把"降低认知负担"重新带回"为了删除而删除"。
