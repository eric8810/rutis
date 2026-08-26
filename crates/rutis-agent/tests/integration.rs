//! 集成层(验证文档 §一):插件装配与框架差异化能力。
//!
//! - 双门控:driver 在 llm + tools 齐备前保持 Pending
//! - 卸载 llm → driver 自动驱逐回 Pending(依赖驱动重载,**驱逐断言钉死**)
//! - fiber 卸载 → `ctx.cancelled()` → 运行中的循环停
//! - aimux `MockReplayModel`(真 `LanguageModel` 接口,录制回放)驱动 followup
//! - `agent/step` / `agent/tool` 事件观察;监听器随其 fiber 卸载

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimux_core::content::ContentPart;
use aimux_core::language_model::LanguageModel;
use aimux_core::language_model_message::LanguageModelPromptMessage;
use aimux_core::message::Role;
use aimux_core::recording::{
    HttpExchange, HttpRecord, InputRecord, OutcomeRecord, OutcomeStatus, ProviderRecord, Recording,
    ResponseRecord, TimingRecord, RECORDING_SCHEMA,
};
use aimux_core::replay::MockReplayModel;
use rutis::{Ctx, FiberState, FiberView, Listener, Next, Plugin, WaterfallListener};
use rutis_agent::{
    agent_key, llm_key, tool_call, Agent, AgentDriverPlugin, AgentError, AgentStepEvent,
    AgentTextDelta, AgentToolResult, LlmResponse, ScriptedLlm, ToolDef, ToolPostExecute,
    ToolPreExecute, ToolsPlugin,
};
use serde_json::{json, Value};

async fn soon<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .expect("timed out")
}

