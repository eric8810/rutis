# Rust 移植设计 v1 审阅报告 — gpt-5.6-sol(2026-08-17)

> 审阅对象:docs/design-rust-port.md v1(已按本报告修订为 v2,见 v2 §0 修订摘要)。
> 基准:src/ TS 源码。裁决:**当前设计不能完整复现 TS 版 Cordis,不建议按现稿直接放行 M1**。
> 原文照档,未修改。

---

## 总体裁决

**结论:当前设计不能完整复现 TS 版 Cordis 的工作,不建议按现稿直接放行 M1。**

它能够实现一个受 Cordis 启发的、强类型 Rust 插件框架,但不是 TS 最小内核的语义移植。主要阻断项是:

1. `waterfall` 被翻译成"可变值传递",而 TS 实际是可否决、可嵌套的 continuation/middleware 链。
2. 类型化事件设计丢失字符串事件的开放扩展、统一监听器集合、动态返回值和 `thisArg` 过滤语义。
3. `Plugin` trait 把插件错误地限制成"创建一个 Service",无法表达 TS 的无服务、多服务、纯 effect 插件。
4. 配置校验被放进 `create`,无法保证 `update()` 的"校验失败零副作用"。
5. `BoxFuture` 缺少生命周期参数,文档给出的 `init(&Context) -> BoxFuture<_>` 通常无法编译。
6. reflect/provider/inject、intercept、Runtime 共享、内部 waterfall 扩展点、fiber inertia 等均缺少实现级契约。
7. 测试与第 10 条契约主要来自 Python agent 示例,不是 TS ground truth。

建议先修改设计,再实施 M1。若只把目标改名为"Cordis-inspired typed Rust subset",则可以实施,但必须放弃"完整复现 TS"的表述。

## 1. 第二节 10 条契约逐项核对

### 1.1 fiber 状态机:方向正确,但过度简化

- TS 的稳定状态由 `uid`、`_error`、epoch 推导(fiber.ts:647-652)。
- provider/inject 变化经 `inertia` 串联 `_reload()`/`_unload()`(fiber.ts:704-717)。
- plugin 执行错误在 `_reload()` 内被 logger 记录,仅在当前 epoch 仍有效时写入 `_error`(fiber.ts:749-778)。
- `await()` 等待所有 inertia 后才抛 startup/config 错误(fiber.ts:817-823)。
- class plugin 先跑实例 init hooks,再调用 `instance[symbols.init]()`(fiber.ts:265-273)。

### 1.2 inject 门控:主线正确,缺 provider 可用性判定

- 构造后逐项 `_checkImpl()` 再 `_refresh()`(fiber.ts:324-333)。
- `_refresh()` 以 provider fiber uid 构造 epoch,缺任一依赖即 INACTIVE(fiber.ts:720-740)。
- notify 按 inject 名和 isolate scope filter 更新 fiber(reflect.ts:307-335)。
- 遗漏:provider 可携带 `check`(reflect.ts:115-125);`check` 以 traceable service 为 this,返回 false 或抛错都使依赖不可用(fiber.ts:689-700);strict get 要求 provider fiber ACTIVE(reflect.ts:233-243);notify 扫描共享 Runtime 全部 fiber(reflect.ts:314-329)。

### 1.3 EffectRecord 处置契约:核心方向正确,作用域表述不精确

- 执行/清理双通道(fiber.ts:438-450);cleanup 全量执行、单 effect 内严格 LIFO、异步串行(fiber.ts:506-533);单错误原样、多错误 AggregateError 且不 flatten(fiber.ts:124-130);dispose task 恰好一次、重复调用 join(fiber.ts:548-581);`await effect` 得到 disposer 本身(fiber.ts:630-632);stale generation 失败不污染新一代(fiber.ts:749-778)。
- 纠正 A:"全量 LIFO"只对 EffectRecord 内部严格成立;fiber 卸载顶层 `_disposables.clear()` 逆序取出但 `Promise.all` 并发清理(fiber.ts:781-799)。
- 纠正 B:清理错误的观察——直接调用者 vs 结构所有者经 `cleanupReporters` 恰好报告一次(fiber.ts:468-483、789-798)。

### 1.4 events 五模式:存在严重翻译失真

- `emit`:TS 在调用栈内同步调用,promise-like 才附 `.catch()`(events.ts:194-206);换 tokio::spawn 改变同步前缀。
- `parallel`:`Promise.allSettled` 后抛 AggregateError(events.ts:182-192);设计未声明多错误聚合。
- `bail` 条件:`null`/`undefined`/`false` 不 bail(events.ts:8-15);`Option::Some` 不能声称精确复现。
- **`waterfall`(最严重)**:continuation/middleware——最后参数是内层 `next`,调用才进入下一层,不调用即 veto,可前后包裹,返回最外层返回值(events.ts:235-254)。`&mut V` fold 无法表达 veto/嵌套/around。影响 internal/config(fiber.ts:743-747)、internal/update(fiber.ts:857-885)、internal/get/set(reflect.ts:144-196)、per-fiber update hooks(events.ts:145-160)。
- 其他遗漏:prepend/global(events.ts:113-125)、once(events.ts:315-329)、`internal/listener` bail 替换(events.ts:299-312)、非 internal 事件先触发 `internal/dispatch`(events.ts:170-179)、listener 经 reflect trace binding(events.ts:304-307)。

