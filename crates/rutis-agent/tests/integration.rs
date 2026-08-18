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
use futures::StreamExt;
use rutis::{Ctx, FiberState, FiberView, Listener, Plugin};
use rutis_agent::{
    agent_key, llm_key, tool_call, Agent, AgentDriverPlugin, AgentError, AgentStepEvent,
    AgentToolEvent, LlmResponse, ScriptedLlm, ToolDef, ToolsPlugin, TurnEvent,
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

/// 消费一个 turn,收集全部事件。
async fn collect_turn(agent: &Arc<dyn Agent>, input: &str) -> Vec<TurnEvent> {
    let mut events = Vec::new();
    let mut stream = agent.followup(input);
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }
    events
}

fn final_answer(events: &[TurnEvent]) -> Result<String, AgentError> {
    match events.last() {
        Some(TurnEvent::Done(r)) => match r {
            Ok(s) => Ok(s.clone()),
            Err(e) => Err(error_clone(e)),
        },
        other => panic!("expected Done tail, got {other:?}"),
    }
}

fn error_clone(e: &AgentError) -> AgentError {
    match e {
        AgentError::Stopped => AgentError::Stopped,
        AgentError::MaxSteps(n) => AgentError::MaxSteps(*n),
        AgentError::Llm(s) => AgentError::Llm(s.clone()),
    }
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
    let events = soon(collect_turn(&agent, "ping")).await;
    assert_eq!(final_answer(&events).unwrap(), "pong");
    // 流式路径:TextDelta 到达且拼接等于终答
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::TextDelta(d) => Some(d.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "pong");
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

    // 后台消费 turn;工具启动后卸载 driver fiber
    let (tx, mut rx) = tokio::sync::mpsc::channel::<TurnEvent>(16);
    let consume = tokio::spawn(async move {
        let mut stream = agent.followup("go slow");
        while let Some(ev) = stream.next().await {
            if tx.send(ev).await.is_err() {
                break;
            }
        }
    });
    started.notified().await; // 工具已启动(同步点,不靠 sleep)
    driver_view.dispose().await.unwrap();

    // 循环停:Done(Err(Stopped)) 到达,任务收尾
    let mut saw_stopped = false;
    soon(async {
        while let Some(ev) = rx.recv().await {
            if let TurnEvent::Done(Err(AgentError::Stopped)) = ev {
                saw_stopped = true;
                break;
            }
        }
    })
    .await;
    assert!(
        saw_stopped,
        "expected Done(Err(Stopped)) after fiber unload"
    );
    soon(consume).await.unwrap();
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
    impl Listener<AgentToolEvent> for ToolL {
        fn call<'a>(
            &'a self,
            _c: &'a Ctx,
            e: &'a AgentToolEvent,
        ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
            let ev = self.0.clone();
            let e = e.clone();
            Box::pin(async move {
                ev.lock().unwrap().push(Seen::Tool(e.name.clone(), e.ok));
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
        ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>>
        {
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
    let events_run = soon(collect_turn(&agent, "w")).await;
    assert_eq!(final_answer(&events_run).unwrap(), "30");

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
    let events_run2 = soon(collect_turn(&agent, "again")).await;
    assert_eq!(final_answer(&events_run2).unwrap(), "31");

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(events.lock().unwrap().len(), count_after_run); // 无残留监听器
    tools_view.dispose().await.unwrap();
    driver_view.dispose().await.unwrap();
}
