# Rust 设计与实现维护性简化评审

> 日期：2026-08-18  
> 评审对象：[Rust 设计](design-rust-port.md) 与 [`rust/`](../rust/) 当前实现  
> 评审目标：在不影响既有功能和并发语义的前提下，降低长期维护成本与认知负担。  
> 核心原则：非必要不引入特殊形态、重复状态、冗余对象或第二套生命周期规则。代码行数不是目标。

## 一、结论

当前实现已经具备较完整的功能和测试基础，核心架构没有根本性过度设计。Fiber mailbox、状态快照、代际取消、Effect exactly-once 和类型化事件等机制都有明确职责，不应为了代码更短而删除。

真正需要简化的不是“对象数量”，而是以下几处**同一个事实由多套机制共同表达**的问题：

| 需要表达的事实 | 当前参与机制 | 建议收敛后的模型 |
|---|---|---|
| Fiber 前序工作是否已经完成 | `resolved`、`intents_inflight`、Notify、watch 双检、stale drain | 由 mailbox `Settle` barrier 直接回答 |
| Effect 是否还能登记 | transition 状态锁、effects 锁、登记前状态检查 | 状态与 effects 归属同一个 lifecycle 临界区 |
| Binding 是否仍然可见 | provider 状态、`removing`、被复制的 Binding | 共享的 `Arc<Binding>` 单一身份 |
| once listener 是否已经消费 | EventBus mutex、`AtomicBool` | 只由 EventBus mutex 管理，取出即删除 |

这四处是本轮值得实施的简化。它们减少的是维护者必须同时记住的不变量，而不只是代码行数。

此外，当前实现仍有两个需要优先修正的并发窗口，以及几个错误处理问题。修正时应尽量利用上述收敛后的模型，不再增加 reservation、Preparing 对象或额外旁路状态。

## 二、评审标准

本轮不以“能否删除一个结构体”作为判断标准，而使用以下问题判断一项改动是否真的更简单：

1. 是否减少了必须跨对象、跨锁或跨任务保持一致的状态？
2. 是否让一个生命周期只有一个明确的所有者？
3. 是否能用项目已有的核心模型表达，而不增加例外路径？
4. 新概念是否替代了多条旧规则，而不是仅仅把代码搬到新抽象里？
5. 出现失败或并发竞争时，维护者能否从局部代码直接判断行为？

如果一项改动只减少字段、分配或几行包装代码，但没有减少上述负担，就不应作为独立的简化工作实施。

## 三、应保留的核心模型

以下机制虽然增加了实现体量，但职责清楚，并对应真实的功能或并发需求：

- **Fiber mailbox**：串行化 load、unload、restart、refresh 和 dispose 等控制操作。
- **watch snapshot**：向外部提供低成本、可订阅的状态观察。
- **generation CancellationToken**：隔离不同装载代际的后台任务。
- **EffectRecord 的 Live / Draining / Done 三态**：保证 cleanup exactly-once，并让多个调用方共享结果。
- **TransitionTask**：为可等待的状态转换提供统一完成语义和错误身份。
- **类型化事件与内部类型擦除的分层**：同时保留公开 API 的类型安全和内部异构存储。
- **regular event 与 waterfall event 分离**：两者的执行和错误传播语义确实不同。
- **`Arc<CordisError>` 错误身份**：确保并发等待者观察到同一个聚合错误。

这些机制之间的职责边界基本合理。把它们合并成更通用的“万能状态机”“通用 Completion”或统一事件枚举，反而会增加理解成本。

## 四、优先修正的实现问题

### P1：`Binding::clone()` 复制了可变并发状态

位置：`rust/crates/min-cordis/src/registry.rs` 的 `Binding`、手写 `Clone`、`lookup()` 和 `mark_binding_removing()`。

`Binding` 包含 `removing: AtomicBool`，但手写 `Clone` 会创建一个新的 AtomicBool。`lookup()` 得到副本后，原 Binding 即使被标记为 removing，副本仍可能看到旧值。

这使 service removal 与 `get()` 并发时，已经进入移除流程的服务仍可能短暂可见。

建议修正：

- Registry 保存 `Arc<Binding>`；
- lookup 返回同一个 Binding 的 Arc；
- 删除 `Binding` 的手写 `Clone`。

这不是为了少一个实现，而是为了建立一条更直接的不变量：

> Registry、lookup、依赖解析和 disposer 观察的是同一个 Binding 身份。

### P1：`Ctx::effect()` 存在检查与登记之间的竞争窗口

位置：`rust/crates/min-cordis/src/ctx.rs` 的 `Ctx::effect()`，以及 `fiber.rs` 的 effect drain 流程。

当前流程先检查 Fiber 状态，释放 transition 锁，执行 effect factory，最后才将 EffectRecord 加入单独的 effects 集合。

如果 Fiber 在检查之后进入 Unloading，并已经取走 effects，随后加入的新 EffectRecord 将不会被本轮 unload 清理，可能遗留监听器、服务或后台任务。