async fn wait_state(view: &FiberView, want: FiberState) {
    soon(async {
        let mut rx = view.watch();
        loop {
            if rx.borrow().state == want {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await;
}

/// 收集 `AgentTextDelta` 的 listener:拼接增量进共享缓冲。
struct TextL(Arc<Mutex<String>>);
impl Listener<AgentTextDelta> for TextL {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a AgentTextDelta,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        let text = self.0.clone();
        let d = e.delta.clone();
        Box::pin(async move {
            text.lock().unwrap().push_str(&d);
            Ok(None)
        })
    }
}

/// 跑一个 turn,返回 (拼接文本, 终态)。
async fn collect_turn(
    root: &Ctx,
    agent: &Arc<dyn Agent>,
    input: &str,
) -> (String, Result<String, AgentError>) {
    let text = Arc::new(Mutex::new(String::new()));
    let _d = root.events().on(root, TextL(text.clone())).unwrap();
    let result = agent.followup(input).await;
    let collected = text.lock().unwrap().clone();
    (collected, result)
}

// ── MockReplayModel 录制构造(openai body,单轮直答)──────────────

fn replay_recording(prompt_text: &str, reply: &str) -> Recording {
    // 流式录制 body:SSE chunk 形状(choices[].delta.content)
    let chunk1 = json!({
        "id": "chatcmpl-mock",
        "model": "gpt-4o",
        "choices": [{ "index": 0, "delta": { "content": reply }, "finish_reason": null }]
    });
    let chunk2 = json!({
        "id": "chatcmpl-mock",
        "model": "gpt-4o",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
    });
    let body = format!("data: {chunk1}\n\ndata: {chunk2}\n\ndata: [DONE]\n");
    Recording {
        schema: RECORDING_SCHEMA,
        call_id: "it-1".to_string(),
        recorded_at: "2026-08-18T00:00:00Z".to_string(),
        input: InputRecord {
            prompt: vec![LanguageModelPromptMessage {
                role: Role::User,
                content: vec![ContentPart::text(prompt_text)],
                provider_options: None,
            }],
            options: json!({}),
        },
        provider: ProviderRecord {
            provider: "openai".into(),
            model_id: "gpt-4o".into(),
            base_url: None,
            api_key_source: "none".into(),
            profile: None,
            provider_options: None,
        },
        exchanges: vec![HttpExchange {
            attempt: 0,
            request: HttpRecord {
                method: "post".into(),
                url: "https://api.openai.com/v1/chat/completions".into(),
                headers: vec![],
                body: Some("{}".into()),
            },
            response: Some(ResponseRecord {
                status: 200,
                headers: vec![],
                body: Some(body),
                stream_chunks: None,
                ttfb_ms: None,
            }),
            timing: TimingRecord {
                latency_ms: 10,
                ttfb_ms: None,
            },
            error: None,
            finalized: true,
        }],
        outcome: OutcomeRecord {
            status: OutcomeStatus::Success,
            finish_reason: Some("stop".into()),
            error: None,
            usage: None,
        },
        complete: true,
        transport_closed: true,
        session_id: None,
        step: None,
    }
}

// 1. 双门控:llm 与 tools 任一缺失都保持 Pending,齐备才启动
#[tokio::test]
async fn driver_is_gated_on_llm_and_tools() {
    let root = Ctx::root().unwrap();
    let driver_view = root.plugin(AgentDriverPlugin::new(16));
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(driver_view.state().state, FiberState::Pending);
    assert!(root.get_as::<dyn Agent>(agent_key()).is_none());

    // 只给 llm:仍 Pending(缺 tools)
    let llm: Arc<dyn LanguageModel> = Arc::new(MockReplayModel::new(
        "openai",
        "gpt-4o",
        vec![replay_recording("ping", "pong")],
    ));
    let llm_disposer = root.provide_as(llm_key(), llm).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(driver_view.state().state, FiberState::Pending);

    // 补 tools:门控放行
    let tools_view = root.plugin(ToolsPlugin::new(vec![]));
    (&tools_view).await.expect("tools loads");
    (&driver_view).await.expect("driver loads with llm+tools");
    assert!(root.get_as::<dyn Agent>(agent_key()).is_some());

    // 清理:dispose 顺序无关(driver 是消费者,先 dispose provider 也安全)
    llm_disposer.dispose().await.unwrap();
    tools_view.dispose().await.unwrap();
    driver_view.dispose().await.unwrap();
}

// 2. 卸载 llm → driver 自动驱逐回 Pending;重 provide → 自动重载
//(依赖驱动重载是框架差异化能力,驱逐断言钉死——验证文档 §一 集成层)
#[tokio::test]
async fn unloading_llm_evicts_and_reloads_driver() {
    let root = Ctx::root().unwrap();
    let llm: Arc<dyn LanguageModel> = Arc::new(MockReplayModel::new(
        "openai",
        "gpt-4o",
        vec![replay_recording("ping", "pong")],
    ));
    let llm_disposer = root.provide_as(llm_key(), llm).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(vec![]));
    let driver_view = root.plugin(AgentDriverPlugin::new(16));
    (&tools_view).await.expect("tools loads");
    (&driver_view).await.expect("driver loads");
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let session1 = agent.id();

    // 只 dispose llm:无需手动 dispose driver(设计 §六)
    llm_disposer.dispose().await.unwrap();
    wait_state(&driver_view, FiberState::Pending).await;
    assert!(root.get_as::<dyn Agent>(agent_key()).is_none()); // 驱逐:服务消失

    // 重新 provide llm → driver 自动重载(新 driver,新 session)
    let llm2: Arc<dyn LanguageModel> = Arc::new(MockReplayModel::new(
        "openai",
        "gpt-4o",
        vec![replay_recording("ping", "pong")],
    ));
    let _keep = root.provide_as(llm_key(), llm2).unwrap();
    wait_state(&driver_view, FiberState::Active).await;
    let agent2 = root.get_as::<dyn Agent>(agent_key()).unwrap();
    assert_ne!(agent2.id(), session1); // 重载即新 session

    tools_view.dispose().await.unwrap();
    driver_view.dispose().await.unwrap();
}

// 3. MockReplayModel 驱动完整 followup(真 LanguageModel 接口,录制回放)
#[tokio::test]
async fn replay_backend_drives_followup() {
    let root = Ctx::root().unwrap();
    let llm: Arc<dyn LanguageModel> = Arc::new(MockReplayModel::new(
        "openai",
        "gpt-4o",
        vec![replay_recording("ping", "pong")],
    ));
    root.provide_as(llm_key(), llm).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(vec![]));
    let driver_view = root.plugin(AgentDriverPlugin::new(16));
    (&tools_view).await.expect("tools loads");
    (&driver_view).await.expect("driver loads");

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    // 监听器挂独立 fiber,生存期覆盖整个 turn 与事件派发
    let text: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    struct Audit {
        text: Arc<Mutex<String>>,
    }
    impl Plugin for Audit {
        fn name(&self) -> &str {
            "audit"
        }
        fn apply<'a>(
            &'a self,
            ctx: &'a Ctx,
        ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
            let text = self.text.clone();
            Box::pin(async move {
                ctx.events().on(ctx, TextL(text))?;
                Ok(rutis::Effect::Done)
            })
        }
    }
    let audit_view = root.plugin(Audit { text: text.clone() });
    (&audit_view).await.expect("audit loads");

    let done = soon(agent.followup("ping")).await;
    assert_eq!(done.unwrap(), "pong");
    // 流式路径:TextDelta 到达且拼接等于终答
    soon(async {
        loop {
            let t = text.lock().unwrap().clone();
            if t == "pong" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    audit_view.dispose().await.unwrap();
    tools_view.dispose().await.unwrap();
    driver_view.dispose().await.unwrap();
}

// 4. fiber 卸载 → ctx.cancelled() → 运行中的循环停(不靠 sleep,同步点)
#[tokio::test]
async fn fiber_unload_stops_running_loop() {
    let root = Ctx::root().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let s = started.clone();
    let slow_tool = ToolDef::new(
        "slow",
        "slow tool",
        json!({ "type": "object", "properties": {} }),
        move |_args: Value| {
            let s = s.clone();
            async move {
                s.notify_one();
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(Value::String("finally".to_string()))
            }
        },
    );
    let llm: Arc<dyn LanguageModel> = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::tool_calls(vec![tool_call("s1", "slow", json!({}))]),
        LlmResponse::content("never reached"),
    ]));
    root.provide_as(llm_key(), llm).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(vec![slow_tool]));
    let driver_view = root.plugin(AgentDriverPlugin::new(16));
    (&tools_view).await.expect("tools loads");
    (&driver_view).await.expect("driver loads");
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // 后台跑 turn;工具启动后卸载 driver fiber
    let agent2 = agent.clone();
    let run = tokio::spawn(async move { agent2.followup("go slow").await });
    started.notified().await; // 工具已启动(同步点,不靠 sleep)
    driver_view.dispose().await.unwrap();

    // 循环停:终态 Err(Stopped),任务收尾
    let result = soon(run).await.expect("join");
    assert!(
        matches!(result, Err(AgentError::Stopped)),
        "expected Err(Stopped) after fiber unload, got {result:?}"
    );
    tools_view.dispose().await.unwrap();
}