### 1.5 scoped dispatch:描述过度泛化

dispatch 可选显式 `thisArg`;filter 取 `thisArg[Context.filter]`;非 global listener 才经过 filter;filter 接收 listener 所属 context(events.ts:170-179)。Service 默认 filter 才是按服务名比较 isolate 标签(service.ts:61-63)。需保留 `global` 绕过。

### 1.6 plugin 契约:只覆盖 class-like happy path

- TS 支持三种入口(function/constructor/{apply} 对象)(registry.ts:91-126、216-222)。
- function/object 插件可直接返回 disposer/promise/iterator/async iterator,不必创建 Service(fiber.ts:370-413)。
- update 是 internal/update waterfall,可 veto 或替换(fiber.ts:843-885)。
- 非 ACTIVE 的 update 保存 raw config、强制新 generation、吞掉 startup error(fiber.ts:857-876)。

### 1.7 config 校验:校验与构造混合破坏原子性

- TS 独立 `resolveConfig()`:Standard Schema v1 入口、input→output 规范化、issue 聚合、拒绝 async validator(fiber.ts:16-61、utils.ts:26-55)。
- ACTIVE fiber 的 update 在改 `_config`/`config` 前先解析(fiber.ts:877-884)。
- "校验器就是 create 里的显式检查"导致校验与副作用不可分离、无法复现 normalization、update 为校验不得不构造新服务。

### 1.8 root fiber 不死:结论正确

root 构造后清空 bootstrap disposables(context.ts:70-85);root dispose 绑定为 restart()(fiber.ts:334-346)。

### 1.9 同 fiber ctx.set 不通知:正确,但缺 set 权限契约

set 仅允许已 provide 且当前 fiber是 provider;只替换 value 不 notify(reflect.ts:245-265)。

### 1.10 agent loop:不是 TS Cordis 核心契约

src/ 无 agent loop;第 10 条应移到"Rust agent 示例验收标准"。

## 2. TS 已有但设计未覆盖的重要机制(裁决表)

| 机制 | 证据 | 裁决 |
|---|---|---|
| 真正的 continuation waterfall | events.ts:235-254 | M1 必须 |
| parallel 多错误聚合 | events.ts:188-192 | M1 必须 |
| emit 同步调用、仅 promise rejection 遏制 | events.ts:200-205 | M1 必须 |
| prepend/global/once | events.ts:113-125、299-329 | M1 必须 |
| 通用 `thisArg[filter]` 协议 | events.ts:170-179 | M1 必须 |
| internal/dispatch | events.ts:170-175 | 完整 parity M1 必须 |
| internal/listener 注册替换 | events.ts:299-312 | M1/M2 必须 |
| extend() 继承与 shadow 保留 | context.ts:92-109 | Context scope 部分 M1/M2 必须;动态 shadow M4 |
| isolate name→label 映射 | context.ts:111-127 | M1/M2 必须 |
| intercept 原型链/祖先合并 | context.ts:129-147、service.ts:75-102 | M2 必须 |
| provider scope-key store、重复 provide 错误 | reflect.ts:277-304 | M2 必须 |
| provider check predicate | reflect.ts:115-125、fiber.ts:689-700 | M2 必须 |
| provider 卸载先 notify/等待 dependents 再删 self store | reflect.ts:297-303 | M2 必须 |
| strict/non-strict get | reflect.ts:225-243 | M2 必须 |
| internal/get/set waterfall | reflect.ts:144-196 | 完整 parity M2 必须 |
| accessor/mixin | reflect.ts:338-390 | M4;不做会丢公开扩展能力 |
| Service intercept config merge | service.ts:75-102 | M2 必须 |
| callable Service/invoke+extend | service.ts:45-73 | M4;永久删除必须列公开能力损失 |
| Plugin Runtime 按 callback 共享 | registry.ts:128-138、311-325 | M2 必须 |
| registry map API 与 delete-by-callback | registry.ts:224-285 | delete/runtime M2 必须;遍历 M4 |
| function/class/object 三种 plugin shape | registry.ts:91-126 | 能力 M2 必须 |
| Standard Schema 验证入口及 normalization | fiber.ts:50-61、utils.ts:26-55 | M2 必须 |
| internal/config 和 internal/update waterfall | fiber.ts:743-747、843-885 | M2 必须 |
| inertia 与 load/unload 串联 | fiber.ts:704-717、781-809 | M2 必须 |
| force generation / stale 隔离 | fiber.ts:720-740、764-777 | M2 必须 |
| Effect iterable/async iterable | fiber.ts:76-101、370-413 | iterable M2;诊断 metadata M4 |
| 状态/plugin observer 逐回调隔离 | fiber.ts:132-149、654-670 | M2 必须 |
| logger/error sink | events.ts:203-205、fiber.ts:763-769 | 最小 error sink M1 必须;完整 logger M4 |
| traceable/caller/shadow | utils.ts:166-275 | M4;不能称为"不是损失" |
| logger 完整命名/调用服务 | logger.ts:44-82 | M4 可选;error sink 不能推迟 |

