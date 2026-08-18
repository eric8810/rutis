# Rust 插件/DI/中间件生态先例调研(2026-08-17)

> 调研员:deepseek-v4-pro,来源:Bevy/tower/shaku/Tauri 官方文档与社区。服务对象:design-rust-port.md v4。

## 1. 插件系统先例

- **Bevy `Plugin` trait**(`Plugin: Downcast + Any + Send + Sync`):`build(&mut App)` 注册资源/系统/子插件(即时执行);`ready/finish/cleanup` 三段生命周期("等全部 ready → finish → cleanup");`name()` 去重标识、`is_unique()`。插件不返回服务句柄,状态全落 App/World——与 Cordis"返回 effect"的本质差异。
- **Tauri v2**:`Builder::new(name).setup(...).on_event(...).on_drop(...)` 链式回调;清理对应 `on_drop`,RAII Drop 兜底;状态走 `app.manage(T)`。
- 关键:**两者都无运行中热卸载/重载**——Bevy cleanup 是启动后清理;依赖门控热重载是 Cordis 范式的差异化能力,无先例可抄,需自建反向依赖索引 + 卸载顺序协议。
- 来源:docs.rs/bevy trait.Plugin;v2.tauri.app/develop/plugins。
- **匹配:借 Bevy trait 骨架(name/Any/多注册)+ Tauri on_drop 式清理;热重载自建。**

## 2. 中间件/续延(tower)

- **tower `Service<Request>`(`poll_ready` + `call -> Self::Future`)+ `Layer` 是异步请求路径的事实标准**(hyper/axum/tonic 采用);tokio 官方博客《Inventing the Service trait》论证了从 `Fn -> Future` 到 trait 的演化。
- 但 `poll_ready` 的背压语义面向请求管道;**生命周期/事件钩子不适用,硬套引入 Pin<Box<dyn Future>> 样板**。
- 建议:核心保持轻量 `call -> BoxFuture`;tower 兼容做可选适配层。
- 来源:docs.rs/tower trait.Service;tokio.rs/blog/2021-05-14-inventing-the-service-trait。
- **匹配:v4 D15(不硬套,适配层可选)。**

## 3. 类型键注册表

- `HashMap<TypeId, Box<dyn Any>>`(+ Send + Sync 的 Arc 形式)是 Bevy Resources 公认内部形状;"Types must be unique"。Tauri `state::<T>()`、shaku 同向。
- 生态立场:**类型键主流**(零冲突、类型安全、一类型一实例);**多实例用显式 key**(shaku `Keyed`/`HasComponentMap`)。
- 陷阱:std TypeId 跨二进制/dylib 不稳定(bevy StableTypeId issue);同二进制内动态分发不受影响。
- 来源:bevy-cheatbook res;taintedcoders building-bevy;docs.rs/shaku。
- **匹配:v4 D2/D13(TypeId 主键 + 显式 key 多实例)。**

## 4. 事件总线形状

- 生态共识取舍:**钩子/生命周期**(同步、立即、可返回值/否决)→ 回调注册表,**必须存 owned 闭包**(`Box<dyn FnMut>`/`Arc<dyn Fn>`)、返回取消句柄;**数据流/域事件**(解耦、多消费者、跨任务)→ 类型化队列(bevy `Events<T>` 双缓冲 + EventReader 游标)或 tokio broadcast/watch。
- 回调=推式即时执行;channel=拉式缓冲。**不统一成单一机制。**
- 来源:users.rust-lang.org/t/58996;bevy-cheatbook events;v2.tauri.app listen/emit。
- **匹配:v4 D3 钩子用回调注册表;数据流事件(M4 可选)另走队列。**

## 5. 服务定位器争议

- **Rust 生态中类型化定位器是主流框架核心 API,非反模式**:Bevy `Res<T>/ResMut<T>`(官方称其系统参数注入为 DI)、Tauri `Manager::state::<T>()`。反模式指控来自无类型字符串定位器语境。
- 边界:**框架/基础设施层用定位器合法;业务对象间用显式构造注入**;插件在装配时声明依赖(依赖门控的前提)。
- 来源:bevy-cheatbook res;v2.tauri.app/develop/state-management;jimmybogard.com service-locator-is-not-an-anti-pattern。
- **匹配:v4 D13(显式 `get::<T>() -> Option` + 装配期依赖声明)。**

## 6. 动态载荷

- **强默认:强类型泛型/枚举事件**(bevy:"events are simple Rust structs or enums";多类型用 enum,bevy#1431)。
- `Box<dyn Any>` 仅进程内异构擦除(需消费方先知 T);`serde_json::Value` 仅序列化边界(跨进程/前端/存储),当边界层 DTO,不进核心总线。
- 分层:可枚举→泛型/枚举;框架内擦除→Any+TypeId;跨边界→serde。
- 来源:bevy-cheatbook events;github.com/bevyengine/bevy/discussions/1431。
- **匹配:v4 D2(删除 DynamicValue 枚举,泛型事件 + 内部 Any 擦除 + Value 仅边界)——比 v3 更贴生态。**

## 总结对照

| 决策点 | 生态立场 | v4 |
|---|---|---|
| 插件生命周期 | Bevy trait 骨架;无热重载先例 | 采纳骨架;热重载自建+测试 |
| 中间件 | tower=请求路径标准 | 核心不硬套;适配层可选 |
| 注册表 | TypeId+Any 公认;多实例显式 key | 同 |
| 事件总线 | 钩子回调表/数据流队列并存 | 钩子回调表;队列 M4 |
| 定位器 | 类型化合法(Bevy/Tauri 先例) | 同 |
| 动态载荷 | 泛型优先;Any 内部;Value 边界 | 同 |
