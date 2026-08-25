# 工作文档:记忆压缩(self_compact)——长会话不退化

> 开始:2026-08-25。短期目标 1:热迭代能力验证——实际写一个新的主线
> 相关能力。选择记忆压缩:记忆指针(上一轮)解决"恢复后感知历史",
> 本能力解决"历史超长时不退化"——agent 会话越久,prompt 越长,
> 成本/延迟/注意力衰减;需要压缩。

## 设计
- `Session` 增加 `summary: Option<String>`(serde default,SessionFile 向后兼容)。
- prompt 组装:有 summary 时作为 system 消息前置(`<记忆摘要>`)。
- 新工具 `self_compact`(第 7 个 self_* 工具):
  - 参数:`summary`(模型生成的摘要文本)+ `keep`(保留最近 N 条,默认 20)。
  - 执行:summary 存 session;messages 裁剪到最近 keep 条;持久化。
  - 返回值:压缩前/后消息数。
- 由模型自主决策何时压缩(长会话中调用工具),符合"agent 自我演进"。

## 热迭代验证
- 以工具形式加入(plugin 装配面)→ 测试 → cargo build/test → 提交推送。
- 验证路径:driver 重载后 session summary 保留(跨代)。

## 测试
- Session roundtrip 含 summary。
- prompt 含 summary(作为 system)。
- self_compact 执行:消息缩短、summary 保存。
- 压缩后恢复:summary 保留、prompt 含摘要。

## 边界
- 不做自动触发(模型自主调用);不做 LLM 后台摘要(模型自己总结历史后
  调用工具,摘要文本由模型提供)。
- 摘要替换的是"被裁剪的消息",最近 keep 条原样保留。

## 实现完成
- `Session.summary: Option<String>`(serde default,旧文件兼容)。
- `Session::compact(summary, keep)`:(before, after) 裁剪 + 保存摘要。
- `SessionSnapshot.summary()` 暴露;`SessionSnapshot::persist` 写 summary。
- `Agent::compact(summary, keep)` trait 方法(driver 实现,压缩后立即落盘)。
- `self_compact` 工具(第 7 个 self_*):模型自主总结早期对话 + 裁剪,
  参数 summary(必填)/ keep(默认 20)。
- prompt 组装:有 summary → system 前置 `<记忆摘要>`;与记忆指针并存。
- **关键修复**:保持"无附加内容时传 None"语义(不引入空 system 消息),
  否则破坏 MockReplayModel 等对 prompt 结构的断言(integration 测试抓住)。

## 测试(15 个全绿)
- session_compact_trims_and_keeps_summary:裁剪 + 摘要保存 + 恢复后摘要保留。
- legacy_session_file_without_summary_loads:旧文件无 summary → None 兼容。
- compacted_session_injects_summary_into_prompt:压缩后 prompt system 含摘要、
  早期消息被裁剪、最近消息保留。
- self_tools schema 测试更新:6 → 7 个工具(含 self_compact)。
- 全 agent 套件 + cli + workspace check 绿。

## 验收
- cargo test -p rutis-agent 全绿(15 session_persist + 9 self_tools + 其余)。
- 热迭代闭环:新能力以工具形式加入 → 测试 → 构建 → 提交推送。
