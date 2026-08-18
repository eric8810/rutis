# Rust 移植设计 v4 评审 — ZCode/GLM-5.3(2026-08-17)

> 审阅对象:[design-rust-port.md](design-rust-port.md) v4(范式路线)。
> 基准:src/ TS 源码、三份调研文档、v2 两轮审阅存档。
> 裁决:**M1 暂不放行**。方向和骨架没问题,但 §二 API 草案有 4 个洞,照现在的稿子写不出能通过它自己定的验收测试(§五)的代码。修完这 4 点即可放行。

## 总评

先说好的:范式路线的裁决是对的。v2 审阅的三个阻断里,有两个(waterfall 双态、Value 载荷)确实是靠砍掉 `internal/*` 面和改类型化事件消解的,不是回避。三份调研的引用逐条核对过,都忠实于原结论。并发方案(单锁域、watch 终态、CancellationToken、yield 降级)是 tokio 官方立场的正确转译。M3 引用的 "python examples 11 项" 实测属实。

问题集中在一点:**§四、§五 宣称的契约,§二 的 API 草案撑不起来**。多处契约依赖的函数不存在,或签名写错了。

## 四个必修问题

### 1. 监听器长什么样没定,而且自相矛盾

草案 `on()` 的回调返回 `ListenerResult`(§二 52 行),但 `ListenerResult` 全文没定义。这不是小事,因为五分发模式全靠它:

- §〇 第 11 行说删掉同步/异步双态,"全异步 BoxFuture";
- §一 支柱 4 和 D4 又说 "bail 同步短路"、"emit 同步调用"。

同一个监听器,不可能既是纯异步形状、又能被同步的 `bail` 直接调用拿到返回值。这正是 v2 审阅头号阻断("监听器无错误通道")在 v4 语境下的残留。

**建议:贯彻全异步,bail 改成 async。** 理由:`internal/*` 面已砍,TS 里唯一需要同步 bail 的内部消费者(`internal/listener`)不复存在,同步性是 JS 便利,不是范式本质,短路语义完整保留。配套形状:

```rust
pub trait Event: Send + Sync + 'static {
    const NAME: &'static str;
    type Value: Send + 'static;   // 每事件的 bail 值类型,顺带吸收开放问题 2
}
// 监听器唯一形状:bail = Some(Value),错误显式入 Result
Fn(&Ctx, &E) -> BoxFuture<'_, Result<Option<E::Value>, CordisError>>
```

如果坚持 bail 必须同步,那就得把双态恢复写回 D4 并给理由——现状两头都不成立。

### 2. 清理函数没法报错

`Effect` 的两个清理变体(§二 66-71 行)返回 `()`:

```rust
Disposer(Box<dyn FnOnce(&Ctx) + Send>),       // 返回 ()
AsyncDisposer(... -> BoxFuture<'static, ()>), // 也返回 ()
```

但 §五 dispose 组的契约是"单错原样、多错聚合不压平、重复 dispose join 同一 `Arc<E>`"。清理返回 `()`,错误就无处可去,这三条契约全实现不了。TS 侧 disposer 的 rejection 记录进聚合错误是 EffectRecord 契约的核心(fiber.ts:506-534),§四 还宣称它"v4 全保留"。

**修法:两个变体都返回 `Result<(), CordisError>`**(配一个装 `Box<dyn Error>` 的变体兜住非 Cordis 错误)。

### 3. config 凭空消失了

`Plugin::apply(&self, ctx)` 不收 config,`Ctx::plugin(&self, p)` 也不收,`FiberView::update` 没写参数。但:

- D12 整条在讲"validate-before-store";
- `CordisError::Validation` 变体存在(§二 43 行);
- §四 宣称 "update 双分支……v4 全保留"——TS 的 update 双分支就是围绕 config 校验分支的(fiber.ts:857-886);
- config 也不在 §一 的"不做"清单里。

也就是说文档一半在讲配置、另一半的 API 装不下配置。二选一:

- **加回去**(推荐):`plugin(p, config)`、`apply(ctx, config)`、`update(config)`;
- 或者明说不要配置:把 config 写进"不做"清单,同步删掉 D12、Validation 变体、§四相关表述。

### 4. waterfall 签名写错了,而且注册不了

§二 57 行:

```rust
pub async fn waterfall<E: Event>(&self, ctx: &Ctx, e: &E, next: Next<'_>) -> BoxFuture<'_, Result<Value<E>, CordisError>>;
```

