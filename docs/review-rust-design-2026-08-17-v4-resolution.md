# design-rust-port.md v4 → v5 修订决议(v2)

> 2026-08-17 首版;2026-08-17 v2 合并 codex、zcode 第二轮评审。
> 评审来源:[review-rust-design-2026-08-17-v4.md](review-rust-design-2026-08-17-v4.md)(Dim)、[review-rust-design-2026-08-17-v4-codex.md](review-rust-design-2026-08-17-v4-codex.md)、[review-rust-design-2026-08-17-v4-zcode.md](review-rust-design-2026-08-17-v4-zcode.md),及 codex、zcode 对 v1 决议的二轮意见。
> 修订原则:**架构设计期最小冗余**——能用关联类型解决的不拆 trait,能用单 Arc 解决的不建多 struct,能删除的承诺不新增机制。
> 裁决:**v4 从"定稿"降级为"草案"**,以下修订完成后出 v5 再冻结。

## 一、阻断项(评审共识,照单全收)

1. **监听器类型未定义且自相矛盾**(三份命中):`ListenerResult`/`Value<E>` 全文未定义,"全异步 BoxFuture"(§〇)与"bail 同步短路/emit 同步调用"(§一、D4)打架。
2. **waterfall 签名无法编译且注册面缺失**(三份命中):`async fn -> BoxFuture` 双重 future;`on()` 的监听器形状没有 next 参数,waterfall 监听器无法注册。
3. **Effect 清理无法报错**(codex、zcode 命中):两个 Disposer 变体返回 `()`,与 §五"单错原样/多错聚合/重复 dispose join 同一 `Arc<E>`"契约直接冲突。
4. **config 凭空消失**(三份命中):`Plugin::apply`/`Ctx::plugin`/`FiberView::update` 均不收 config,但 D12 整条在讲 validate-before-store,`Validation` 变体存在,§四宣称 update 双分支保留。

## 二、事件系统(D16-D17)

### D16:事件系统全异步化,删 bail,四分发

- 监听器统一唯一形状,用 helper trait 解决生命周期(alias 里 `BoxFuture<'_>` 返回位置的 `'_` 无输入可推导,E0106;alias 路线禁用):

```rust
pub trait Event: Send + Sync + 'static {
    const NAME: &'static str;        // 仅日志诊断,不参与唯一性与分发
    type Value: Send + 'static;      // serial 短路值类型,顺带裁决开放问题 2
}

trait Listener<E: Event>: Send + Sync + 'static {
    fn call<'a>(&'a self, ctx: &'a Ctx, e: &'a E)
        -> BoxFuture<'a, Result<Option<E::Value>, CordisError>>;
}
// blanket impl for Fn(&Ctx, &E) -> BoxFuture<'_, ...>
```

- **bail 删除,serial 保留短路语义**,分发模式改四种:emit(fire-and-forget)/ parallel(并发全等,聚合错误)/ serial(顺序至短路)/ waterfall(中间件续延,可 veto)。TS 的 bail 与 serial 短路语义本就相同,唯一区别是同步性;bail 的同步消费者仅 `internal/listener`,已随 internal 面删除。§一、§五、D4 同步改"四分发"。
- **fire-and-forget(emit)载荷 `Arc<E>`**,spawn 的 task 持 owned 数据,不借用调用栈。
- **parallel/serial 的 JoinSet 任务持 `Ctx` 克隆 + `Arc<E>`**,spawn 前把借用落成 owned,任务满足 `'static`。
- 不拆 `Event`/`QueryEvent`/`WaterfallEvent` 三 trait,分发模式差异由调用方法表达,不进类型系统。

### D17:waterfall 独立注册面

- 新增 `on_waterfall::<E>()`,监听器形状:

```rust
trait WaterfallListener<E: Event>: Send + Sync + 'static {
    fn call<'a>(&'a self, ctx: &'a Ctx, e: &'a E, next: Next<'a>)
        -> BoxFuture<'a, Result<E::Value, CordisError>>;
}
```

