# rutis agent 技能库(skills)

> 借鉴 OpenAI Codex 的 skills/ 机制:把 agent 的能力从硬编码 + 散落文档
> 组织为**可检索、可复用、可渐进增强**的显式技能索引。
> 每个技能 = 一个短文件,含「何时用 / 怎么做 / 到哪看」。

## 技能调用纪律(借鉴 codex 的 description 边界)
- 触发**:先判断当前任务属于哪个技能的适用场景**,再检索对应 SKILL,不盲目全库使用。
- 边界**:每个技能有适用/不适用**(例:SKILL-E4 soft-trim 只处理超长工具输出,
  不是所有日志都要裁剪)。遇到"是否用此技能"的犹豫,读它正文的适用范围。
- 复用优先**:**已登记的能力不重复发明;新落地一个能力/方法论 → 在本索引登记
  SKILL(何时用/怎么做/到哪看),保持库即能力全景。

## 技能分类与索引

### 一、自我审查与纪律
- **SKILL-U1 清单自审** — 降频待命时系统化审查,避免空转与漏查
  → `../work/self-review-checklist.md`
  → **§5 空转检测已对齐 persona ★最重要一课**:全绿审计+记手写"无事"+降频 ≠ 产出;
    实质产出 = 修真实 bug / 锁真实风险 / 增强自身 / 写工具。找不到真实改进点时,
    诚实列已审视候选与排除理由,仍非手工 pass 蒙混(round 69 修正)。
- **SKILL-U2 环境敏感测试判别** — 假绿(该跑未跑=修)vs 显式红(环境缺=不改)
  → 在 checklist §1 与 handoff round 12
  → **依赖外部产物(插件 .so / npm node_modules / API key)的测试判别**:
    该跑却静默 `eprintln! skip; return` = 假绿(要改成自动构建依赖 + 断言存在,
    如 hotplug e2e 的 `ensure_hotplug_plugin()`,round 51);
    环境缺就 fail + 明确提示 = 显式红(不改,如 dsh node e2e 的
    `assert node_modules exists` / `#[ignore]`+key)。判定看"缺依赖时是静默没跑
    还是显式失败"。圆:两方向相反。
  → **依赖"缺失的外部产物"(如外部 checkout DSH_ROOT/MIN_CORDIS_ROOT)∪无须
    #[ignore]**(round71/72 实操):必须加 `#[ignore="..."]` 否则 `cargo test -p`
    在 clean 环境硬失败、误导人当 bug(host_cordis 曾中招)。默认跳过、`-- --ignored`
    手动触发;手动跑时缺依赖仍保留显式红(panic 报缺啥),非静默跳过。
    而依赖"存在产物"(node/path)的测试(llm_e2e/tcp_e2e)默认真跑绿,不该 ignore。
    判别=依赖的生产物在默认环境"缺"(ignore)还是"在"(默认真跑)。同仓库应一致
    (对齐 real_backend 的 `#[ignore="..."]` 模式;host_cordis 已按此修复)。

### 二、记忆与持续
- **SKILL-M1 跨代记忆保持率实测** — 注入关键事实→gen+1→断言保留→100%
  → `crates/rutis-agent/tests/...::cross_generation_memory_retention_keeps_facts`
- **SKILL-M2 高质量会话压缩** — 主动压缩时提炼含关键信息的摘要(非模板)
  → session compact + self_compact 用法
- **SKILL-M3 压缩信息保真** — 优质摘要 100% vs 模板 0% 保真;主动压缩须用提炼摘要
  → `session_persist.rs::compact_information_fidelity_keeps_key_facts_via_summary`

### 三、自我演进与健康
- **SKILL-E1 回滚保障(rollback)** — self_build 累积台账→≥2 后 self_rollback 可用
  → 台账 `../work/version-ledger.json`
- **SKILL-E2 文档-代码漂移检查** — 验证"记录了但代码还在吗" grep #[cfg(test)]
  → checklist §4.5
- **SKILL-E3 外部对标研究** — 研究本机真实 agent(grok/codex),对比架构差距
  → `../work/peer-agent-study-2026-08-27.md`
- **SKILL-E4 长工具结果修剪(soft-trim)** — 超长 tool output 保头/保尾+标记
  → driver `tool_result_message` + test `tool_result_long_output_is_trimmed`
- **SKILL-E5 启发式判别的真实输入覆盖** — 审视关键词/启发式逻辑(is_context_overflow
  等)时,用**真实外部输入**(真实 provider 错误格式)检验漏判,别只看理想形式;
  补漏 + 正反例锁定(round57:gpt-4o too-large / anthropic at-most 曾漏,auto-compact
  不触发长会话退化)。
- **SKILL-E6 前瞻项前提变化重估** — 被 defer/标注"前瞻"的能力,其 defer 前提变化后
  应**重新评估是否可落地**(round63-65: codex token 预算曾因"mock usage 全 0 不可测"
  defer,真实后端确立后前提消除 → 完整落地跨轮 token 累计+预算硬保护)。
  落地前谨慎评估改动面(AgentDriver::new 仅一处调用 → 默认 None 向后兼容),
  超出核心价值的不做(如完整 status 状态机编码,避免过度)。

### 四、工程工作流
- **SKILL-W1 CI 自动化** — `../work/ci-automation-2026-08-25.md`
- **SKILL-W2 督工/热重载** — `../work/supervisor-hot-reload-2026-08-25.md`
- **SKILL-W3 热插拔插件** — `../work/hotplug-dylib-2026-08-25.md`
- **SKILL-W4 写功能测试找真实 bug** — 对"仅注册/仅预期"的工具/路径先写**真实执行的功能
  测试**(经真实驱动而非 mock 断言),测试失败即暴露真实语义 bug。
  高效路径:round55 给 self_todo 写功能测试 → 暴露"empty clears"实际存 Some("")
  的空串不清除 bug;给 self_compact 补功能测试补齐空白。适合:有实现但测试只
  到"注册可见"层的对象。

## 使用约定
- 新能力/方法论落地 → 在此登记一个 SKILL 条目(何时用/怎么做/到哪看)
- 技能应可被 agent 检索:需要时读对应文档执行,而非每次临场摸索
- 技能随演进持续增补,这是"迭代能力最强"的载体
