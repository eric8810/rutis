# min-cordis Rust 实现评审(对应设计 v5)

> 2026-08-17。评审对象:[rust/](../rust/) workspace(min-cordis 核心 2075 行 + min-cordis-agent 415 行,55 测试全绿)。
> 基线:[design-rust-port.md](design-rust-port.md) v5(§二 API 草案、§三 D1-D30、§四 偏差清单、§五 契约测试、§九 放行条件)。
> 方法:内部三路并行评审(并发正确性 / 测试质量 / 设计一致性)+ gpt-5.6-sol 独立评审(零提示)。sol 新增 5 个真 bug,全部经独立复核属实。

## 总体判断

**架构边界守住了,核心范式全部成立;实现层有 10 个真 bug,均不碰架构。**

v5 定的关键边界逐条核对无侵蚀:

- **crate 分层**:核心零 serde,仅在 agent crate("边界 crate"裁决落实)。
- **事件类型化**:内部擦除 `Arc<dyn Any>` 只出现在 bus/event 两文件,公共 API 全泛型;无字符串事件、无 Value 载荷退化。
- **dyn 兼容**:`Plugin`/`Listener`/`WaterfallListener` 均可 `Arc<dyn>`,注册表未被泛型污染。
- **锁不跨 await**:fiber transition 锁只做入队与改状态,回调全在锁外(D5/D24 物理隔离:`set_state` 入队、`flush_status` 锁外发)。
- **恰好一次归 TransitionTask,观察归 watch**:两机制不混(D20/D25 分层守住)。
- **config 烘进实例**:注册表只存 `Arc<dyn Plugin>`,不碰 `Any` 擦除(codex 最小路线照走)。
- **事件不过滤 isolate**:分发路径零 scope 检查(D29 落实)。

16 条放行条件的设计意图全部落实,D 系列决策在代码里均有注释锚点。**零 unsafe**,所有权是规范的 Arc/Weak + 短临界区 Mutex + 专用同步原语(watch/mpsc/Notify/CancellationToken 各司其职)。

## 必修 bug(10 个,按严重度)

### 第一梯队:资源泄漏 + 永久挂起

#### 1. `apply` 失败后资源不回滚(sol 新增,四路中最重要单一发现)

