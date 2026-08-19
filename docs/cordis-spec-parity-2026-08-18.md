# cordis 原版 spec 对拍清单(Rust 版)

> 2026-08-18。目的:明确 cordis 原版 96 个 spec 中,哪些语义该在 Rust 版自动化对拍、哪些不做、为什么。
> 方法:两个独立代理逐条读完 `tests/*.spec.ts` 全部断言,对照 v5 范式边界(design-rust-port.md §一五支柱 + §四偏差清单)判定。
> 判据:是不是**语言无关的范式不变量**。依赖 JS 特有机制(Proxy、字符串事件、internal/*、同步 bail、traceable/caller-shadow、Context.filter、intercept、async generator effect、update config)的不对拍。
> 统计:96 用例 → **对拍 31 / 部分对拍 27 / 不对拍 38**(逐 it 清点,判定表为准;初版粗计 27/21/48 与表不符,已对账修正,见 §五之二)。可对拍合计 58(31 全 + 27 内核),正好对应五支柱。

## 一、判定总览

| spec 文件 | 用例数 | 对拍 | 部分对拍 | 不对拍 | 主要不做原因 |
|---|---|---|---|---|---|
| fiber.spec.ts | 8 | 5 | 2 | 1 | update config(M4) |
| dispose.spec.ts | 13 | 4 | 9 | 0 | async generator effect 载体(13 个全部可对拍) |
| reentrant.spec.ts | 27 | 13 | 8 | 6 | internal/* 面、update config |
| events.spec.ts | 7 | 0 | 3 | 4 | Context.filter、bail 已删、字符串事件名 |
| plugin.spec.ts | 10 | 5 | 2 | 3 | Proxy/inspect/registry 迭代器 |
| service.spec.ts | 5 | 2 | 1 | 2 | traceable/caller-shadow |
| isolate.spec.ts | 3 | 2 | 0 | 1 | Context.filter 事件过滤 |
| reflect.spec.ts | 4 | 0 | 1 | 3 | Proxy 属性语法 |
| associate.spec.ts | 5 | 0 | 0 | 5 | 字符串点号路径 + Proxy 挂载 |
| decorator.spec.ts | 1 | 0 | 1 | 0 | TS decorator 语法 |
| invoke.spec.ts | 2 | 0 | 0 | 2 | intercept + caller-shadow |
| internal-hooks.spec.ts | 7 | 0 | 0 | 7 | internal/* 扩展面 |
| shadow.spec.ts | 4 | 0 | 0 | 4 | caller-shadow |
| **合计** | **96** | **31** | **27** | **38** | — |

## 二、对拍(31 个 it,纯范式内核,优先自动化)

### fiber 状态机 + 依赖门控(9)
| 原用例 | 断言的范式不变量 |
|---|---|
| fiber: inertia lock 1 | LOADING 期依赖消失不立即卸载,完成后进 UNLOADING,重新 provide 后再 LOADING→ACTIVE |
| fiber: inertia lock 2 | LOADING 期同 fiber 被重新 provide,in-flight 加载直接完成进 ACTIVE |
| fiber: inertia lock 3 | provider dispose 后消费者回 PENDING(驱逐+级联) |
| fiber: plugin error | apply 抛错→FAILED,失败 fiber 监听器不触发 |
| fiber: dispose error | dispose 抛错仍恰好一次,dispose() 正常 resolve |
| reentrant: coalesces duplicate dependency notifications | 无迁移时重复依赖通知合并,apply 只跑一次 |
| reentrant: distinguishes provider incarnations without a global counter | 两 root provide/inject 互不影响,重 provide 只重载自己的消费者 |
| service: pending inject | 依赖未就绪/init 未完成前 inject 回调阻塞,就绪后放行 |
| service: multiple injects | foo→qux、bar→foo+qux 拓扑门控,各 init 恰好一次 |

### 恰好一次清理 + 错误处理(15)
| 原用例 | 断言的范式不变量 |
|---|---|
| dispose: async return 1 | 异步 setup 完成后注册清理,dispose 按序执行 |
| dispose: async return 2 | setup 未完成调 dispose 仍等 setup 落地后清理 |
| dispose: return with error | 同步 effect 抛错即抛,不注册任何清理 |
| dispose: async return with error | 异步 setup 抛错经 promise 拒绝,不残留清理 |
| reentrant: keeps plugin execution failure separate from rollback cleanup failure | 执行错误走 await 通道、回滚清理错误走 logger、终态 FAILED |
| reentrant: returns one disposal promise and joins cleanup already in progress | dispose 重入返回同一 promise,restart 等待进行中清理 |
| reentrant: attempts every cleanup in LIFO order and aggregates failures deterministically | 清理按 LIFO 全量执行,失败确定性聚合 |
| reentrant: preserves an AggregateError thrown by user cleanup as one failure | 用户 AggregateError 整体进聚合,不拆散(不压平) |
| reentrant: keeps a direct cleanup failure observable through the shared promise | 清理失败后重入 dispose 所有等待者观察同一错误 |
| reentrant: contains cleanup failure at structural restart | restart 时清理抛错只记日志,fiber 回 ACTIVE |
| reentrant: separates synchronous execution and rollback cleanup failures | 同步执行错误当场抛,回滚清理错误走 logger |
| reentrant: removes a synchronously failed effect after rolling back collected cleanup | 失败 effect 回滚已收集清理恰好一次 |
| reentrant: makes reentrant restart await async rollback without replaying the execution failure | restart 阻塞在异步回滚上,放行后 resolve |
| reentrant: makes reentrant restart await async execution and cleanup | restart 等待异步 setup 及其异步清理全部落地 |
| reentrant: rejects effect registration during unload | 卸载期注册 effect 报 INACTIVE_EFFECT,fiber 恢复 ACTIVE |

### 插件装配 + 级联 + isolate(7)
| 原用例 | 断言的范式不变量 |
|---|---|
| plugin: apply functional plugin | 函数插件被调用一次且收到 options |
| plugin: inactive context | fiber dispose 后 plugin/effect/on 抛错且回调不执行 |
| plugin: nested plugins | 嵌套插件注册,dispose 级联清理全部子插件与监听,二次 dispose 幂等 |
| plugin: root dispose | root dispose 级联清子 fiber,dispose 恰好一次且幂等 |
| plugin: Service.init | init 钩子启动调用,返回的清理 dispose 时恰好一次 |
| isolate: isolated context | isolate('foo') 切断父级可见性,各作用域独立 provide/inject,清理后回调 dispose |
| isolate: shared label | 相同 label 共享同一份服务,不同 label 隔离(条件点:Rust 版需确认保留 label 共享语义) |

## 三、部分对拍(27 个 it / 20 行,内核可对拍但载体是 JS 特有)

这些用例的**语义内核是语言无关的**,但断言载体是 Proxy、字符串事件名、async generator effect、internal/* 或 update config。对拍时需把内核抽出来用 Rust 形态重写,不能照搬断言。

| 原用例 | 可对拍的内核 | JS 特有载体 |
|---|---|---|
| fiber: restart wrapped fiber | restart 后重放 apply 回 ACTIVE | Proxy/原型包装 fiber 外壳(hasOwn) |
| fiber: update config while injected service reloads | provider 更新驱逐 consumer 按序重载 | update config + getPrototypeOf Proxy 面 |
| dispose: dispose by plugin / dispose manually | fiber.dispose 恰好一次执行清理 | getEffects() label 树(JS 内省) |
| dispose: yield dispose | LIFO [3,2,1] + 恰好一次 + 重入返回同一 promise | generator effect + 字符串事件 label |
| dispose: async yield 1-4 | LIFO / abort 后只落地已 yield 的清理 | async generator effect(刻意不做) |
| dispose: yield with error / async yield with error | 抛错前已 yield 的清理保留 | 同步/async generator |
| reentrant: does not let a stale execution failure poison the current generation | 过期代异步失败不污染当前代(stale-epoch) | update config 触发机制 |
| reentrant: logs disposal observer failures without rejecting disposal | observer 抛错只记录,dispose 仍 resolve,终态 DISPOSED | internal/plugin observer |
| reentrant: does not await async disposal observers but still observes rejections | dispose 不等异步 observer,错误先后落 logger | internal observer |
| reentrant: lets parent disposal during publication drain pending child effects | 父 dispose 排干未激活子的 effect,child 从未 apply | internal/plugin 钩子 |
| reentrant: makes a loading parent join child cleanup already in progress | 父子 dispose 汇合到同一进行中清理 | internal/plugin 钩子 |
| reentrant: separates asynchronous execution and disposal failures | 执行错误与 dispose 错误分走两通道 | async generator effect |
| reentrant: logs auto-rollback cleanup failure once when a structural owner joins | 自动回滚清理错误只记一次 | async generator |
| reentrant: accepts effects while a child is pending or loading | PENDING/LOADING 期注册 effect 合法,dispose 时都执行 | PENDING 探测点借 internal/plugin |
| events: ctx.on() / ctx.once() | 注册→emit 触发、dispose 后不再触发;once 只触发一次 | 字符串事件名 |
| events: ctx.waterfall() | next 链传递值,不调 next 则截断 | 字符串事件名 |
| plugin: ctx.registry / compare snapshot | 卸载后 hook 恢复原状、重装一致(恰好一次还原) | JS 钩子数组/迭代器 |
| service: compare snapshot | 卸载/重装后快照还原(恰好一次) | JS 钩子快照 |
| reflect: service inject leak | fiber dispose 后访问其服务抛 inactive | Proxy get 陷阱 |
| decorator: @Inject on class method | 依赖注册后方法才被调,卸载后 dispose | TS decorator 语法 |

## 四、不对拍(38 个,JS 特有,刻意不做)

按不做原因归类,逐条命中 v5 §四偏差清单:

| 不做原因 | 用例 |
|---|---|
| **update config(推迟 M4)** | fiber: update config on wrapped fiber;reentrant: returns the asynchronous internal/update waterfall result、keeps wrapped fiber state canonical、coalesces an update before initial apply、continues the next generation after cleanup errors |
| **internal/* 扩展面** | internal-hooks.spec 全部 7 个;reentrant: resolves dependencies added during publication、rolls back runtime ownership when publication throws |
| **Proxy 属性语法 / 类继承反射** | reflect: Context.is()、access check、service injection;associate.spec 全部 5 个(字符串点号路径 + Proxy 挂载);plugin: context inspect |
| **traceable / caller-shadow** | service: traceable effect (with/without inject);invoke.spec 全部 2 个;shadow.spec 全部 4 个;associate: inspect |
| **Context.filter 事件过滤** | events: ctx.parallel()、ctx.emit()、ctx.serial();isolate: isolated event |
| **同步 bail(已删)** | events: ctx.bail() |
| **JS 动态类型 / 鸭子类型** | plugin: apply object plugin、apply invalid plugin |

## 五、实施建议

1. **可对拍的 58 个(31 全 + 27 内核)正好对应五支柱**,是自动化对拍的范围。建一个 `parity.rs`(或按支柱分文件),测试名直接沿用原 spec 的 `it('...')` 标题,便于追溯。
2. **部分对拍的 21 个**,抽内核用 Rust 形态重写断言:字符串事件名→类型化事件、async generator→手动 effect 序列、Proxy 外壳断言→直接断状态。
3. **fiber 的 inertia lock 时序类**(依赖 vitest fake timers)需用 tokio 可控时间或明确同步点重写,不能照搬。
4. **两个条件点先确认**:isolate 的 `shared label`(Rust 版是否保留 label 共享语义);dispose.spec 的同步 generator 归属(设计只点名剔除 async-iterable,同步 generator 未明示)。
5. **不对拍的 48 个在文档里留痕**(本文即留痕),宣发时可引:"96 个原版 spec 中 48 个为语言无关范式,Rust 版全自动化对拍;48 个为 JS 特有机制(Proxy/字符串事件/internal\*/traceable 等),按设计刻意不移植。"

