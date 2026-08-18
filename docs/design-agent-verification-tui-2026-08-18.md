# Agent 验证方式与 TUI 交互设计

> 2026-08-18。配套 [design-min-agent-2026-08-18.md](design-min-agent-2026-08-18.md)(最小 agent 框架)。本文定义两件事:**怎么验证**(三层测试 + 真实端到端)与**怎么做最基础的 TUI 用户交互**。

## 一、验证方式(三层 + 真实)

| 层 | 后端 | 验证什么 | 何时跑 |
|---|---|---|---|
| **单元** | 脚本后端(按序弹出 `LlmResponse`) | 循环逻辑:无工具→终答;有工具→执行→回喂→终答;多轮 history 连续;max_steps 截断;cancel 步间生效;工具失败/panic 回喂不崩 | CI 每次 |
| **集成** | aimux `MockReplayModel`(走真 `LanguageModel` 接口,录制回放) | 插件装配:双门控(llm+tools);卸载 llm→driver 自动驱逐;fiber 卸载→`ctx.cancelled()`→循环停;`agent/step`/`agent/tool` 事件观察 + 监听器随 fiber 卸载 | CI 每次 |
| **真实端到端** | 真实 provider(DeepSeek 云端 或 Ollama 本地) | 真模型多轮 followup、工具调用、终答质量、history 连续 | 手动/演示,**不进 CI** |

### 单元层:脚本后端

按序弹出响应的 `ScriptedLlm`(现有),实现 `LanguageModel`。多轮 history 断言:第二轮 `followup` 时后端收到的 prompt 含第一轮全部消息——钉死"session 是连续 loop 的事实源"。

### 集成层:MockReplayModel

`MockReplayModel::new(provider, model_id, recordings)` 实现 `LanguageModel`,按输入匹配录制、不发真 API(aimux `replay.rs`)。**驱逐断言必须钉死**:dispose llm 服务后断言 driver fiber 回 Pending——这是框架差异化能力。时序用同步点(Notify/gate)不靠 sleep。

### 真实端到端层:demo 即验证

```rust
// examples/demo.rs(或 #[ignore] 测试),真实后端
let model = aimux_providers::provider("deepseek", None, "deepseek-chat", None)?;  // None → 读 DEEPSEEK_API_KEY
let root = Ctx::root()?;
root.provide_as(llm_key(), Arc::from(model))?;
root.plugin(ToolsPlugin::new(vec![weather_tool]));
let agent_view = root.plugin(AgentDriverPlugin::new(16));
(&agent_view).await.expect("gated on llm+tools");
let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
let a1 = agent.followup("weather in Oslo?").await?;
let a2 = agent.followup("and in Bergen?").await?;   // history 连续
// 卸载 llm → driver 自动驱逐(依赖驱动重载)
```

**真实层做成 `#[ignore]` 测试 + 可运行 demo**,需要 key/本地服务时手动触发:`cargo test -p min-cordis-agent -- --ignored`。没 key 时 CI 不挂。

## 二、TUI 交互设计(最基础用户交互)

### 目标

最基础的交互:用户输入 → 流式看到 agent 思考/工具调用/回答 → 继续输入(多轮)。**TUI 是真实端到端验证的载体**——它把 demo 的"脚本化两轮"变成"人实时多轮"。

### 技术选型

- **ratatui + crossterm**:Rust TUI 事实标准,跨平台,与 tokio 集成成熟。当前 crate 无 TUI 依赖,新增这两个。
- **aimux `do_stream` 流式**:`followup` 返回 `BoxStream<TurnEvent>`(见设计文档 §3),`TextDelta` 逐块到达。**流式是首版需求**,TUI 逐字输出。

### 交互模型:一个 TUI 前端插件 + agent 服务

TUI 是个**消费 `Agent` 服务的前端**,不是 loop 的一部分。它自己是个 fiber(生命周期由框架管):

```
┌─ TuiPlugin(fiber)──────────────────────────┐
│ 输入行:读用户输入 → agent.followup(text)     │
│ 显示区:订阅 agent/* 事件 + 流式 TextDelta    │
│ 状态栏:agent.status(idle/running)           │
└─────────────────────────────────────────────┘
        ↓ 依赖(injects)
  agent 服务(AgentDriverPlugin)
```

- **输入** → `agent.followup(input)` 返回 `BoxStream<TurnEvent>`,逐块消费;
- **显示** → 消费 `TurnEvent::TextDelta`(逐字)/ `ToolCall` / `ToolResult`;`agent/*` 事件作辅助观察;
- **取消** → Esc/Ctrl+C 触发 `agent.cancel()`(中断当前 turn,history 保留);
- **退出** → 卸载 TUI fiber,级联停 driver。

### 最小 TUI 界面(三栏)

```
┌──────────────────────────────────────────────┐
│ 对话区(滚动):user/assistant/tool 消息        │
│   you> weather in Oslo?                      │
│   ⚙ get_weather({"city":"Oslo"}) → 18° clear │
│   agent> Oslo: 18 degrees, clear sky.        │
├──────────────────────────────────────────────┤
│ 状态:idle | running(step 2) | [Esc 取消]     │
├──────────────────────────────────────────────┤
│ 输入> _                                       │
└──────────────────────────────────────────────┘
```

### 最小交互闭环(对应 dsh turn flow 的裁剪)

```
用户输入 → agent.followup(input) 得 BoxStream<TurnEvent>
  → session.push(user)
  → loop: llm stream
        → TextDelta 逐块 → TUI 逐字显示
        → ToolCall → TUI 显示 "⚙ 工具名(参数)" → 执行 → ToolResult 显示
        → 无 tool_call → Done(终答文本)
  → status: running → idle
  → 等待下一次输入
```

这对应 dsh 的 turn flow,但裁掉 `agent/pre-step` 重写、inbox 排队、steer——最小 TUI 要"输入→流式逐字→工具可见→取消",**流式逐字是首版需求**。

### 不做什么(边界)

- 不做多 agent 切换、不做会话持久化恢复、不做命令系统(`/clear` 等)、不做 markdown 渲染/语法高亮、不做鼠标。
- 流式进首版;`agent/*` 事件作辅助观察通道(ToolCall/ToolResult 也经 TurnEvent 流给 TUI,事件用于其他观察方)。

## 三、落地顺序

1. **单元 + 集成测试**(脚本后端 + MockReplayModel):改 agent crate 的同时补齐,CI 跑。
2. **demo 改真实后端**:`provider("deepseek", None, ...)`,等 key 真跑验证。
3. **TuiPlugin**:ratatui+crossterm,首版多轮 + 工具可见 + 取消;流式后续加。
