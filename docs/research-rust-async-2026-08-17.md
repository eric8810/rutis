# Rust async/tokio 最佳实践调研(2026-08-17)

> 调研员:gpt-5.6-terra,来源:tokio/futures/Rust 官方文档。服务对象:design-rust-port.md v4。
> 每题:结论 + 来源 + 与设计决策匹配度。

## 1. dyn 兼容的 async trait

- 原生 AFIT 不能直接 `dyn`(Rust Reference dyn-compatibility:async fn 有 opaque 返回类型)。
- 推荐:显式 `type BoxFuture<'a,T> = Pin<Box<dyn Future<Output=T> + Send + 'a>>`;**与 `futures::future::BoxFuture` 定义完全一致**。
- `async-trait`(0.1.89 维护中)展开后即此形状,适合作者体验优先;dynosaur 是专门方案,非生态默认。
- 来源:doc.rust-lang.org/reference/items/traits.html#dyn-compatibility;docs.rs/async-trait;docs.rs/futures futures::future::BoxFuture。
- **匹配:手写 BoxFuture 为核心 ABI(高)。**

## 2. 不跨 await 持锁

- tokio 官方:std::sync::Mutex 用于短小、纯内存、低竞争临界区是正确选择;**不得跨 .await 持 guard**;跨 await 的 I/O 资源才用 tokio::sync::Mutex;高竞争先考虑重构/消息传递。
- std RwLock 文档给出读锁→等写锁→再读锁死锁示例;**持锁期间不得调用可能重入的回调**。
- "锁内快照/克隆回调,释放后执行"与官方"锁 guard 在 .await 前析构"的指导一致,是稳健框架模式;要求快照值语义有效。
- 来源:tokio.rs/tokio/tutorial/shared-state;docs.rs/tokio tokio::sync::Mutex;doc.rust-lang.org/std/sync/struct.RwLock.html。
- **匹配:完全;作为硬规则。**

## 3. 从同步上下文 spawn

- `tokio::spawn` 需要运行时上下文,否则 panic;`Handle::current()` 在无 runtime 时也 panic,`Handle::try_current()` 返回错误不 panic;Handle 可自由 clone。
- **库 API 最佳实践:优先注入 Handle;自动获取用 try_current() 返回明确错误;不隐式建 runtime。**
- 来源:docs.rs/tokio tokio::runtime::Handle。
- **匹配:修正 v3"构造捕获 current"为"注入优先 + try_current 兜底"。**

## 4. 一次性任务多观察者 + 错误 identity

- JoinHandle 是"owned permission to join",非多观察者广播。
- `FutureExt::shared()` 要求 Output: Clone,给各观察者克隆值;`Result<T, Arc<E>>` 可保错误 identity 但成功值也须 Clone,且无晚订阅。
- **`tokio::sync::watch`(MPMC last-value,Receiver 独立游标)最匹配"一次完成、多等待者、晚订阅、缓存终态、Arc 错误 identity"**。
- oneshot 是一发一收,需自管 fan-out,中匹配。
- `Arc<Error>` 共享 identity 是公认建模(anyhow::Error 非 Clone;identity = Arc::ptr_eq)。
- 来源:docs.rs/tokio JoinHandle/watch/oneshot;docs.rs/futures-util FutureExt::shared;docs.rs/anyhow。
- **匹配:v4 改用 watch(高)。**

## 5. 协作取消

- **优先 `tokio_util::sync::CancellationToken`**:clone 广播、`cancelled().await` cancel-safe、子 token 层级传播;tokio 官方 graceful shutdown 教程推荐。
- AtomicBool 仅适合同步轮询快路径:不会唤醒 await 中的任务,需自管内存序/唤醒/层级。
- 来源:tokio.rs/tokio/topics/shutdown;docs.rs/tokio-util CancellationToken。
- **匹配:v4 统一 CancellationToken(完全)。**

## 6. yield_now 语义

- `tokio::task::yield_now()` 把任务放回 pending 队尾,**不保证其他任务先运行,甚至可能立即重新 poll 当前任务;轮询顺序变化不算 breaking change**;可能被上层组合器拦截。
- **不能等价 JS `await Promise.resolve()` 微任务屏障**;不能承载"其他任务已见状态"的可见性/顺序正确性。提交可见性由锁释放/channel/watch 版本/ack 保证。
- 作为公平性/响应性 hint 合理。
- 来源:docs.rs/tokio tokio::task::yield_now。
- **匹配:v4 降级为纯 hint(修正 v3 的检查点语义)。**

## 7. 库错误设计

- API guidelines C-GOOD-ERR:错误实现 `std::error::Error`,通常 `Send + Sync + 'static`。
- **不强制 Clone**(非 guidelines 要求;错误常带非 Clone source/backtrace);并发共享用 `Arc<E>`。
- thiserror 派生、不泄漏进公共 API;聚合多错误:`Vec<E>`(或带插件标识的 Vec<PluginFailure>),或命名 `AggregateError{errors}`(注意 source() 单链,需自定义 accessor);富上下文需求才评估 error-stack。
- 来源:rust-lang.github.io/api-guidelines C-GOOD-ERR;docs.rs/thiserror;docs.rs/error-stack。
- **匹配:v4 CordisError enum + thiserror + Arc 共享 + Vec 聚合(高)。**
