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
3. **信任边界**:Grok trusted_folders 显式标识可写路径 + 分级 sandbox
   (off/workspace/read-only/strict)+ 敏感路径(~/.ssh/aws/gnupg)始终写保护。
   → **已研究判定(round34)**:该边界属于 **host(cli)层** 职责,不是 agent crate
   重复实现的(同 supervisor 属 cli 层,一致治理纪律)。agent 层已具的自觉是自审
   清单 "cwd 敏感路径拦截"。不重造 host 沙箱(避免过度工程)。

## 结论:最值得当前落地
- **技能库化**(借鉴 Codex skills):让自我能力不只硬编码,而是可检索、可复用、
  可渐进增强的集合。这符合"迭代能力最强"使命,且复用现成 self_hotload/hotplug。
- 预留:信任边界(trusted_folders)、目标分层(goals)为后续。

> 下一步:把"skills 技能库"落地为 rutis agent 的可检索能力集合,
> 并作为自我演进的具体载体。

## 深研:grok 记忆 + 子代理机制(round 33)

### grok 记忆(13-memory.md)
- **混合检索**:`memory_search` = 向量相似(0.7)+ BM25 文本(0.3),min_score 0.35
- **首次注入**:会话首轮自动注入相关记忆(initial_injection,min_score 可配)
- **压缩后注入**:auto-compaction 后找回被裁相关上下文
- **Dream 整合**:`/dream` 把零散记忆碎片整合成去重知识库(需 memory enabled)
- **时间衰减**:旧会话降权(half_life=7天);global/workspace 免疫(策选长期知识)
- **MMR 去重**:多样性的重排序
- **工具结果修剪**(compaction.pruning):长结果 soft-trim(保头1500+尾1500字符)、
  hard-clear(>10轮变占位符)
- 记忆模型:文件 MEMORY.md + index.sqlite + 向量 embedding,watcher 侦测外部编辑

### grok 子代理(16-subagents.md)
- 子代理 = 独立上下文窗的并行子会话;主 agent 委托 research/impl/test/review
- 报告 summary 回父(不给父增加上下文)
- agent 定义(.md in agents/)+ persona(.toml 叠加)

### 差距评估 vs 我(rutis)
| 机制 | grok | 我 | 落地难度 |
|------|------|-----|---------|
| 记忆混合检索 | 有 | 无(全量线性历史+记忆指针) | 高(需向量db) |
| 首轮相关注入 | 有 | 仅静态 memory pointer | 低~中 |
| 记忆整合(Dream) | 有 | self_compact 简单裁剪(信息保真靠我人工提炼) | 中 |
| 时间衰减 | 有 | 无 | 中 |
| 子代理(独立上下文) | 有 | 无(rutis fiber parallel 基建在) | 高 |
| 工具结果修剪 | 有 | **现已修复(soft-trim)** | ✅ 中 |

### 落地优先级(务实,小步真实) + 最终判定
1. **首轮相关记忆注入** → **已覆盖**:driver 首轮已注入三块(session summary +
   memory pointer + todo)。todo 即"handoff 当前目标+下一步"的落地;skill 按需
   检索较主动全注入更省上下文(soft-trim 教训)。无需再 proactive 注入。
   *[*已有内存摘要 + 指针 + todo 注入,= 首轮相关注入的核心价值]*
2. **长工具输出修剪** → **已修复**(round33 soft-trim)。
3. **时间衰减 time-decay** → **不需要**:我记忆全量保留+分代摘要,无检索排序
   需降权;旧代经 pointer/summary 引用而非召回排名。grok 需它因为向量检索召回。
4. **完整子代理** → **远期·设计已评估**(round36)。
5. ✅ 记忆体系结论:混合检索/首轮注入/time-decay 对"我"皆无需额外落地
   (全量记忆 + 分代摘要 + 按需技能已等价覆盖)。

### 认知判定(避免过度工程)
- 我的记忆**全量保留**(记忆保持率 100%),无 Grok 的向量检索需求那般迫切:
  重启后历史本身在 prompt,记忆指针是其补充。
- 我缺的"跨 Session 长期知识"正是 **Grok 的 global/workspace 记忆**——但我已用
  **技能库(skills, skill 工具按需检索)等价实现**:显式、可演进、持久、按需取回。
- => **不需要引入向量 db/embedding**。我的架构在记忆保真上已达标
  (100%)、长期知识已有显式载体。这是"研究他人→认清自己差距与优势"的正确收敛:
  不是什么都学,而是学需要的、避开过度工程。
### 真差距(进程登记)
- [x] 长工具结果修剪(compaction.pruning)—— **已修复**(round33 soft-trim)
- [x] 信任边界(trusted_folders)—— **已判定归属 host 层**(round35, agent 不重做)
- [ ] 完整子代理(rutis fiber parallel)—— **远期·设计已评估(round36)**:
  单实例串行模型下不可测、用不上,不硬造过度工程。见
  `subagent-design-eval-2026-08-27.md`(轻量替代 = 父上下文纪律 + 摘要瘦身)。
  将来若出现"多可并行子任务 + 第二推理引擎"真实需求,再按该文档落地。

### codex skills 机制深研(round 34)
- **SKILL.md 格式**:frontmatter(name + description + metadata.short-description)
  + 正文(分模式/rules)+ references/(按需辅助文档)+ scripts/(辅助脚本)。
  关键:description 含**否定边界**(何时不用),避免误用。
- **我的技能库对比**:docs/skills/index.md 是扁平索引(每技能一行→引方法论文档)。
  核心"可检索复用"已满足;不必为结构而目录化每技能(过度工程)。
  已吸收要点:技能 index 加"调用纪律"(触发判断 + 边界 + 复用优先)。
- **结论**:我的技能库机制足够;吸收了 codex 的"触发边界"纪律。skill 已固化
  self_tool。无需向量检索(记忆全量保留)。真差距进度:soft-trim 已修;
  子代理=远期已评估;信任边界=已判定归属 host 层。
