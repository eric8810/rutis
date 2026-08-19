# rutis Rust 移植设计(v5,范式路线)

> 2026-08-17 v5 草案。**路线**:范式移植——实现 Cordis 核心范式,Rust 惯用表达,不锚定 TS 96 spec,验收 = 范式契约自证。
> **状态**:草案。v4 经三轮评审([review-rust-design-2026-08-17-v4.md](review-rust-design-2026-08-17-v4.md)、[review-rust-design-2026-08-17-v4-codex.md](review-rust-design-2026-08-17-v4-codex.md)、[review-rust-design-2026-08-17-v4-zcode.md](review-rust-design-2026-08-17-v4-zcode.md))与两轮决议修订([review-rust-design-2026-08-17-v4-resolution.md](review-rust-design-2026-08-17-v4-resolution.md)),修订落实后经 §九 放行条件核对方可改回定稿。
> **历史**:v1→v3 一比一路线与双模型两轮审阅存档于 [review-rust-design-2026-08-17-sol.md](review-rust-design-2026-08-17-sol.md)、[review-rust-design-2026-08-17-deepseek.md](review-rust-design-2026-08-17-deepseek.md)、[review-rust-design-2026-08-17-round2-sol.md](review-rust-design-2026-08-17-round2-sol.md)、[review-rust-design-2026-08-17-round2-deepseek.md](review-rust-design-2026-08-17-round2-deepseek.md);v3 语义考据(契约行号锚点)仍是参考资产。
> **生态背书**:三份调研([research-rust-async-2026-08-17.md](research-rust-async-2026-08-17.md)、[research-rust-ecosystem-2026-08-17.md](research-rust-ecosystem-2026-08-17.md)、[research-hot-reload-2026-08-17.md](research-hot-reload-2026-08-17.md))——async/tokio 官方文档 + Bevy/tower/shaku/Tauri 先例 + 热更新案例(生命周期级先例 = Erlang/OSGi;代码级 dylib/subsecond 为正交互补)。

## 〇、v4 → v5 变更摘要

| v4 的税 | v5 处置 | 决议 |
|---|---|---|
| `ListenerResult`/`Value<E>` 未定义,全异步与同步 bail/emit 打架 | 监听器统一 helper trait `call<'a>` 返回 `BoxFuture<'a>`;`Event::Value` 关联类型;**bail 删除,四分发** | D16 |
| waterfall `async fn -> BoxFuture` 双重 future,`on()` 注册不了 waterfall 监听器 | 独立 `on_waterfall` 注册面 + `WaterfallListener` trait;`waterfall` 改普通 `fn -> BoxFuture`;`next` 为调用方兜底续延 | D17 |
| Effect 两个 Disposer 变体返回 `()`,错误无处可去 | 均返回 `Result<(), CordisError>`;闭包捕获 owned `Ctx`,不传 `&Ctx` | D18 |
| config 凭空消失(apply/plugin/update 均不收) | config 烘进插件实例(构造时自持),`Plugin::validate` 校验自持 config;`update` 推迟 M4 | D19 |
| `PluginFailed(Box<dyn Error>)` 与 `Arc<CordisError>` identity 冲突 | 分层:identity 归 `TransitionTask`(缓存 `Arc<CordisError>`),包装归 `PluginFailed`(`#[source] Box<dyn Error>`),不递归 | D25 |
| panic 策略空白 | 写明任务边界:spawn 必须观察 JoinHandle(JoinSet 收),panic 经 JoinError 路由 ErrorSink;async disposer panic 在 task 边界 catch 包 `PluginFailed` | D30 |
| 反向索引仅 `HashMap<TypeKey, Vec<PluginId>>`,isolate 串线 + 粒度太粗 | 反向边 = `(PluginId, generation, ServiceKey)` 三元组 | D21 |
| `isolate(label)` 无服务名参数,与 TS 粒度不符 | `isolate(key: ServiceKey, label)` 按 ServiceKey 隔离,同 label 合并 | D21 |
| 环检测承诺无法兑现(服务动态注册,A/B 互依双双 Pending) | 删除 cycle detection;`InjectUnsatisfied` 仅"unsatisfied";M4 可加 `provides()` manifest | D22 |
| fiber 状态无法观察中间态(watch last-value 跳态) | 类型化 `FiberStatusChanged` 事件走自家总线;锁内 FIFO 入队、锁外分发;保证提交顺序不保证 listener 完成顺序 | D24 |
| CancellationToken 选了但没暴露给插件 | 每 fiber generation 独立 child token,`ctx.cancelled().await` 暴露;卸载五步顺序写明 | D27 |
| 资源所有权规则空白(listener/service 是否自动归 fiber) | 经 `Ctx` 注册的自动归 fiber;`Disposer` 仅提前释放,不放回 `Effect` | D28 |
| 事件跨 isolate 可见性未表态 | 裁决:事件不过滤,isolate 仅隔离注册表;作用域事件用户自建子总线 | D29 |
| `ctx.effect()` 忘写 | 补入 `Ctx` 草案;增量式(async-iterable)effect 列入"刻意不保真" | D23 |
| thiserror 未入依赖清单,serde 在核心 crate | thiserror 普通依赖;serde/serde_json 移 agent 示例 crate | §八 |

## 一、范式定义(实现目标,五支柱)