- `Next<'a>` 为 `&'a mut dyn FnMut(&'a Ctx, E) -> BoxFuture<'a, Result<E::Value, CordisError>>` 一类。**v5 冻结前**:waterfall 公开签名必须在最小骨架中过 `cargo check` 且能注册监听器(borrowed `Next` 能编译即可采用);**M1 细化**:`Next` 的具体形状(borrowed vs owned)允许在实现时定稿。时间点统一:可编译是冻结门槛,形状细节是 M1 自由裁量。
- `waterfall` 方法签名修为普通 `fn -> BoxFuture`(去掉双重 future);`next` 是调用方提供的兜底续延,作为 waterfall 的终态参数,不与事件并列。

## 三、Effect 与错误(D18、D25、D30)

### D18:Effect 清理带错误返回

```rust
pub enum Effect {
    Done,
    Disposer(Box<dyn FnOnce() -> Result<(), CordisError> + Send>),
    AsyncDisposer(Box<dyn FnOnce() -> BoxFuture<'static, Result<(), CordisError>> + Send>),
    Many(Vec<Effect>),
}
```

清理闭包**捕获 owned `Ctx`(`Arc<CtxInner>` 克隆)**,入参不再传 `&Ctx`,消除 `'static` future 捕获借用的编译陷阱。

### D25 + 二轮修正:错误 identity 与错误包装分层

- **identity 由 `TransitionTask` 管**:transition 完成结果缓存为 `Arc<CordisError>`,多观察者 join 同一 Arc,`exactly_once_same_error` 测试断言这里。
- **包装由 `CordisError` 变体管**:`PluginFailed` 保留 `#[source] Box<dyn Error + Send + Sync>`(包 apply 外来的非 Cordis 错误);apply 自身返回的 `CordisError` 直接传播,不再递归包一层。`PluginFailed` 不递归装 `CordisError`。
- 非 Cordis 来源错误的统一兜底:disposer/listener  panic 或外来错误,在任务边界包成 `PluginFailed` 后进入聚合/ErrorSink。

### D30(新):panic 任务边界写明

- fire-and-forget 的 spawn **必须观察 JoinHandle**(收进 JoinSet 或 spawn 内转发),panic 经 `JoinError` 路由到 ErrorSink;直接丢弃 handle 则 panic 丢失,禁用。
- async disposer 轮询期 panic 在 task 边界 `catch_unwind`(或 JoinSet 的 JoinError 转换)包成 `PluginFailed`,与 Err 同路径聚合。
- listener panic 与 Err 同路径进 ErrorSink,对齐 TS 把 listener 抛错路由到 logger 的语义。

## 四、Plugin 与 config(D19)

### D19:config 烘进实例,擦除在构造期完成

- 裁决选**最小路线**(codex 路线):config 在插件构造时定型,不进 `Plugin` trait 关联类型。

```rust
// Plugin trait 无 type Config;config 由具体插件类型自己持有
pub trait Plugin: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn injects(&self) -> &[TypeKey] { &[] }
    fn validate(&self) -> Result<(), CordisError> { Ok(()) }  // 校验自持有 config
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>>;
}

// 注册仍按值收,内部 Arc 化
fn plugin(&self, p: impl Plugin) -> FiberView;
```

- 具体插件类型(如 `MyPlugin { config: MyConfig }`)在 `new(config)` 时收 config,`validate` 校验自持有的那份。注册表只存 `Arc<dyn Plugin>`,**不碰 `Any` 擦除**。
- `FiberView::update` 推迟 M4,届时设计 `PluginFactory<Config>` + config 擦除 + update 事务。§四"update 双分支"改标"保留语义,落点 M4"。

## 五、并发与索引(D20-D22 + 二轮修正)

### D20:恰好一次 = 锁内单 Arc,watch 退化为纯观察

- transition mutex 内保存 `Option<Arc<TransitionTask>>`,同代 dispose/restart join 同一个 `Arc`;跨代(靠 task 内嵌 generation 字段)新建。
- watch 只负责对外状态观察与诊断,不承担恰好一次;**不建** `FiberSnapshot`/`Transition` 独立 struct,generation 内嵌 `TransitionTask` 与 watch 载荷。
- 晚订阅者协议写明:先 `borrow()` 检查当前快照是否终态,再 `changed().await` 循环。

