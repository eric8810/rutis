# 最小 Agent 框架设计(基于 rutis + aimux)

> 2026-08-18。目标:定义基于 rutis 五支柱的**最小 agent 框架**——要素清单、各自形态、哪些是插件、边界在哪。
> 依据:[deepseek-harness](../deepseek-harness/docs/architecture.md)(插件拆分)、[aimux](../aimux/) `LanguageModel`/`CallOptions`/`Tool`(统一 LLM 访问层)、[design-rust-port.md](design-rust-port.md) v5(五支柱)。
> 定位:M3 agent crate 的设计依据。**先定义,后实现。**

## 一、核心洞察

1. **dsh:service(资源)和 loop(行为)分开。** `core/agent` 提供 `Agent` 接口,`core/agent-loop` 提供实现接口的 driver——loop 是实现接口的 driver 插件,不是装着 `run` 的容器。
2. **aimux:LLM seam 已被实现。** 329 个 provider 折叠进单个 `dyn LanguageModel`,自带 `CallOptions`/`GenerateResult`/`FunctionTool`。**直接消费,不自造 trait。**
3. **session 是连续 loop 的载体,不是持久化日志。** 多轮对话的 history 活在 session 里;dsh 的事件日志+surface 投影两层是持久化/compaction/回放逼出来的,最小内存版**直接存模型可见消息,一层**。

## 二、要素清单与形态判定

最小 agent 有六个要素。插件只出现在**有资源要管理、有生命周期要挂载**的地方(cordis 范式:插件 = 装配单元)。

| 要素 | 形态 | 是否插件 | 理由 |
|---|---|---|---|
| **LLM 后端** | `Arc<dyn LanguageModel>` 服务 | **服务**,直接 provide | aimux 已实现;有真实装配逻辑(连接/凭证/清理)才写成插件,否则 `provide_as` 一行 |
| **工具集** | `ToolRegistry` 服务 | **是插件** | 统一注册、门控、可热替换、schema 汇入 prompt。真装配单元 |
| **session** | 内存有序消息记录 | 不是插件 | 连续 loop 的事实源;driver 持有,随 fiber 释放 |
| **agent 循环** | 实现 `Agent` 接口的 driver | **是插件** | loop 本体;fiber 管生命周期 |
| **事件观察** | `agent/*` 事件(step/tool) | 随 driver fiber 挂载 | 监听器归 driver fiber 所有(D28) |
| **停止/取消** | fiber `CancellationToken` | 不是插件 | D27,driver 内 `ctx.cancelled().await` |

**两个真插件**:`ToolsPlugin`、`AgentDriverPlugin`。**一个直接 provide 的服务**:LLM(aimux)。**不写空壳**:`LlmPlugin`。**内存对象**:session。

## 三、要素定义

### 1. LLM 服务——直接 provide

```rust
use aimux_core::LanguageModel;
pub fn llm_key() -> TypeKey { TypeKey::of::<dyn LanguageModel>() }

// 使用方:
ctx.provide_as(llm_key(), Arc::new(my_aimux_model))?;
```

### 2. Session——连续 loop 的事实源(内存,一层)

```rust
/// 连续对话的载体:有序消息记录。不做持久化/回放/compaction。
pub struct Session {
    id: SessionId,
    messages: Vec<ModelMessage>,  // 直接存模型可见消息,无事件→投影两层
}
impl Session {
    pub fn id(&self) -> SessionId;
    pub fn push(&mut self, msg: ModelMessage);       // user / assistant / tool 结果
    pub fn messages(&self) -> &[ModelMessage];        // 只读快照,防外部改
}
```

dsh 的 `deriveMessages()` 在这里就是 `messages()`——无 surface、无 generation、无深冻结。

### 4. Agent trait——多轮、可观察、可取消、流式

```rust
/// 一个 turn 的流式输出:文本增量 + 工具调用边界 + 终态。
/// TUI/前端逐块消费;session 仍由 driver 回写。
pub enum TurnEvent {
    TextDelta(String),                       // 模型文本增量(流式)
    ToolCall { name: String, args: Value },  // 工具调用开始
    ToolResult { name: String, ok: bool, output: String },  // 工具结果
    Done(Result<String, AgentError>),        // turn 终态(终答或错误)
}

pub trait Agent: Send + Sync + 'static {
    fn id(&self) -> SessionId;                        // 与 session 共享身份
    fn status(&self) -> AgentStatus;                  // idle | running
    fn session(&self) -> &Session;                    // 持有 session,连续 loop 载体
    /// 提交一条用户消息:push 进 session,驱动一个 turn。
    /// 返回该 turn 的事件流(文本增量逐块到达;工具调用/结果可见;Done 收尾)。
    fn followup<'a>(&'a self, input: &'a str) -> BoxStream<'a, TurnEvent>;
    /// 中断当前 turn,session(history)保留,下次 followup 继续
    fn cancel(&self);
}
```