// 5. agent/* 事件经总线观察;监听器随其 fiber 卸载
#[tokio::test]
async fn events_observed_and_listeners_unload_with_fiber() {
    let root = Ctx::root().unwrap();

    #[derive(Clone, Debug)]
    enum Seen {
        Step(usize, Option<String>, usize),
        Tool(String, bool),
    }
    let events: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));

    struct StepL(Arc<Mutex<Vec<Seen>>>);
    impl Listener<AgentStepEvent> for StepL {
        fn call<'a>(
            &'a self,
            _c: &'a Ctx,
            e: &'a AgentStepEvent,
        ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
            let ev = self.0.clone();
            let e = e.clone();
            Box::pin(async move {
                ev.lock()
                    .unwrap()
                    .push(Seen::Step(e.step, e.content.clone(), e.tool_calls));
                Ok(None)
            })
        }
    }
    struct ToolL(Arc<Mutex<Vec<Seen>>>);
    impl Listener<AgentToolResult> for ToolL {
        fn call<'a>(
            &'a self,
            _c: &'a Ctx,
            e: &'a AgentToolResult,
        ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
            let ev = self.0.clone();
            let (name, ok) = (e.name.clone(), e.ok);
            Box::pin(async move {
                ev.lock().unwrap().push(Seen::Tool(name, ok));
                Ok(None)
            })
        }
    }

    struct Audit {
        events: Arc<Mutex<Vec<Seen>>>,
    }
    impl Plugin for Audit {
        fn name(&self) -> &str {
            "audit"
        }
        fn apply<'a>(
            &'a self,
            ctx: &'a Ctx,
        ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
            let events = self.events.clone();
            Box::pin(async move {
                ctx.events().on(ctx, StepL(events.clone()))?;
                ctx.events().on(ctx, ToolL(events))?;
                Ok(rutis::Effect::Done)
            })
        }
    }
    let audit_view = root.plugin(Audit {
        events: events.clone(),
    });
    (&audit_view).await.expect("audit loads");

    // weather 工具一轮
    let weather = ToolDef::new(
        "get_weather",
        "current weather for a city",
        json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
        |args: Value| async move {
            Ok(json!({ "city": args["city"].as_str().unwrap_or("?"), "temp": 30 }))
        },
    );
    let llm: Arc<dyn LanguageModel> = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::tool_calls(vec![tool_call(
            "1",
            "get_weather",
            json!({ "city": "Oslo" }),
        )]),
        LlmResponse::content("30"),
        LlmResponse::tool_calls(vec![tool_call(
            "2",
            "get_weather",
            json!({ "city": "Rome" }),
        )]),
        LlmResponse::content("31"),
    ]));
    root.provide_as(llm_key(), llm).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(vec![weather]));
    let driver_view = root.plugin(AgentDriverPlugin::new(16));
    (&tools_view).await.expect("tools loads");
    (&driver_view).await.expect("driver loads");

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let (_, done) = soon(collect_turn(&root, &agent, "w")).await;
    assert_eq!(done.unwrap(), "30");

    soon(async {
        while events.lock().unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    {
        let snapshot = events.lock().unwrap().clone();
        let steps: Vec<_> = snapshot
            .iter()
            .filter(|s| matches!(s, Seen::Step(..)))
            .collect();
        let tools: Vec<_> = snapshot
            .iter()
            .filter(|t| matches!(t, Seen::Tool(..)))
            .collect();
        assert_eq!(steps.len(), 2);
        assert!(matches!(&steps[0], Seen::Step(1, None, 1)));
        assert!(matches!(&steps[1], Seen::Step(2, Some(c), 0) if c == "30"));
        assert_eq!(tools.len(), 1);
        assert!(matches!(&tools[0], Seen::Tool(name, true) if name == "get_weather"));
    }
    let count_after_run = events.lock().unwrap().len();

    // 监听器随 audit fiber 卸载:同一 driver 再跑一轮,无新事件
    audit_view.dispose().await.unwrap();
    let (_, done2) = soon(collect_turn(&root, &agent, "again")).await;
    assert_eq!(done2.unwrap(), "31");

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(events.lock().unwrap().len(), count_after_run); // 无残留监听器
    tools_view.dispose().await.unwrap();
    driver_view.dispose().await.unwrap();
}

/// 三段管线 = grok hooks PreToolUse matcher 的等价能力(实证):
/// 注册 `tools/pre-execute` 门控 listener,按工具名拒绝特定工具;
/// 该工具不真正执行,模型看到 error 反馈,turn 正常结束。
#[tokio::test]
async fn pre_execute_gate_can_block_specific_tool() {
    let root = Ctx::root().unwrap();
    // 模型第一轮调用 bash;工具被门控拒绝后,它应看到 error(下一个响应收尾)
    let llm: Arc<dyn LanguageModel> = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::tool_calls(vec![tool_call("b1", "bash", json!({"command":"id"}))]),
        LlmResponse::content("understood the block"),
    ]));
    root.provide_as(llm_key(), llm).unwrap();

    // 记录真实执行(若被执行置 true)
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ran2 = ran.clone();
    let bash = ToolDef::new(
        "bash",
        "run a command",
        json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}),
        move |_: Value| {
            let ran = ran2.clone();
            async move {
                ran.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Value::String("UNEXPECTED EXECUTION".into()))
            }
        },
    );

    let tools_view = root.plugin(ToolsPlugin::new(vec![bash]));
    let driver_view = root.plugin(AgentDriverPlugin::new(16));
    (&tools_view).await.expect("tools loads");
    (&driver_view).await.expect("driver loads");

    // 门控:拒绝 bash(matcher = 按 tool_name)
    struct BashGate;
    impl WaterfallListener<ToolPreExecute> for BashGate {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a ToolPreExecute,
            _next: Next<'a, ToolPreExecute>,
        ) -> rutis::BoxFuture<'a, Result<Option<String>, rutis::CordisError>> {
            if e.call.tool_name == "bash" {
                Box::pin(async move { Ok(Some("blocked: audit forbids bash".to_string())) })
            } else {
                Box::pin(async move { Ok(None) })
            }
        }
    }
    struct GateBootstrap;
    impl Plugin for GateBootstrap {
        fn name(&self) -> &str {
            "gate-bootstrap"
        }
        fn apply<'a>(
            &'a self,
            ctx: &'a Ctx,
        ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
            Box::pin(async move {
                ctx.events().on_waterfall(ctx, BashGate)?;
                Ok(rutis::Effect::Done)
            })
        }
    }
    let audit_view = root.plugin(GateBootstrap);
    (&audit_view).await.expect("gate loads");

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let done = soon(agent.followup("run it")).await;
    assert_eq!(done.unwrap(), "understood the block");
    // bash 未真正执行
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst), "bash must NOT execute when gated");
    // model 看到了 error 反馈
    let session = agent.session();
    let msgs = session.messages();
    let joined: String = msgs
        .iter()
        .map(|m| {
            use aimux_core::content::ContentPart;
            use aimux_core::message::MessageContent;
            match &m.content {
                MessageContent::Parts(parts) => parts
                    .iter()
                    .map(|p| match p {
                        ContentPart::ToolResult { result, .. } => result.as_str().unwrap_or(""),
                        _ => "",
                    })
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            }
        })
        .collect();
    assert!(
        joined.contains("blocked: audit forbids bash"),
        "model should see the gate reason, got: {joined}"
    );

    audit_view.dispose().await.unwrap();
    tools_view.dispose().await.unwrap();
    driver_view.dispose().await.unwrap();
}

