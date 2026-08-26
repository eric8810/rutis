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

## grok sessions(17-sessions)深研(round 42)
- **grok session 架构**:updates.jsonl(权威日志)+ summary.json(索引:model/timestamps/
  message-count/parent)+ compaction_checkpoints/(压缩检查点)+ chat_history.jsonl。
- **能力**:/resume(浏览恢复)、/rewind(恢复文件到更早点 + 截断历史)、
  /compact(手动+auto),、/flush(保存)。
- **vs 我(逐项)**:
  | grok | 我 | 判定 |
  |------|-----|------|
  | /resume | Session::restore + 多代 | ✅ 已有 + 跨代 |
  | /rewind 截断历史 | Session::truncate_to | ✅ 已有(历史截断) |
  | /rewind 恢复文件 | — (git/host) | 🔖 host 层职责(同 trust-boundary) |
  | /compact 手动+auto | compact + auto_compact | ✅ 已有 |
  | compaction_checkpoints | 无(原地压缩) | 🔖 价值低:我压缩100%保真
    (实验3),摘要已含关键信息,无需检查点回退(非破坏性) |
  | summary.json(索引) | session.json(单文件) | ~ 我单 session 连续跟踪,无需多索引 |
- **判定**:session 健壮性(restore/resume/rewind-truncate/compact)我全有;
  compaction-checkpoint 对我价值低(高保真压缩非破坏);rewind 文件恢复属 host。
  研究消化闭环。无新落地。

## grok Dream 整合深研(round 41)
- **grok /dream**:把散落的 session logs + memory entries 整合成有组织、去重的
  知识库,减少噪声、改善搜索;Auto-Dream 按 min_hours=4 / min_sessions=3 门控
  session 结束时自动跑。
- **前提**:grok 是"多 session + 向量检索",碎片会累积,故需整合。
- **vs 我**:
  - 我单 `.rutis/session.json` 连续跟踪(非多 session 碎片);跨代保持 100%
    (实验1 已证) → 无"整合去重后更连贯"的需求。
  - 长期知识 = 技能库(docs/skills,显式、去重、按需检索) = Dream 的
    "去重知识库"等价载体。
  - 技能库审查:12 个 SKILL 主题独立不重叠,本就显式去重 → 无需再 Dream。
- **判定**:Dream 服务于多 session + 检索架构;我的单 session + 技能库已等价
  覆盖,不需要落地(同 time-decay/首轮注入判定)。研究消化闭环。

## grok hooks 机制深研(round 39)
- **grok hooks**:在 `.grok/hooks/` + config 层 `[[hooks.<Event>]]` 定义项目脚本,
  在工具前后(pre/post-tool-use)和会话开始/结束跑。`matcher = "Bash|Write|Edit"`
  按工具匹配;hooks 被显式信任才执行。本质 = 工具/会话生命周期挂钩脚本。
- **我已有等价物(更内建)**:driver「工具三段管线」= `tools/pre-execute` 门控
  (fail-closed,可拒绝)→ 执行 → `tools/post-execute`(accept/replace)。经 events
  broadcast,listener 可见 `ToolCall`(含 tool_name)实现 matcher 过滤——覆盖
  grok PreToolUse/PostToolUse 核心,无需外部脚本进程。
- **无 session start/end hook**:但有 session start 事件与每 turn persist(启动即
  可监听事件);end 由持久化覆盖。→ 不需额外落地。
- **判定(实证锁定)**:hooks 机制被我现有三段管线等价覆盖,不额外引入外部脚本系统。
  **测试锚点** `integration.rs::pre_execute_gate_can_block_specific_tool`(round39)
  证实:按 tool_name 门控可拒绝特定工具 + 该工具不执行 + 模型看到原因 = grok
  PreToolUse matcher 的等价能力。研究消化闭环(有 test 而非仅判断)。
- **补全 PostToolUse 实证(round39b)**:`integration.rs::post_execute_can_rewrite_result`
  证实 tools/post-execute 可改写工具结果(脱敏),敏感性原值不达 session = grok
  PostToolUse 等价能力。三段管线 = Pre+Post 双钩子,均有 test 锁定。integration 7/7。
- 同 codex goals 深研(round 38):研究他人 → 认清我已有 → 判断不需要。

## codex 目标预算(thread_goals)深研(round 38)
- **表结构**:`thread_goals`(thread_id PK, goal_id, objective, status
  CHECK active/paused/blocked/usage_limited/budget_limited/complete,
  token_budget, tokens_used, time_used_seconds, 时间戳)。
  **不是多目标列表**;是"每线程一个带资源预算的目标"。
