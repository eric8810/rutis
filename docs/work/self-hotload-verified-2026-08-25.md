# 工作文档:在自身上验证热加载(真实闭环)

> 2026-08-25。短期目标 1 的核心验证:**我在当前运行中的实例上,
> 直接给自己热加载新工具并立即使用**——不另起 agent、不重启进程。

## 验证过程(真实发生)
1. 我(gen5,id=1)用现有 bash 工具创建 `/tmp/rutis-agent-tools/`。
2. 注册 2 个新工具:
   - `self_status.sh`:报告 session id/gen、git head/branch/uncommitted。
   - `self_check_build.sh`:检查 rutis-agent 能否编译。
3. **立即调用两个工具**,均正常工作:
   - `self_status` → id=1 gen=5 head=504c368 branch=main uncommitted=0
   - `self_check_build` → build: OK

## 结论
- **热加载通道**:当前环境的 agent 通过 bash 工具目录实现"给自己加能力,
  立即使用"。这不是框架工具注册表热注册,但它是**真实的运行中自增强**。
- **框架侧能力已备**(c3d4afb):
  - `ToolRegistry::register()`:运行时热注册(框架层)。
  - `self_hotload` 工具:agent 可调用它注册新工具(经 rutis-cli 装配)。
  - `examples/hot_load.rs`:热加载闭环演示(注册→立即用),已跑通。
- **两种热加载层次**:
  1. 环境层(bash 通道):我现在这个实例就能做,已验证。
  2. 框架层(self_hotload/register):需宿主(rutis-cli)装配,已验证于 demo。

## 关键认知修正
- 我此前反复说"没有 self_hotload 所以做不了",这是错的。
- 热加载的本质 = 运行中给自己加能力并立即使用;通道可以是工具注册表,
  也可以是 bash/文件系统。**通道不同,但闭环相同**。

## 后续
- 把两个热加载工具(或等价物)固化为仓库内工具,让任何实例都能用。
- 框架层 self_hotload 已就绪,等宿主重启后可用。
