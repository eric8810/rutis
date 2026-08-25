//! 离线 TUI demo:ScriptedLlm 后端(无 key 即可跑),验证 TUI 交互闭环。
//!
//! ```text
//! cargo run -p rutis-agent --example tui_scripted
//! ```
//!
//! 输入任意问题回车:脚本先产出一段 reasoning(思考),再回一次工具调用,
//! 最后给终答。Ctrl+Q 退出。用于验证 reasoning/工具/文本 三类块的渲染。

use std::sync::Arc;

use aimux_core::language_model::LanguageModel;
use rutis::Ctx;
use rutis_agent::{
    llm_key, tool_call, AgentDriverPlugin, LlmResponse, ScriptedLlm, ToolDef, ToolsPlugin,
    TuiPlugin,
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

#[tokio::main]
async fn main() {
    let llm = Arc::new(ScriptedLlm::new(vec![
        // 第一次:先 reasoning(思考),再工具调用
        LlmResponse::tool_calls(vec![tool_call(
            "1",
            "get_weather",
            json!({ "city": "Oslo" }),
        )])
        .with_reasoning(
            "用户想知道天气。我需要先查一下奥斯陆的天气,调用 get_weather 工具,参数 city=Oslo。",
        ),
        // 第二次:先 reasoning,再给终答
        LlmResponse::content(
            "奥斯陆今天 18 度,晴。这是离线脚本后端的固定回复:无论输入什么,都会先演示一次 \
             工具调用,再给出这段较长的中文文本——用来在无 API key 的环境下验证 TUI 的 \
             流式渲染与按显示宽度折行。",
        )
        .with_reasoning("工具已返回 18 度晴,现在直接回答用户。"),
        // 之后的 turn:直接回答
        LlmResponse::content("(scripted) That's all I have."),
    ]));
    let service: Arc<dyn LanguageModel> = llm;

    let root = Ctx::root().expect("run inside a tokio runtime");
    root.provide_as(llm_key(), service).expect("provide llm");
    let tools_view = root.plugin(ToolsPlugin::new(vec![weather_tool()]));
    let driver_view = root.plugin(AgentDriverPlugin::new(10000));
    let tui_view = root.plugin(TuiPlugin::new().with_intro(vec![
        "[scripted backend] 回复为固定脚本,与输入无关(离线冒烟用)。会演示 reasoning(折叠块)。"
            .to_string(),
        "接真实模型:cargo run -p rutis-agent --example tui".to_string(),
    ]));
    (&tools_view).await.expect("tools loads");
    (&driver_view).await.expect("driver loads");
    if let Err(e) = (&tui_view).await {
        eprintln!("tui failed: {e}");
    }
    tui_view.dispose().await.unwrap();
    driver_view.dispose().await.unwrap();
    tools_view.dispose().await.unwrap();
}