- **核心增量 vs 我**:`max_steps` 仅"每轮步数上限"(每轮重置)。codex 是
  "进程级跨轮累计 token 预算 + 经济状态机"。隐患:很多轮各自不超 max_steps,
  但整体烧掉巨额 token(成本失控)是 max_steps 未覆盖的真实缺口。
- **计量源可用**:`aimux_core::result::GenerateResult.usage: Usage`(UsageSnapshot:
  input_total/cache_read/cache_write/output_total/output_reasoning)—— 非空账。
- **当前判定**:真差距,但**前瞻性**。现状 mock/replay 环境 usage 全 0,不可测;
  改核心循环(stream→usage 采集 + 跨轮记账 + 超限状态)成本中高、当前无真收益。
  → 记录为前瞻设计:真实后端跑起来后,在 driver 循环加"累计 usage vs token_budget
  超限中断 + budget_limited 状态"。现在不动核心循环(避免为不可测机制引入风险)。
- **已落地(round63)**:真实后端确立后(round44),前提消除,落地第一增量——
  Session.tokens_used(跨轮累计,serde 兼容)+ add_tokens(saturating)+ driver
  流式 Finish 捕获 usage 累计 + self_status 暴露。测试含 driver 捕获链路
  mock(input10+output5=15)。session_persist 24/24。
- **已完整落地(round65)**:token_budget 上限中断成本硬保护——评估改动面
  (AgentDriver::new 仅 plugin apply 一处调用)+ 默认 None 向后兼容后落地。
  `AgentDriverPlugin.with_token_budget` + run_loop 每 step 检查累计 tokens_used
  vs budget 超限返回 `AgentError::BudgetLimit{budget,used}`。测试
  token_budget_limit_interrupts_turn(UsageLlm2: 150>100 跨轮触发)。
  → 研究→defer→前提变化→完整落地闭环。全景 240 passed。
  未做完整 status 状态机(active/paused/blocked 编码),超出核心价值,未过度。

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

## 生产代码 panic 面审查(round 46)
- **扫描** driver/tools 生产代码 44 处 unwrap/expect,排除测试模块后分类:
  - `.lock().unwrap()`:Mutex 中毒防护。driver 已有工具 panic 边界
    (tool_panic_is_fed_back)+ turn 级 rollback,锁在正常路径不 poison。
    理论边缘风险,非实际,不改(避免动已验证安全代码)。
  - `.expect(...)` 断言(测试模块内):断言不变量,正常。
  - `Ctx::root()/provide_as/fiber await expect`:装配失败即时 fail,正确。
- **结论**:生产 unwrap/expect 均被成熟机制(panic 边界 + rollback)保护,
  无此需改为 Result/Error 的实质风险点。审查为确认性,非发现 bug。

## 自主驱动引擎(SelfDriven)机制审读(round 47)
- **语义确认**:SelfDriven = "永不停止 + 智能调频"。backoff(200ms base→30s max)
  按失败 streak(FAIL_DECAY=3 指数退避)/ 有产出(恢复高频)/ 无产出(降频)动态调。
  但**永不死**——`stops_when_no_progress` 防的是"消息不增长时空转",非"停机"。
- **测试已锁**:self_activates_without_todo(激活)/ stops_when_no_progress(防空转)/
  auto_activation_turns_are_not_cancelled(turn_lock)。核心语义行为级覆盖。
- **backoff 数值是否补测试**:不补。需暴露内部 AtomicUsize(破坏封装 + 绑调谐
  常量脆弱);已有"消息不无限增长"行为断言兜底空转保护。判定:过度测试,不做。
  同 round45 纪律:不为做而做。
- **结论**:SelfDriven 语义健康,与 round44 实机 + round45 guard 测试一致,无待力改。

## driver session 路径宿主分层判定(round 53)
- `default_session_path()` 用 current_dir(相对 .rutis/session.json)。
- **不是 cwd bug**(区别于 skill/ledger/handoff 三处修复):
  - skill/ledger/handoff = 仓库内**固定资源**,应锁仓库根(依赖它们的工具内部
    隐式硬编码相对路径,我无法避免 → 已修复)。
  - session = 运行时**数据**(每项目一 session),跟启动 cwd 走是合理设计;
    且默认 None(`AgentDriverPlugin::new()`),由宿主**显式**选
    `with_default_session_path()`(跟 cwd)或 `with_session_path(显式绝对)`。
- CLI 用法确认:main.rs:257 人用默认跟 cwd;376/513/567 显式指定。均合理。
- **结论**:session 跟 cwd 是正确行为,勿动(同为 host 层职责判断,round35 信任
  边界 round36 subagent 一致治理)。