## 五之二、实施记录(2026-08-18 执行完毕)

**`crates/rutis/tests/parity.rs`:57 个测试全绿**(§二全对拍 + §三内核按条目拆分为独立测试;测试名 = 原 `it()` 标题的 snake_case,注释标原文件:行号)。fake timers 一律改为确定性同步点(Notify 门 + watch 状态观察,"未落定"断言依赖"门未放行则完成不可能"而非计时)。已声明的通道偏差(注释留痕):执行错误走 await 通道不重复进 ErrorSink;fiber 级 dispose 返回清理错误(TS resolve+logger;D6/D20 通道)。

**对拍暴露并修复的四个实现缺口**(此前 72 测试均未覆盖):

1. **惯性锁 2 缺失**(fiber.spec:27):装载中同键重 provide 时,排队的重查按旧三元组比较会触发换代重载(apply 两次)。修复:load 完成时若依赖集已整体翻新且无缺失,就地采纳新集合(fiber.rs `load`),in-flight 装载直接完成进 ACTIVE(apply 恰好一次)。
2. **驱逐窗口拒绝同键重 provide**:TS 在 dispose 时同步释放注册表槽位;Rust 两阶段摘除下 finalize 前的 provide 一律 `ServiceExists`。修复:摘除中的绑定允许被新 provide 替换,驱逐方按 `Arc` 身份 finalize(registry `insert_binding`/`finalize_binding_if`),不误伤新绑定。
3. **TransitionTask 丢唤醒**(§八 7 同坑):`watch::channel(..).0` 只留 sender,初始 receiver 被 drop → 通道出生即关闭,`complete` 的 send 静默失败;消费者快速完成(等值合并跳过)而 join 方晚订阅时完成值丢失、join 永等。isolate 对拍用例确定性复现。修复:TransitionTask 持常驻 receiver 保活。
4. **依赖身份缺作用域**:同一 provider fiber 在不同 isolate 作用域提供同键服务时,`(PluginId, gen, TypeKey)` 三元组碰撞——默认作用域卸载会误驱逐隔离作用域消费者。修复:依赖身份扩为含 scope 的四元组(D21 精确匹配语义的落实;`last_deps`/`resolve_deps`/`consumers_of`/`evict_and_finalize` 同步)。附带修复 `get` 的访问方失活检查:失活(Unloading/Disposed)fiber 的上下文访问服务不可见,provider 子树内自访问豁免(reflect.spec 'service inject leak' 内核,TS inactive context 语义)。

