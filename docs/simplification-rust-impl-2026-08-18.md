# Rust 实现简化反思(simplification review)

> 2026-08-18。对象:[rust/](../rust/) 实现(核心 ~2100 行,70 测试全绿,两轮评审修复完毕)与 [design-rust-port.md](design-rust-port.md) v5。
> 标尺:**降低维护与认知负担**——非必要不引入奇怪的形态和冗余的对象与结构。行数是次要指标,首要指标是"维护者必须记住的概念与不变式数量"。
> 方法:通读全部核心源码(fiber.rs / registry.rs / effect.rs / ctx.rs / bus.rs / event.rs),逐概念审问"它挣得回自己的复杂度吗";对每个删除候选做全路径等值推理,并复核上一轮快速结论(其中三条被推翻,见 §五)。

## 结论

可做,且有一处结构性冗余:**反向依赖索引与 `last_deps` 存了同一信息,整个 reverse map 可删**。合计约 **-190 行**(核心 ~2100 → ~1900),更重要的是:删一个跨结构存储系统及其同步义务、删一个特殊旗标、删三个 Adapter 结构、删一个手写 Clone、删一个死变体、删一个幽灵类型;公共 API 相应缩小。风险集中在两项,均有现成钉子测试护航。

## 一、真冗余:同一信息存了两遍

### S1. 删除反向依赖索引,复用 `last_deps`(结构性,最优一刀)

`Registry.reverse: HashMap<(PluginId, u64, TypeKey), Vec<Weak<FiberInner>>>`(registry.rs:54-57)与每个 fiber 的 `last_deps` 存的是**同一个集合**:`load()` 中 `add_consumer_edges(this, &deps)` 与 `last_deps = Some(deps)` 用的是同一个 `deps`(fiber.rs:342-343);卸载侧 `drain_effects` 同时清两者。TS 原版亦无反向索引(notify 时逐 fiber 重解析)。

**修法**:provider 摘除时扫 `inject_index[key]`,只给 `last_deps` 含该三元组的 fiber 发驱逐 join 任务。删除 `reverse` map、`add_consumer_edges`、`remove_consumer_edges`、`take_reverse_entry`(约 -50 行),换 ~12 行过滤。

**等值性逐路径核验**:装载窗口一致(last_deps 在 apply 前设置,与 reverse 边同点);失败/重载窗口一致(fail_load 与 unload 同点清除);三元组匹配精度一致;每次卸载免去 O(全部边) 的 `remove_consumer_edges` 扫描。代价:摘除时从 O(直接消费者) 变 O(声明该键的 fiber 数),此规模下可忽略。

**认知收益**:消灭一整类维护义务——"reverse 边必须与绑定生命周期严格同步"这条不变式从系统里消失。跨结构同步正是历次 bug 的产地。

**设计修订**:D21 从"反向边三元组索引"改为"摘除时按 `last_deps` 含三元组判定消费者"。

### S2. `resolved` 标志可证冗余

settle 等待"resolved + 在途为零"(fiber.rs:33-40, 123, 237-251),但 `ctx.plugin()` 返回前必 post 初始 RefreshDeps(`intents_inflight ≥ 1`),驱动处理完归零——"首次依赖解析是否完成"完全被在途计数蕴含。

**逐路径验证**:root 从不 post 意图(初始 Active、计数 0 即稳定 ✓);spawn 与 post 之间无外部观察者可插入(plugin() 同步函数)✓;post 失败回退计数的路径落在 settle 的终态捷径之后 ✓;首个意图处理后 state 可能不变(Pending→Pending),`notify_inflight_drained` 的同值快照发送负责唤醒 ✓。

**修法**:删 `Snapshot.resolved` 公共字段、`Trans.resolved`、`mark_resolved()` 及其快照发布(~25 行)。原竞态(注册后立即 await 提前返回)由全量契约测试钉着回归。

## 二、怪形态:可以消失的结构

### S3. 三个带 PhantomData 的 Adapter → 毯式实现

`ListenerAdapter<L, E>`、`WaterfallAdapter<L, E>`、`TerminalAdapter<E, T>`(event.rs)存在的唯一理由是桥接类型化 trait 到擦除 trait——这正是本地 trait 对外来 trait 做 blanket impl 的标准场景:

```rust
impl<E: Event, L: Listener<E>> ErasedCall for L { ... }
impl<E: Event, L: WaterfallListener<E>> ErasedWaterfallCall for L { ... }
impl<E: Event, T: Terminal<E>> ErasedTerminal for T { ... }  // T: Sized
```

三个结构体、三份 PhantomData、三段包装(~40 行)全部删除;event.rs 收敛为"类型化 trait + 擦除 trait + 毯式桥"。coherence 干净(ErasedCall 系本地 trait,无其他实现者)。

### S4. `Hook`/`WfHook` 两对复制 → 泛型合一

