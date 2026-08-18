# Rust 移植设计 v2 二审报告 — gpt-5.6-sol(2026-08-17)

> 审阅对象:docs/design-rust-port.md v2(已按本报告修订为 v3,见 v3 §0"v2→v3"表)。
> 基准:src/ TS 源码。裁决:**v2 尚未达到放行 M1 的条件**(较 v1"不放行"显著进步,8 类前提 4 已修/5 部分修/0 未修,但"部分修"含 3 个 M1 级阻断)。

## 最终判断

**M1 不放行;M2 设计覆盖显著提高但不能按现稿宣称完整落实;“M2 后 85-90%”声明偏高。**

覆盖率:照 v2 稿实现 ≈ 65-75%;修复本报告阻断后 ≈ 78-85%;M4 后 ≈ 85 左右。目标表述建议改为"复现 Cordis 核心生命周期、事件、依赖与插件管理语义,对 JS 动态对象/Proxy/traceable 面提供 Rust 能力替代"。

## 三个 M1 级阻断

1. **`ListenerReturn` 无错误通道**(D2/D7 自相矛盾):`Sync(Value)`/`Async(BoxFuture<Value>)` 不含 Result,无法实现"同步 Err 照抛 / 异步 Err 进 sink / parallel 聚合全部 Err"。需 `Sync(Result<...>)` 或独立可恢复错误类型。
2. **waterfall 被统一异步化**:TS `waterfall()` 是同步函数直接返回值(events.ts:245-254);`Next -> BoxFuture` 使 `internal/get/set`(reflect.ts:153-167/191-193)、`_resolveConfig`(fiber.ts:743-746)、ACTIVE update 的 validate-before-store 全部被迫 async 或 block_on(tokio 下死锁)。需 Sync/Async 双态续延返回。
3. **`serde_json::Value` 不能承载能力值**:internal/plugin 传 Fiber、internal/listener 换 disposer、internal/get 返回 service 对象、internal/get/set 参数含 Context/Error——需 DynamicValue 分层或 typed internal API。

## M2 级问题

- **RuntimeKey 身份未裁决**(§8 开放):TS 是 resolve(plugin)→apply 引用身份;"每注册处分配 id"会把本应合并/分离的 runtime 判反。
- **shared future 输出须 Clone**:`Result<(), CordisError>` 的克隆语义/错误 identity 未定义;建议 `Arc<CordisError>`。
- **store 键矛盾**:映射表 `HashMap<LabelId, Impl>` 与 D9 "(LabelId,name)" 互相矛盾;TS 是单 symbol 键。
- **C4 与 M2 冲突**:C4 把 iterable/async-iterable 列为核心契约,M2 又不做 Stream——三处冲突。
- **intercept 里程碑**:v1 标准 M2 必须,v2 移 M4 但又宣称"M2 完成 C1-C9 全部/85-90%",自相矛盾。
- `apply: fn` 指针不能捕获闭包状态,需 trampoline 或 Arc<dyn Fn>。
- observer(internal/plugin/status)逐回调隔离无明确实现决策与专门测试。

## 其他发现

- **emit async 同步前缀**(高风险):Rust async fn 首次 poll 才执行;直接 spawn 丢失 TS"执行到第一个 await"的前缀;需 poll-once/构造期约定/明确偏离。
- once(dispose-then-call)与快照分发自洽,但需补 3 个重入测试。
- **行号抽查 18 处全部属实**(2 处偏移:emit 实止 206、dispatch 止 179)。
- **测试计数错**:tests/*.spec.ts 实为 **96**(文档写 62);dispose.spec 13 非 14;plugin.spec 10 非 11——测试矩阵未完成逐测试映射,不能据此证明 85-90%。
- 偏离清单:v2 重定性正确(traceable 列能力损失、ErrorSink 进 M1 等);仍缺 5 项(JSON-only 限制定性过轻、同步 waterfall 改 async、async 前缀、闭包插件状态、错误 identity/Clone)。

## M1 放行最小修订(7 项)

listener 返回带 Result;emit 前缀方案或偏离声明;waterfall 同步/异步双态(保 internal/get/set 不 block_on);废除"全 Value"、capability-aware DynamicValue;internal/listener 替换类型可实现;once+快照+重入测试;96/13/10 计数修正。

M2 放行前另需 7 项:RuntimeKey 精确身份;shared 输出 Clone/Arc 方案;store 键统一;intercept 里程碑一致化;C4/M2 冲突消除;apply 捕获状态方案;observer 逐回调隔离测试。