### D21:反向依赖索引挂 (PluginId, generation, ServiceKey)

- 消费者激活时记录实际绑定的 `(PluginId, generation, ServiceKey)`;反向索引键为同一三元组。**二轮修正**:仅 `(PluginId, generation)` 太粗——插件可提供 0..n 服务,提前 dispose 单个服务不应连带驱逐其他服务的消费者,`ServiceKey` 必须进键。
- provider 卸载时按三元组精确驱逐,天然隔离 isolate。
- **不建** `ServiceKey`/`BindingKey`/`ProviderId` 三类型:`ServiceKey` = `TypeKey`(TypeId + qualifier);`ProviderId` = `(PluginId, generation)` 元组。
- `isolate` 签名补 ServiceKey:`isolate(&self, key: ServiceKey, label: &str) -> Ctx`,粒度对齐 TS([context.ts:123-127](../src/context.ts#L123));同 label 两次调用合并作用域(TS 语义);子作用域回退父作用域。

### D22:依赖环检测降级

- 首版删除通用 cycle detection 承诺;缺依赖保持 Pending(合法状态,不报错);`CordisError::InjectUnsatisfied` 去掉 "cycle" 字样。
- M4 可加可选 `provides()` manifest 做静态诊断,不进首版。

### D24 修正:状态事件发布与顺序写明

- `FiberStatusChanged` 事件发布:**transition 锁内只做 FIFO 入队,锁外分发**(listener 回调在锁外执行)。锁内绝不调用户代码,与 D5"回调/锁外执行"一致;监听器在回调里重入注册表(`ctx.plugin()`)不死锁。
- **顺序语义写明**:保证事件**提交顺序**(入队 FIFO,watch 载荷携带 generation 作 sequence);**不保证**异步 listener 的**完成顺序**(spawn 调度顺序非契约,D9)。测试只断言提交顺序,不断言回调完成顺序。
- 不新建排序机制、不加发布队列之外的字段。

## 六、补漏(D23、D27-D29)

### D23:`Ctx::effect()` 补入草案

支柱 1"交回清理"、`effect_yields_disposer` 测试、监听器随 fiber 卸载全靠它。TS 的增量式(async-iterable)effect 明确列入"刻意不保真"清单。

### D27(新):CancellationToken 暴露给插件

- 每个 fiber generation 拥有独立 child token,经 `ctx.cancelled().await`(或 `ctx.cancellation_token()`)暴露给插件。
- 卸载五步顺序写明:① 标记 transition 为 unloading → ② cancel 当前 generation → ③ 等待正在执行的 apply 退出 → ④ 严格执行 EffectRecord 清理 → ⑤ 发布最终状态。
- 写明协作取消限制:插件从不观察 token 且不自行返回时 dispose 无限等待;首版不暗示能强制终止任意 Rust Future。

### D28(新):资源所有权规则写明

- 通过某 Fiber 的 `Ctx` 创建的 listener、service、child plugin **自动归该 Fiber 所有**;返回的 `Disposer` 仅表示提前释放,不需要也不应再放回 `Effect`。
- `Effect` 只负责未经 `Ctx` 注册的外部资源。
- isolate 出的 `Ctx` 保留原 Fiber 所有权。

### D29(新):事件跨 isolate 可见性表态

- TS 靠 `Context.filter` + thisArg 过滤([events.ts:170-179](../src/events.ts#L170))。v4 裁决:**事件分发不跨 isolate 过滤**,isolate 仅隔离服务注册表;事件总线全局唯一,监听器收到的是全局事件。如需作用域事件,用户自己用 `isolate` 建子总线(不拦)。此裁决写入"刻意不保真"清单。

## 七、§四偏差清单(新建,逐条表态)

| TS 能力 | 裁决 | 落点 |
|---|---|---|
| `bail` 同步短路 | 删(D16),语义并入 serial | — |
| `internal/*` 事件面 | 刻意不保真 | — |
| `check()` 谓词门控([fiber.ts:689-701](../src/fiber.ts#L689)) | 保留 | M2 |
| `on` 的 prepend 选项、监听器顺序控制 | 保留(影响 serial/waterfall 结果) | M1 |
| `once` | 保留 | M1 |
| `intercept` 服务配置拦截([service.ts:86-102](../src/service.ts#L86)) | 刻意不保真(随 Proxy 面一起删) | — |
| 沿 fiber 父链的服务查找([reflect.ts:154-166](../src/reflect.ts#L154)) | 保留,改为 `Ctx` 父链回溯 | M2 |
| `Context.filter` 事件过滤 | 刻意不保真(D29) | — |
| `update` 双分支 | 保留语义 | M4 |
| 增量式(async-iterable)effect | 刻意不保真 | — |
| "卸载 = 注册表摘除,消费者缓存的 `Arc<T>` 保实例存活" | 写明语义,reload 组测试断言依据 | M2 |
| 驱逐排干顺序(消费者并发、provider 最后) | 写明精确契约 | M2 |
| 重入(卸载中 provide/effect)报 `INACTIVE_EFFECT` | 保留([fiber.ts:434-436](../src/fiber.ts#L434)) | M2 |

## 八、文档自洽性修正(顺手清)

1. thiserror 是普通依赖(§二 37 行硬 derive),依赖清单补上;"不引 async-trait(thiserror 可选)"笔误改"不引 async-trait"。
2. `serde/serde_json` 移出核心 crate,进 agent 示例 crate。
3. "不引 futures-util"写明前提:parallel 用 `tokio::task::JoinSet`。
4. `provide<T>(value: T)` / `provide_as<T>(value: Arc<T>)` 的 Arc 不对称写明理由:前者值语义便捷入口,后者 trait 对象/共享实例入口;失败处理统一 `Result`(`on` 的裸 `Disposer` 改 `Result<Disposer>`)。
5. `PluginId` 明确为 `FiberView` 字段(`FiberView.id: PluginId`)。
6. ErrorSink 最小定义:`Arc<dyn Fn(CordisError) + Send + Sync>`,默认实现接 logger;入 M1 交付物。
7. §四"v4 全保留"措辞过头,改为按 §七清单逐条标注。
8. §五验收数字自洽:下限 5 支柱×(4+2)=30,表述改"≥30,目标 40";serial 补测试入 events 组;支柱 1(0..n 装配)补独立测试组;驱逐排干顺序、并发 dispose、跨代隔离等竞态测试补入 lifecycle/reload 组。
9. M1 承诺去掉依赖 fiber 的两条测试(`cancel_wakes_awaiters`、`listener_unloads_with_fiber` 移入 M2 验收)。
10. 开放问题 2(`Value<E>` 形状)已被 D16 裁决,从 §七删除;§七剩余条目重新编号。

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
- [ ] 状态事件发布写明"锁内 FIFO 入队、锁外分发";顺序语义写明"保证提交顺序,不保证 listener 完成顺序",测试断言相应对齐;
- [ ] waterfall 公开签名在最小骨架过 `cargo check` 且可注册监听器(v5 冻结门槛);`Next` 形状细节允许 M1 定稿;
- [ ] `CordisError` 变体分层写明:identity 归 `TransitionTask`,包装归 `PluginFailed`(`#[source] Box<dyn Error>`),不递归;
- [ ] 资源所有权规则(D28)、事件跨 isolate 裁决(D29)、§四偏差清单全部入文档;
- [ ] serial 与 bail 的差异已裁决(bail 删,四分发);
- [ ] 公开 API 骨架通过 `cargo check`(最小 crate,只含 trait/enum/type alias + 空实现),重点验证 helper trait 生命周期、`Arc<dyn Plugin>` 擦除、JoinSet `'static`。

满足全部条件后,v5 可从"草案"改回"定稿",M1 放行。
