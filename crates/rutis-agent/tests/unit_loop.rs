//! 单元层(验证文档 §一):脚本后端 `ScriptedLlm`(实现真 `LanguageModel`)
//! 验证循环逻辑——无工具终答 / 工具往返回喂 / 多轮 history 连续 /
//! max_steps 截断 / cancel 步间生效 / 工具失败与 panic 回喂不崩。

use std::sync::Arc;
use std::time::Duration;

use aimux_core::content::ContentPart;
use aimux_core::language_model::LanguageModel;
use aimux_core::message::Role;
use futures::StreamExt;
use rutis::{Ctx, FiberState, FiberView};
use rutis_agent::{
    agent_key, llm_key, tool_call, Agent, AgentDriverPlugin, AgentError, AgentStatus, LlmResponse,
    ScriptedLlm, ToolDef, ToolsPlugin, TurnEvent,
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

/// 消费一个 turn 的事件流,返回 (拼接文本, 终态)。
async fn run_turn(agent: &Arc<dyn Agent>, input: &str) -> (String, Result<String, AgentError>) {
    let mut text = String::new();
    let mut done = None;
    let mut stream = agent.followup(input);
    while let Some(ev) = stream.next().await {
        match ev {
            TurnEvent::TextDelta(d) => text.push_str(&d),
            TurnEvent::Done(r) => done = Some(r),
            TurnEvent::ToolCall { .. } | TurnEvent::ToolResult { .. } => {}
        }
    }
    (text, done.expect("stream ends with Done"))
}

async fn soon<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .expect("timed out")
}

// 1. 无工具:TextDelta 流式 + Done 终答
#[tokio::test]
async fn direct_answer_without_tools() {
    let root = Ctx::root().unwrap();
    let (_v, llm) = load_driver(&root, vec![LlmResponse::content("42")], vec![], None).await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    let (text, done) = soon(run_turn(&agent, "meaning of life?")).await;
    assert_eq!(done.unwrap(), "42");
    assert_eq!(text, "42"); // 拼接增量 = 终答

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

    let mut deltas = 0;
    let mut stream = agent.followup("stream");
    while let Some(ev) = stream.next().await {
        if let TurnEvent::TextDelta(_) = ev {
            deltas += 1;
        }
    }
    assert_eq!(deltas, 3); // "abcd" "efgh" "ij"
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

    let (text, done) = soon(run_turn(&agent, "weather in Oslo?")).await;
    assert_eq!(done.unwrap(), "Oslo: 30 degrees");
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
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
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

    let mut events = Vec::new();
    let mut stream = agent.followup("add twice");
    while let Some(ev) = stream.next().await {
        if matches!(
            ev,
            TurnEvent::ToolCall { .. } | TurnEvent::ToolResult { .. }
        ) {
            events.push(ev);
        }
    }
    assert_eq!(*order.lock().unwrap(), vec![(1, 2), (3, 4)]);
    assert!(matches!(&events[0], TurnEvent::ToolCall { name, .. } if name == "add"));
    assert!(matches!(&events[1], TurnEvent::ToolResult { ok: true, output, .. } if output == "3"));
    assert!(matches!(&events[2], TurnEvent::ToolCall { name, .. } if name == "add"));
    assert!(matches!(&events[3], TurnEvent::ToolResult { ok: true, output, .. } if output == "7"));

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

    let (_, done) = soon(run_turn(&agent, "try it")).await;
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

    let (_, done) = soon(run_turn(&agent, "call something odd")).await;
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

    let (_, done) = soon(run_turn(&agent, "go")).await;
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

    let (_, done) = soon(run_turn(&agent, "double 4")).await;
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

    let (_, d1) = soon(run_turn(&agent, "q1")).await;
    assert_eq!(d1.unwrap(), "first");
    let (_, d2) = soon(run_turn(&agent, "q2")).await;
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

    let (_, done) = soon(run_turn(&agent, "go")).await;
    assert!(matches!(done, Err(AgentError::Stopped)));
    assert_eq!(llm.calls.lock().unwrap().len(), 1); // 第二次模型调用没发生

    // 取消不粘滞:下一 turn 正常
    let (_, done) = soon(run_turn(&agent, "again")).await;
    assert_eq!(done.unwrap(), "never reached");
}

// 11. cancel 中断流式输出(流中 select,不等 Finish)
#[tokio::test]
async fn cancel_interrupts_mid_stream() {
    let root = Ctx::root().unwrap();
    // 后端无响应可弹 → do_stream 直接报错;这里改为:响应在,但消费到一半取消
    let (_v, _llm) = load_driver(
        &root,
        vec![LlmResponse::content("abcdefghij")],
        vec![],
        None,
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    let mut stream = agent.followup("stream");
    let first = stream.next().await; // 第一个 TextDelta
    assert!(matches!(first, Some(TurnEvent::TextDelta(_))));
    agent.cancel();
    let mut last = None;
    while let Some(ev) = stream.next().await {
        if matches!(ev, TurnEvent::Done(_)) {
            last = Some(ev);
        }
    }
    match last {
        Some(TurnEvent::Done(Err(AgentError::Stopped))) => {}
        other => panic!("expected Done(Err(Stopped)), got {other:?}"),
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

    let (_, done) = soon(run_turn(&agent, "run forever")).await;
    assert!(matches!(done, Err(AgentError::MaxSteps(3))));
    assert_eq!(llm.calls.lock().unwrap().len(), 3);
}

// 13. llm 失败终止 turn
#[tokio::test]
async fn llm_error_terminates_turn() {
    let root = Ctx::root().unwrap();
    let (_v, _llm) = load_driver(&root, vec![], vec![], None).await; // 无响应可弹
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    let (_, done) = soon(run_turn(&agent, "hi")).await;
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

    let mut stream = agent.followup("hi");
    let _ = stream.next().await; // 推动 stream 启动(懒执行)
    assert_eq!(agent.status(), AgentStatus::Running);
    while let Some(ev) = stream.next().await {
        if matches!(ev, TurnEvent::Done(_)) {
            break;
        }
    }
    assert_eq!(agent.status(), AgentStatus::Idle);

    // 错误路径也回 idle
    let (_, done) = soon(run_turn(&agent, "no responses left")).await;
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