不建议通过新增以下概念修正：

- effect registration reservation；
- PreparingEffect；
- registration ticket；
- 第二套“正在登记”计数器。

这些方案会继续扩大生命周期状态空间。

建议修正：让 Fiber 的 lifecycle 状态与 effects 集合归属同一个临界区：

1. 在锁外执行用户的 effect factory，避免持锁调用用户代码；
2. 创建 EffectRecord；
3. 在同一个 lifecycle 临界区内完成“检查状态 + 加入 effects”；
4. 如果生命周期已经变化，立即清理新建的 EffectRecord，并返回登记失败。

对应的不变量是：

> Fiber 状态转换和 effect 集合变更由同一个生命周期域串行化。

### P2：非终止 unload 会静默丢弃 cleanup 错误

位置：`rust/crates/min-cordis/src/fiber.rs` 的 `unload()`。

cleanup 错误已经被聚合，但进入 `Pending` 的分支既不返回错误，也没有交给 ErrorSink。restart 或依赖刷新期间的清理失败因此完全不可见。

为了保持 restart 成功的现有行为，建议将此类聚合错误发送到 ErrorSink，而不是让它改变下一代 Fiber 的状态。

### P2：`JoinError::into_panic()` 没有区分 panic 与 cancellation

位置包括：

- `rust/crates/min-cordis/src/bus.rs`；
- `rust/crates/min-cordis/src/effect.rs`；
- `rust/crates/min-cordis-agent/src/lib.rs`。

Tokio task 既可能 panic，也可能被取消。直接调用 `into_panic()` 会在 cancellation 情况下再次 panic，破坏原本的错误收敛路径。

建议统一区分：

- panic：转换成现有 panic error；
- cancellation：转换成明确的任务取消错误；
- EffectRecord 无论遇到哪种结果，都必须最终进入 Done。

### P2：失效 Ctx 返回了未取消的 token

位置：`rust/crates/min-cordis/src/ctx.rs` 的 `cancellation_token()`。

Fiber 已不存在时，当前代码返回一个新的未取消 token。保留下来的旧 Ctx 调用 `cancelled().await` 时可能永远等待。

建议返回一个已经取消的 token，使“Fiber 已不存在”等价于“其代际已经结束”。

## 五、建议实施的认知简化

### S1：用 mailbox `Settle` barrier 取代稳定性推断协议

#### 当前问题

Fiber 已经通过 mailbox 串行处理控制操作，但 `FiberView::into_future()` 又从外部组合多个信号推断“Fiber 是否真正稳定”：

- `Snapshot.resolved`；
- `intents_inflight`；
- `notify_inflight_drained`；
- watch 双重读取；
- driver 退出时的 stale intent drain 和 yield。

维护者必须理解这些状态的更新顺序及全部竞争窗口，才能判断一次 await 为什么不会提前返回或永久等待。

#### 建议模型

新增一个明确的 mailbox intent：

```rust
Intent::Settle(TransitionTask)
```

driver 处理 Settle 时，它之前进入 mailbox 的控制操作已经完成。Settle 根据当时状态完成 TransitionTask。

Dispose 时先关闭 receiver，再排空已经进入队列的待完成 intent。发送发生在关闭之前则一定会被排空；发生在关闭之后则 send 失败，由发送侧立即完成任务。

在这个模型下，可以评估删除：

- `Snapshot.resolved`；
- `Trans.resolved`；
- `intents_inflight`；
- `notify_inflight_drained`；
- 用于推断稳定性的双检与 yield 协议。

#### 为什么它更简单

Settle 虽然增加一个 Intent 变体，但替代了多条分散的不变量。它使用的是项目已经存在的 actor/mailbox 模型，而不是新增第二种同步体系。

需要明确的契约只有一句：

> Settle 保证它之前进入同一 mailbox 的操作已经完成；与 Settle 并发投递的操作按 mailbox 实际顺序线性化。

如果产品语义要求等待“未来仍可能到来的并发消息”，任何有限时刻都无法严格判断稳定；因此 mailbox 顺序本身应当成为 settle 的定义。

### S2：统一 Fiber lifecycle 状态和 effects 的所有权域

这一项同时修正 `Ctx::effect()` 的并发窗口。

目标不是把所有 Fiber 数据塞进一把锁，而是只收拢必须原子一致的两项：

- 当前生命周期是否允许登记 effect；
- 当前生命周期拥有的 EffectRecord 集合。

状态观察、事件发布、CancellationToken 等不需要为了减少字段而强行合并。

建议形成以下规则：

- effect factory 永远在锁外运行；
- 状态检查与 EffectRecord 入集合在同一临界区完成；
- unload 在同一临界区切换状态并取走 effects；
- 未能登记的 effect 立即走现有 cleanup 语义，不引入新的半成品状态。

### S3：让 Binding 具有单一共享身份

Registry 改存 `Arc<Binding>`，删除对内部可变状态的快照式复制。