1. **插件 = 装配单元**:一次 `apply`,可提供 0..n 服务、注册 0..n 监听、交回清理(effect)。
2. **fiber = 生命周期容器**:状态机(Pending/Loading/Active/Failed/Disposed/Unloading)+ 依赖门控(声明的依赖全部就绪才启动,含 `check()` 谓词)+ 级联卸载 + 恰好一次清理。
3. **服务 = 类型键注册表 + 作用域**:`TypeId` 主键 + isolate 按 ServiceKey 隔离(同类型跨作用域可并存;同接口多实例用显式 key,shaku Keyed 模式);服务读取沿 `Ctx` 父链回溯。
4. **事件总线 = 四分发语义**:emit(触发即忘,fire-and-forget)/ parallel(并发全等,聚合错误)/ serial(顺序至短路)/ waterfall(中间件续延,可 veto)。
5. **依赖驱动重载**:provider 卸载 → 声明依赖它的 fiber 被驱逐并自动重载;重载经反向依赖索引 `(PluginId, generation, ServiceKey)`,驱逐顺序 = 消费者并发排干、provider 最后。

**不属范式(不做)**:Proxy 属性语法、traceable/caller-shadow、JS 对象/callback 引用身份、运行时字符串事件、`internal/*` 扩展点面(TS 等价面)、同步 bail、`intercept` 服务配置拦截、`Context.filter` 事件过滤、增量式(async-iterable)effect。

## 二、核心 API 草案

### 所有权模型(先读)

- `Ctx` = `Arc<CtxInner>`,`Clone` 廉价;`isolate`/`plugin` 均返回新 `Ctx` 共享内核。
- `Plugin` 注册后存 `Arc<dyn Plugin>`(config 已烘进实例,见 D19)。
- `Disposer`/`Effect` 闭包捕获 owned `Ctx`(`Arc` 克隆),不借用调用栈。
- 经某 Fiber 的 `Ctx` 注册的 listener/service/child plugin **自动归该 Fiber 所有**;`Disposer` 仅提前释放,不放回 `Effect`(D28)。

### 类型与错误

```rust
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum CordisError {
    #[error("service {0:?} not found in scope")] ServiceNotFound(String),
    #[error("plugin failed")] PluginFailed(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("multiple errors: {errors:?}")] Aggregate { errors: Vec<CordisError> },  // 不压平
    #[error("fiber disposed")] InactiveEffect,
    #[error("config validation failed: {issues:?}")] Validation { issues: Vec<String> },
    #[error("dependency unsatisfied: {0:?}")] InjectUnsatisfied(Vec<String>),  // 无 cycle 承诺
}
```

### 事件:类型化载荷 + 回调注册表

```rust
pub trait Event: Send + Sync + 'static {
    const NAME: &'static str;        // 仅日志诊断,不参与唯一性与分发
    type Value: Send + 'static;      // serial 短路值类型
}

// 监听器:helper trait 解决生命周期(alias 里 BoxFuture<'_> 的 '_ 无输入可推导,E0106)
trait Listener<E: Event>: Send + Sync + 'static {
    fn call<'a>(&'a self, ctx: &'a Ctx, e: &'a E)
        -> BoxFuture<'a, Result<Option<E::Value>, CordisError>>;
}
// blanket impl for Fn(&Ctx, &E) -> BoxFuture<'_, Result<Option<E::Value>, CordisError>>

trait WaterfallListener<E: Event>: Send + Sync + 'static {
    fn call<'a>(&'a self, ctx: &'a Ctx, e: &'a E, next: Next<'a>)
        -> BoxFuture<'a, Result<E::Value, CordisError>>;
}
// Next<'a> = &'a mut dyn FnMut(&'a Ctx, E) -> BoxFuture<'a, Result<E::Value, CordisError>>
// (v5 冻结前最小骨架过 cargo check;形状细节 M1 定稿)

pub struct EventBus { /* 内部: HashMap<TypeId, Vec<Hook>>;Arc<Inner> + Mutex */ }
impl EventBus {
    pub fn on<E: Event>(&self, ctx: &Ctx, l: impl Listener<E>) -> Result<Disposer, CordisError>;
    pub fn on_waterfall<E: Event>(&self, ctx: &Ctx, l: impl WaterfallListener<E>) -> Result<Disposer, CordisError>;
    pub fn once<E: Event>(&self, ctx: &Ctx, l: impl Listener<E>) -> Result<Disposer, CordisError>;
    // prepend 选项经 EventOptions { prepend: bool }

    pub fn emit<E: Event>(&self, ctx: &Ctx, e: Arc<E>);                 // fire-and-forget;spawn 观察 JoinHandle
    pub async fn parallel<E: Event>(&self, ctx: &Ctx, e: Arc<E>) -> Result<(), CordisError>;  // JoinSet,聚合全部错误
    pub async fn serial<E: Event>(&self, ctx: &Ctx, e: &E) -> Result<Option<E::Value>, CordisError>;  // 顺序至短路
    pub fn waterfall<E: Event>(&self, ctx: &Ctx, e: &E, next: Next<'_>) -> BoxFuture<'_, Result<E::Value, CordisError>>;
}

pub type ErrorSink = Arc<dyn Fn(CordisError) + Send + Sync>;  // 默认接 logger
```

### 插件:dyn 兼容,config 烘进实例

