# design-rust-port.md v4 评审报告

> 2026-08-17。评审对象:[design-rust-port.md](design-rust-port.md)(v4,范式路线)。三路并行评审:Rust 技术正确性、TS 源语义保真、文档内部一致性,并核对 `src/events.ts`、`src/fiber.ts`、`src/context.ts`、`src/reflect.ts` 源码。
>
> **总结论**:范式路线本身成立,D5(std Mutex 短临界区)/D6(watch 终态)/D7(CancellationToken)/D9(yield_now 无顺序保证)/D14(Erlang/OSGi 先例)等关键决策均有调研真实支撑。问题集中在两处:§二 API 草案有 4 个阻断性缺陷(无法编译或无法据以实现),文档存在多处内部矛盾。M1 动手前需修订。

## 一、阻断性问题(M1 前必须解决)

### 1.1 `emit` 签名与 async 监听器矛盾(L52-53)

```rust
pub fn emit<E: Event>(&self, ctx: &Ctx, e: &E);  // 同步调用;async 变体 spawn+遏制
```

`tokio::spawn` 要求 `'static`,`&E`/`&Ctx` 借用进不了 task。载荷需要 `Arc<E>` 或 `E: Clone`。此外:

- `ListenerResult`(L52 `on` 的回调返回类型)全文未定义,§7 开放问题也未列。
- "同步调用 + async 变体"是同一方法两模式还是两个方法,全文无交代,§5 的 `emit_sync_async_contained`(L118)测试无法据此编写。

**修复**:载荷统一 `Arc<E>`(或 `E: Clone`);定义 `ListenerResult` 最小形状;明确 emit 与 async 变体的分野。

### 1.2 `waterfall` 是双重 future,无法编译(L57)

```rust
pub async fn waterfall<E: Event>(...) -> BoxFuture<'_, Result<Value<E>, CordisError>>;
```

`async fn` 已经包了一层 Future,再返回 `BoxFuture` 是双层嵌套。要么去掉 `async` 改 `fn -> BoxFuture`,要么去掉 `BoxFuture`。`Next<'_>` 形状未定义(应为 `&mut dyn FnMut(&Ctx, E) -> BoxFuture<'_, ...>` 一类)。与 D1/D4 "call→Future 形状"的表述也不符。

### 1.3 `bail` 同步与统一回调类型冲突(L52、L56)

