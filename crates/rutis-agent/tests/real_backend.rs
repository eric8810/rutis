//! 真实端到端层(验证文档 §一 第三层):真实 provider 多轮 followup +
//! 工具调用 + history 连续。**不进 CI**:需要 key / 本地服务,手动触发:
//!
//! ```text
//! cargo test -p rutis-agent --test real_backend -- --ignored
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aimux_core::language_model::LanguageModel;
use rutis::Ctx;
use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, AgentTextDelta, AgentToolCall, ToolDef,
    ToolsPlugin,
};
use serde_json::{json, Value};

fn weather_tool() -> ToolDef {
    ToolDef::new(
        "get_weather",
        "current weather for a city",
        json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
        |args: Value| async move {
            let city = args["city"].as_str().unwrap_or("?").to_string();
            Ok(json!({ "city": city, "temp": 18, "sky": "clear" }))
        },
    )
}

/// 收集 `AgentTextDelta` 的 listener:拼接增量进共享缓冲。
struct TextL(Arc<Mutex<String>>);
impl rutis::Listener<AgentTextDelta> for TextL {
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

/// 见到 `get_weather` 工具调用即置位的 listener。
struct ToolFlagL(Arc<AtomicBool>);
impl rutis::Listener<AgentToolCall> for ToolFlagL {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a AgentToolCall,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        let flag = self.0.clone();
        let name = e.name.clone();
        Box::pin(async move {
            if name == "get_weather" {
                flag.store(true, Ordering::SeqCst);
            }
            Ok(None)
        })
    }
}

/// 跑一个 turn,返回拼接增量文本(终态断言在调用侧)。
async fn run_turn(root: &Ctx, agent: &Arc<dyn Agent>, input: &str) -> String {
    let text = Arc::new(Mutex::new(String::new()));
    let _d = root.events().on(root, TextL(text.clone())).unwrap();
    let result = agent.followup(input).await.expect("turn ok");
    drop(result);
    let collected = text.lock().unwrap().clone();
    collected
}

fn backend() -> Arc<dyn LanguageModel> {
    let provider = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    let model_id = std::env::var("AIMUX_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
    let model = aimux_providers::provider(&provider, None, &model_id, None)
        .unwrap_or_else(|e| panic!("build {provider}/{model_id}: {e}"));
    Arc::from(model)
}

#[tokio::test]
#[ignore = "real backend: needs DEEPSEEK_API_KEY (or AIMUX_PROVIDER/AIMUX_MODEL for local ollama)"]
async fn real_backend_multi_turn_with_tool() {
    // 无 key 时明确跳过(而不是在 provider 构造处 panic)
    let provider = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    let env_var = format!("{}_API_KEY", provider.to_uppercase().replace('-', "_"));
    if std::env::var_os(&env_var).is_none() && provider != "ollama" && provider != "lmstudio" {
        eprintln!("skipping: set {env_var} (or AIMUX_PROVIDER=ollama with local service)");
        return;
    }

    let root = Ctx::root().unwrap();
    root.provide_as(llm_key(), backend()).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(vec![weather_tool()]));
    let driver_view = root.plugin(AgentDriverPlugin::new(16));
    (&tools_view).await.expect("tools loads");
    (&driver_view).await.expect("driver loads");
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // 第一轮:期望模型调用 get_weather(经 agent/tool-call 事件观察)
    let saw_tool = Arc::new(AtomicBool::new(false));
    let _tool_l = root
        .events()
        .on(&root, ToolFlagL(saw_tool.clone()))
        .unwrap();
    let a1 = run_turn(
        &root,
        &agent,
        "What's the weather in Oslo? Use the get_weather tool.",
    )
    .await;
    assert!(
        saw_tool.load(Ordering::SeqCst),
        "expected a get_weather tool call, answer was: {a1}"
    );
    assert!(!a1.is_empty(), "final answer empty");

    // 第二轮:history 连续(模型能指代第一轮)
    let a2 = run_turn(&root, &agent, "And in Bergen?").await;
    assert!(!a2.is_empty(), "second turn empty");
    assert!(agent.session().messages().len() >= 5); // u1,a1,u2,a2 + 工具往返
}