```rust
pub trait Plugin: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn injects(&self) -> &[TypeKey] { &[] }              // 依赖门控声明
    fn validate(&self) -> Result<(), CordisError> { Ok(()) }  // 校验自持有 config
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>>;
}

pub enum Effect {
    Done,
    Disposer(Box<dyn FnOnce() -> Result<(), CordisError> + Send>),
    AsyncDisposer(Box<dyn FnOnce() -> BoxFuture<'static, Result<(), CordisError>> + Send>),
    Many(Vec<Effect>),
}
```

### 服务注册表:TypeId 主键 + 显式 key 多实例

```rust
// ServiceKey = TypeKey(TypeId + qualifier)
impl Ctx {
    pub fn provide<T: Send + Sync + 'static>(&self, value: T) -> Result<Disposer, CordisError>;
    pub fn provide_as<T: ?Sized + Send + Sync + 'static>(&self, key: TypeKey, value: Arc<T>) -> Result<Disposer, CordisError>;
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>>;   // 沿父链回溯,校验 isolate
    pub fn effect(&self, f: impl FnOnce() -> Effect) -> Result<Disposer, CordisError>;  // D23
    pub fn plugin(&self, p: impl Plugin) -> FiberView;               // FiberView.id: PluginId
    pub fn isolate(&self, key: ServiceKey, label: &str) -> Ctx;      // 按 ServiceKey 隔离
    pub fn cancelled(&self) -> impl Future<Output = ()>;             // D27:CancellationToken 暴露
}
```

## 三、关键设计决策(逐条带依据)