[fiber.rs:327-340](../rust/crates/min-cordis/src/fiber.rs#L327)。`fail_load` 只清依赖边和 `last_deps`,**不排干 effects**——失败插件已注册的服务/监听器/子插件全部泄漏存活:

- 失败插件的事件监听器仍接收事件(总线不查 fiber 状态)。
- 子插件可能继续 Active。
- 服务绑定占位残留,后续同键 provide 得 `ServiceExists`。

**失败装配的原子性没了**——支柱 1(装配单元)的实现语义,设计评审阶段未覆盖。`ctx.provide`/事件注册/`ctx.plugin` 都立即把 EffectRecord 放入 fiber([ctx.rs:290-307](../rust/crates/min-cordis/src/ctx.rs#L290)、[ctx.rs:221-287](../rust/crates/min-cordis/src/ctx.rs#L221)),apply 返 Err 后无人清理。

**修法**:fail_load 走与 unload 相同的 LIFO 回滚再进 Failed;补"先注册服务和监听器、随后返 Err"的契约测试。

#### 2. `dispose()` 与 `restart()` 并发,restart 永久挂起(sol 新增)

[fiber.rs:580-595](../rust/crates/min-cordis/src/fiber.rs#L580)。restart 只查 `state == Disposed`;Dispose 已入队但状态未变时 restart 也入队。非 root 处理完 Dispose 后驱动 return([fiber.rs:437](../rust/crates/min-cordis/src/fiber.rs#L437)),排队的 Restart 永远等不到 `TaskDone::Done`,调用者卡在 [fiber.rs:599-609](../rust/crates/min-cordis/src/fiber.rs#L599)。

**修法**:restart 检查 `terminal_task.is_some()` 即返 `InactiveEffect`;或驱动退出前完成所有排队任务。补并发 dispose/restart 超时测试。

#### 3. 消费者 Dispose 与 provider 驱逐并发,provider 清理永久挂起(sol 新增)

[ctx.rs:257-284](../rust/crates/min-cordis/src/ctx.rs#L257)。消费者 Dispose 先入队、驱动退出,随后的 RefreshDepsJoin 无人处理;provider 的 disposer 永久等待 join_task,级联阻塞上层卸载。

**修法**:post 返失败即完成驱逐任务;或驱动退出前排干并拒绝剩余带完成信号的意图。补"provider dispose 与 consumer dispose 同时"测试。

#### 4. `EffectRecord::drain` 无取消安全,Future 被丢弃后永久卡 Draining(sol 新增)

[effect.rs:64-86](../rust/crates/min-cordis/src/effect.rs#L64)。首个调用者切状态到 Draining 后在自身 Future 跑清理;调用方 select!/超时/abort 丢弃该 Future,状态不恢复。之后所有 drain 进入 join 并永久等待([effect.rs:123-137](../rust/crates/min-cordis/src/effect.rs#L123))。`Disposer::dispose()` 公开返回可任意取消的 Future([effect.rs:153-159](../rust/crates/min-cordis/src/effect.rs#L153))。

**修法**:清理放独立 tokio 任务,所有调用者只 join 共享结果;补 abort 首个 disposer 后再 dispose 的测试。

#### 5. `serial` 是并发分发,不是顺序(内部三路命中)

[bus.rs:272-296](../rust/crates/min-cordis/src/bus.rs#L272)。循环里每个 hook `spawn_on` 后立即 `join_next().await`——所有 hook 同时跑,按**完成序**短路。TS serial 语义(await 完第 N 个才调第 N+1 个,按**注册序**短路)被破坏:被短路的靠后监听器副作用已执行。

`serial_bails` 能过是因监听器近零耗时,spawn 序≈完成序。

**修法**:顺序 spawn+await,不建 JoinSet;签名从 `Arc<E>` 改回 `&E`。补"前面慢 Some、后面快 Some"对抗用例。

### 第二梯队:panic 杀死驱动

#### 6. `validate()` / `check()` panic 直接杀 fiber 驱动(sol 新增)

[fiber.rs:284-288](../rust/crates/min-cordis/src/fiber.rs#L284)、[registry.rs:169-188](../rust/crates/min-cordis/src/registry.rs#L169)。apply 有 CatchUnwind([fiber.rs:294-300](../rust/crates/min-cordis/src/fiber.rs#L294)),这两个用户回调边界没有——panic 终止 fiber 驱动,`intents_inflight` 不递减,FiberView await/restart/dispose 全挂起。

**修法**:所有用户回调边界统一 catch;validate panic→Failed,check panic→依赖错误/ErrorSink。补 validate/check panic 后 FiberView 稳定完成的测试。

#### 7. 异步清理闭包 `f()` 调用时 panic 在任务边界外(sol 新增)

[effect.rs:102-107](../rust/crates/min-cordis/src/effect.rs#L102)。先 `f()` 再 spawn——`f()` 本身 panic 不进 CordisError,在驱动里直接杀驱动。

**修法**:`f()` 调用本身也 catch_unwind,成功创建 Future 后再 spawn。分别补"闭包调用时 panic"与"Future poll 时 panic"两测试。

#### 8. `join_task` / `settle_inner` 把 sender drop 当成功(内部三路)

[fiber.rs:606-608](../rust/crates/min-cordis/src/fiber.rs#L606)。`rx.changed().await` 返 `Err` 即 `return Ok(())`;FiberInner 整体释放时 sender drop,dispose 静默返 Ok,掩盖从未执行的卸载。`settle_inner`([fiber.rs:637](../rust/crates/min-cordis/src/fiber.rs#L637))同构。

**修法**:sender drop 返 `Err(Arc<CordisError>)`。

### 第三梯队:已知竞态

#### 9. provide TOCTOU 静默丢服务(内部三路 + sol 复核)

[ctx.rs:217-240](../rust/crates/min-cordis/src/ctx.rs#L217)。预检 lookup 与闭包内 insert_binding 无互斥;并发同键时后到者 ServiceExists 只进 error_sink,调用者拿 Ok+无效 Disposer,服务未注册,破坏 `provide_as` 的 Result 契约。

**修法**(sol):插入改为同步原子主操作,ServiceExists 直接返调用方,再登记负责删除该精确 binding 的 Effect。补两任务同时 provide 同键测试。

#### 10. `plugin()` 先 spawn 再注册 parent effect 的窗口(内部三路)

[ctx.rs:313-332](../rust/crates/min-cordis/src/ctx.rs#L313)。parent 已 Disposed 走 `registered.is_err()` 补救,但 spawn_fiber 内已 register_inject;Dispose 处理后非 root 驱动 return,后入队的 RefreshDeps 永不处理,留半注册 fiber。

**修法**:先注册 parent effect 再 spawn_fiber,或补救分支显式清理 inject_index。

#### 11. unload 的 `mem::take(effects)` 无 guard(内部三路)

[fiber.rs:356](../rust/crates/min-cordis/src/fiber.rs#L356)。某 record.drain panic 则后续 effects 全丢,状态停在 Unloading。

## agent crate 问题(sol 新增)

#### 12. `stopped`/`steps` 跨运行共享

[agent/lib.rs:209-217](../rust/crates/min-cordis-agent/src/lib.rs#L209)。`stop()` 永久写 true,后续 run 立即 Stopped([agent/lib.rs:244-251](../rust/crates/min-cordis-agent/src/lib.rs#L244));并发两个 run 互相取消,事件步号穿插([agent/lib.rs:295-301](../rust/crates/min-cordis-agent/src/lib.rs#L295))。

**修法**:每次 run 独立运行状态+取消令牌,fiber 级 token 作父级取消源;`steps()` 明确为累计指标或删除。补连续两次运行、并发运行测试。

#### 13. 工具 runner panic 让 Agent Future panic

[agent/lib.rs:319-340](../rust/crates/min-cordis-agent/src/lib.rs#L319)。工具错误契约只处理 `Result::Err`,runner 调用或 poll 中 panic 无法转成模型可见的 `"error: ..."`,与 crate 文档"工具失败回喂模型"语义有缺口([agent/lib.rs:11-12](../rust/crates/min-cordis-agent/src/lib.rs#L11))。

**修法**:工具执行边界捕获 panic;补同步创建 Future panic、异步 poll panic 两测试。

#### 14. 空 LLM 响应静默返回空串

[agent/lib.rs:303-305](../rust/crates/min-cordis-agent/src/lib.rs#L303)。`content==None && tool_calls.is_empty()` 得 `Ok("")`,掩盖后端协议错误;`LlmResponse` 公开字段允许轻易构造该状态。

**修法**:增加 `AgentError::InvalidResponse`,或将响应建模为排除非法状态的枚举。

#### 15. agent 事件测试断言顺序但 emit 不保证完成序

[agent.rs:394-408](../rust/crates/min-cordis-agent/tests/agent.rs#L394)。emit 为每监听器建独立任务([bus.rs:221-239](../rust/crates/min-cordis/src/bus.rs#L221)),调度完成顺序无保证,存在偶发失败风险。

**修法**:顺序属契约时改 serial 分发,或按序列号排序断言。

## 测试缺口(合并两路)

| 缺口 | 对应 bug |
|---|---|
| serial"前面慢 Some、后面快 Some"对抗用例(contract.rs:773) | bug 5 |
| 自动重载闭环:重新 provide → 消费者回 Active(contract.rs:966、:1089);`check_evicts` 谓词翻回 true 后再激活 | 支柱 5 后半截 |
| apply 部分注册后失败的事务回滚 | bug 1 |
| dispose/restart 并发;consumer/provider dispose 并发 | bug 2、3 |
| abort 首个 disposer 后再 dispose | bug 4 |
| validate/check panic 后 FiberView 稳定完成 | bug 6 |
| 清理闭包调用 panic / Future poll panic 分别覆盖 | bug 7 |
| 同键并发 provide 的返回值 | bug 9 |
| agent 连续运行、并发运行、逐运行取消;空 LLM 响应;LLM panic;工具 panic | bug 12-14 |
| parallel"快错+慢 Ok"全等语义(contract.rs:743) | 测试强度 |
| auto_ownership 读 counter(contract.rs:232);emit_async_safe 校验 `e.value==42`(contract.rs:688) | 测试强度 |

高风险契约 `concurrent_dispose_join`(:356)、`exactly_once_same_error`(:617)、`dispose_during_dispose`(:639)断言均为真并发/真 `Arc::ptr_eq` identity,强度足够。

## 设计一致性

### 已申报偏差(§八 有记录,属合理实现选择)

- serial 载荷 `&E`→`Arc<E>`(JoinSet `'static`)。
- waterfall 第三参 `Next<'_>`→泛型 `Terminal<E>`,新增公开 trait `Terminal`。
- `Next` 由 `&mut dyn FnMut(&Ctx, E)` 改为零参 `pub struct Next<'a, E>`。

### 未申报偏差(轻)

- `TransitionTask` 未内嵌 generation([fiber.rs:70](../rust/crates/min-cordis/src/fiber.rs#L70)):功能由 restart 清 `terminal_task` 覆盖,偏差点在形状而非行为。
- `Ctx` 多暴露 `handle()`([ctx.rs:69](../rust/crates/min-cordis/src/ctx.rs#L69))、`events()`([ctx.rs:116](../rust/crates/min-cordis/src/ctx.rs#L116))、`cancellation_token()`([ctx.rs:338](../rust/crates/min-cordis/src/ctx.rs#L338)):均只读访问器,未破所有权模型。
- `CordisError::ServiceNotFound` 是死变体:`get` 走 Option(D13),核心无构造点。
- `Plugin::injects()` 返回切片,迫使插件长期保存 `Vec<TypeKey>`(sol):固定依赖可用静态数组或关联常量。
- `AgentLoopPlugin::with_max_steps(0)` 运行即 `MaxSteps(0)`(sol):构造时校验更早暴露配置错误。
- 重复工具名被 `HashMap::collect` 静默覆盖(sol,[agent/lib.rs:233-241](../rust/crates/min-cordis-agent/src/lib.rs#L233)):构造函数应返 Result 报重复名。
- `tool_schemas()` 来自 `HashMap::values()` 顺序不稳定(sol,[agent/lib.rs:253-268](../rust/crates/min-cordis-agent/src/lib.rs#L253)):需确定性请求快照时保留原始 Vec 顺序。

### 确认无误(独立复核排除了疑似问题)

- **waterfall 递归别名**([bus.rs:15-33](../rust/crates/min-cordis/src/bus.rs#L15)):`invoke(self)` 按值消费,`&mut` 沿调用链独占下移,无 alias。
- **once 的 fired 认领**([bus.rs:198-207](../rust/crates/min-cordis/src/bus.rs#L198)):锁内 `swap(true)` 并发恰好一次;serial 跳过者下次被过滤。
- **CatchUnwind**([event.rs:166-186](../rust/crates/min-cordis/src/event.rs#L166)):`AssertUnwindSafe` 包 poll 是标准做法,无 UB。
- **resolve_dep 时序**([registry.rs:170-189](../rust/crates/min-cordis/src/registry.rs#L170)):通知链完整(inject_index 注册于 spawn_fiber 早于一切 load),无永久等待。
- **StoredValue 双重包装**([registry.rs:13-24](../rust/crates/min-cordis/src/registry.rs#L13)):`Box<Arc<T>>` 擦除路径正确还原 `T: ?Sized` 胖指针。
- **EffectRecord::join 丢唤醒**:依赖 `Notify::notified()`"创建即注册"语义,tokio 文档确认无丢唤醒。

## clippy 不过(sol 复核)

`cargo clippy --workspace --all-targets -- -D warnings` 失败:

- 未使用导入 `TaskDone`([ctx.rs:11-13](../rust/crates/min-cordis/src/ctx.rs#L11))。
- 多余裸指针类型转换([registry.rs:123-128](../rust/crates/min-cordis/src/registry.rs#L123))。
- 测试中若干未使用导入、变量、死代码警告。

## 架构评审(gpt-5.6-sol,独立)

**总体评分 8/10。** 五条架构观察:

1. **模块边界合理,核心耦合集中在 `Ctx/Fiber/Registry`**。`event` 定义类型化接口、`bus` 承担擦除与分发、`effect` 管理资源清理,职责清楚。`ctx ↔ fiber` 是单 crate 内围绕共享生命周期对象的相互引用,编译正常;架构上两者共同组成运行时内核,未来可考虑合并为 `runtime` 私有层,避免 `registry` 直接依赖 `FiberInner/Intent`([registry.rs:51](../rust/crates/min-cordis/src/registry.rs#L51)、[fiber.rs:98](../rust/crates/min-cordis/src/fiber.rs#L98))。

2. **公共接口与内部机制分层清晰**。`Plugin/Event/Listener/Terminal` 保持类型化,擦除适配器、`EffectRecord`、`TransitionTask`、`StoredValue` 均 crate 私有。主要泄漏是公共 `Disposer`/`FiberView` 返回 `Arc<CordisError>`,把内部"错误身份共享"策略写进 API([effect.rs:154](../rust/crates/min-cordis/src/effect.rs#L154))——可接受的生命周期契约,但会增加后续错误模型调整成本。

3. **类型擦除边界正确**。注册和调用入口保持泛型,异构容器内部才按 `TypeId/Any` 擦除;事件载荷借用擦除、返回值拥有式擦除,服务保存 `Arc<T>`,均符合所有权语义。运行时 downcast 风险限制在内部适配层。

4. **并发原语与语义匹配**。`watch`(最新状态+可重复 join)、`mpsc`(串行化状态机意图)、`Notify`(一次清理)、`CancellationToken`(协作取消)、std Mutex(短临界区)各司其职。**风险**:`UnboundedSender` 在高频 refresh/update 场景可能积压([fiber.rs:114](../rust/crates/min-cordis/src/fiber.rs#L114)),M4 前宜加意图合并或有界背压。

5. **扩展性中等偏好**。tower 适配层、`PluginFactory<Config>` 可作外围 crate/泛型构造层加入;`provides` 可经 `Plugin` 默认方法扩展。**`update` 会触及"配置烘进不可变 `Arc<dyn Plugin>`"及 generation 状态机**([plugin.rs:6](../rust/crates/min-cordis/src/plugin.rs#L6)),需新增明确的替换协议——这是 M4 唯一需要预先规划的架构改动。整体模型接近 Bevy 插件装配加 actor supervisor,适合动态生命周期框架;tower 保持适配层,不进核心。

## 处置优先级

1. **bug 1(apply 失败回滚)**——最严重,资源泄漏面最大,支柱 1 原子性。
2. **bug 2、3、4(三类永久挂起)**——并发路径致命。
3. **bug 6、7(panic 杀驱动)**——健壮性底线。
4. **bug 5(serial 顺序)**——语义偏差。
5. **bug 8、9、10、11**——已知竞态收尾。
6. **bug 12-15(agent)**——M3 质量。
7. **测试补齐 + clippy 清零**;删 `ServiceNotFound` 死变体;`TransitionTask` 内嵌 generation 对齐 D6 形状。
