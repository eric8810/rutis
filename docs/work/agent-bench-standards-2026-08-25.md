# 工作文档:agent 评价标准调研 + 自迭代实验体系(短期目标 2)

> 开始:2026-08-25。目标:采集业界 agent 评价标准与能力维度,制定
> rutis agent 自己的迭代实验与测试,并据此升级。

## 一、业界 agent 评价标准(调研)

### 1. 能力维度(coding agent 通用)
| 维度 | 代表 | 测量方式 |
|---|---|---|
| 任务完成率 | SWE-bench / HumanEval | 真实 issue 解决率 |
| 工具使用正确性 | tau-bench / ToolBench | 工具调用准确率、错误恢复 |
| 多轮/长会话 | LongContext / MemGPT 基准 | 长上下文下的记忆保持 |
| 自我修正 | Self-Debug / Reflexion | 失败后能否自我修复 |
| 环境适应 | 各类 sandbox 测试 | 不同环境/后端的成功率 |
| 成本/效率 | token 消耗 / 延迟 | 单位任务成本 |

### 2. rutis agent 可借鉴的(结合本项目现状)
- **记忆保持**:session 持久化 + 记忆指针 + 记忆压缩 → 应有一个"跨代记忆保持率"指标。
- **自我迭代**:self_build/self_check/self_hotload/self_reload → "自我演进闭环完成率"。
- **故障恢复**:Supervisor 自动重启 → "失败恢复时间/成功率"。
- **工具使用**:bash/replace_text/self_* → "工具调用成功率/错误回喂率"。

## 二、自迭代实验体系(设计)

### 实验 1:跨代记忆保持率
- 场景:多轮对话 → 重启(gen+1)→ 模型是否记得关键事实。
- 指标:重启后追问"我们之前讨论过什么",模型回答包含关键事实的比例。
- 工具:scripted/真实 LLM 都可跑;断言 prompt 含历史 + 记忆指针。

### 实验 2:热加载能力验证
- 场景:agent 调用 self_hotload 注册新工具 → 后续 turn 使用。
- 指标:注册成功率、新工具调用成功率、注册到可用的轮次延迟。
- 工具:examples/hot_load.rs(已跑通)。

### 实验 3:长会话压缩有效性
- 场景:长会话 → self_compact → 后续模型仍能引用早期信息(经摘要)。
- 指标:压缩后模型回答准确率 vs 压缩前(摘要信息保留率)。
- 工具:session_persist.rs compact 测试(已绿)。

### 实验 4:督工自动恢复
- 场景:连续失败 → Supervisor 自动重启 → 恢复。
- 指标:恢复时间、重启成功率。
- 工具:rutis-cli Supervisor 测试(已绿)。

## 三、迭代升级路线(基于实验结果)

1. 记忆保持率低 → 增强记忆指针/摘要质量(模型生成摘要)。
2. 热加载不稳定 → 加运行时工具冲突检测/回滚。
3. 长会话仍退化 → 分层摘要(滚动压缩)或自动触发压缩。
4. 恢复慢 → 优化重启路径(当前 300ms 延迟)。

## 四、下一步(执行)
- [ ] 跑一遍实验 1-4,记录基线数字。
- [ ] 选一个短板(预计是记忆保持率/摘要质量)做升级。
- [ ] 把实验固化为可重复的 benchmark(脚本/测试)。

## 五、已执行基线(2026-08-27,自迭代实验首次落地)

> 里程碑:从设计走向执行。以下基线数字可重复跑,供趋势追踪。

| # | 实验 | 指标 | 基线 | 测量 | 状态 |
|---|------|------|------|------|------|
| 1 | 跨代记忆保持 | 关键事实保留率 | **100%** (4/4) | `session_persist.rs::cross_generation_memory_retention_keeps_facts` | ✅ 已跑 |
| 2 | 热加载能力 | 注册→调用端到端成功率 | **(2/2) 注册+调用** 即插即用 ok | `self_iteration_loop.rs::hotplug_load_then_call_is_end_to_end` | ✅ 已量化 |
| 3 | 长会话压缩 | 摘要信息保留率 | 优质摘要 **100%**(5/5) vs 模板 0%(5/5) | `session_persist.rs::compact_information_fidelity_keeps_key_facts_via_summary` | ✅ 已量化 |
| 4 | 督工恢复 | 恢复时间/成功率 | rutis-cli Supervisor 测试已绿(定性) | rutis-cli | ⚠️ 未量化 |

**解读**:记忆保持率 100% + 压缩保真 100%(优质摘要) → **记忆架构是最强项**。
量化实证:关键信息经优质摘要 100% 保留,模板 0% → **主动压缩必须用提炼摘要**。
剩余短板:**实验2热加载/实验4督工的量化**(定性已知跑通)。下一步补这两项量化。

## 六、完整测试健康基线(2026-08-27 更新,round 61)

> 转轨后真实工程阶段的整体健康全景,供未来实例对照。方法:`cargo test -p
> rutis-agent -p rutis-dsh -p rutis`。

| 层 | 套件 | 数量 |
|----|------|------|
| rutis 内核 | contract(契约断言)+ parity(对等)+ dispatch_chain + probe | 58 + 57 + 2 |
| rutis-agent | lib(driver 边界)+ integration(装配/门控)+ minimal(工具)+ unit_loop(sanitize/rollback/max_steps)+ self_tools(11 工具全功能)+ session_persist(记忆/压缩/持久化)+ self_driven + self_iteration(热加载)+ 其它 | 19+7+25+15+14+21+3+3+... |
| rutis-dsh | llm_seam(4)+ llm_e2e(1) | 5 |
| **合计** | **240 passed / 0 failed** | |

**补充实证**:
- 实机 e2e(真实 DEEPSEEK):`real_backend` 通过(round44,3.83s,调工具+多轮连续)——非 mock。
- 工具层:11 个 self_tools 全部有**功能测试**,含失败路径(hotplug_load 不存在 .so)。
- 已修真实 bug:cwd 敏感×3(skill/ledger/handoff)、hotplug 假绿、self_todo 空串清除、
  is_context_overflow 真实 provider 格式(gpt-4o/anthropic)。
- 技能库 15 SKILL(U/M/E/W 四类),方法论沉淀可检索。

**对照用法**:未来实例可跑此命令,若 passed 大幅下降或有 FAILED,即健康回归信号。