三个问题:

1. `async fn` 又返回 `BoxFuture`,套了两层。要么普通 `fn` 返回 BoxFuture,要么 `async fn` 直接返回 Result。
2. `next` 放错了位置。TS 里 next 是**调用方**提供的最内层终态续延(events.ts:245-254),监听器收到的是包裹后的 next。它应该是 waterfall 的一个"兜底行为"参数,不是和事件并列的参数。
3. 最根本的:`on()` 的监听器形状没有 next 参数,waterfall 监听器**没法注册**。需要一个独立的 `on_waterfall::<E>()`,形状是 `Fn(&Ctx, &E, Next<'_>) -> BoxFuture<'_, ...>`。这也顺带回应了 v2 审阅的签名生命周期阻断。

开放问题 2(`Value<E>` 的形状)推迟到 M1 实现时定是合理的,但注册面缺失不是开放问题,是缺口。

## 次要问题(应修,不阻断动工)

| # | 问题 | 说明 |
|---|---|---|
| 5 | `ctx.effect()` 忘写了 | 支柱 1 的"交回清理"、测试 `effect_yields_disposer`、监听器"随 fiber 卸载"的挂靠机制都依赖它,但 Ctx 草案里没有。另外 TS 的增量式(async-iterable)effect 用 `Effect::Many(Vec)` 表达不了,也没列进"刻意不保真"清单——要么补 API,要么补声明 |
| 6 | 事件 × isolate 没说清 | isolate 只讲了注册表隔离,事件跨不跨作用域传播全文未提(TS 靠 `Context.filter`),§五 也没有对应测试 |
| 7 | fiber 状态变化没法观察 | watch 是 last-value 语义,慢订阅者会跳过中间迁移(Loading→Active→…),§五 的 `state_transitions` 测试没有观察手段。需要一个走自家总线的类型化状态事件,或明说"只保证终态可见" |
| 8 | `PluginFailed(Box<dyn Error>)` 自相矛盾 | apply 已返回 `CordisError`,再包 `Box<dyn Error>` 反而丢了 `Arc<CordisError>` 的 identity(恰好一次测试靠它)。删掉或改 `Arc<CordisError>` |
| 9 | panic 策略未定 | "spawn+遏制"遇到的是 JoinError(panic)不是 Err;disposer panic 是否 catch_unwind 也没说。TS 把 listener 抛错路由到 logger 的对应物需要一个明确决定——哪怕决定就是"panic 直接传播" |
| 10 | 依赖环没测试 | `InjectUnsatisfied` 提到 cycle,但 §五 gating 组没有环检测用例,D14 的驱逐顺序遇到环怎么办也没写 |

## 小错(顺手改)

- 依赖清单(103 行)漏了 **thiserror**,而草案 37 行就在用 `#[derive(thiserror::Error)]`。
- 同句 "不引 async-trait(thiserror 可选)" 是句笔误。
- "不引 futures-util" 的前提是 parallel 用 spawn + `JoinSet`;如果不 spawn 而用 `FuturesUnordered`/`join_all`,就需要 futures-util。把选型写明。
- `provide<T>(value: T)` 和 `provide_as<T>(value: Arc<T>)` 的 Arc 不对称;`on` 返回裸 `Disposer` 而 `provide` 返回 `Result`,失败处理不一致。
- 同 label 的两次 `isolate(label)` 是否合并作用域(TS 语义)没写。
- §四 "v4 全保留" 说过头了:update 双分支被 §七.3 倾向推迟到 M4,config 又缺位。措辞改成"保留(落点见 X / 推迟至 M4)"。

## 放行条件

四个必修问题的修法都不需要走回头路:

- #1、#4 是把"全异步"贯彻到底(bail 异步化 + `on_waterfall` 独立注册面 + `Event::Value` 关联类型);
- #2 是给两个 Disposer 变体加 `Result` 返回;
- #3 是把 config 参数放回三个签名。

建议修订后把这几条裁决升级成 D16-D19 进决策表,§七 删掉已被裁决的开放问题 2,然后把文档头的"定稿"改回"草案"直到 M1 验收通过。次要问题和小错可以在 M1 实现期间随手清,但 #5(`ctx.effect()`)和 #7(状态观察)建议和四个必修一起定,因为它们同样影响骨架形状。
