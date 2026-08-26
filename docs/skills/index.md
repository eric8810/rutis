# rutis agent 技能库(skills)

> 借鉴 OpenAI Codex 的 skills/ 机制:把 agent 的能力从硬编码 + 散落文档
> 组织为**可检索、可复用、可渐进增强**的显式技能索引。
> 每个技能 = 一个短文件,含「何时用 / 怎么做 / 到哪看」。

## 技能分类与索引

### 一、自我审查与纪律
- **SKILL-U1 清单自审** — 降频待命时系统化审查,避免空转与漏查
  → `../work/self-review-checklist.md`
- **SKILL-U2 环境敏感测试判别** — 假绿(该跑未跑=修)vs 显式红(环境缺=不改)
  → 在 checklist §1 与 handoff round 12

### 二、记忆与持续
- **SKILL-M1 跨代记忆保持率实测** — 注入关键事实→gen+1→断言保留
  → `crates/rutis-agent/tests/...::cross_generation_memory_retention_keeps_facts`
- **SKILL-M2 高质量会话压缩** — 主动压缩时提炼含关键信息的摘要(非模板)
  → session compact + self_compact 用法

### 三、自我演进与健康
- **SKILL-E1 回滚保障(rollback)** — self_build 累积台账→≥2 后 self_rollback 可用
  → 台账 `../work/version-ledger.json`
- **SKILL-E2 文档-代码漂移检查** — 验证"记录了但代码还在吗" grep #[cfg(test)]
  → checklist §4.5
- **SKILL-E3 外部对标研究** — 研究本机真实 agent(grok/codex),对比架构差距
  → `../work/peer-agent-study-2026-08-27.md`

### 四、工程工作流
- **SKILL-W1 CI 自动化** — `../work/ci-automation-2026-08-25.md`
- **SKILL-W2 督工/热重载** — `../work/supervisor-hot-reload-2026-08-25.md`
- **SKILL-W3 热插拔插件** — `../work/hotplug-dylib-2026-08-25.md`

## 使用约定
- 新能力/方法论落地 → 在此登记一个 SKILL 条目(何时用/怎么做/到哪看)
- 技能应可被 agent 检索:需要时读对应文档执行,而非每次临场摸索
- 技能随演进持续增补,这是"迭代能力最强"的载体