| # | 决策 | 依据(调研/先例/评审) |
|---|---|---|
| D1 | `BoxFuture<'a,T>` 手写别名;所有需 dyn 的 trait 用它 | Rust Reference dyn-compat;async-trait 宏展开即此形状;显式 ABI 优于宏 |
| D2 | 事件载荷类型化(`Event` trait + `Listener` helper trait),内部 `Arc<dyn Any + Send + Sync>` 擦除;serde_json::Value 仅序列化边界(agent crate) | bevy cheatbook/bevy#1431;qubit-event-bus |
| D3 | 回调注册表式总线(钩子语义):owned `Arc<dyn Listener>`;不引入 channel | users.rust-lang EventBus 共识;Tauri listen/emit;钩子=推式即时,channel=拉式缓冲 |
| D4 | 四模式:emit fire-and-forget(spawn 观察 JoinHandle,panic/Err 路由 ErrorSink);parallel 聚合 `Aggregate{errors}`(JoinSet);serial 顺序至短路;waterfall = CPS(`Next<'_>`),veto=不调 next;全异步 | tower Service 先证;resolution D16(删 bail:bail 与 serial 短路语义本就相同,同步性仅服务已删的 `internal/listener`) |
| D5 | fiber 状态机:`transition: Mutex<Transition>` 单锁域内改状态+读 token;**锁内只做 FIFO 入队,绝不持锁跨 await,回调/锁外执行** | tokio shared-state 官方(短临界区 std Mutex);std RwLock 死锁示例→快照后释放;resolution D24 修正 |
| D6 | 恰好一次 = 锁内单 `Arc<TransitionTask>`(内嵌 generation);同代 join 同一 Arc,跨代新建;watch 仅观察(晚订阅先 `borrow()` 再 `changed().await` 循环) | codex 评审:watch last-value 证明不了同代只执行一次;resolution D20 最小修法(不建三件套) |
| D7 | 协作取消统一 `CancellationToken`:每 fiber generation 独立 child token,`ctx.cancelled().await` 暴露;卸载五步:①标 unloading → ②cancel 当前代 → ③等 apply 退出 → ④EffectRecord 清理 → ⑤发布终态;写明协作取消限制(不观察 token 则 dispose 无限等待) | tokio graceful-shutdown 官方;resolution D27 |
| D8 | runtime 接入:构造注入 `Handle` 优先;自动路径 `Handle::try_current()` → 明确错误;绝不隐式建 runtime | tokio Handle 文档 |
| D9 | yield_now 仅公平性 hint;stale/epoch 检查只靠锁内 token 比较,无调度顺序假设;listener 完成顺序不作契约 | tokio yield_now 文档;resolution D24 |
| D10 | 插件身份 = `FiberView.id: PluginId` + `Plugin::name()` 显示名;`is_unique` 式去重可选 | Bevy Plugin trait(name/is_unique) |
| D11 | 错误:thiserror 派生 `CordisError`(**普通依赖**),`Error + Send + Sync + 'static`,**不 Clone**;共享 `Arc`;聚合 `Vec` | API guidelines C-GOOD-ERR;thiserror 文档 |
| D12 | 配置校验:`Plugin::validate` 校验自持有 config,validate-before-store;不引 schema 库 | 范式自足;resolution D19 |
| D13 | 服务定位器 `get::<T>() -> Option<Arc<T>>`:显式、类型键、Option 失败;沿父链回溯 | Bevy `Res<T>`/Tauri `state::<T>()`;resolution §七清单 |
| D14 | 依赖门控重载:反向边 `(PluginId, generation, ServiceKey)`;provider 卸载 → 驱逐该三元组精确匹配的消费者(并发排干,provider 最后,保清理期自访问) | Erlang/OSGi 范式先例;resolution D21 修正(isolate 串线 + 单服务粒度) |
| D15 | tower 兼容 = 可选适配层(不进核心);`Service`+`Layer` 面向请求路径,生命周期钩子不硬套 | tokio blog Inventing the Service trait |
| D16 | 事件系统全异步化,删 bail,四分发;监听器 helper trait `call<'a>`;`Event::Value` 关联类型;emit/parallel 载荷 `Arc<E>`,spawn 任务持 owned `Ctx` + `Arc<E>` | resolution D16;zcode/codex 二轮(helper trait 解 E0106,JoinSet `'static` 要求) |
| D17 | waterfall 独立 `on_waterfall` 注册面 + `WaterfallListener` trait;`waterfall` 普通 `fn -> BoxFuture`;`next` 为调用方兜底续延 | resolution D17;zcode 评审(注册面缺失是缺口非开放问题) |
| D18 | Effect 两个 Disposer 变体返回 `Result<(), CordisError>`;闭包捕获 owned `Ctx`,不传 `&Ctx` | resolution D18;codex/zcode 评审(清理错误无处可去) |
| D19 | config 烘进实例:`Plugin` 无关联类型,具体插件 `new(config)` 自持,`validate` 校验自持;注册表只存 `Arc<dyn Plugin>` 不碰 `Any`;`update` 推迟 M4 | resolution D19;codex 最小路线 |
| D20 | 恰好一次 = 锁内单 `Arc<TransitionTask>`;watch 纯观察 | 见 D6 |
| D21 | 消费者判定 = `(PluginId, generation, ServiceKey)` 三元组精确匹配;`isolate(key: ServiceKey, label)` 按 ServiceKey 隔离,同 label 合并,子作用域回退父。简化后(§八 24)不建反向索引:provider 摘除时按声明注入该键、且 `last_deps` 含三元组的 fiber 判定消费者 | 见 D14;resolution D21;simplification S5 |
| D22 | 删除环检测承诺;缺依赖保持 Pending;`InjectUnsatisfied` 无 cycle;M4 可加 `provides()` manifest | codex 评审(服务动态注册,环无法证明) |
| D23 | `Ctx::effect()` 补入草案;增量式(async-iterable)effect 刻意不保真 | resolution D23;zcode 评审(支柱 1 依赖) |
| D24 | `FiberStatusChanged` 事件:锁内 FIFO 入队、锁外分发;保证提交顺序(generation 作 sequence),不保证 listener 完成顺序 | resolution D24;zcode+用户评审修正 |
| D25 | 错误分层:identity 归 `TransitionTask`(缓存 `Arc<CordisError>`),包装归 `PluginFailed`(`#[source] Box<dyn Error>`),不递归 | resolution D25 二轮修正;codex/zcode 评审 |
| D26 | (并入 D27) | — |
| D27 | CancellationToken 暴露 + 卸载五步 + 协作取消限制 | 见 D7 |
| D28 | 资源所有权:经 `Ctx` 注册的自动归 fiber;`Disposer` 仅提前释放,不放回 `Effect`;isolate 出的 `Ctx` 保留原 fiber 所有权 | resolution D28;codex 评审;TS [reflect.ts:277-304](../src/reflect.ts#L277) |
| D29 | 事件跨 isolate 不过滤;isolate 仅隔离注册表;作用域事件用户自建子总线 | resolution D29;TS `Context.filter` 刻意不保真 |
| D30 | panic 任务边界:spawn 必须观察 JoinHandle(JoinSet 收);listener panic 经 JoinError 路由 ErrorSink;async disposer panic 在 task 边界 catch 包 `PluginFailed` | resolution D30;codex 二轮 |
| D31 | emit 派发序(2026-08-19 增补,修订 D4 的逐事件 spawn):fire-and-forget 不变,**同事件类型按发射序串行派发**——尾链:单次持锁内"取上一派发任务句柄 → spawn 新任务 → 存尾"(必须原子,两段锁在并发同类型 emit 下分叉链,实测 4 任务并发监听器);任务内先 await 上一任务、再按注册序逐个 await 监听器。代价:同事件多监听器由并发改串行(投递序所迫);panic 经 CatchUnwind 不断链,监听器内重入 emit 同类型排链尾不死锁;**跨事件类型不保证顺序**(已知边界,消费方需全序时按载荷自序);监听器完成顺序仍不作契约(D9) | 对齐 cordis/dsh emit 的同步顺序性(JS 单线程免费;dsh `core/agent/src/dispatch.ts` 快照 + 逐个 contained 调用);Rust 直译逐事件 spawn 丢失该保证:多线程背靠背 emit 实测错位 ~30%、位移最大 52(单线程与 ≥1ms 间隔零乱序);回归钉死于 `rutis/tests/dispatch_chain_probe.rs`(并发不分叉)与 `rutis-agent/tests/order_probe.rs`(单发射者序) |

**依赖**:tokio(rt-multi-thread, sync, macros)+ tokio-util(CancellationToken)+ thiserror(普通依赖)。**不引 futures-util**(parallel 用 `tokio::task::JoinSet`)、不引 async-trait。**serde/serde_json 仅 agent 示例 crate**,不进核心。

## 四、与 TS 版的关系(偏差清单,逐条表态)

语义考据照 v3(行号锚点可信)。以下为逐条裁决,不再笼统宣称"v4 全保留":

| TS 能力 | 裁决 | 落点 |
|---|---|---|
| `bail` 同步短路([events.ts:228-233](../src/events.ts#L228)) | 删,语义并入 serial(D16) | — |
| `internal/*` 事件面([events.ts:340-362](../src/events.ts#L340)) | 刻意不保真 | — |
| `check()` 谓词门控([fiber.ts:689-701](../src/fiber.ts#L689)) | 保留 | M2 |
| `on` 的 prepend 选项、监听器顺序控制([events.ts:114-119](../src/events.ts#L114)) | 保留(影响 serial/waterfall 结果) | M1 |
| `once`([events.ts:323-329](../src/events.ts#L323)) | 保留 | M1 |
| `intercept` 服务配置拦截([service.ts:86-102](../src/service.ts#L86)) | 刻意不保真(随 Proxy 面删) | — |
| 沿 fiber 父链的服务查找([reflect.ts:154-166](../src/reflect.ts#L154)) | 保留,`Ctx` 父链回溯 | M2 |
| `Context.filter` 事件过滤([events.ts:170-179](../src/events.ts#L170)) | 刻意不保真(D29) | — |
| `update` 双分支([fiber.ts:857-886](../src/fiber.ts#L857)) | 保留语义 | M4 |
| 增量式(async-iterable)effect([fiber.ts:397-409](../src/fiber.ts#L397)) | 刻意不保真 | — |
| 同步 generator effect(`yield` 逐个交回清理) | 刻意不保真:Rust 无 generator 语法,用 `Effect::Many`/多次 `ctx.effect()` 表达;LIFO/恰好一次语义内核保留 | — |
| "卸载 = 注册表摘除,消费者缓存的 `Arc<T>` 保实例存活"([reflect.ts:297-303](../src/reflect.ts#L297)) | 写明语义,reload 组测试断言依据 | M2 |
| 驱逐排干顺序(消费者并发、provider 最后)([reflect.ts:299-336](../src/reflect.ts#L299)) | 写明精确契约 | M2 |
| 重入(卸载中 provide/effect)报 `INACTIVE_EFFECT`([fiber.ts:434-436](../src/fiber.ts#L434)) | 保留 | M2 |
| EffectRecord LIFO([fiber.ts:508](../src/fiber.ts#L508))、聚合不压平([125-130](../src/fiber.ts#L125))、恰好一次([548-552](../src/fiber.ts#L548)) | 保留(D18/D20) | M2 |
| 六态状态机([fiber.ts:160-167](../src/fiber.ts#L160)) | 保留 | M2 |

TS 96 spec 降级为灵感来源;其中纯范式部分(fiber/dispose/reentrant)可选择性参考移植断言。

## 五、范式契约测试(验收,自证)

| 组 | 契约 | 代表测试 |
|---|---|---|
| assembly(支柱 1) | 0..n 服务/监听/清理装配;`effect` 交回 Disposer;注册自动归 fiber | `plugin_provides_n_services`、`plugin_registers_n_listeners`、`effect_yields_disposer`、`auto_ownership` |
| lifecycle | 状态机全迁移;init 失败→Failed;dispose 幂等;root dispose 清子树后可重启;并发 dispose join 同一 `Arc<TransitionTask>`;跨代隔离;`Loading→Unloading→Loading` 不丢唤醒 | `state_transitions`、`init_failure_marks_failed`、`dispose_idempotent`、`root_restart`、`concurrent_dispose_join`、`cross_generation_isolation` |
| gating | 依赖未齐→Pending;后到 provider 激活(顺序无关);`check()` 谓词失败驱逐;缺依赖长期 Pending 不报错 | `waits_for_dependency`、`late_provider_activates`、`check_evicts`、`pending_not_failed` |
| dispose | record 内 LIFO 串行;单错原样/多错聚合不压平;恰好一次(join 同一 `Arc<E>`);清理中再 dispose 不死锁;清理中注册新 effect 报 `INACTIVE_EFFECT` | `lifo_serial`、`aggregate_no_flatten`、`exactly_once_same_error`、`dispose_during_dispose`、`effect_during_cleanup_fails` |
| events | emit fire-and-forget(异步安全读载荷,panic/Err 进 ErrorSink);parallel 全错误聚合;serial 顺序至短路;waterfall veto/包裹/最外层返回;prepend 顺序;once;监听器卸载与 dispatch 竞争确定 | `emit_async_safe`、`emit_error_sink`、`parallel_aggregates`、`serial_bails`、`waterfall_veto_around`、`prepend_order`、`once_once`、`listener_unload_race` |
| registry | TypeId 注册/取回/重复错;显式 key 多实例;isolate 按 ServiceKey 隔离,同 label 合并;父链回溯 | `typed_roundtrip`、`keyed_multi_instance`、`isolate_scoping`、`parent_chain_lookup` |
| reload | 门控重载顺序(消费者并发、provider 最后);清理期自访问有效;重入报错;isolate A 卸载不驱逐 B;单服务卸载不连带 | `eviction_order`、`self_access_during_cleanup`、`reentrant_provide_fails`、`isolate_no_cross_evict`、`single_service_evict` |
| cancel | CancellationToken 停止 agent;fiber 卸载级联唤醒等待者;loading 中 cancel 唤醒观察 token 的插件;多次 cancel 幂等;父 cancel 传播子 | `agent_stop`、`cancel_wakes_awaiters`、`cancel_during_loading`、`cancel_idempotent`、`cancel_cascades` |
| agent(M3) | python examples 11 项范式语义 | `agent_*` 系列 |

**验收**:≥30,目标 40;每个范式支柱至少 4 个正例 + 2 个失败模式。

## 六、里程碑

- **M1**:类型系统 + EventBus(四模式 + `on_waterfall` + prepend/once)+ ErrorSink + Handle + panic 任务边界 → events 组。
- **M2**:Ctx/Registry(TypeId+key+isolate)+ Fiber(状态机/门控/EffectRecord/TransitionTask/watch 观察)+ `Ctx::effect` + `ctx.cancelled` + 依赖重载 → assembly/lifecycle/gating/dispose/registry/reload/cancel 组。
- **M3**:agent 示例 crate(CancellationToken stop、LLM trait、ToolSpec、事件观察;serde/serde_json 在本 crate)→ agent 组;可运行 demo。
- **M4(可选)**:tower 适配层、`PluginFactory<Config>` + `FiberView::update`、`provides()` manifest 环诊断、多实例命名作用域进阶、状态迁移式热更(Erlang code_change 对应物);与代码级热更工具(subsecond/hot-lib-reloader)的组合点:它们供新代码,本框架负责安全换件(dispose→驱逐→装配→重载),正交不耦合。

## 七、开放问题

1. `Event::NAME` 常量保留字符串诊断用途(仅日志)——已裁决保留,不参与分发;此处仅记录 M1 实现时是否加 `#[allow(dead_code)]` 一类的细节。
2. 同类型多实例的 key 类型:`&'static str` vs newtype `Key<T>`——倾向后者(shaku Keyed 精神),M2 定。
3. `FiberView` 的 `restart` 是否带 config 重注入(M4 `update` 前置形态)——M4 定。

## 八、实现记录

**2026-08-17 实现完成**(M1+M2+M3,M4 未动)。代码:`rust/` workspace。简化复盘(候选清单与执行计划)见 [simplification-rust-impl-2026-08-18.md](simplification-rust-impl-2026-08-18.md)。

- **crate**:`rutis`(核心,crates/rutis,~2100 行)、`rutis-agent`(示例应用,含 demo/tui)。
- **依赖**:tokio 1.53.1(rt-multi-thread/sync/macros/time)、tokio-util 0.7.19、thiserror 2.0.20;agent crate 另加 serde/serde_json(边界,§三)。未引 futures-util/async-trait ✓。
- **测试**:核心契约 58(§五 全组 + 评审补充 + 简化批次;单线程与并行模式均绿)+ agent 13 + doc 1 = **72**;`cargo run -p rutis-agent --example demo` 可运行;`cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --all -- --check` 清零。
- **实现期决策与偏差**(均为评审决议框架内的落定):
  1. `Aggregate { errors: Vec<Arc<CordisError>> }`、`ErrorSink = Fn(Arc<CordisError>)`:D11(不 Clone)与 D25(identity 缓存 Arc)的推论,成员保持 identity。
  2. 新增 `CordisError::ServiceExists`(同键同作用域重复注册);`InjectUnsatisfied` 无 cycle 字样 ✓(D22)。
  3. `parallel` 载荷 `Arc<E>`(JoinSet `'static` 要求);`serial` 内联顺序 await、载荷 `&E`(对齐 §二 草案签名;评审往返后定稿:实现本就是逐个 spawn+await 的注册序串行,JoinSet 写法有"看起来并发"的误读危害,已重写为内联 + CatchUnwind panic 遏制)。
  4. `Next<'a, E>` 定稿:类型化零参 `call()`(TS waterfall 语义:载荷固定,值经返回值向上流动);waterfall 内联 CPS,监听器 panic 向分发者传播(借用调用栈,无法 spawn)。
  5. 监听器闭包 HRTB 返回型推断受限:blanket impl 覆盖 `Fn` 路线,惯用写法为 `fn` 项或小结构体(测试两者均有示范)。
  6. `parallel` 单错原样、多错聚合(crate 一致语义;TS 恒聚合为偏差,已声明)。
  7. watch 保活:`FiberInner` 持有常驻 receiver——无 receiver 时 tokio watch 通道视为关闭,`send` 静默失败(实现期实锤的坑)。
  8. settle(`FiberView` 的 `IntoFuture`)语义:已解析 + 非 Loading/Unloading + **无在途意图**(`intents_inflight` 计数)——防"resolved Pending + 未处理的再装载通知"竞态(demo 场景实锤)。
  9. 卸载五步的落位:意图串行驱动;dispose/restart/驱逐在**入队前预取消当前代 token**(驱动串行,不预取消则运行中的 apply 等不到第②步);apply 不被中止,协作退出后驱动继续(D7 ③"等 apply 退出"忠实落法)。每代 token 为新建(非 child 派生),load 换新。
  10. root 驱动常驻(root 可重启;应用生命周期资源);非 root 终态后驱动退出,句柄仍可 join 缓存终态。
  11. 实现期补充 API:`Ctx::get_as`/`provide_as_with_check`/`refresh`(check() 谓词重查触发)/`root_view`(root dispose/restart 入口);`FiberView::name`;`FiberStatusChanged{seq}` 携带序号。
  12. 提前释放单服务的驱逐:`mark_binding_removing`(严格解析立即失败)→ 驱逐 → 排干后 `finalize_binding`(清理期自访问保留,§四"卸载=注册表摘除"语义)。
  13. agent 工具错误回喂格式 `error: {e}`(python repr 的简化);`Snapshot.resolved` 字段为 settle 语义新增。
- **独立评审(2026-08-17,实现后核对)结论与处置**:一致性高,§八 13 条申报偏差全部核实属实;另发现 2 个未申报问题,已修复并补契约测试——
  14. (修复)root `dispose→restart→再 dispose` 曾复用陈旧终态任务(第二次为空操作);现 root 重启时清除 `terminal_task`(fiber.rs Restart 分支),`root_restart_dispose_cycle` 钉死。
  15. (修复)once 监听器曾被挪到分发队尾,破坏注册序(§四 顺序控制);现改为锁内原子认领(`fired: AtomicBool` swap)、**条目保位**,认领后的条目留驻注册表由 Disposer/卸载清理(TS 自摘除的功能等价物),`once_keeps_position` 钉死。
  16. (申报补遗)waterfall 第三参定名 `terminal: T`(公开 `Terminal` trait,D17"调用方兜底续延"的落定);`Next<'a, E>` 泛型化。apply 内联执行补 `CatchUnwind` poll 边界(D30 之外的必要补充,否则插件 panic 击穿驱动任务);token 为独立 `Mutex<CancellationToken>` 而非 D5 字面的"单锁域内"(状态迁移仍仅 transition 锁,已确认无序问题);`InjectUnsatisfied` 核心不构造(门控失败一律静默 Pending,D22),仅 agent 示例在绕过门控的防御路径使用。
- **实现评审([review-rust-impl-2026-08-17.md](review-rust-impl-2026-08-17.md))修复批次**:评审 10 个核心 bug 中 9 个确认属实并全部修复,1 个(serial 并发)经代码核实为误报(逐个 spawn+await 结构上即注册序串行)但补了对抗测试;agent 侧修 #13/#15,#12/#14 与 python 参考行为一致保持现状。本轮落定:
  17. **失败装载回滚**(#1,对齐 TS fiber.ts:749-779 失败路径):`fail_load` 经 UNLOADING 排干 apply 半注册资源再进 Failed;清理错误路由 ErrorSink,Failed 携带装载错误(装配原子性,支柱 1)。测试 `apply_failure_rolls_back`。
  18. **挂起族修复**(#2/#3/#4):`dispose()` 终态任务**调用即登记**(不依赖 future 首 poll),restart 检查 `terminal_task`/驱动存活即拒 `InactiveEffect`;`FiberInner.alive` 标志 + 驱动退出前 `drain_stale`(排干残留意图并完成其任务,含 alive 置位竞态的迟到投递);`EffectRecord` 清理移入独立 spawn 任务,所有调用者 join 共享终态——`Disposer::dispose()` 的 future 被 drop 不再卡 Draining(取消安全)。
  19. **panic 边界补齐**(#6/#7):validate/check() 是用户回调,catch_unwind 后分别转 Failed / 视为不就绪(TS fiber.ts:695-698 语义,记日志差异见 21);异步清理闭包的 `f()` 调用与 Future poll 双边界捕获;清理任务全程兜底,必定写回 Done(#11 闭合)。join/settle 的通道关闭从"伪装 Ok"改为 Err(#8)。
  20. **provide 原子化**(#9):插入为同步主操作,重复注册(`ServiceExists`)直接返调用方,不再"error sink + 假 Ok";Effect 只登记删除清理,登记失败的极窄竞态回滚插入。`plugin()` 在 parent 失活分支不再补发装载意图(#10)。
  21. 保持项与申报:check() panic 不记日志(TS 记, Registry 无 sink 依赖,行为等价为"不就绪");`stopped`/`steps` 跨运行粘滞、空 LLM 响应返回空串——与 python 参考一致,评审建议的按运行取消与 `InvalidResponse` 属加强项,留后续;`TransitionTask` 仍未内嵌 generation(D6 行为由 restart 清 terminal_task 覆盖,形状偏差申报);agent 工具执行移入任务边界,runner panic 转模型可见错误(#13);agent 事件测试断言改为内容与步序无关到达序(#15)。新增重载闭环测试:`provider_reload_reactivates`、`check_recovery_reactivates`、`parallel_waits_all`、`concurrent_provide_single_winner`、`disposer_drop_is_cancel_safe`、`evict_after_consumer_disposed_completes` 等。
  22. (终轮)serial 重写为内联顺序 await(载荷 `&E`,不 spawn;panic 经 CatchUnwind 转 `PluginFailed`):语义与此前逐个 spawn+await 完全一致(注册序短路由 `serial_register_order_adversarial` 双模式钉死),消除 JoinSet 写法的误读危害并回归 §二 草案签名;新增 `serial_panic_contained`。评审所指"按完成序短路"经代码与测试核实不成立——历次实现均为注册序。
- **简化批次([simplify-conclusion-2026-08-18.md](simplify-conclusion-2026-08-18.md) 及其修正案,2026-08-18 执行)**:五项正确性修复先行,随后三项收敛,全部 72 测试双模式全绿、clippy `-D warnings` 清零、fmt 基线建立。
  23. **正确性**:`Arc<Binding>` 共享身份(删手写 Clone,`removing` 置位即刻全员可见);`Ctx::effect` 登记原子化(状态检查与 effects 入队同临界区,factory 锁外;生命周期已越界则立即排干新 record 的清理并返 `InactiveEffect`);非终止卸载的清理错误路由 ErrorSink(不再静默吞);`JoinError` 区分 panic/取消(`into_panic` 在取消场景二次 panic,core/agent 全部改判);死 fiber 的 `cancellation_token` 返回预取消 token。emit 改单层 spawn(任务内 CatchUnwind + 尾部自路由)。
  24. **收敛(同一事实只说一遍)**:once 监听器锁内取出即删除——恰好一次由总线锁互斥直接保证,`fired` 原子与双份认领逻辑删除,快照保位;`Hook<C>` 泛型合一;**Settle 栅栏**——settle(`IntoFuture`)= 在 mailbox 里排一个 `Intent::Settle` 并 join,"稳定了吗"由 FIFO 顺序直接回答,替换五个旁路信号(`resolved`/`intents_inflight`/`notify_inflight_drained`/双重借阅/drain 的 yield 协议;错误只认 Failed,红线);`post_join` 发送后复查 alive 封退出竞态;**反向索引删除**——驱逐判定回归唯一事实源 `last_deps`(consumers_of:声明注入该键且 last_deps 含三元组),跨结构同步义务消失,见 D21 修订。
  25. 行数持平(fmt 重排与注释抵消),概念净减:一个跨结构存储系统、两个同步机制(fired/inflight)、一个特殊旗标(resolved)、一个手写 Clone、一套 watch 轮询 settle、一套 yield 排空协议。Snapshot 公共字段少 `resolved`。未删公开 API(`ServiceNotFound`/`Key<T>` 保留);Adapter 维持(Rust 类型系统必要形状)。新增测试:`restart_cleanup_errors_to_sink`、`mixed_concurrent_ops_complete`。
- **cordis spec 对拍批次([cordis-spec-parity-2026-08-18.md](cordis-spec-parity-2026-08-18.md) §五之二,2026-08-18 执行)**:新建 `tests/parity.rs`,原版 96 spec 中可对拍条目(§二全对拍 + §三内核)全部落地为 57 个测试,测试名沿用原 `it()` 标题、注释标原行号;fake timers 一律改确定性同步点。对拍暴露四个实现缺口并修复:
  26. **惯性锁 2**(fiber.spec:27):load 完成时依赖集已整体翻新且无缺失 → 就地采纳新集合,in-flight 装载直接完成进 ACTIVE 不换代重载。
  27. **驱逐窗口可替换**:摘除中的绑定允许被同键新 provide 替换(TS dispose 同步释放槽位语义);驱逐方按 `Arc` 身份 finalize(`finalize_binding_if`),不误伤新绑定。
  28. **TransitionTask 保活**(§八 7 同坑):持常驻 receiver,封"快速完成 + 晚订阅"的完成值丢失(对拍确定性复现的丢唤醒)。
  29. **依赖身份含作用域**:`last_deps`/驱逐判定从三元组扩为 `(PluginId, gen, TypeKey, scope)` 四元组——同 provider 在不同 isolate 作用域提供同键服务不再互为消费者;`get` 增访问方失活检查(失活 fiber 上下文访问服务不可见,provider 子树自访问豁免;TS inactive context 语义)。测试合计 129(57 对拍 + 58 契约 + 13 agent + 1 doc),双/单线程模式与连续多轮均绿,clippy/fmt 清零。

## 九、放行条件(v5 冻结前核对)

- [ ] 四种事件模式都有完整、可编译的监听器签名(helper trait 路线,无 `ListenerResult` 占位符);
- [ ] fire-and-forget 不持有非 `'static` 借用(载荷 `Arc<E>`),spawn 任务持 owned `Ctx` + `Arc<E>`;
- [ ] parallel/serial 的 JoinSet 任务 `'static` 成立;
- [ ] Effect 两个 Disposer 变体都返回 `Result<(), CordisError>`,闭包捕获 owned `Ctx`;
- [ ] 并发 dispose join 锁内同一 `Arc<TransitionTask>`,watch 仅观察;
- [ ] 反向依赖边含 `(PluginId, generation, ServiceKey)` 三元组;
- [ ] `isolate()` 签名含 ServiceKey,同 label 合并写明;
- [ ] `InjectUnsatisfied` 无 cycle 承诺;
- [ ] `Plugin` trait 含可选 `validate`(校验自持有 config),`FiberView::update` 标注 M4;
- [ ] `Ctx::effect()`、`ctx.cancelled()`、ErrorSink 定义、panic 任务边界全部入草案;
- [ ] `CordisError` 变体分层写明:identity 归 `TransitionTask`,包装归 `PluginFailed`(`#[source] Box<dyn Error>`),不递归;
- [ ] 资源所有权规则(D28)、事件跨 isolate 裁决(D29)、§四偏差清单全部入文档;
- [ ] bail 删,四分发,§一/§五/D4 同步改;
- [ ] 状态事件发布写明"锁内 FIFO 入队、锁外分发";顺序语义写明"保证提交顺序,不保证 listener 完成顺序",测试断言相应对齐;
- [ ] waterfall 公开签名在最小骨架过 `cargo check` 且可注册监听器(v5 冻结门槛);`Next` 形状细节允许 M1 定稿;
- [ ] 公开 API 骨架通过 `cargo check`(最小 crate,只含 trait/enum/type alias + 空实现),重点验证 helper trait 生命周期、`Arc<dyn Plugin>` 擦除、JoinSet `'static`。
