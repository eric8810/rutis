# Rust 移植设计 v1 审阅报告 — deepseek-v4-pro(2026-08-17)

> 审阅对象:docs/design-rust-port.md v1(已按本报告修订为 v2,见 v2 §0 修订摘要)。
> 基准:src/ 9 文件 + tests/*.spec.ts(62)+ test/core.test.ts(10)。裁决:**不放行 M1–M3 全量实现,放行前先修订设计**。
> 原文照档,未修改。

---

## 0. 总览

设计在方向上正确(契约优先、机制映射其次、traceable 列非目标),happy-path 生命周期 + 五模式分发描述基本忠实。三类系统性问题:

1. **基准错位**:§6 测试矩阵以 Python 测试命名/契约为锚;契约 10 是 Python 移植产物,TS 仓库无 agent loop。
2. **静默丢能力**:Standard Schema 校验、intercept 配置合并、callable Service(internal invoke/extend)、internal/* 扩展点、生成器 effect、once/prepend/global、registry delete-by-callback——未进"不做"清单也未进偏离清单。
3. **并发映射会实现爆掉**:BoxFuture 'static 与 &Context 借用不编译;RwLock 不可重入导致同步重入监听器死锁;tokio::spawn 假定始终在 runtime 内;inertia 被简化成"处置任务句柄"丢失核心同步原语。

净能力覆盖率估算(按现设计直接实现):约 50–60%,集中在表面层。

## 1. 十条契约逐条对照

### 契约 1(状态机)—— 基本忠实,一处顺序表述错误

- FiberState(fiber.ts:160-167);_getState 推导(uid===null→DISPOSED; _error→FAILED; epoch!==INACTIVE→ACTIVE; 否则 PENDING)(fiber.ts:647-652)。
- PENDING→LOADING 的触发是 _refresh() 算出非空 epoch 后 _setEpoch 转 _reload(fiber.ts:729-741、704-718),不是"注入齐全"本身。
- init 是 Service.init 符号,返回值即 effect(fiber.ts:264-272)。

### 契约 2(inject 门控)—— 方向对,缺祖先链读与 check

- notify 的 filter 比较 ctx[symbols.isolate][name](reflect.ts:314-336)。
- 缺:ctx.foo 祖先链 store 走读(reflect.ts:155-166,沿 fiber.parent.fiber 上行、比较 isolate label、prop in fiber.inject 时短路报错);Impl.check 谓词(reflect.ts:123-124、fiber.ts:689-701),false/抛错让消费者继续 PENDING。

### 契约 3(处置契约)—— 结论忠实,缺 effect 形状与报告去重

对照 fiber.ts:429-634:全量 LIFO+async 串行(runCleanups splice(0).reverse()+next() 串行)✓;单个原样/多个聚合不压平(combineCleanupErrors)✓;恰好一次+重复 join(startDisposal)✓;await effect 得 disposer(wrapper.then)✓;stale 代际(_reload epoch===oldEpoch)✓。
缺:(a) Effect 形状族 Disposable | Promise<Disposable> | Iterable | AsyncIterable(fiber.ts:83-93)及 epoch 中止语义;(b) cleanupReporters/effectInertia 的"失败恰好报告一次"去重(fiber.ts:112-117、468-484)。

### 契约 4(events)—— 三处失真/遗漏

- emit 遏制到 ctx.logger.error(events.ts:200-207);设计写"路由 on_error sink"是 Python 概念。
- parallel 的 AggregateError 聚合被漏(events.ts:188-192)。
- waterfall 根本性失真:CPS——监听器收 (...args, next),可 veto、可替换结果(events.ts:245-254);&mut V 折叠无法表达。
- serial 返回首个 bail 值被漏述(events.ts:215-220)。

### 契约 5(scoped dispatch)—— 大致对,"label 字段"是单数、漏 global

filter 由 thisArg[Context.filter] + hook.global 决定(events.ts:176-178);Service filter 按服务名比较 isolate 标签(service.ts:61-63)。store 按 label 为键(ReflectService.store: Dict<Impl, symbol>,reflect.ts:209、237-243),按 name 键会破坏隔离。漏 global 与 prepend。

### 契约 6/7(plugin + 校验)—— update PENDING 分支、internal/update、Standard Schema 全丢

- update 非 ACTIVE 分支:延迟校验、_config 落地、force 刷新新代际(fiber.ts:861-876)。
- internal/update waterfall 可否决/替换 restart(fiber.ts:880-885)。
- resolveConfig 走 runtime.Config['~standard'].validate 并抛聚合 ValidationError(fiber.ts:50-62、utils.ts:31-56);schemastery 已删≠Standard Schema 入口已删。

### 契约 8(root 不死)—— 忠实;root dispose 即 restart(fiber.ts:345)。

### 契约 9(set 不通知)—— 忠实(reflect.ts:254-265)。

### 契约 10(agent loop)—— 不是 TS 契约;应标注为"Python 示例移植"。

## 2. 未覆盖机制(逐文件,裁决表)

| 机制 | 证据 | 裁决 |
|---|---|---|
| Impl.check 谓词 | reflect.ts:123-124、fiber.ts:689-701 | M2 必须 |
| 祖先链 store 走读 | reflect.ts:155-166 | M2 必须 |
| store 按 isolate label 为键 | reflect.ts:209、237-243 | M2 必须 |
| internal/* 全部扩展点 | events.ts:340-363 等 | M4 或显式非目标;update/config 倾向 M2 保留 |
| Standard Schema 校验入口 | fiber.ts:50-62 | M2 必须(或明确声明丢弃) |
| intercept 配置合并 | context.ts:141-147、service.ts:86-102 | M4 或显式丢弃 |
| callable Service(invoke)+ extend | service.ts:17-21、65-73 | M4/永久不做但需声明 |
| accessor/mixin | reflect.ts:345-391 | M4/永久不做但需声明 |
| once/prepend/global | events.ts:113-119、299-329 | prepend/once M2 应补;global 与契约 5 联动 |
| registry delete-by-callback + runtime 身份 + 多形态 | registry.ts:216-222、252-261、92-137 | delete-by-callback M2;多形态 M4 但需声明 |
| 生成器/异步生成器 effect | fiber.ts:83-93、370-414 | M2 至少 async 单 disposer;generator 形状 M4 但必须声明 |
| update PENDING 分支 | fiber.ts:861-876 | M2 必须 |
| Context.is 品牌 | context.ts:61-68 | 永久不做(TypeId 替代) |
| logger | logger.ts | M4;但遏制 sink 依赖见 §3 |

## 3. async/并发映射评估

### 3.1 BoxFuture 签名不编译

`Box<dyn Future + Send>` 默认 'static;&self/&Context 借用无法被捕获。改 BoxFuture<'a, T> 生命周期化或 Arc<Context>/Arc<Self>。M2 落地第一天就撞上。

### 3.2 同步重入死锁

TS 单线程无锁,once() 派发中同步 dispose() 安全(events.ts:323-329)。RwLock 不可重入:dispatch 持读锁调监听器、监听器调 off() 拿写锁 → 死锁。需"快照后释放"或原子结构。

### 3.3 emit spawn 假设 runtime 存在

同步上下文(插件构造期、create 内、drop 路径)tokio::spawn panic "no reactor running"。需 Context 构造时捕获 Handle。Err 与 panic 应分两条通道。

### 3.4 恰好一次 + inertia

- 恰好一次需要共享 future:JoinHandle 只能复用 JoinError 不能复用 Result 错误;需 futures_util::FutureExt::shared() 或 oneshot/OnceCell——基本逼着引 futures-util,与"最小依赖"矛盾。
- **inertia 不是处置任务**:是加载/卸载过渡的串行化 promise(fiber.ts:213、704-718、817-823),负责重复通知合并、加载中 dispose 延后、卸载后接续重载、inertia lock 1/2/3(fiber.spec)。RwLock+"读判写后释放再 await"表达不了 inertia lock 2。

### 3.5 stale 代际在 tokio 下未定义原子检查顺序

_reload 在 await 前后各检查 epoch(fiber.ts:749-779);epoch 读写须原子/锁内,时序未定义则旧代失败毒化新代。

### 3.6 会爆掉的点汇总

1. BoxFuture<'static> vs &Context → 编译失败;2. 同步重入 vs RwLock → 死锁;3. 同步上下文 spawn → panic;4. 共享 future → 被迫引 futures-util;5. inertia 缺失 → 合并/延后语义错;6. waterfall &mut V → veto 不可表达;7. store 按 name 键 → 隔离破坏。

## 4. 偏离清单审查:不完整

已列但措辞问题:§5.3 以 Python on_error 视角描述(应直接对 TS:同步错误照抛)。
应列未列:Standard Schema、intercept 合并、callable invoke+extend、accessor/mixin、once/prepend/global、internal/* 扩展点、delete-by-callback/runtime 内省/多形态、generator effect、对象式 inject per-service config。要么进"不做"要么进偏离,现在会误导"其余全部对齐"。

## 5. 裁决

**不放行 M1–M3 全量;先修订设计。**

- M1 可先行的前提:waterfall 恢复续延模型;补 BoxFuture<'a>/Arc、同步重入结构、runtime Handle;补 parallel AggregateError + serial 返回 bail 值。
- M2 不满足放行条件,需先设计:inertia 串行化/合并/延后的 tokio 等价物、恰好一次共享 future(含 futures-util 决策)、epoch 原子时序、Standard Schema 入口、Impl.check、祖先链读、label 键 store、update PENDING 分支、internal/update+config、delete-by-callback。
- 测试基准必须换回 TS(internal-hooks 5 条、reentrant 19 条、dispose generator 形状等目前无钉子)。

覆盖率:按现设计 50–60%;补齐 M2 缺口 → 85–90%(剩余集中在 traceable/intercept/accessor/generator/多形态/logger 的 M4/永久不做项)。

## 未决不确定性

- 未逐条读 shadow/associate/decorator.spec(非目标面,影响低)。
- 未运行测试套件,未读 python 实现(仅核对测试名),Python 侧 on_error/logger 边界依赖 HANDOFF 转述。
- 'static 结论是语言规则推断,以编译器为准。