**`followup` 返回 `BoxStream<TurnEvent>` 而非 `Result<String>`**——流式是第一性需求(TUI 逐字输出)。终答从 `TurnEvent::Done` 取;非流式调用方收集 `TextDelta` 拼合即可。

**裁掉**(dsh 有但最小不做):inbox 多边界排队、steer/inject(人在回路)、fork/resume(要持久化)、runMaintenance/whenIdle(维护调度)、reset(新建 session/agent 即可)。

### 4. AgentDriver——循环本体

```rust
pub struct AgentDriver {
    llm: Arc<dyn LanguageModel>,
    tools: Arc<ToolRegistry>,
    session: Mutex<Session>,       // 唯一事实源,history 跨 turn 连续
    status: AtomicUsize,           // AgentStatus
    cancel_token: CancellationToken,
    max_steps: usize,
}

impl Agent for AgentDriver {
    fn followup<'a>(&'a self, input: &'a str) -> BoxFuture<'a, Result<String, AgentError>> {
        Box::pin(async move {
            self.session.lock().unwrap().push(ModelMessage::user(input));
            self.set_status(AgentStatus::Running);
            let out = self.run_loop().await;          // 见 4.1
            self.set_status(AgentStatus::Idle);
            out
        })
    }
    fn cancel(&self) { self.cancel_token.cancel(); }
    // id / status / session 访问器省略
}
```

#### 4.1 循环(感知→思考→行动→观察),流式

```rust
fn followup<'a>(&'a self, input: &'a str) -> BoxStream<'a, TurnEvent> {
    Box::pin(async_stream::stream! {
        self.session.lock().unwrap().push(ModelMessage::user(input));
        self.set_status(AgentStatus::Running);
        for step in 0..self.max_steps {
            if self.cancel_token.is_cancelled() { yield TurnEvent::Done(Err(AgentError::Stopped)); return; }

            // 思考:从 session 取历史,流式调 aimux
            let prompt = convert_to_language_model_prompt(self.session.lock().unwrap().messages(), None);
            let mut result = match self.llm.do_stream(&CallOptions {
                prompt, tools: Some(self.tools.schemas()), ..Default::default()
            }).await {
                Ok(r) => r, Err(e) => { yield TurnEvent::Done(Err(AgentError::Llm(e.to_string()))); return; }
            };

            // 感知+观察:逐块收 TextDelta,转发 TUI;同时累积 assistant 内容
            let mut text = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            while let Some(part) = result.stream.next().await {
                match part {
                    Ok(StreamPart::TextDelta { delta, .. }) => {
                        text.push_str(&delta);
                        yield TurnEvent::TextDelta(delta);            // 逐块给 TUI
                    }
                    Ok(StreamPart::ToolCall { tool_name, args, .. }) => calls.push(/* ... */),
                    Ok(StreamPart::Finish { .. }) => break,
                    Ok(StreamPart::Error { error }) => { yield TurnEvent::Done(Err(AgentError::Llm(error.to_string()))); return; }
                    _ => {}
                }
            }
            self.emit(AgentStepEvent { step, .. });
            self.session.lock().unwrap().push(assistant_message(text.clone(), &calls));

            if calls.is_empty() { yield TurnEvent::Done(Ok(text)); return; }  // 终答

            // 行动 + 观察:执行工具,失败回喂,panic 任务边界(评审 #13)
            for call in calls {
                yield TurnEvent::ToolCall { name: call.name.clone(), args: call.arguments.clone() };
                let out = self.tools.execute(&call).await;  // panic-safe
                self.emit(AgentToolEvent { name: call.name.clone(), ok: out.ok, .. });
                yield TurnEvent::ToolResult { name: call.name.clone(), ok: out.ok, output: out.output.clone() };
                self.session.lock().unwrap().push(tool_result_message(&call, out));
            }
        }
        yield TurnEvent::Done(Err(AgentError::MaxSteps(self.max_steps)));
    })
}

fn cancel(&self) { self.cancel_token.cancel(); }
```

**session 仍是唯一事实源**:流式每块 TextDelta 一边转发 TUI、一边累积进 assistant 消息回写 session;工具调用/结果同样回写。多轮 `followup` 自然连续。**流式路径用 `do_stream`**(非 `do_generate`),`TextDelta` 逐块到达,`Finish`/`Error` 收尾。

