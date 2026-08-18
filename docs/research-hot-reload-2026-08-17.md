# 热更新案例梳理(2026-08-17)

> 服务对象:[design-rust-port.md](design-rust-port.md) v4。回答:"依赖门控热重载"有没有可参考的先例。
> 结论先行:**代码级热更**(dylib/热补丁)与**生命周期级热重载**(实例卸载/重装配)是两个正交问题;rutis(原 min-cordis)v4 做的是后者,先例在 Erlang/OSGi 一系;前者(dyxlib/subsecond)是可组合的互补能力。

## 一、两种热更新

| | 代码级(换机器码) | 生命周期级(换运行实例) |
|---|---|---|
| 换什么 | dlopen 新 dylib / 补丁运行中进程 | 代码不变;旧实例效果清掉、重跑装配(可带新配置) |
| 典型 | hot-lib-reloader、subsecond | Erlang 热升级、OSGi、Cordis update/restart |
| rutis v4 对应 | **不做**(正交,可组合) | **M2 核心**(fiber 驱逐/重载/restart) |

## 二、代码级案例(Rust 生态)

### hot-lib-reloader(Bevy 社区实用档)

- 机制:可变代码放 dylib crate → 文件监听 → 重编译 → dlopen 新库 → 宏生成包装层换函数指针;状态全部外置宿主,库只暴露 `#[no_mangle]` 纯函数。
- 教训:跨边界类型不能有泛型/inline;同编译器同版本才稳;Windows DLL 占用需改名复制加载。
- 来源:<https://robert.kra.hn/posts/hot-reloading-rust/> · <https://github.com/rksm/hot-lib-reloader-rs> · Bevy 官方 ECS 热更 issue 指向它:#15613。

### Subsecond(Dioxus 0.7 hotpatch,2025)

- 机制:拦截 rustc 链接阶段、手动驱动编译,直接对运行中进程打补丁;无需 dylib 工程拆分;跨 macOS/Windows/Linux/iOS/Android;alpha 阶段。
- 定位:前端/实时迭代的亚秒反馈,不是插件生命周期工具。
- 来源:<https://docs.rs/subsecond> · HN 44369642 · Bevy #19296(hotpatching systems)讨论引用。

### abi_stable / nullderef 系(放弃档)

- 目标:不同 rustc 版本编译的 dylib 安全互操作。
- 结论:代价高、坑多(ABI 稳定层、类型限制),多数项目放弃,转数据驱动。
- 来源:<https://nullderef.com/blog/plugin-abi-stable/> · <https://docs.rs/abi_stable/>。

### Bevy 本尊(佐证)

- 官方只内置**资产**热重载(数据);ECS 系统/插件的运行时热更停留在 issue(#15613、#19296),指向外部工具。**框架级插件卸载/重载无先例**——印证 v4 调研判断。
- 来源:<https://bevy-cheatbook.github.io/assets/hot-reload.html>。

## 三、生命周期级案例(rutis 范式的先例)

### Erlang/OTP(最正统祖师爷)

- 热代码升级协议:**挂起进程 → 加载新版本 → `code_change/3` 迁移状态 → 恢复**;supervisor 树按依赖顺序重启;"升级不是异常,是日常"。
- 映射:fiber unload → re-apply ≈ suspend/load/code_change/resume;Cordis 的 update()/restart() 即此思想的 JS 转世。
- v4 差异:**不做状态迁移**(code_change 那步)——reload = 干净的卸载+重装配;状态迁移留 M4 以后评估。
- 来源:<https://learnyousomeerlang.com/relups> · <https://stackoverflow.com/questions/37368376/>。

### OSGi(Eclipse 插件运行时,Java)

- bundle 生命周期(RESOLVED→STARTED→STOPPED)+ 服务注册表 + 声明式依赖:**依赖满足才激活,provider 消失自动解绑消费者**——与 Cordis inject 门控/驱逐逐点对应,"服务注册表+依赖驱动激活"的老牌先例。

### .NET AssemblyLoadContext(反面教材:卸载为什么难)

- 可收集 ALC 理论可卸载程序集,但**一条引用泄漏,旧程序集就赖在内存**;实践 TypeLoadException/卸不掉是常态。
- 启示:Rust 所有权恰好治此病——effect disposer 列表 + watch 终态 + Arc 归零,无 GC 悬挂引用。
- 来源:<https://jordansrowles.medium.com/real-plugin-systems-in-net-assemblyloadcontext-unloadability-and-reflection-free-discovery-81f920c83644>。

### 前端 HMR(React Fast Refresh 等)

- 模块级换新:能保的状态保,保不住整页刷新;错误边界兜住换件瞬间。
- 与 rutis"清理期自访问仍有效"(provide 卸载三段序)同类问题:**换件瞬间,依赖旧件的代码怎么办**。

## 四、落到 v4

1. **定位**:M2 的生命周期热重载(provider 卸载→驱逐消费者→就绪后自动重载)属 Erlang/OSGi 一系;Rust 生态无先例不是风险,先例在别语言范式,且 Rust 所有权是更好用的地基。
2. **不做状态迁移**:reload 即干净重装配(对应 Erlang code_change 的省略),已在 §一范式定义。
3. **正交组合点(写入 M4 候选)**:subsecond / hot-lib-reloader 负责"拿到新代码",rutis 负责"安全换件"——旧插件 dispose → 依赖者驱逐 → 新插件装配 → 依赖者重载。代码级工具不进核心,不做耦合。
4. **风险登记**:TypeId 跨 dylib 不稳定(bevy StableTypeId issue)——若未来真组合 dylib 方案,注册表键需换稳定 id;当前同二进制内动态分发不受影响。
