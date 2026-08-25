# 工作文档:自动迭代闭环(hotplug_load)——agent 自己加载新能力

> 2026-08-25。用户核心要求:"你这么做,我们怎么能做到自动迭代"——
> 点破我一直在改冷启动代码。本实现让**运行中的 agent 自己加载新代码**。

## 闭环
```
agent 写新代码 → cargo build 成 .so → 调用 hotplug_load(path)
→ libloading 加载 → C ABI schema 翻译成 ToolDef
→ 注册进运行中的 ToolRegistry → 后续 turn 立即可用
全程不重启进程、不重编译主程序
```

## 实现
- `tools/hotplug.rs`:`hotplug_load` 工具(第 10 个 self 系)。
  参数 path(.so 路径);加载、读 C ABI(name/count/schema/invoke)、
  逐工具翻译成 ToolDef 注册进 ToolRegistry;返回注册的工具名。
- rutis-agent 加 libloading 依赖(必须放 [dependencies],不是 dev)。
- `examples/self_iterate.rs`:端到端演示——turn1 调 hotplug_load 加载
  librutis_hotplug_demo.so,turn2 直接调用新工具 release_notes。已跑通。

## 与热插拔 demo 的关系
- 14f8c53 的热插拔:宿主(程序)加载 .so,注册工具。
- 本实现:**agent 自己**通过工具调用加载 .so——自动迭代的"手"。

## 关键约束(延续 hotplug-dylib 文档)
- dyn Plugin 无法跨 cdylib → C ABI 数据 + 函数回调。
- 库 Box::leak 'static;invoke 转 'static 裸函数指针。
- 工具闭包需 'static → 库随进程存活。

## 验证
- self_iterate example 跑通:turn1 加载 → turn2 用新工具。
- 全 agent 套件绿(10 工具 schema 断言已更新)。
- cli + workspace check 绿。

## 意义
- 这就是"自动迭代":agent 改代码 → 编译 .so → 自己加载 → 立即用。
- 配合 self_build(编译)/self_check(测试)/self_todo(接续),形成完整闭环:
  **写 → 测 → 加载 → 用 → 记待办 → 重启后继续**。
