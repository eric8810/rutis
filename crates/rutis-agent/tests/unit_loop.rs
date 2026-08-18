//! 单元层(验证文档 §一):脚本后端 `ScriptedLlm`(实现真 `LanguageModel`)
//! 验证循环逻辑——无工具终答 / 工具往返回喂 / 多轮 history 连续 /
//! max_steps 截断 / cancel 步间生效 / 工具失败与 panic 回喂不崩。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimux_core::content::ContentPart;
use aimux_core::language_model::LanguageModel;
use aimux_core::message::Role;
use rutis::{Ctx, FiberState, FiberView, Listener};
use rutis_agent::{
    agent_key, llm_key, tool_call, Agent, AgentDriverPlugin, AgentError, AgentStatus, AgentTextDelta,
    AgentToolCall, AgentToolResult, AgentTurnEnd, LlmResponse, ScriptedLlm, ToolDef, ToolsPlugin,
};
use serde_json::{json, Value};

fn weather_tool() -> ToolDef {
    ToolDef::new(
        "get_weather",
        "current weather for a city",
        json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
        |args: Value| async move {
            let city = args["city"].as_str().unwrap_or("?").to_string();
            Ok(json!({ "city": city, "temp": 30 }))
        },
    )
}

/// 组装 llm(直接 provide)+ tools + driver(插件机制)。
async fn load_driver(
    root: &Ctx,
    responses: Vec<LlmResponse>,
    tools: Vec<ToolDef>,
    max_steps: Option<usize>,
) -> (FiberView, Arc<ScriptedLlm>) {
    let llm = Arc::new(ScriptedLlm::new(responses));
    let service: Arc<dyn LanguageModel> = llm.clone();
    root.provide_as(llm_key(), service).unwrap();
    root.plugin(ToolsPlugin::new(tools));
    let view = root.plugin(AgentDriverPlugin::new(max_steps.unwrap_or(16)));
    (&view).await.expect("driver loads (gated on llm+tools)");
    (view, llm)
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

/// 统计 `AgentTextDelta` 次数的 listener。
struct CountL(Arc<AtomicUsize>);
impl Listener<AgentTextDelta> for CountL {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a AgentTextDelta,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        let n = self.0.clone();
        Box::pin(async move {
            n.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    }
}

/// `AgentTextDelta` → mpsc 通道。
struct DeltaTxL(tokio::sync::mpsc::Sender<String>);
impl Listener<AgentTextDelta> for DeltaTxL {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a AgentTextDelta,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        let tx = self.0.clone();
        let d = e.delta.clone();
        Box::pin(async move {
            let _ = tx.send(d).await;
            Ok(None)
        })
    }
}

/// `AgentTurnEnd` → mpsc 通道。
struct TurnEndTxL(tokio::sync::mpsc::Sender<()>);
impl Listener<AgentTurnEnd> for TurnEndTxL {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a AgentTurnEnd,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        let tx = self.0.clone();
        Box::pin(async move {
            let _ = tx.send(()).await;
            Ok(None)
        })
    }
}

/// 挂观察 listener 的常驻 fiber:效应注册进该 fiber 的记录,
/// 测试全程存活,测试末尾 dispose 回收。返回 (FiberView, 子 Ctx)。
async fn observer(root: &Ctx, f: impl Fn(&Ctx) + Send + Sync + 'static) -> (FiberView, Ctx) {
    struct Observer<F> {
        f: F,
        tx: Mutex<Option<tokio::sync::oneshot::Sender<Ctx>>>,
    }
    impl<F: Fn(&Ctx) + Send + Sync + 'static> rutis::Plugin for Observer<F> {
        fn name(&self) -> &str {
            "observer"
        }
        fn apply<'a>(
            &'a self,
            ctx: &'a Ctx,
        ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
            (self.f)(ctx);
            if let Some(tx) = self.tx.lock().unwrap().take() {
                let _ = tx.send(ctx.clone());
            }
            Box::pin(async move { Ok(rutis::Effect::Done) })
        }
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let view = root.plugin(Observer {
        f,
        tx: Mutex::new(Some(tx)),
    });
    (&view).await.expect("observer loads");
    let child = soon(rx).await.expect("observer ctx");
    (view, child)
}

