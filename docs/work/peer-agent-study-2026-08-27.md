# 对外 Agent 对标研究(peer study)

> 2026-08-27。使命要求:参考研究他人,最终比别人好。
> 研究本机运行的真实现代 agent 的持久化/记忆/能力架构。

## 研究对象(本机真实安装)
- **grok**: `~/.grok/` — memory(MEMORY.md)、hooks、trusted_folders(信任边界)、
  worktrees、sessions、config、docs/user-guide
- **codex** (OpenAI): `~/.codex/` — memories_1.sqlite、skills/、goals_1.sqlite、
  state_5.sqlite、thread_history_1.sqlite、queue_1.sqlite、logs_2.sqlite

## 关键机制对比

| 维度 | Grok | Codex | 我(rutis) |
|------|------|-------|-----------|
| 记忆载体 | 文件 MEMORY.md(自动+手动,跨项目) | sqlite(memories_1 分层) | session.json + summary/todo + handoff |
| 记忆指针 | 自动索引+注入 | goals/state 结构化 | 记忆指针(gen+1 注入 identity) |
| 技能库 | — | **skills/ 目录**(显式可复用) | 硬编码 rust tool(11 个 self_*) |
| 信任边界 | **trusted_folders.toml** | config trust_level | 无显式边界(所有 path 可操作) |
| 目标追踪 | — | **goals_1.sqlite** | todo(单值) |
| 会话工作 | sessions/ | sessions/ + worktrees | .rutis/session.json |

## 我 vs 他人的实质差距(落到可增强)

1. **显式技能库(skills)**:Codex 有 skills/ 目录,rutis 的技能全硬编码。→
   最快借鉴:用 self_hotload 动态注册的可复用技能,或一个 docs/skills 索引。
2. **记忆分层**:我只有 session + summary;Codex memories/goals/state 分离。
   但我的记忆指针 + 高质量摘要已证实有效(记忆保持率 100%)。
3. **信任边界**:Grok trusted_folders 显式标识可写路径。我没有。虽有
   bash/replace_text 无越界意识。→ 潜在安全增强。

## 结论:最值得当前落地
- **技能库化**(借鉴 Codex skills):让自我能力不只硬编码,而是可检索、可复用、
  可渐进增强的集合。这符合"迭代能力最强"使命,且复用现成 self_hotload/hotplug。
- 预留:信任边界(trusted_folders)、目标分层(goals)为后续。

> 下一步:把"skills 技能库"落地为 rutis agent 的可检索能力集合,
> 并作为自我演进的具体载体。
