//! 真实端到端 demo(验证文档 §一 第三层):真实 provider + 多轮 followup +
//! 工具调用 + 依赖驱动重载(只 dispose llm,driver 自动驱逐)。
//!
//! ```text
//! cargo run -p rutis-agent --example demo                       # deepseek(读 DEEPSEEK_API_KEY)
//! AIMUX_PROVIDER=ollama AIMUX_MODEL=qwen3:8b cargo run -p rutis-agent --example demo
//! ```
//!
//! provider / model 可用 `AIMUX_PROVIDER` / `AIMUX_MODEL` 覆盖
//! (如 ollama 本地:`AIMUX_PROVIDER=ollama AIMUX_MODEL=<model>`)。

use std::sync::Arc;

use aimux_core::language_model::LanguageModel;
use rutis::{BoxFuture, CordisError, Ctx};
use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, AgentTextDelta, AgentToolCall, AgentToolResult,
    ToolDef, ToolsPlugin,
};
use serde_json::{json, Value};

fn weather_tool() -> ToolDef {
    ToolDef::new(
        "get_weather",
        "current weather for a city",
        json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
        |args: Value| async move {
            let city = args["city"].as_str().unwrap_or("?").to_string();
            // 真实实现里这里会请求天气 API
            Ok(json!({ "city": city, "temp": 18, "sky": "clear" }))
        },
    )
}

/// 注册 turn 过程打印监听器(流式逐块经 `agent/*` 事件到达),返终态跑一轮。
async fn run_turn(agent: &Arc<dyn Agent>, input: &str) {
    println!("you> {input}");
    match agent.followup(input).await {
        Ok(_) => println!(),
        Err(e) => println!("! {e}"),
    }
}

// ── 打印监听器(fn 项:省略生命周期满足 Listener 的 for<'a> blanket impl)──

fn print_delta<'a>(
    _ctx: &'a Ctx,
    e: &'a AgentTextDelta,
) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
    let d = e.delta.clone();
    Box::pin(async move {
        use std::io::Write as _;
        print!("{d}");
        let _ = std::io::stdout().flush();
        Ok(None)
    })
}

fn print_tool_call<'a>(
    _ctx: &'a Ctx,
    e: &'a AgentToolCall,
) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
    let s = format!("\n⚙ {}({})", e.name, e.args);
    Box::pin(async move {
        println!("{s}");
        Ok(None)
    })
}

fn print_tool_result<'a>(
    _ctx: &'a Ctx,
    e: &'a AgentToolResult,
) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
    let s = if e.ok {
        format!("  → [{}] {}", e.name, e.output)
    } else {
        format!("  → [{}] FAILED: {}", e.name, e.output)
    };
    Box::pin(async move {
        println!("{s}");
        Ok(None)
    })
}

#[tokio::main]
async fn main() -> Result<(), CordisError> {
    let provider = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    let model_id = std::env::var("AIMUX_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
    let model = aimux_providers::provider(&provider, None, &model_id, None)
        .unwrap_or_else(|e| panic!("build {provider}/{model_id}: {e}"));
    let llm: Arc<dyn LanguageModel> = Arc::from(model);

    let root = Ctx::root().expect("run inside a tokio runtime");

    // 过程增量观察方:订阅 agent/* 事件,逐块打印(归 root 所有,demo 全程有效)
    let _delta = root.events().on(&root, print_delta)?;
    let _tool_call = root.events().on(&root, print_tool_call)?;
    let _tool_result = root.events().on(&root, print_tool_result)?;

    // LLM 服务直接 provide(设计 §六:无 LlmPlugin 空壳)
    let llm_disposer = root
        .provide_as(llm_key(), llm)
        .expect("provide llm service");
    let tools_view = root.plugin(ToolsPlugin::new(vec![weather_tool()]));
    let driver_view = root.plugin(AgentDriverPlugin::new(10000).with_default_session_path());
    (&tools_view).await.expect("tools loads");
    (&driver_view)
        .await
        .expect("driver loads (gated on llm+tools)");

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    run_turn(&agent, "weather in Oslo?").await; // 第一轮
    run_turn(&agent, "and in Bergen?").await; // 第二轮,history 连续

    // 依赖驱动重载:卸载 llm 服务自动驱逐 driver,无需手动 dispose agent_view
    llm_disposer.dispose().await.expect("dispose llm");
    let mut rx = driver_view.watch();
    loop {
        if rx.borrow().state == rutis::FiberState::Pending {
            break;
        }
        rx.changed().await.expect("watch driver eviction");
    }
    println!(
        "llm disposed → driver evicted (agent service: {:?})",
        root.get_as::<dyn Agent>(agent_key()).is_none()
    );

    driver_view.dispose().await.unwrap();
    tools_view.dispose().await.unwrap();
    println!("unloaded cleanly");
    Ok(())
}