可永久不做(仅当目标改为"Rust 习语化子集"):JS Proxy 属性语法、callable object 表面形状、JS prototype identity、跨 realm brand、JS 错误栈拼接、任意 JS object/class/apply 入口形状——但必须保留其能力等价物。

## 3. Rust 机制映射评估

### 3.1 类型化事件 §4.2(阻断)

- `(TypeId, NAME)` 割裂同名事件;跨 crate 互操作损失。
- 同步/异步 listener 静态拆分两套,无法保留统一 prepend 顺序、emit 同步前缀。
- waterfall 签名错误;建议拥有型 payload + 索引化链 + boxed continuation。
- 缺 listener owner context 与 dispatch receiver。

### 3.2 BoxFuture §4.3(阻断)

`Box<dyn Future + Send>` 省略生命周期默认 'static;借用 &self/&Context 的 async block 非 'static,签名不编译。需 `BoxFuture<'a, T>` 或 Arc 拥有型参数。影响所有 async 面。

### 3.3 锁卫生 §4.4

必要不充分:check-then-act race 需 generation token 提交校验;JoinHandle 不可多 join,需共享完成状态(spawn 'static 要求);FiberView IntoFuture 应拥有 Arc 并循环 notify;TS 顶层 unload 并发 Promise.all,勿实现成全局串行。

### 3.4 panic 政策 §4.5

监听器不 spawn 则 poll 内 panic 不成 JoinError;全 spawn 又破坏同步前缀。建议分四路径:同步 Err/同步 panic/async Err/async panic。parallel 正常 Err 聚合返回,不路由 sink。

### 3.5 Plugin trait §4.6(M2 架构阻断)

- 恰好一个 Service 的约束错误(可零/多服务/纯 effect)。
- associated type/const 不适合 dyn registry,需 type-erased descriptor + trampoline。
- `dyn Service` downcast 需显式 as_any 或 `Arc<dyn Any>`;需回答跨 generation 类型更换、get 类型不符错误、set 换类型、isolate store 键。
- `Send + Sync` 是真实能力收紧,必须列入偏离。
- 校验放 create 不可接受:应拆 resolve_config(纯)→ apply(副作用)。

## 4. 偏离清单审查

- ctx.foo→get:语法偏离可,底层解析能力不能一起删。
- traceable:是能力损失,应如实表述。
- 类型化事件:开放事件能力损失(跨 crate 互操作、运行时事件名、internal/dispatch 通用观察、动态返回值)。
- Plugin trait 能力损失未列(开放 shape、零/多服务、编译期注册、Send+Sync)。
- waterfall 改值传递不能称偏离,是错误。
- schemastery≠Standard Schema:校验入口是保留能力。
- logger M4 与 M1 events 矛盾:最小 ErrorSink 必须 M1。
- 不应以 Python on_error 定义 TS parity。

## 5. 覆盖率与放行

- 表面方法名覆盖 60-70%;按可表达程序 35-45%。
- 能表达:编译期固定事件集、单一强类型载荷、无 waterfall veto、每插件单 Send+Sync Service、简单校验、静态依赖、简单门控、无 accessor/mixin/traceable、无运行时 registry 操作、示例 agent。
- 不能表达(14 类):internal/update veto/包裹、internal/config/get/set continuation、纯监听插件、多服务插件、check() 动态可用性、intercept 祖先合并、共享 callback registry 管理、运行时字符串事件/跨 crate 同名、async 同步前缀、prepend/global/once 顺序、caller-shadow、Standard Schema normalization、非 Send+Sync 状态、iterable effect。

**不放行 M1。** 最低放行条件:waterfall continuation、emit 不 spawn、parallel all-settled 聚合、listener owner/receiver/filter/global/prepend/once、事件身份决策(字符串主键或明示 TypeId 损失)、M1 ErrorSink、BoxFuture<'a>、Python agent 测试移出契约矩阵。M2 前还需:type-erased descriptor、plugin 不绑单 Service、纯校验、(scope,name) store 与 owner-only set、provider check、inertia/generation/waiter、dispose exactly-once 共享、intercept 合并、Runtime 共享/delete-by-callback。

## 未决不确定性

- 无 Rust 实现,Send/Sync、共享 disposal waiter、dyn registry 的具体失败形式待实现验证。
- agent 11 项未逐项复核 Python 源码(不属 TS ground truth)。
- 覆盖率为估算,非双实现实测。
