# min-cordis Rust 移植设计 v4 评审

> 被评审文档：[design-rust-port.md](design-rust-port.md)  
> 评审日期：2026-08-17  
> 评审范围：核心范式、公共 API、异步生命周期、依赖重载与验收计划  
> 评审结论：**方向通过，设计冻结暂不通过**

## 一句话结论

v4 从“一比一移植”转向“范式移植”是正确选择，五个核心支柱也基本选对了；但 EventBus、Effect、Fiber transition 和 isolate 依赖索引尚未形成可以直接实现的闭合契约。

建议把当前状态从“定稿”改为“RFC / 待决”，先解决 4 个阻断问题，再开始 M1 和 M2。否则实现过程中大概率会重写核心接口。

## 总体评价

| 维度 | 评价 | 说明 |
|---|---|---|
| 范式边界 | 好 | 五支柱明确，非目标也划得清楚，没有继续背负 JS 特有语义 |
| Rust 方向 | 基本正确 | 类型化事件、显式 PluginId、CancellationToken、锁不跨 await 都是合理选择 |
| API 完整度 | 不足 | 多个公开类型仍是占位符，部分签名彼此冲突 |
| 并发正确性 | 不足 | `watch`、generation、重复 dispose 和快速重载之间的关系没有闭合 |
| 作用域正确性 | 不足 | 反向依赖索引没有纳入 isolate 身份 |
| 可测试性 | 尚可 | 契约测试方向正确，但缺少几类决定架构成败的竞争测试 |

值得保留的设计包括：

- 以范式契约而不是 TS 96 个测试作为验收标准；
- `PluginId` 显式身份，不再依赖闭包地址；
- `CancellationToken` 作为统一的协作取消机制；
- transition mutex 内只做短临界区操作，绝不持锁跨 await；
- provider 清理期间保留自访问能力；
- 清理错误不压平，单错原样、多错聚合。

下面的问题并不是要求恢复 TS 的接口，而是当前 v4 自己声明的契约尚不能由草案 API 实现。

---

## 阻断问题 1：EventBus 的类型模型还没有闭合

### 当前问题

草案只提供一种监听器注册方式：

```rust
pub fn on<E: Event>(
    &self,
    ctx: &Ctx,
    f: impl Fn(&Ctx, &E) -> ListenerResult + Send + Sync + 'static,
) -> Disposer;
```

但五种分发模式需要三类不同能力：

- `emit`：同步调用，异步结果在后台运行；
- `parallel`、`serial`：等待异步结果；
- `bail`：必须同步拿到结果才能短路；
- `waterfall`：监听器必须接收 `next`，并返回链的最终值。

现在的 `ListenerResult` 和 `Value<E>` 没有定义，也没有说明事件类型、监听器返回值和分发模式如何关联。

另外还有两个具体问题：

