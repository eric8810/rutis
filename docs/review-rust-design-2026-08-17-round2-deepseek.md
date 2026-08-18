# Rust 移植设计 v2 二审报告 — deepseek-v4-pro(2026-08-17)

> 审阅对象:docs/design-rust-port.md v2(已按本报告修订为 v3,见 v3 §0"v2→v3"表)。
> 基准:src/ 全部 9 文件 + tests/*.spec.ts(实测 96 条)+ test/core.test.ts(10 条)。裁决:**M1/M2 均未达"直接放行",已从 v1"不放行"推进为"小修后放行"**。

## 一句话结论

v1 的七个爆炸点:5 已修、2 部分修;v1 §2"必须"机制 15 项无一漏项。但并发/async 修订留 **3 个 M1 级阻断(A/C/B)**与 **4 个 M2 级协议缺口(D/E/F/G)**。覆盖率估计(修复后)M2 ≈ 85-90%,events 面声称对齐度偏高 1-2 条。

## M1 级阻断

- **A(编译级)D3 waterfall 签名生命周期未绑定**:`dyn Fn` 里四个 `'_` 各自独立晚绑定,无法把 Next 借期、`&mut [Value]` 借期、返回 BoxFuture 借期绑成同一 `'a`。需 helper trait:`fn call<'a>(&'a self, ...) -> BoxFuture<'a, Value>`。v1 3.1 换马甲重现。
- **B(语义偏离)emit 丢失 async 监听器同步前缀**:TS emit 同步调用 cb,async 体到首个 await 前同步执行(events.ts:200-207);Rust future 惰性,spawn 后才 poll。poll-once(noop waker)或如实列偏离。
- **C(自相矛盾)同步错误通道缺失**:枚举无 Err 与 D7"同步 Err=照抛"冲突;TS parallel 把同步 throw 也聚合(events.spec `members(['test','async'])`)、serial 的 `await cb()` 把同步 throw 转拒绝;"枚举无 Err/需当值聚合/拒捕 panic"三者互斥,§6 声称的 parallel 对齐自相矛盾。

## M2 级缺口

- **D epoch 表示与原子时序**:“内容哈希**或序号**”——序号破坏等值合并(reentrant.spec coalesces);epoch 与 generation 两个原子分读撕裂(TS 是单一 token,fiber.ts:739);D6 AtomicU64 与 D10"锁内读"矛盾。
- **E 换链协议未钉 + 措辞错误**:TS `_setEpoch`(fiber.ts:704-718)是"更新 epoch + 完成时重读续链",非命令队列;lost-wakeup(解锁置 None 与并发 epoch 变更竞态)无协议。
- **F internal/dispatch 必须先于快照**:TS dispatch()(events.ts:170-180)先 emit internal/dispatch 再构建监听列表,其中注册的监听器对本轮可见;v2 未钉先后。
- **G Value 载荷与函数型参数冲突**:internal/get/set 的 ctx/error 非 Value;internal/dispatch 的 waterfall args 含 next 函数(internal-hooks.spec:36-40 钉住 `typeof seen[4][2][1] === 'function'`);per-fiber `_hooks` 的 Rust 落点未给。

## 核销摘要

- v1 §3.1-3.6 七爆炸点:BoxFuture<'a>✓、快照重入✓、Handle✓、futures-util✓、store label 键✓;inertia 部分修(D/E)、waterfall 部分修(A/G)。
- v1 §5 M1 三前提:BoxFuture/Arc/Handle✓;waterfall、parallel/serial 部分(A/C/B)。
- v1 §5 M2 前提清单:11 项全部有落点,唯 per-fiber `_hooks` 表示未展开(G)。
- v1 §2"必须"15 项:无一漏项。
- 偏离清单 9 项"应列未列":全部落实,定性正确。
- **契约行号抽查 12 处全部准确**(2 处轻微偏移:inertia 字段在 fiber.ts:213;EffectRecord 实为 429-634)。
- **测试矩阵**:62→96 错;dispose.spec 14→13;plugin.spec 11→10;**reflect.spec(4 条)整体缺失**(inject leak→C3、重复 provide→C3、Context.is→非目标、mixin→B6)。

## 裁决

- **M1**:暂不放行——先修 D2 错误通道 + D3 签名 + 补列 B 偏离。
- **M2**:有条件放行——补齐 D6/D10 协议(epoch 原子时序、换链、internal/dispatch 先于快照、Value vs 函数型参数)后可动工。
- 覆盖率:修复后 M2 ≈ 85-90%;events 面因 A/C 残余 1-2 条偏差(parallel 同步错误聚合、internal/dispatch 诊断 fidelity)。