/// 跑一个 turn:先注册 text listener 收增量,followup 回终态。
///(emit 逐事件 spawn,派发与返回并发;文本断言只在增量语义
/// 专门的测试里做,见 direct_answer_without_tools。)
async fn run_turn(
    observer: &Ctx,
    agent: &Arc<dyn Agent>,
    input: &str,
) -> (String, Result<String, AgentError>) {
    let text = Arc::new(Mutex::new(String::new()));
    let _d = observer.events().on(observer, TextL(text.clone())).unwrap();
    let result = agent.followup(input).await;
    let collected = text.lock().unwrap().clone();
    (collected, result)
}

async fn soon<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .expect("timed out")
}

// 1. 无工具:TextDelta 流式 + 终答
#[tokio::test]
async fn direct_answer_without_tools() {
    let root = Ctx::root().unwrap();
    let (_v, llm) = load_driver(&root, vec![LlmResponse::content("42")], vec![], None).await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (_ov, _octx) = observer(&root, move |ctx| {
        ctx.events().on(ctx, DeltaTxL(tx.clone())).unwrap();
    })
    .await;

    let done = soon(agent.followup("meaning of life?")).await;
    assert_eq!(done.unwrap(), "42");
    // 拼接增量 = 终答(单 delta;等事件派发)
    let text = soon(rx.recv()).await.expect("text delta");
    assert_eq!(text, "42");

    // 首次调用:仅一条 user 消息,无 tools schema
    let calls = llm.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].message_texts(),
        vec![(Role::User, "meaning of life?".to_string())]
    );
    assert!(calls[0].tools.is_empty());
}

