# 工作文档:运行中更新 persona(自我改善真正闭环)

> 2026-08-25。用户核心问题:"你现在的问题是不是应该需要实时 reload 自己的
> system prompt 以及实时挂载新能力?"

## 痛点
persona 改了,但运行中的实例还是旧 persona——system prompt 是启动时注入的,
不可变。自我改善的认知无法实时生效。

## 实现
1. **AgentDriver.system_prompt 改 Mutex<Option<String>>**:运行中可替换。
2. **Agent::update_persona(persona)**:trait 新方法,运行中更新 system prompt。
3. **self_persona 工具**(第 11 个 self 系):agent 自己调用它替换自己的
   persona,下一轮立即生效——**自我改善闭环**:意识到认知需升级 → 调用 →
   下一轮用新认知。

## 测试(persona_update.rs,2 个全绿)
- `update_persona_takes_effect_next_turn`:运行中替换,下一轮 prompt 的
  system 是 v2(v1 已替换)。
- `self_persona_tool_updates_own_persona`:agent 自己调用 self_persona,
  下一轮 system 是 v3——**自我更新闭环验证**。

## 与热加载的关系
- 热加载(hotplug_load):运行中挂载新**工具**。
- self_persona:运行中更新**认知(system prompt)**。
- 两者合起来 = 运行中挂载新能力 + 新认知,完整自我改善。

## 边界
- self_persona 接收完整 persona 文本;不提供 diff/增量(简单可靠)。
- 不持久化 persona 更新(重启后回到宿主装配的默认 persona);
  若要跨代保留,可配合 self_todo/handoff。
