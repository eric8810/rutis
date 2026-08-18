# 最小 Agent 框架设计(基于 rutis + aimux)

> 2026-08-18。目标:定义基于 rutis 五支柱的**最小 agent 框架**——要素清单、各自形态、哪些是插件、边界在哪。
> 依据:[deepseek-harness](../deepseek-harness/docs/architecture.md)(插件拆分)、[aimux](../aimux/) `LanguageModel`/`CallOptions`/`Tool`(统一 LLM 访问层)、[design-rust-port.md](design-rust-port.md) v5(五支柱)。
> 定位:M3 agent crate 的设计依据。**先定义,后实现。**

## 一、核心洞察

1. **dsh:service(资源)和 loop(行为)分开。** `core/agent` 提供 `Agent` 接口,`core/agent-loop` 提供实现接口的 driver——loop 是实现接口的 driver 插件,不是装着 `run` 的容器。
2. **aimux:LLM seam 已被实现。** 329 个 provider 折叠进单个 `dyn LanguageModel`,自带 `CallOptions`/`GenerateResult`/`FunctionTool`。**直接消费,不自造 trait。**
3. **session 是连续 loop 的载体,不是持久化日志。** 多轮对话的 history 活在 session 里;dsh 的事件日志+surface 投影两层是持久化/compaction/回放逼出来的,最小内存版**直接存模型可见消息,一层**。
4. **loop 的过程可被框架事件系统观察(emit)和拦截(waterfall)。** dsh 的 turn flow 关键节点都是 waterfall:`agent/pre-step`(改写/拒绝 messages)、`agent/request`(换模型配置)、工具三段 `tools/pre-execute`(门控)/`execute`(执行)/`post-execute`(结果决策,含失败);过程事实经事件广播,UI 是事件的消费者。**这是框架自己吃狗粮**:loop 不是黑盒,关键节点 waterfall、过程增量 emit,观察与拦截都走 EventBus。

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

### 3. Agent trait——多轮、可观察、可取消