结构与 `take_hooks`/`take_wf_hooks` 各写两遍,包括最微妙的 once 认领(`fired.swap(true)` 原子抢夺)——**双份微妙逻辑是维护隐患**(修一处漏一处)。泛型 `Hook<C>` + 单个认领辅助函数收成一个(~-25 行,零风险)。

### S5. `Binding` 手写 Clone + 快照式旗标 → `Arc<Binding>`

`removing: AtomicBool` 靠手写 Clone 逐字段拷值(registry.rs:36-49),克隆体看不到后续置位——语义碰巧正确但是脚枪。注册表改存 `Arc<Binding>`,旗标天然共享,Clone impl 整个删除(~-14 行,纯改善)。

## 三、边角清理

| # | 项 | 说明 |
|---|---|---|
| S6 | 删 `ServiceNotFound` 死变体 | 核心零构造点(已核实);`get` 走 Option 是 D13 裁决。测试改用其他变体作通用错误载荷 |
| S7 | 删 `Key<T>` 幽灵类型 | 全仓库一个测试在用,零外部用户;§七.2 的"倾向性"裁决产物,按"非必要不引入"原则改判为 `TypeKey::keyed::<T>()` 足够 |
| S8 | emit 双层 spawn → 单层 | 现在 spawn 监听任务再 spawn 观察者防 panic 丢失;改为任务内 `CatchUnwind` 包住调用、尾部自路由 ErrorSink——一次 spawn,D30 语义不丢,任务数减半 |
| S9 | drive 两处重复收尾 | Restart-root 分支的 `continue` 绕过循环尾(复制了一份计数递减);Dispose 臂直用 `done_tx.send` 不走 `complete_task`。重排单出口 |
| S10(可选) | `RefreshDepsJoin` → `RefreshDeps(Option<Arc<Task>>)` | 四变体收二,"Join 后缀命名"消失;`Intent::task()`/`complete_intent` 随之简化 |

## 四、公共 API 变化汇总

`Snapshot` 少 `resolved` 字段;`CordisError` 少 `ServiceNotFound`;`Key<T>` 删除(改判);其余全为 crate 内部。

## 五、曾提出、经复核推翻的候选(勿再试)

上一轮快速反思给出的三条建议,深入推理后不成立或得不偿失,记录在案:

1. **"EffectRecord 用 watch 替 `Mutex<EffectState> + Notify`"——不可行。** 认领("Live→Draining 只许一次")本质是 CAS;`watch::send` 是无条件覆盖,两个并发 drain 都会以为自己是第一个。Mutex 认领不可省,Notify 换 watch 无净收益。effect.rs 现状(175 行)无怪形态,**不动**。
2. **"dispose 改走快照 join,拆解 TransitionTask"——不该做。** 行数略省,但把"一个统一 join 概念"拆成三种习惯用法(dispose 等快照、restart 用 oneshot、驱逐再一种),认知负担反升。TransitionTask 的统一性(所有可 join 操作带一个任务、完成一次、Arc 缓存)**本身就是简化后的形态**,保留。
3. **"删 `Ctx::root_with`"——不删。** 零调用不等于死代码:它是 D8"注入 Handle 优先"的文档化 API 承诺,4 行,零维护成本。

## 六、核查过、不可再简的(承重墙清单)

| 机制 | 为什么不可省 |
|---|---|
| `StoredValue` 的 `Box<Arc<T>>` 双层 | `?Sized` 服务(`Arc<dyn Trait>`)无法 coerce 到 `Arc<dyn Any>`;Box 包装是最小解(两轮评审"确认无误") |
| 常驻 `snapshot_rx` | tokio watch 无 receiver 即视为关闭、send 静默失败——实锄过的坑 |
| `alive` + `drain_stale` | 替代方案只有驱动常驻(每 fiber 泄漏一个任务)或 join 永等(死锁) |
| `intents_inflight` 与 `alive` 两个原子 | 前者管 settle 稳定性,后者管投递守卫,职责不同 |
| 锁内 FIFO 入队(`status_queue` + `flush_status`) | D24 提交顺序契约 |
| `fail_load` 与 `unload` 的 ~20 行相似 | 合并需参数化错误路由(装载错误 vs 清理错误聚合),概念数不降反升 |
| `NextState` 两变体枚举 | 用类型约束卸载目标域,比传 `FiberState` 更防错 |
| `provided` 列表 | Active 时通知提供键;改扫描全 bindings 是 O(n) 换 O(1),无净收益 |

## 七、执行计划

顺序按风险递增,每步全量双模式(单线程/并行)回归:

1. S5、S4(零风险形态清理)
2. S3(毯式桥,低风险)
3. S6-S9(边角)
4. S2(resolved 删除——中风险,全套契约测试护航)
5. S1(reverse map 删除——中风险,`eviction_order`/`isolate_no_cross_evict`/`single_service_evict`/`evict_after_consumer_disposed_completes` 钉驱逐精度)

设计文档配套修订:D21 措辞、§七.2 的 Key 裁决改判、§八 追加简化批次记录。