1. `async fn waterfall(...) -> BoxFuture<...>` 会产生两层 Future；应当使用普通 `fn -> BoxFuture`，或者 `async fn -> Result`，不能两者同时使用。
2. `emit(ctx: &Ctx, e: &E)` 无法直接把异步监听器交给 Tokio 后台任务，因为后台 Future 必须拥有 `'static` 数据，不能继续借用调用栈中的 `ctx` 和 `e`。Tokio 的 [`spawn`](https://docs.rs/tokio/latest/tokio/task/fn.spawn.html) 明确要求 Future 为 `Send + 'static`。

### 影响

这是 M1 的基础接口。如果先按当前草案实现，进行到 `bail`、异步 `emit` 或 waterfall 类型擦除时就必须重新设计 Hook 表和公开 API。

### 建议决策

先在下面两条路线中选择一条：

#### 路线 A：保留同步 bail

拆分监听器类型和注册面，例如：

- `on_sync`：用于同步 `emit` / `bail`；
- `on_async`：用于 `parallel` / `serial`；
- `on_waterfall`：监听器显式接收 `Next`。

这是语义最清楚的方案，但 API 数量会增加。

#### 路线 B：事件系统全部异步

所有监听器统一返回 `BoxFuture`，`bail` 也变成异步短路。这样实现最简单，但需要放弃“五模式中的同步 bail”承诺。

无论选择哪条路线，都建议：

- 给 `Event` 增加关联返回类型，或者拆成 `Event` / `QueryEvent` / `WaterfallEvent`；
- fire-and-forget 分发接收拥有所有权的 `Arc<E>` 或 `E`，不要把借用数据交给后台任务；
- `Next` 应传给 waterfall 监听器，而不是作为 EventBus 分发函数自身的普通参数模型；
- 先写一份能够通过 `cargo check` 的完整 API 骨架，再进入实现。

### 验收条件

- 五种模式各自的监听器签名明确；
- 没有未定义的 `ListenerResult` / `Value<E>`；
- waterfall 只有一层 Future；
- 异步 emit 不捕获非 `'static` 借用；
- 类型擦除后仍能安全地按 `TypeId` 还原事件和返回值。

---

## 阻断问题 2：Effect API 无法表达清理失败

### 当前问题

草案中的 Effect 为：

```rust
pub enum Effect {
    Done,
    Disposer(Box<dyn FnOnce(&Ctx) + Send>),
    AsyncDisposer(Box<dyn FnOnce(&Ctx) -> BoxFuture<'static, ()> + Send>),
    Many(Vec<Effect>),
}
```

同步和异步 disposer 都只能返回 `()`，因此无法实现契约测试中声明的：

- 单个清理错误原样返回；
- 多个清理错误聚合；
- 用户自己的 Aggregate 不被压平；
- 重复 dispose 观察到同一个 `Arc<CordisError>`。

此外，`FnOnce(&Ctx) -> BoxFuture<'static, ()>` 只能在创建 Future 之前短暂读取 `Ctx`，不能让返回的 Future 在 await 期间继续借用它。这个签名不足以支持常见的异步清理逻辑。

现有 TS 版已经把执行结果和清理结果分成两个通道，并对清理实行严格 LIFO、完整执行、错误聚合和重复调用 join，参见 [`src/fiber.ts`](../src/fiber.ts#L438-L633)。v4 虽然不需要复制实现结构，但需要保留自己已经承诺的这些不变量。

### 建议修改

让 disposer 捕获它需要的 owned context，并统一返回 `Result`：

```rust
pub enum Effect {
    Done,
    Disposer(
        Box<dyn FnOnce() -> Result<(), CordisError> + Send>,
    ),
    AsyncDisposer(
        Box<dyn FnOnce() -> BoxFuture<'static, Result<(), CordisError>> + Send>,
    ),
    Many(Vec<Effect>),
}
```

如果清理确实必须访问 `Ctx`，闭包可以捕获一个可拥有、可克隆的 `Ctx`，而不是借用调用方传入的 `&Ctx`。

同时需要一个内部 `EffectRecord`，而不是仅依靠 `FnOnce` 保证恰好一次：

- 第一个调用者启动清理；
- 后续调用者 join 同一个清理任务；
- `Many` 严格逆序、串行执行；
- 一个清理失败不能阻止剩余清理；
- 单错原样返回，多错包装为 `Aggregate`；
- 清理结果缓存为共享终态。

### 验收条件

- 同步和异步清理都能返回错误；
- 并发调用同一 disposer 只执行一次；
- 所有调用者收到同一终态；
- `Many` 的执行顺序和聚合规则有明确测试；
- 异步清理不依赖悬空的 `&Ctx` 借用。

---

## 阻断问题 3：`watch<FiberState>` 不能独立承担 transition join

### 当前问题

设计把 `tokio::sync::watch` 同时用于：

- 对外观察 Fiber 状态；
- 等待一次 load / unload 完成；
- 支持晚订阅；
- 保证重复 dispose join 同一结果；
- 共享同一个错误 identity。

这些职责并不完全相同。

Tokio `watch` 只保留最新值；接收者可能看不到中间状态。新建订阅还会把当前值视为已读，如果直接调用 `changed()`，它等待的是下一次变化，而不是立即返回当前终态。正确使用需要先检查当前快照，再循环等待。参见 [`tokio::sync::watch`](https://docs.rs/tokio/latest/tokio/sync/watch/) 的 change notification 说明。

更重要的是，`watch` 只能通知“状态变了”，不能自动证明：

- 两次 dispose 等待的是同一代 unload；
- restart 不会覆盖上一代调用者要观察的错误；
- `Loading → Active → Unloading` 快速变化时，没有调用者把后一代状态误认为前一代完成；
- 恰好一次清理已经由某个唯一任务执行。

### 建议修改

把“状态观察”和“操作完成”拆成两层：

```rust
struct FiberSnapshot {
    generation: u64,
    state: FiberState,
    last_error: Option<Arc<CordisError>>,
}

struct Transition {
    generation: u64,
    operation: Option<Arc<TransitionTask>>,
    token: TransitionToken,
}
```

- `watch<FiberSnapshot>`：只负责状态观察和诊断；
- `TransitionTask`：代表某一代唯一的 load / unload 操作，保存可共享完成结果；
- transition mutex：决定是创建新任务，还是 join 已有任务；
- waiter：记录目标 generation，只在该代操作完成后返回。

`watch` 可以继续保留，但不应被描述为“天然保证恰好一次”。恰好一次来自锁内保存并复用同一个 `TransitionTask`。

### 验收条件

- 并发 dispose 只创建一个 unload task；
- dispose 与 restart 竞争时，每个调用者等待正确 generation；
- 快速状态变化不会导致误完成或永久等待；
- 晚订阅者先检查当前快照，再等待变化；
- 上一代错误不会被下一代状态覆盖。

---

## 阻断问题 4：反向依赖索引没有包含 isolate

### 当前问题

服务注册表允许同一种类型在不同 isolate 中同时存在，但 D14 的反向索引是：

```rust
HashMap<TypeKey, Vec<PluginId>>
```

如果两个 isolate 都注册了相同的 `TypeKey`，A 作用域的 provider 卸载时，仅凭这个索引无法区分 A、B 两组消费者，可能错误驱逐 B 作用域的 Fiber。

这会直接破坏“同类型跨作用域可并存”的核心支柱。

### 建议修改

至少区分三个概念：

```rust
struct ServiceKey {
    type_id: TypeId,
    qualifier: Option<KeyId>,
}

struct BindingKey {
    scope_id: ScopeId,
    service: ServiceKey,
}

struct ProviderId {
    plugin_id: PluginId,
    generation: u64,
}
```

反向依赖索引应按 `BindingKey` 或实际解析到的 `ProviderId` 建立，而不是只按 `TypeKey` 建立。

更推荐让每个已激活消费者记录自己实际绑定的 provider：

```text
consumer generation
    └── dependency BindingKey
            └── resolved ProviderId
```

这样 provider 卸载、同类型替换和快速重注册都能精确判断消费者是否已经 stale。

文档还需要明确 `isolate(label)` 的解析规则：

- 子作用域是否回退到父作用域；
- isolate 是隔离整个服务容器，还是只隔离指定 ServiceKey；
- 同名 label 是否有意合并作用域；
- ScopeId 的生命周期由谁管理。

### 验收条件

- A 作用域 provider 卸载不会驱逐 B 作用域消费者；
- 同一作用域内 keyed 多实例不会串线；
- provider 快速替换后，旧 generation 的卸载不会驱逐已绑定新 provider 的消费者；
- isolate 查找和父级回退规则有独立测试。

---

## 重要问题 5：当前模型无法可靠检测依赖环

### 当前问题

`Plugin` 只声明 `injects()`，没有声明 `provides()`。服务是在 `apply()` 执行过程中动态注册的。

考虑下面的情况：

```text
插件 A：依赖服务 B，启动后提供服务 A
插件 B：依赖服务 A，启动后提供服务 B
```

两个插件都会停在 Pending，`apply()` 永远不会执行，因此框架看不到它们准备提供的服务，也就无法证明这是一个环。

这与 `InjectUnsatisfied` 中的“dependency cycle or unsatisfied”错误承诺冲突。另一方面，“依赖暂时不满足”本来就是合法 Pending 状态，不应立即成为错误。

### 建议决策

推荐首版保持动态插件能力，并缩小承诺：

- 缺少依赖时保持 Pending，不报错；
- 删除首版的通用 cycle detection 承诺；
- `InjectUnsatisfied` 拆成更明确的错误，或从首版删除；
- M4 可增加可选 `provides()` manifest，用于静态诊断和启动时环检测。

如果 cycle detection 是首版硬需求，则必须要求插件在执行前完整声明 `provides()`，不能继续把服务提供视为完全动态行为。

---

## 重要问题 6：配置和 update 的公开承诺不一致

### 当前问题

文档同时存在以下表述：

- `FiberView` 支持 `dispose/restart/update`；
- D12 要求插件自带 `validate(config)`；
- “与 TS 的关系”声称保留 update 双分支不变量；
- 开放问题又倾向于 M2 只做 `dispose + restart`，把 update 留到 M4。

但当前 `Plugin` trait 没有配置关联类型、配置参数或 `validate` 方法，`Ctx::plugin()` 也没有接收配置。

### 建议决策

按文档已经表达的倾向，建议首版选择最小路线：

- M2 只提供 `dispose + restart`；
- 配置作为构造完成的 Plugin 实例的一部分；
- 从首版 `FiberView`、契约测试和“不变量”描述中移除 update；
- M4 再设计 `PluginFactory<Config>`、配置擦除和 update 事务。

如果仍要在 M2 支持 update，就必须先回答：

- 配置类型如何在 dyn Plugin 后擦除；
- 新配置由谁校验；
- 校验失败是否保留旧配置；
- loading / unloading 期间 update 如何选择 generation；
- update 是修改同一 Plugin，还是创建新 Plugin 实例。

---

## 重要问题 7：资源所有权和取消传播没有落到 API

### 资源所有权

`on()` 和 `provide()` 返回 `Disposer`，插件的 `apply()` 又返回 `Effect`，目前没有说明：

- listener/service 是否已经自动归当前 Fiber 所有；
- 插件是否还需要把 disposer 放入返回的 Effect；
- 手工提前 dispose 后，Fiber 卸载是否会再次执行；
- isolate 出来的 Ctx 是否保留原 Fiber 所有权。

现有 TS 版的行为是：通过 Fiber Context 注册的 listener 和 service 自动成为 Fiber effect，返回的 disposer 只用于提前释放。`ctx.provide()` 的自动所有权和“等待消费者排干后再删除自身快照”可参考 [`src/reflect.ts`](../src/reflect.ts#L277-L304)。

建议 v4 明确采用相同的所有权原则，但不必复制 TS 接口：

> 通过某个 Fiber 的 Ctx 创建的 listener、service 和 child plugin 自动归该 Fiber 所有；返回的 Disposer 只表示提前释放，不需要再次放入 Plugin 返回的 Effect。

Plugin 返回的 Effect 只负责没有通过 Ctx 注册的外部资源。

### 取消传播

D7 选择了 `CancellationToken`，但 `Ctx` 和 `Plugin::apply` 没有向插件暴露它，因此插件无法协作响应 Fiber 卸载。

建议每个 generation 拥有独立 child token，并通过下面至少一种方式暴露：

```rust
ctx.cancellation_token()
ctx.cancelled().await
```

建议卸载顺序明确为：

1. 标记 transition 为 unloading；
2. cancel 当前 generation；
3. 等待正在执行的 apply 退出；
4. 严格执行 EffectRecord 清理；
5. 发布最终状态。

同时应明确：这是协作取消。插件如果从不观察 token，也不自行返回，dispose 可以无限等待；框架首版不应暗示能够强制终止任意 Rust Future。

---

## 需要顺手修正的文档问题

这些问题不阻断架构，但应在下一版一并修正：

1. `CordisError` 无条件使用 `#[derive(thiserror::Error)]`，因此 `thiserror` 是普通依赖，不是可选依赖。
2. `serde/serde_json` 如果只用于 agent 示例和 LLM 边界，应放进 agent crate，不进入核心 crate。
3. D10 说注册返回 `PluginId`，公共 API 却只显示返回 `FiberView`；应明确 `PluginId` 是 `FiberView` 字段还是单独返回值。
4. `ErrorSink` 出现在里程碑，但没有接口、所有权和失败策略。
5. 应明确 panic 策略：插件 panic 是终止任务、转成 `CordisError`，还是要求调用方保证不 panic。
6. `Event::NAME` 可以保留为日志字段，但应明确它不参与唯一性和分发。

---

## 建议补充的竞争与失败测试

当前测试矩阵覆盖面不错，但下面这些测试会直接决定架构是否正确，建议在实现前先写成契约描述：

### EventBus

- 异步 emit 在调用方返回后仍能安全读取事件载荷；
- async listener 失败只进入 ErrorSink，不产生未观察任务错误；
- waterfall veto 后不会执行后续监听器和 terminal next；
- listener 卸载与正在进行的 dispatch 竞争时行为确定。

### Effect / Fiber

- 10 个并发 dispose 只执行一次清理，并 join 同一结果；
- dispose 与 restart 同时发生时，每个 waiter 只观察自己的 generation；
- `Loading → Unloading → Loading` 快速切换不会丢失唤醒；
- 旧 generation 的晚到错误不会污染新 generation；
- 清理中再次调用 dispose 不死锁；
- 清理中注册新 effect 明确失败。

### Registry / Reload

- A isolate 的 provider 卸载不影响 B isolate；
- provider 删除后立刻以同一 ServiceKey 重建，旧卸载不驱逐新消费者；
- provider 清理期间能访问自身快照，但普通消费者不能建立新的旧绑定；
- 多层依赖链按消费者到 provider 的顺序排干；
- 缺失依赖长期保持 Pending，不被错误标记为 Failed。

### Cancellation

- loading 中 cancel 能唤醒观察 token 的插件；
- 多次 cancel 幂等；
- 父 Fiber cancel 传播到所有子 Fiber；
- 不响应取消的插件被明确记录为协作取消限制。

---

## 推荐的修订顺序

### 第一步：冻结四个基础模型

先只定义并评审以下类型，不写业务实现：

1. Event、Hook、Next 和五种分发结果；
2. Effect、EffectRecord 和清理错误；
3. FiberSnapshot、Transition、TransitionTask 和 generation；
4. ServiceKey、BindingKey、ScopeId 和 ProviderId。

这四组类型决定了后续实现是否需要返工。

### 第二步：建立可编译的 API 骨架

创建最小 crate，只包含 trait、enum、type alias 和空实现，通过：

```text
cargo check
cargo test --doc
```

重点验证 dyn compatibility、Future 生命周期、Send/Sync 和类型擦除，不急着实现状态机。

### 第三步：先实现 transition 和 EffectRecord

EventBus 相对独立，但 Fiber 生命周期是 Registry、Service 和 reload 的共同基础。先用纯状态机测试证明：

- 同代合并；
- 跨代隔离；
- 恰好一次；
- 取消与清理顺序；
- 错误 identity。

### 第四步：实现带作用域的服务绑定和重载

先让消费者记录实际 ProviderId，再建立反向索引。不要先写只有 TypeKey 的全局扫描或索引，再补 isolate。

### 第五步：最后接入 agent 示例

agent 示例适合作为端到端验收，但不应反向决定核心事件和生命周期 API。`serde_json::Value`、LLM 消息和 ToolSpec 保持在边界 crate。

---

## 下一版设计的通过标准

满足以下条件后，可以认为设计已达到“可冻结、可实现”状态：

- [ ] 五种事件模式都有完整、可编译的监听器签名；
- [ ] fire-and-forget 不持有非 `'static` 借用；
- [ ] Effect 能表达同步/异步成功与失败；
- [ ] 并发 dispose 明确 join 同一个 TransitionTask；
- [ ] watch 只承担观察职责，operation completion 单独建模；
- [ ] 所有 transition 都带 generation；
- [ ] 服务身份包含 TypeId、qualifier 和 ScopeId；
- [ ] 消费者记录实际绑定的 ProviderId；
- [ ] Pending、Failed、Disposed 的含义互不混淆；
- [ ] 配置/update 是否进入 M2 已做唯一选择；
- [ ] Ctx 注册资源的 Fiber 所有权规则明确；
- [ ] CancellationToken 已暴露给插件；
- [ ] 关键竞争测试已写入验收矩阵；
- [ ] 公开 API 骨架通过 `cargo check`。

## 最终意见

**建议有条件通过路线选择，但暂缓设计冻结。**

v4 最重要的进步，是已经摆脱了“为了兼容 TS 而在 Rust 中模拟 JS”的负担。当前问题不是路线错误，而是从原则到可实现契约还差一层精确定义。

只要先关闭 EventBus、Effect、Transition 和 Scope Binding 这四个问题，其余里程碑可以沿现有方向继续推进；如果不先关闭，它们会在 M1/M2 同时暴露，导致核心接口和测试一起返工。