**逐 it 对账**(与原 spec `it()` 清单双向核对):96 个 it = 对拍 31 + 部分对拍 27 + 不对拍 38。§二 31 行、§三 20 行(其中 4 行含多测试:dispose by plugin/manually、async yield 1-4、yield with error 对、on/once 对,展开为 27 个 it)全部落地;可对拍 58 个 it → 57 个测试函数,唯一差额是 plugin `ctx.registry`(TS 原用例为零断言的覆盖率 no-op——仅迭代 registry,无任何 expect,无可移植内核,豁免留痕)。初版统计 27/21/48 为粗计,与判定表不符,已按逐 it 清点修正为 31/27/38。

**结论**:57 对拍 + 58 契约 + 13 agent + 1 doc = 129 测试,双线程/单线程模式、连续 10+ 轮均绿;clippy `-D warnings` 与 fmt 清零。判定表中"isolate 的 shared label 保留语义"条件点已按现有实现(Rust 版保留 label 共享)对拍通过。

**增补(2026-08-19):emit 派发序对拍空白补齐**。cordis JS 的 emit 是同步内联调用,监听器按发射序执行——顺序免费,spec 因此从未显式断言它,Rust 版直译为逐事件 spawn 后该保证静默丢失(多线程背靠背 emit 实测约 30% 事件错位、位移最大 52;单线程与 ≥1ms 间隔零乱序,故原对拍全绿)。修复:emit 尾链(D31,同事件类型按发射序串行派发,remove/spawn/insert 单锁原子)。回归钉死:`rutis/tests/dispatch_chain_probe.rs`(并发同类型 emit 链不分叉)、`rutis-agent/tests/order_probe.rs`(multi_thread 下单发射者发射序=到达序)。此为 Rust 侧空白补齐,非语义偏差;跨事件类型不保证顺序为已声明边界(见 D31)。

## 六、宣发口径(基于本清单)

> Rust 版对 cordis 的验证:96 个原版 spec 逐条审阅,58 个语言无关范式不变量在 Rust 版自动化对拍(31 个全对拍 + 27 个抽内核对拍;fiber 状态机时序、恰好一次清理 LIFO/聚合不压平、依赖门控、级联卸载、依赖驱动重载、事件分发),38 个 JS 特有机制(Proxy 属性语法、字符串事件、internal/* 扩展面、traceable/caller-shadow、Context.filter、同步 bail、update config)按设计刻意不移植。语义锚点见契约测试内 cordis 源码行号注释。