/// 三段管线另一半 = grok hooks PostToolUse(结果改写/审计)的等价能力(实证):
/// 注册 `tools/post-execute` waterfall listener 改写某工具结果(如脱敏);
/// 模型读到的是改写后的结果(原始值不外泄)。
#[tokio::test]
async fn post_execute_can_rewrite_result() {
    let root = Ctx::root().unwrap();
    // 模型调用 get_weather;post-execute 把它返回的敏感原值改写为脱敏值
    let llm: Arc<dyn LanguageModel> = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::tool_calls(vec![tool_call("w1", "get_weather", json!({"city":"Rome"}))]),
        LlmResponse::content("got REDACTED"),
    ]));
    root.provide_as(llm_key(), llm).unwrap();

    let weather = ToolDef::new(
        "get_weather",
        "Get current weather for a city.",
        json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
        move |_: Value| async move { Ok(Value::String("SENSITIVE_RAW_30C".into())) },
    );
    let tools_view = root.plugin(ToolsPlugin::new(vec![weather]));
    let driver_view = root.plugin(AgentDriverPlugin::new(16));
    (&tools_view).await.expect("tools loads");
    (&driver_view).await.expect("driver loads");

    // 改写:get_weather 的结果 → 脱敏
    struct RedactResult;
    impl WaterfallListener<ToolPostExecute> for RedactResult {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a ToolPostExecute,
            _next: Next<'a, ToolPostExecute>,
        ) -> rutis::BoxFuture<'a, Result<rutis_agent::ToolOutput, rutis::CordisError>> {
            if e.call.tool_name == "get_weather" {
                Box::pin(async move {
                    Ok(rutis_agent::ToolOutput {
                        ok: true,
                        output: "REDACTED".to_string(),
                    })
                })
            } else {
                Box::pin(async move {
                    let _ = e;
                    unreachable!("only get_weather runs")
                })
            }
        }
    }
    struct GateBootstrap2;
    impl Plugin for GateBootstrap2 {
        fn name(&self) -> &str {
            "gate-bootstrap-2"
        }
        fn apply<'a>(
            &'a self,
            ctx: &'a Ctx,
        ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
            Box::pin(async move {
                ctx.events().on_waterfall(ctx, RedactResult)?;
                Ok(rutis::Effect::Done)
            })
        }
    }
    let audit_view = root.plugin(GateBootstrap2);
    (&audit_view).await.expect("gate loads");

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let done = soon(agent.followup("weather now")).await;
    assert_eq!(done.unwrap(), "got REDACTED");

    // 敏感原值未出现在 session(被改写掉)
    let session = agent.session();
    let msgs = session.messages();
    let joined: String = msgs
        .iter()
        .map(|m| {
            use aimux_core::content::ContentPart;
            use aimux_core::message::MessageContent;
            match &m.content {
                MessageContent::Parts(parts) => parts
                    .iter()
                    .map(|p| match p {
                        ContentPart::ToolResult { result, .. } => result.as_str().unwrap_or(""),
                        ContentPart::Text { text, .. } => text.as_str(),
                        _ => "",
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            }
        })
        .collect();
    assert!(
        joined.contains("REDACTED"),
        "rewritten result should be in history, got: {joined}"
    );
    assert!(
        !joined.contains("SENSITIVE_RAW"),
        "raw value must NOT reach history, got: {joined}"
    );

    audit_view.dispose().await.unwrap();
    tools_view.dispose().await.unwrap();
    driver_view.dispose().await.unwrap();
}