// 2. 流式:一条内容以多个 TextDelta 到达(4 字符分块)
#[tokio::test]
async fn text_arrives_as_multiple_deltas() {
    let root = Ctx::root().unwrap();
    let (_v, _llm) = load_driver(
        &root,
        vec![LlmResponse::content("abcdefghij")],
        vec![],
        None,
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    let deltas = Arc::new(AtomicUsize::new(0));
    let text: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let (n, t) = (deltas.clone(), text.clone());
    let (_ov, _octx) = observer(&root, move |ctx| {
        ctx.events().on(ctx, CountL(n.clone())).unwrap();
        ctx.events().on(ctx, TextL(t.clone())).unwrap();
    })
    .await;
    agent.followup("stream").await.unwrap();
    soon(async {
        while deltas.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    assert_eq!(deltas.load(Ordering::SeqCst), 3); // 3 个增量
    // 增量内容集合 = {"abcd","efgh","ij"}(派发逐事件 spawn,不保证顺序)
    let got = text.lock().unwrap().clone();
    for chunk in ["abcd", "efgh", "ij"] {
        assert!(got.contains(chunk), "{got}");
    }
    assert_eq!(got.len(), 10); // 拼接总量 = 完整响应
}

// 3. 工具往返:第二次模型调用看到 assistant 工具调用与工具结果
#[tokio::test]
async fn tool_round_trip() {
    let root = Ctx::root().unwrap();
    let (_v, llm) = load_driver(
        &root,
        vec![
            LlmResponse::tool_calls(vec![tool_call(
                "1",
                "get_weather",
                json!({ "city": "Oslo" }),
            )]),
            LlmResponse::content("Oslo: 30 degrees"),
        ],
        vec![weather_tool()],
        None,
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (_ov, _octx) = observer(&root, move |ctx| {
        ctx.events().on(ctx, DeltaTxL(tx.clone())).unwrap();
    })
    .await;

    let done = soon(agent.followup("weather in Oslo?")).await;
    assert_eq!(done.unwrap(), "Oslo: 30 degrees");
    // 拼接增量 = 终答(4 字符分块;按序收齐)
    let mut text = String::new();
    while text.len() < "Oslo: 30 degrees".len() {
        text.push_str(&soon(rx.recv()).await.expect("text delta"));
    }
    assert_eq!(text, "Oslo: 30 degrees");

    let calls = llm.calls.lock().unwrap();
    let second = &calls[1];
    // history:user / assistant(tool_call) / tool(result)
    let roles: Vec<Role> = second.prompt.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
    // assistant 消息含 ToolCall part
    match &second.prompt[1].content[0] {
        ContentPart::ToolCall {
            tool_call_id,
            tool_name,
            input,
            ..
        } => {
            assert_eq!(tool_call_id, "1");
            assert_eq!(tool_name, "get_weather");
            assert_eq!(input, &json!({ "city": "Oslo" }));
        }
        other => panic!("expected ToolCall part, got {other:?}"),
    }
    // tool 结果回喂(非字符串返回值 JSON 序列化为文本)
    match &second.prompt[2].content[0] {
        ContentPart::ToolResult {
            tool_call_id,
            result,
            ..
        } => {
            assert_eq!(tool_call_id, "1");
            let parsed: Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
            assert_eq!(parsed, json!({ "city": "Oslo", "temp": 30 }));
        }
        other => panic!("expected ToolResult part, got {other:?}"),
    }
    // schema 每步都提供
    assert_eq!(second.tools.len(), 1);
    match &second.tools[0] {
        aimux_core::options::Tool::Function(f) => {
            assert_eq!(f.name, "get_weather");
            assert_eq!(f.input_schema, weather_tool().tool.input_schema);
        }
        other => panic!("expected function tool, got {other:?}"),
    }
}

// 4. 多工具调用按序执行,ToolCall/ToolResult 事件成对可见
#[tokio::test]
async fn multiple_tool_calls_run_in_order() {
    let root = Ctx::root().unwrap();
    let order = Arc::new(Mutex::new(Vec::new()));
    let o = order.clone();
    let add_tool = ToolDef::new(
        "add",
        "sum two integers",
        json!({ "type": "object", "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } } }),
        move |args: Value| {
            let o = o.clone();
            async move {
                let a = args["a"].as_i64().unwrap();
                let b = args["b"].as_i64().unwrap();
                o.lock().unwrap().push((a, b));
                Ok(Value::String((a + b).to_string()))
            }
        },
    );
    let (_v, llm) = load_driver(
        &root,
        vec![
            LlmResponse::tool_calls(vec![
                tool_call("t1", "add", json!({ "a": 1, "b": 2 })),
                tool_call("t2", "add", json!({ "a": 3, "b": 4 })),
            ]),
            LlmResponse::content("done"),
        ],
        vec![add_tool],
        None,
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    #[derive(Debug)]
    enum Seen {
        Call(String),
        Result { ok: bool, output: String },
    }

    /// 工具事件 → mpsc 通道(保序)。
    struct ToolCallTxL(tokio::sync::mpsc::Sender<Seen>);
    impl Listener<AgentToolCall> for ToolCallTxL {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a AgentToolCall,
        ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
            let tx = self.0.clone();
            let name = e.name.clone();
            Box::pin(async move {
                let _ = tx.send(Seen::Call(name)).await;
                Ok(None)
            })
        }
    }
    struct ToolResultTxL(tokio::sync::mpsc::Sender<Seen>);
    impl Listener<AgentToolResult> for ToolResultTxL {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a AgentToolResult,
        ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
            let tx = self.0.clone();
            let (ok, output) = (e.ok, e.output.clone());
            Box::pin(async move {
                let _ = tx.send(Seen::Result { ok, output }).await;
                Ok(None)
            })
        }
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Seen>(8);
    let (tx1, tx2) = (tx.clone(), tx.clone());
    let (_ov, _octx) = observer(&root, move |ctx| {
        ctx.events().on(ctx, ToolCallTxL(tx1.clone())).unwrap();
        ctx.events().on(ctx, ToolResultTxL(tx2.clone())).unwrap();
    })
    .await;
    agent.followup("add twice").await.unwrap();
    // 工具执行本身有序(driver 逐个执行),每类事件序列即执行序
    let mut calls = Vec::new();
    let mut results = Vec::new();
    for _ in 0..4 {
        match soon(rx.recv()).await.expect("tool event") {
            Seen::Call(n) => calls.push(n),
            Seen::Result { ok, output } => results.push((ok, output)),
        }
    }
    assert_eq!(*order.lock().unwrap(), vec![(1, 2), (3, 4)]);
    assert_eq!(calls, vec!["add".to_string(), "add".to_string()]);
    assert_eq!(
        results,
        vec![(true, "3".to_string()), (true, "7".to_string())]
    );

    // 回喂顺序
    let calls = llm.calls.lock().unwrap();
    let fed: Vec<&ContentPart> = calls[1]
        .prompt
        .iter()
        .filter(|m| m.role == Role::Tool)
        .flat_map(|m| m.content.iter())
        .collect();
    assert_eq!(fed.len(), 2);
    assert!(
        matches!(&fed[0], ContentPart::ToolResult { tool_call_id, .. } if tool_call_id == "t1")
    );
    assert!(
        matches!(&fed[1], ContentPart::ToolResult { tool_call_id, .. } if tool_call_id == "t2")
    );
}

// 5. 工具失败回喂模型,循环不崩
#[tokio::test]
async fn tool_error_is_fed_back_not_fatal() {
    let root = Ctx::root().unwrap();
    let bad = ToolDef::new(
        "bad",
        "always fails",
        json!({ "type": "object", "properties": { "x": { "type": "integer" } } }),
        |_args: Value| async { Err::<Value, String>("boom".to_string()) },
    );
    let (_v, llm) = load_driver(
        &root,
        vec![
            LlmResponse::tool_calls(vec![tool_call("e1", "bad", json!({ "x": 1 }))]),
            LlmResponse::content("recovered"),
        ],
        vec![bad],
        None,
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let (_ov, octx) = observer(&root, |_| {}).await;

    let (_, done) = soon(run_turn(&octx, &agent, "try it")).await;
    assert_eq!(done.unwrap(), "recovered");
    let calls = llm.calls.lock().unwrap();
    match &calls[1].prompt[2].content[0] {
        ContentPart::ToolResult {
            result, is_error, ..
        } => {
            assert_eq!(result, &json!("error: boom"));
            assert_eq!(*is_error, Some(true));
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

// 6. 未知工具报告给模型
#[tokio::test]
async fn unknown_tool_reported_to_model() {
    let root = Ctx::root().unwrap();
    let (_v, llm) = load_driver(
        &root,
        vec![
            LlmResponse::tool_calls(vec![tool_call("u1", "nope", json!({}))]),
            LlmResponse::content("ok"),
        ],
        vec![],
        None,
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let (_ov, octx) = observer(&root, |_| {}).await;

    let (_, done) = soon(run_turn(&octx, &agent, "call something odd")).await;
    assert_eq!(done.unwrap(), "ok");
    let calls = llm.calls.lock().unwrap();
    match &calls[1].prompt[2].content[0] {
        ContentPart::ToolResult { result, .. } => {
            let s = result.as_str().unwrap();
            assert!(s.contains("unknown tool"), "{s}");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

// 7. 工具 runner panic 转模型可见错误,循环不崩
#[tokio::test]
async fn tool_panic_is_fed_back() {
    let root = Ctx::root().unwrap();
    let tool = ToolDef::new(
        "boom",
        "panics on poll",
        json!({ "type": "object", "properties": {} }),
        |_args: Value| async {
            if true {
                panic!("tool boom");
            }
            #[allow(unreachable_code)]
            Ok::<Value, String>(Value::Null)
        },
    );
    let (_v, llm) = load_driver(
        &root,
        vec![
            LlmResponse::tool_calls(vec![tool_call("p1", "boom", json!({}))]),
            LlmResponse::content("recovered2"),
        ],
        vec![tool],
        None,
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let (_ov, octx) = observer(&root, |_| {}).await;

    let (_, done) = soon(run_turn(&octx, &agent, "go")).await;
    assert_eq!(done.unwrap(), "recovered2");
    let calls = llm.calls.lock().unwrap();
    match &calls[1].prompt[2].content[0] {
        ContentPart::ToolResult {
            result, is_error, ..
        } => {
            assert!(result.as_str().unwrap().starts_with("error:"), "{result}");
            assert_eq!(*is_error, Some(true));
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

// 8. 异步工具
#[tokio::test]
async fn async_tool_supported() {
    let root = Ctx::root().unwrap();
    let tool = ToolDef::new(
        "double",
        "async doubling",
        json!({ "type": "object", "properties": { "x": { "type": "integer" } } }),
        |args: Value| async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(Value::String((args["x"].as_i64().unwrap() * 2).to_string()))
        },
    );
    let (_v, llm) = load_driver(
        &root,
        vec![
            LlmResponse::tool_calls(vec![tool_call("d1", "double", json!({ "x": 4 }))]),
            LlmResponse::content("8 it is"),
        ],
        vec![tool],
        None,
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let (_ov, octx) = observer(&root, |_| {}).await;

    let (_, done) = soon(run_turn(&octx, &agent, "double 4")).await;
    assert_eq!(done.unwrap(), "8 it is");
    let calls = llm.calls.lock().unwrap();
    match &calls[1].prompt[2].content[0] {
        ContentPart::ToolResult { result, .. } => assert_eq!(result, &json!("8")),
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

// 9. 多轮 history 连续:第二轮 followup 的 prompt 含第一轮全部消息
//(钉死"session 是连续 loop 的事实源",验证文档 §一 单元层)
#[tokio::test]
async fn multi_turn_history_is_continuous() {
    let root = Ctx::root().unwrap();
    let (_v, llm) = load_driver(
        &root,
        vec![
            LlmResponse::content("first"),
            LlmResponse::content("second"),
        ],
        vec![],
        None,
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    assert_eq!(agent.id(), agent.session().id()); // id 与 session 共享身份
    let (_ov, octx) = observer(&root, |_| {}).await;

    let (_, d1) = soon(run_turn(&octx, &agent, "q1")).await;
    assert_eq!(d1.unwrap(), "first");
    let (_, d2) = soon(run_turn(&octx, &agent, "q2")).await;
    assert_eq!(d2.unwrap(), "second");

    let calls = llm.calls.lock().unwrap();
    assert_eq!(
        calls[1].message_texts(),
        vec![
            (Role::User, "q1".to_string()),
            (Role::Assistant, "first".to_string()),
            (Role::User, "q2".to_string()),
        ]
    );
    // session 访问器与模型所见一致
    assert_eq!(agent.session().messages().len(), 4);
}

// 10. cancel 在步间生效:工具内触发 cancel,下一次模型调用前停止
#[tokio::test]
async fn cancel_takes_effect_between_steps() {
    let root = Ctx::root().unwrap();
    let root2 = root.clone();
    let stop_tool = ToolDef::new(
        "stop",
        "request cancellation",
        json!({ "type": "object", "properties": {} }),
        move |_args: Value| {
            let root2 = root2.clone();
            async move {
                if let Some(agent) = root2.get_as::<dyn Agent>(agent_key()) {
                    agent.cancel();
                }
                Ok(Value::String("stopping".to_string()))
            }
        },
    );
    let (_v, llm) = load_driver(
        &root,
        vec![
            LlmResponse::tool_calls(vec![tool_call("s1", "stop", json!({}))]),
            LlmResponse::content("never reached"),
        ],
        vec![stop_tool],
        None,
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let (_ov, octx) = observer(&root, |_| {}).await;

    let (_, done) = soon(run_turn(&octx, &agent, "go")).await;
    assert!(matches!(done, Err(AgentError::Stopped)));
    assert_eq!(llm.calls.lock().unwrap().len(), 1); // 第二次模型调用没发生

    // 取消不粘滞:下一 turn 正常
    let (_, done) = soon(run_turn(&octx, &agent, "again")).await;
    assert_eq!(done.unwrap(), "never reached");
}

// 11. cancel 中断流式输出:模型流经自定义 `LanguageModel` 挂起在
//     流中(首个增量后等信号),driver 在流内 select——此时 cancel,
//     循环以 Stopped 收尾
#[tokio::test]
async fn cancel_interrupts_mid_stream() {
    let root = Ctx::root().unwrap();
    // 自定义模型:流先吐一个增量,然后停在信号上(模拟长响应)
    let (first_tx, first_rx) = tokio::sync::oneshot::channel::<()>();
    let (deltas_tx, deltas_rx) = tokio::sync::mpsc::channel::<String>(4);
    let _ = deltas_tx; // 发送端由模型内部持有
    struct HangingModel {
        first: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        deltas: Mutex<Option<tokio::sync::mpsc::Receiver<String>>>,
    }
    #[async_trait::async_trait]
    impl LanguageModel for HangingModel {
        fn provider(&self) -> &str {
            "hanging"
        }
        fn model_id(&self) -> &str {
            "hanging"
        }
        async fn do_generate(
            &self,
            _o: &aimux_core::options::CallOptions,
        ) -> Result<aimux_core::result::GenerateResult, aimux_core::error::AiMuxError> {
            unimplemented!("stream only")
        }
        async fn do_stream(
            &self,
            _o: &aimux_core::options::CallOptions,
        ) -> Result<aimux_core::result::StreamResult, aimux_core::error::AiMuxError> {
            let first = self.first.lock().unwrap().take();
            let mut deltas = self.deltas.lock().unwrap().take();
            let stream = async_stream::stream! {
                yield Ok(aimux_core::stream_part::StreamPart::StreamStart { warnings: vec![] });
                yield Ok(aimux_core::stream_part::StreamPart::TextDelta {
                    id: "t".into(),
                    delta: "chunk-1".into(),
                    provider_metadata: None,
                });
                if let Some(rx) = first {
                    let _ = rx.await; // 挂起:直到测试放行(或 drop)
                }
                // 消化 driver emit 回来的 delta(证明 driver 在流内消费);
                // 通道关闭即 driver 已停
                if let Some(rx) = deltas.as_mut() {
                    while rx.recv().await.is_some() {}
                }
                yield Ok(aimux_core::stream_part::StreamPart::TextDelta {
                    id: "t".into(),
                    delta: "chunk-2".into(),
                    provider_metadata: None,
                });
            };
            Ok(aimux_core::result::StreamResult {
                stream: Box::pin(stream),
                request_body: None,
                response_headers: None,
            })
        }
    }
    let model = Arc::new(HangingModel {
        first: Mutex::new(Some(first_rx)),
        deltas: Mutex::new(Some(deltas_rx)),
    });
    let service: Arc<dyn LanguageModel> = model;
    root.provide_as(llm_key(), service).unwrap();
    root.plugin(ToolsPlugin::new(vec![]));
    let view = root.plugin(AgentDriverPlugin::new(16));
    (&view).await.expect("driver loads");
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // 首个增量到(driver 在流内)→ cancel ⇒ 循环 select 到取消
    let (dtx, mut drx) = tokio::sync::mpsc::channel::<String>(4);
    let (_ov, _octx) = observer(&root, move |ctx| {
        ctx.events().on(ctx, DeltaTxL(dtx.clone())).unwrap();
    })
    .await;

    let agent2 = agent.clone();
    let done = tokio::spawn(async move { agent2.followup("stream").await });
    soon(drx.recv()).await.expect("first TextDelta"); // driver 在流内挂起点前
    agent.cancel(); // 流中取消:循环 select 到 token
    drop(first_tx); // 放行模型流(若还在等)
    let result = soon(done).await.expect("join");
    match result {
        Err(AgentError::Stopped) => {}
        other => panic!("expected Err(Stopped), got {other:?}"),
    }
}

// 12. max_steps 约束失控循环
#[tokio::test]
async fn max_steps_bounds_the_loop() {
    let root = Ctx::root().unwrap();
    let endless: Vec<LlmResponse> = (0..10)
        .map(|i| {
            LlmResponse::tool_calls(vec![tool_call(
                format!("c{i}"),
                "get_weather",
                json!({ "city": "x" }),
            )])
        })
        .collect();
    let (_v, llm) = load_driver(&root, endless, vec![weather_tool()], Some(3)).await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let (_ov, octx) = observer(&root, |_| {}).await;

    let (_, done) = soon(run_turn(&octx, &agent, "run forever")).await;
    assert!(matches!(done, Err(AgentError::MaxSteps(3))));
    assert_eq!(llm.calls.lock().unwrap().len(), 3);
}

// 13. llm 失败终止 turn
#[tokio::test]
async fn llm_error_terminates_turn() {
    let root = Ctx::root().unwrap();
    let (_v, _llm) = load_driver(&root, vec![], vec![], None).await; // 无响应可弹
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let (_ov, octx) = observer(&root, |_| {}).await;

    let (_, done) = soon(run_turn(&octx, &agent, "hi")).await;
    match done {
        Err(AgentError::Llm(e)) => assert!(e.contains("no responses left"), "{e}"),
        other => panic!("expected Llm error, got {other:?}"),
    }
}

// 14. 状态迁移:idle → running → idle(终答与错误路径都回 idle)
#[tokio::test]
async fn status_transitions_and_session_grows() {
    let root = Ctx::root().unwrap();
    let (_v, _llm) = load_driver(&root, vec![LlmResponse::content("ok")], vec![], None).await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    assert_eq!(agent.status(), AgentStatus::Idle);

    // turn 进行中信号:agent/pre-step waterfall 挂起点(停在续延前,
    // turn 保持 Running);终止信号:turn-end 事件
    let (stx, mut srx) = tokio::sync::mpsc::channel::<()>(1);
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    struct HoldL {
        entered: tokio::sync::mpsc::Sender<()>,
        gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }
    impl rutis::WaterfallListener<rutis_agent::AgentPreStep> for HoldL {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            _e: &'a rutis_agent::AgentPreStep,
            next: rutis::Next<'a, rutis_agent::AgentPreStep>,
        ) -> rutis::BoxFuture<'a, Result<Result<(Vec<aimux_core::language_model_message::LanguageModelPromptMessage>, Vec<aimux_core::options::Tool>), String>, rutis::CordisError>> {
            let entered = self.entered.clone();
            let gate = self.gate.lock().unwrap().take();
            Box::pin(async move {
                let _ = entered.send(()).await;
                if let Some(gate) = gate {
                    let _ = gate.await; // 挂起:turn 停在 Running
                }
                next.call().await
            })
        }
    }
    let (etx, mut erx) = tokio::sync::mpsc::channel::<()>(1);
    let hold = HoldL {
        entered: stx,
        gate: Mutex::new(Some(gate_rx)),
    };
    let hold_slot = Mutex::new(Some(hold));
    let (_ov, octx) = observer(&root, move |ctx| {
        ctx.events()
            .on_waterfall(ctx, hold_slot.lock().unwrap().take().unwrap())
            .unwrap();
        ctx.events().on(ctx, TurnEndTxL(etx.clone())).unwrap();
    })
    .await;

    let agent2 = agent.clone();
    let done = tokio::spawn(async move { agent2.followup("hi").await });
    soon(srx.recv()).await.expect("turn entered pre-step");
    assert_eq!(agent.status(), AgentStatus::Running); // turn 挂起,Running 稳定
    gate_tx.send(()).unwrap(); // 放行
    soon(erx.recv()).await.expect("turn end");
    soon(done).await.unwrap().unwrap();
    assert_eq!(agent.status(), AgentStatus::Idle);

    // 错误路径也回 idle
    let (_, done) = soon(run_turn(&octx, &agent, "no responses left")).await;
    assert!(done.is_err());
    assert_eq!(agent.status(), AgentStatus::Idle);
    // user 消息在错误时也已入 session(感知先于思考):
    // turn1 = user+assistant,turn2 失败 = 仅 user
    assert_eq!(agent.session().messages().len(), 3);
}

// 15. 卸载 driver fiber 后服务消失
#[tokio::test]
async fn disposing_driver_removes_service() {
    let root = Ctx::root().unwrap();
    let (view, _llm) = load_driver(&root, vec![LlmResponse::content("x")], vec![], None).await;
    assert!(root.get_as::<dyn Agent>(agent_key()).is_some());
    view.dispose().await.unwrap();
    assert!(root.get_as::<dyn Agent>(agent_key()).is_none());
    assert_eq!(view.state().state, FiberState::Disposed);
}