这项修改应保持局部，不需要顺带重写整个 registry，也不需要为了少一次分配而修改 StoredValue 的擦除形式。

### S4：once listener 只由 EventBus mutex 管理

当前 listener 集合已经由 EventBus mutex 串行访问，once 再使用 AtomicBool 属于两套同步机制表达同一个状态。

建议在锁内选中 once listener 时同时将其从集合删除：

- 并发 emit 仍只能有一个获得该 listener；
- disposer 之后调用自然成为 no-op；
- 不再保留已经 fired 的闭包；
- 删除 `fired` 和相关原子顺序推理。

regular hook 与 waterfall hook 可以共享内部的“取出并消费 once”辅助逻辑，但不应为了复用而把两种调用签名包装成复杂的通用枚举。

## 六、不建议作为简化项目实施的改动

### 1. 不为删除一层 Box 重写 StoredValue

这属于存储层细节。除非 profiling 或 API 约束证明它存在问题，否则它不影响主要心智模型，不值得单独修改。

### 2. 不为删除 Option 改写 Disposer

`Disposer` 是否内部持有 `Option<FnOnce>` 对调用方和生命周期理解影响很小。保持当前实现比制造一次无收益的 churn 更好。

### 3. 不把 CancellationToken 强行并入 transition mutex

状态转换与取消信号是两个容易理解的职责。为了少一个 Mutex 将它们耦合，会让锁范围和调用顺序更难判断。

### 4. 不把 status queue 拆成分散的返回值发布

当前 status queue 明确表达“锁内确定顺序、锁外发布事件”。如果改为每个调用点自行接收并发布事件，容易形成新的遗漏规则。没有实际问题时应保留。

### 5. 不引入通用 Completion 抽象

EffectRecord 与 TransitionTask 都有等待完成的行为，但所有权和状态转换不同。强行抽象容易得到一个参数很多、只有实现者理解的基础设施，并未减少领域概念。

### 6. 不主动合并 regular、parallel、serial 和 waterfall

这些模式在调用方式、执行顺序和错误传播上有真实差异。共享小型内部辅助函数可以，但不应把公开语义压进一个带大量分支的统一执行器。

### 7. 不为了“所有副作用都长得一样”增加空 Effect

后台任务应当有清楚的所有权：要么由 generation token 管理生命周期，要么由 Effect disposer 负责 join/abort。`Effect::Done` 不应被用来制造“好像已经登记”的外观。

具体采用哪种所有权方式，应根据任务是否需要等待退出决定，而不是为了结构统一同时保留两套机制。

### 8. 不在没有实际证据时新增 Weak 索引维护协议

如果后续确认 registry 弱引用长期累积，可以在现有遍历点做局部 `retain`。不要预先增加显式注册、注销、代际清理等新协议。

## 七、实施顺序

建议按以下顺序推进，每一步独立提交并通过全部测试：

1. **修正 Binding 身份问题**：Registry 改为 `Arc<Binding>`。
2. **统一 lifecycle 与 effects 的原子边界**：修正 effect 登记与 unload 竞争。
3. **修正错误收敛**：cleanup ErrorSink、JoinError cancellation、失效 Ctx token。
4. **简化 once listener**：锁内消费并删除，移除 AtomicBool。
5. **引入 Settle barrier**：先补契约测试，再删除旧稳定性推断状态。

不建议把这些内容合并成一次大规模重构。尤其 Settle barrier 应当在前面几个 correctness 修复稳定之后单独实施，以便判断行为变化来自哪里。

## 八、验收要求

除现有全量测试外，至少补充以下测试：

1. `get()` 与 service disposer 并发时，removing Binding 不再对外可见。
2. effect factory 执行期间发生 unload 时，新 effect 被立即清理且只清理一次。
3. Settle 排在初始 RefreshDeps 之后时，不会提前完成。
4. Settle 与 provider notification 并发时，结果遵循 mailbox 顺序。
5. Dispose 与 Settle/Restart 并发投递时，所有 TransitionTask 都能完成。
6. once listener 面对并发 emit 仍只调用一次，并且调用后不再保留 Hook。
7. cleanup task 被取消时，EffectRecord 仍进入 Done，所有 waiter 得到一致结果。

验证命令：

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

## 九、最终边界判断

这套实现可以进一步简化，但不应以“删除多少结构体”衡量。

最值得消除的是三类维护负担：

1. 通过多个原子量和快照推断 mailbox 稳定性；
2. 同一生命周期事实分散在多个锁和集合中；
3. 已有互斥保护的状态又额外使用原子量或快照副本维护。

如果完成 S1 至 S4，系统的核心模型可以收敛为：

- mailbox 负责控制操作的顺序与 settle；
- lifecycle 临界区负责状态和 effects 的一致性；
- Registry 中每个 Binding 只有一个共享身份；
- EventBus mutex 负责 listener 集合及 once 消费。

这四句话足以解释主要并发所有权关系。能否达到这种程度，比最终减少多少行代码更能衡量简化是否成功。