bail 是同步 `fn`,只能驱动同步回调;但 `on` 注册的回调类型疑似可返回 future。TS 源码确认 bail 本就纯同步([events.ts:228-233](../src/events.ts#L228))。设计需明确"bail 仅限同步监听器"或改 async,二者选一并写明。

### 1.4 Plugin 实例归属与 Ctx 所有权模型缺失(L61-64、L78)

`ctx.plugin(p: impl Plugin)` 按值收,`restart`/`update` 要重复调 `apply`,内部必须存 `Arc<dyn Plugin>`,文档没写。`isolate(&self) -> Ctx`、`Effect::Disposer(Box<dyn FnOnce(&Ctx)>)` 的 `&Ctx` 来源,都隐含 `Ctx: Clone`(Arc 内核)——这个模型全文未定义,而它是所有签名的地基。

**修复**:§二补一节"所有权模型":`Ctx = Arc<CtxInner>`、`Plugin` 存 `Arc<dyn Plugin>`、Disposer 的 ctx 来源。

## 二、内部矛盾(文档自身打架)

| # | 位置 | 矛盾 |
|---|---|---|
| 2.1 | §0(L19)/D11(L97) vs §2(L37) | thiserror"不入公共 API",但公共类型 `CordisError` 直接 `#[derive(thiserror::Error)]`,且该类型出现在全部公共签名(L54/57/64/75)。L103"thiserror 可选"也无 feature 门控方案 |
| 2.2 | §四(L107) vs §7.3(L137) | "update 双分支"列为"v4 全保留"的范式不变量(源码确有,[fiber.ts:857](../src/fiber.ts#L857)),§7.3 却倾向砍到 M4 |
| 2.3 | §5(L124) vs §5 表 | 验收下限 5 支柱×(4 正例+2 失败)=30,落在"~35-45 个"区间外;reload 组代表测试仅 3 个,不足 4+2 |
| 2.4 | §6 M1(L128) vs §5 表 | M1 承诺交付 events/cancel 组,但 `cancel_wakes_awaiters`(L121)与 `listener_unloads_with_fiber`(L118)均依赖 M2 的 fiber,M1 跑不齐 |
| 2.5 | §0(L16) vs D5(L91) | §0 说"token 不可用序号",D5 的 token 却含"force 代"(代即序号);"等值合并"一说在三份调研中无出处 |
| 2.6 | D12(L98) vs §2(L61-65) | D12 说插件自带 `fn validate(config)`,但 `Plugin` trait 无此方法 |

## 三、与 TS 源码的事实偏差

### 3.1 声明与源码矛盾

1. **isolate 粒度不符**(L79):`Ctx::isolate(label)` 无服务名参数,按整个上下文隔离;TS 的 `isolate(name, label?)` 按单个服务名隔离,同 label 两次调用共享 scope([context.ts:123-127](../src/context.ts#L123))。语义不同且未标注偏差。
2. **"重入不崩"措辞掩盖源码行为**(L120):TS 中 UNLOADING 状态调 effect() 直接抛 `INACTIVE_EFFECT`([fiber.ts:434-436](../src/fiber.ts#L434)、[reflect.ts:278](../src/reflect.ts#L278)),是明确报错而非"不崩"。
3. **inject 门控漏掉 `check()` 谓词**(L107、D14):门控除了 impl 存在还有 `impl.check()` 谓词,失败即驱逐([fiber.ts:689-701](../src/fiber.ts#L689)、[service.ts:15](../src/service.ts#L15))。

### 3.2 漏掉的源码能力(去留未表态)

1. **分发的 this 维度**:TS 五种分发首参可为 thisArg,用于监听器绑定 + `Context.filter` 过滤([events.ts:170-179](../src/events.ts#L170)、[context.ts:46](../src/context.ts#L46))。"五分发语义"清单(L27)完全未提这一正交维度。
2. **`on` 的 prepend/global 选项与 once**([events.ts:114-119](../src/events.ts#L114)、[323-329](../src/events.ts#L323)):注册顺序控制影响 serial/bail/waterfall 结果,属分发语义一部分。
3. **`extend()`/`intercept()`**:原型继承子上下文与服务配置拦截合并([context.ts:101-147](../src/context.ts#L101)、[service.ts:86-102](../src/service.ts#L86));inject 对象形式携带 intercept config([registry.ts:19](../src/registry.ts#L19))。intercept 是依赖声明范式的一部分,"不做"清单未明确其去留。
4. **沿 fiber 父链的服务查找**([reflect.ts:154-166](../src/reflect.ts#L154)):读服务沿父链回溯并校验 isolate key,文档只有 TypeId 平铺注册表。

### 3.3 新发明但未标注的语义

1. **"卸载顺序正确"/`eviction_order`**(L28、L120):TS 的 notify 只收集受影响 fibers 后 `Promise.allSettled` 并发排干([reflect.ts:299-336](../src/reflect.ts#L299)),无驱逐顺序契约。文档两处声称顺序保证,均未标注为新语义。
2. **`exactly_once_same_error` 的 `Arc<E>` identity**:TS 靠 `while (this.inertia) await` join([fiber.ts:307-309](../src/fiber.ts#L307)),无"同一终态对象"契约。已在 D6 标注为强化,可接受。

### 3.4 已核实为真的声明

EffectRecord LIFO([fiber.ts:508](../src/fiber.ts#L508))、聚合不压平([fiber.ts:125-130](../src/fiber.ts#L125))、恰好一次([fiber.ts:548-552](../src/fiber.ts#L548))、六态([fiber.ts:160-167](../src/fiber.ts#L160))、五分发模式([events.ts:32](../src/events.ts#L32))、bail 同步、serial 返回首个 bail 值、waterfall veto、清理期自访问([reflect.ts:297-303](../src/reflect.ts#L297))。§四"行号锚点可信"基本属实。

## 四、需要澄清(不阻断但要写明)

1. **D6 watch join 协议不完整**(L92):判"已是终态"须先 `borrow()` 再 `changed().await`;fiber drop 后 sender 关闭,`changed` 返 Err 无法区分终态。"重复 dispose join 同一终态"要求 dispose 后 fiber 句柄仍存活,未写明。
2. **卸载语义**(L99):消费者缓存的 `Arc<T>` 使服务实例在"卸载"后仍存活,卸载仅为注册表摘除——这个语义要写清楚,否则 reload 组测试断言无依据。
3. **D14 驱逐竞态**(L100):消费者在锁外启动回调中被驱逐,单锁协议不够,复杂度被低估。
4. **`provide(T)` vs `provide_as(Arc<T>)` 不对称**(L75-76):前者无法注册 trait 对象,是否有意未说明。
5. **`on(&self)` 注册需内部可变性**(`Arc<Inner> + Mutex`),未声明;Disposer 回摘注册表的路径未定。
6. **`AsyncDisposer` 生命周期陷阱**(L69):返回 `BoxFuture<'static,()>` 但入参 `&Ctx`,用户把 `&Ctx` 捕获进 future 即编译失败。需文档化"disposer 内先克隆 owned handle"。
7. **serial 无测试**:events 组(L118)只覆盖 emit/parallel/waterfall/bail,支柱 4"五分发语义"缺 serial;支柱 1(0..n 装配)无独立测试组。
8. **依赖断言**:BoxFuture 手写别名确实无需 futures crate,但 parallel 聚合应注明用 `tokio::task::JoinSet` 替代 FuturesUnordered;L36 注释"= futures::future::BoxFuture"易误读为引依赖。
9. **ErrorSink** 仅在 D4(L90)、M1(L128)各出现一次,无定义、无 API、无对应测试组。
10. **serial/bail 返回 `Option<Value<E>>`**(L55-56):`Option` 的语义(无监听?veto?)全文无交代。
11. **M4"状态迁移式热更"**(L131):调研原文是"状态迁移留 M4 以后评估",设计提前进了 M4;与支柱五/M2"依赖重载"的边界(无状态迁移 vs 有状态迁移)未写明。

## 五、处置建议(按优先级)

1. **重写 §二事件签名**:统一载荷 `Arc<E>`(或 `E: Clone`),定义 `ListenerResult`/`Next`/`Disposer`/`Value<E>` 最小形状;`Value<E>` 从 §7 提升为 M1 阻塞项;修掉 waterfall 双重 future;明确 bail 同步边界。
2. **§二补"所有权模型"一节**:`Ctx = Arc<CtxInner>`、`Plugin` 存 `Arc<dyn Plugin>`、Disposer 的 ctx 来源。
3. **消除内部矛盾**:update 去留二选一(建议按 §7.3 砍到 M4,同步改 §四);错误 API 表述对齐 §二;验收数字改自洽(如"≥30,目标 40");M1 测试承诺去掉依赖 fiber 的两条。
4. **§四补"与 TS 的偏差清单"**:isolate 粒度、重入报错、eviction 顺序为新增契约、父链查找/intercept/check 谓词去留,逐条表态。
5. **D14 补驱逐协议竞态分析**:锁外回调中被驱逐的消费者的处置。

路线和生态依据不用动,问题集中在 API 草案的 Rust 生命周期细节和文档自洽性。