### 5. ToolsPlugin / AgentDriverPlugin——两个真插件

```rust
impl Plugin for AgentDriverPlugin {
    fn name(&self) -> &str { "agent-driver" }
    fn injects(&self) -> &[TypeKey] { &[llm_key(), tools_key()] }   // 双门控
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let llm = ctx.get_as::<dyn LanguageModel>(llm_key())
                .ok_or_else(|| CordisError::InjectUnsatisfied(vec!["llm".into()]))?;
            let tools = ctx.get_as::<ToolRegistry>(tools_key())
                .ok_or_else(|| CordisError::InjectUnsatisfied(vec!["tools".into()]))?;
            let driver = Arc::new(AgentDriver::new(llm, tools, ctx.clone(), self.max_steps));
            ctx.provide_as(agent_key(), driver)?;
            // fiber 卸载 → cancel(driver 内 ctx.cancelled() 级联停止)
            Ok(Effect::Done)
        })
    }
}
```

`ToolsPlugin` 同理,`apply` 提供 `ToolRegistry` 服务(schema 产 aimux `Tool`,runner 失败回喂、panic 任务边界)。

## 四、最小 demo

```rust
let root = Ctx::root()?;
root.provide_as(llm_key(), Arc::new(aimux_model))?;        // LLM 服务,无插件空壳
root.plugin(ToolsPlugin::new(vec![weather_tool]));
let agent_view = root.plugin(AgentDriverPlugin::new(16));
(&agent_view).await.expect("driver loads (gated on llm+tools)");

let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
let a1 = agent.followup("weather in Oslo?").await?;         // 第一轮
let a2 = agent.followup("and in Bergen?").await?;           // 第二轮,history 连续

// 依赖驱动重载:卸载 llm 服务自动驱逐 driver,无需手动 dispose agent_view
```

## 五、与 dsh 的核对

**核心一致**:service/loop 分离、Agent 接口、工具注册表、事件观察、取消路径全对上。**agent 模型是有意的范围裁剪**:dsh 的 Agent 是有 inbox/status/session 日志/steer 的长寿命实体(支撑多 turn 会话+人在回路+持久化+UI);我们保留其多轮内核(id/status/session/followup/cancel),裁掉持久化与人因调度。

| dsh | 最小 | 裁掉的理由 |
|---|---|---|
| session = 事件日志+surface 投影两层 | 直接存 messages 一层 | 两层为持久化/compaction/回放服务,内存连续 loop 不需要 |
| append-only+深冻结+缓存 | Vec push + 只读快照 | 防外部改用 `&[ModelMessage]` 即可 |
| compaction / 持久化 seam / session 事件 | 不做 | 超出最小;未来要持久化再把 messages 落成事件日志 |
| inbox / steer / inject / fork / resume / runMaintenance | 不做 | 人因调度与持久化,超出最小 |

## 六、当前实现要改的

| 当前 | 改为 | 理由 |
|---|---|---|
| `LlmPlugin` 空壳 | 删,直接 `ctx.provide_as` | 无装配逻辑 |
| 自造 `LlmService` trait | 消费 `aimux::LanguageModel` | seam 已实现,329 provider |
| `AgentLoopPlugin` + 公开 `run` | `AgentDriverPlugin` 实现 `Agent`,`followup` 触发 turn | dsh 模式;loop 是 driver 内部行为 |
| `Agent` 常驻 `messages`/`steps`/`stopped` | session 持 history,turn 级状态独立 | 修 sol #12;session 是连续 loop 载体 |
| `Agent::inert` | 删 | 怪形态,门控已保证 llm 在 |
| 自造 `ToolSpec` | aimux `FunctionTool` + runner | schema 对齐,直接进 `CallOptions` |
| 无 session(单 turn) | 加内存 session | 连续 loop 的事实源 |
| demo 手动反序 dispose | 只 dispose llm,展示自动驱逐 | 依赖驱动重载是差异化能力 |

## 七、一句话定义

**最小 agent 框架 = 一个 aimux `LanguageModel` 服务 + 一个 `ToolRegistry` 插件 + 一个实现 `Agent` 接口的 driver 插件 + 一个内存 session(连续 loop 的事实源)。** 循环写在 `AgentDriver::followup` 内部:感知(`session.messages()`)→ 思考(`llm.do_generate`)→ 行动(`tools.execute`)→ 观察(回写 session + `agent/*` 事件),逐步检查取消。session 持 history 使多轮连续;fiber 管 driver 生命周期,`ctx.cancelled()` 停。插件只在有资源要管、有生命周期要挂的地方出现。