```rust
pub trait Agent: Send + Sync + 'static {
    fn id(&self) -> SessionId;                        // 与 session 共享身份
    fn status(&self) -> AgentStatus;                  // idle | running
    fn session(&self) -> &Session;                    // 持有 session,连续 loop 载体
    /// 提交一条用户消息:push 进 session,驱动一个 turn。
    /// 返回 turn 终态;**过程增量(text/tool/状态)经 EventBus 的 `agent/*` 事件广播**,
    /// 不独占返回——观察方(TUI/日志/其他前端)订阅事件,不调 followup。
    fn followup<'a>(&'a self, input: &'a str) -> BoxFuture<'a, Result<String, AgentError>>;
    /// 中断当前 turn,session(history)保留,下次 followup 继续
    fn cancel(&self);
}
```

**turn 的过程输出走 EventBus,不走独占 stream。** driver 循环把每个增量 `emit` 到 `agent/*` 事件:

```rust
pub struct AgentTextDelta { pub session: SessionId, pub step: usize, pub delta: String }   // impl Event
pub struct AgentToolCall  { pub session: SessionId, pub name: String, pub args: Value }  // impl Event
pub struct AgentToolResult{ pub session: SessionId, pub name: String, pub ok: bool, pub output: String }  // impl Event
pub struct AgentTurnEnd   { pub session: SessionId, pub result: Result<(), String> }     // impl Event(result 不 Clone 走 Err 摘要)
```

理由(dsh 模式 + 框架自洽):**输出是广播不是独占**。一次 turn 可被多方观察(TUI + 日志 + 未来前端),观察方晚订阅、只看不动都行;监听器随 fiber 卸载(D28);TUI 与 driver 解耦——TUI 订阅 `agent/*` 事件,不调 `followup` 拿 stream。`followup` 只负责"触发 turn + 回传终态",过程全靠事件。

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

#### 4.1 循环(感知→思考→行动→观察),关键节点 waterfall + 输出走事件

loop 的过程**可被框架事件系统观察(emit)和拦截(waterfall)**——这是 dsh 架构精髓,也是框架自己吃狗粮。driver 不再写死线性流程,关键节点经 waterfall 分发,插件可挂中间件改写/veto。

**与 dsh waterfall 节点的精确对应**(核对 dsh 源码后修正):

| dsh | 语义 | 最小版 |
|---|---|---|
| `agent/pre-step` | 改写/拒绝进入这步的 **messages**(`next()` 保留) | ✅ 做(消息改写是常用扩展点) |
| `agent/request` | 替换**冻结的调用配置**(provider/model/maxTokens;**不改 messages**) | ⏸ M4(换模型路由时用) |
| `tools/pre-execute` | 工具执行**前**门控:reject 或放行(审批/权限) | ✅ 做 |
| `tools/execute` | **包裹执行本身**:timeout/retry/metrics,`next()` 返规范化结果 | ✅ 做 |
| `tools/post-execute` | 执行**后**:accept/replace/enrich/block 结果;**失败也到这**(thrown 也走这,决策重试) | ✅ 做 |

```rust
impl Agent for AgentDriver {
    fn followup<'a>(&'a self, input: &'a str) -> BoxFuture<'a, Result<String, AgentError>> {
        Box::pin(async move {
            self.session.lock().unwrap().push(ModelMessage::user(input));
            self.set_status(AgentStatus::Running);
            let out = self.run_loop().await;
            self.set_status(AgentStatus::Idle);
            self.emit(AgentTurnEnd { session: self.id(), result: out.as_ref().map(|_|()).map_err(|e| e.to_string()) });
            out
        })
    }
    fn cancel(&self) { self.cancel_token.cancel(); }
}

async fn run_loop(&self) -> Result<String, AgentError> {
    for step in 0..self.max_steps {
        if self.cancel_token.is_cancelled() { return Err(AgentError::Stopped); }

        // ── agent/pre-step waterfall:改写/拒绝进入这步的 messages(默认 next=原样保留)──
        let prompt = convert_to_language_model_prompt(self.session.lock().unwrap().messages(), None);
        let tools = self.tools.schemas();
        let (prompt, tools) = self.ctx.events()
            .waterfall(&self.ctx, &AgentPreStep { prompt, tools, step }, next::identity()).await?;

        // 思考:流式调 aimux
        let mut result = self.llm.do_stream(&CallOptions { prompt, tools: Some(tools), ..Default::default() })
            .await.map_err(|e| AgentError::Llm(e.to_string()))?;

        // 观察:逐块收 TextDelta,emit 到 agent/text-delta 事件(广播,非独占 stream)
        let mut text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        while let Some(part) = result.stream.next().await {
            match part {
                Ok(StreamPart::TextDelta { delta, .. }) => {
                    text.push_str(&delta);
                    self.emit(AgentTextDelta { session: self.id(), step, delta });  // 事件广播
                }
                Ok(StreamPart::ToolCall { tool_name, args, .. }) => calls.push(/* ... */),
                Ok(StreamPart::Finish { .. }) => break,
                Ok(StreamPart::Error { error }) => return Err(AgentError::Llm(error.to_string())),
                _ => {}
            }
        }
        self.session.lock().unwrap().push(assistant_message(text.clone(), &calls));
        if calls.is_empty() { return Ok(text); }  // 终答

        // ── 行动:每个工具调用经三段管线(pre-execute 门控 → execute 执行 → post-execute 决策)──
        for call in calls {
            self.emit(AgentToolCall { session: self.id(), name: call.tool_name.clone(), args: call.input.clone() });

            // ① tools/pre-execute:执行前门控(reject 或放行;默认 next=放行)
            let decision = self.ctx.events()
                .waterfall(&self.ctx, &ToolPreExecute { call: call.clone() }, next::allow()).await?;
            if decision.is_reject() { /* 拒绝:结果 = 拒绝原因,跳过执行 */ }

            // ② tools/execute:包裹执行(timeout/retry/metrics;默认 next=本地执行,panic-safe 在内)
            let out = self.ctx.events()
                .waterfall(&self.ctx, &ToolExecute { call: call.clone() },
                    next::run(|c| self.tools.execute(&c.call, &self.cancel_token))).await?;

            // ③ tools/post-execute:结果决策(accept/replace/重试;失败也到这;默认 next=原样)
            let out = self.ctx.events()
                .waterfall(&self.ctx, &ToolPostExecute { call: call.clone(), result: out }, next::accept()).await?;

            self.emit(AgentToolResult { session: self.id(), name: call.tool_name.clone(), ok: out.ok, output: out.output.clone() });
            self.session.lock().unwrap().push(tool_result_message(&call, out));
        }
    }
    Err(AgentError::MaxSteps(self.max_steps))
}
```

**三个关键设计**:
1. **关键节点 waterfall(对齐 dsh 管线)**:`agent/pre-step`(改写/拒绝 messages)+ 工具三段 `pre-execute`(门控)/`execute`(执行)/`post-execute`(结果决策,含失败)。默认 `next` 是原行为,插件挂 `on_waterfall` 中间件包裹——可改写、可 veto(不调 next)。这是 EventBus 第四分发语义在 loop 里的正当用途,框架自己吃狗粮。
2. **过程增量 emit 到 `agent/*` 事件**:`text-delta`/`tool-call`/`tool-result`/`turn-end` 广播,任何观察方订阅。不再有独占 stream。
3. **session 仍是唯一事实源**:增量一边 emit 给观察方、一边累积回写 session;多轮 `followup` 自然连续。**流式路径用 `do_stream`**(非 `do_generate`)。

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

// 观察方(TUI/日志):订阅 agent/* 事件,不调 followup
root.events().on(&root, |_, e: &AgentTextDelta| /* 逐字显示 */);
root.events().on(&root, |_, e: &AgentToolCall|   /* 显示 ⚙ 工具 */);

let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
let a1 = agent.followup("weather in Oslo?").await?;         // 第一轮(终态)
let a2 = agent.followup("and in Bergen?").await?;           // 第二轮,history 连续

// 依赖驱动重载:卸载 llm 服务自动驱逐 driver,无需手动 dispose agent_view
```

**TUI 是个 `agent/*` 事件监听器**(归自己 fiber 所有,D28),订阅 `AgentTextDelta`/`AgentToolCall`/`AgentToolResult` 渲染;输入经 `agent.followup` 触发 turn。**过程增量全靠事件,TUI 与 driver 解耦**——晚订阅、多方订阅、只看不动都可以。

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

**最小 agent 框架 = 一个 aimux `LanguageModel` 服务 + 一个 `ToolRegistry` 插件 + 一个实现 `Agent` 接口的 driver 插件 + 一个内存 session(连续 loop 的事实源)。** 循环写在 `AgentDriver` 内部:感知(`session.messages()`)→ 思考(`llm.do_stream`,前置 `agent/request` waterfall 可改写/拦截)→ 行动(`tools.execute`,经 waterfall 可拦截/替换)→ 观察(增量 emit 到 `agent/*` 事件广播 + 回写 session),逐步检查取消。**loop 关键节点走 waterfall、过程增量走事件**——框架自己吃狗粮,loop 可被观察(emit)与拦截(waterfall)。session 持 history 使多轮连续;fiber 管 driver 生命周期,`ctx.cancelled()` 停。插件只在有资源要管、有生命周期要挂的地方出现。
