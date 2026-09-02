//! TUI demo(验证文档 §二 + minimal mode):真实 provider + `TuiPlugin`,
//! 挂 `bash` + `replace_text`(能改文件、能跑命令的最小 coding agent)。
//!
//! ```text
//! cargo run -p rutis-agent --example tui                        # deepseek(读 DEEPSEEK_API_KEY)
//! AIMUX_PROVIDER=ollama AIMUX_MODEL=qwen3:8b cargo run -p rutis-agent --example tui
//! ```
//!
//! 交互:Enter 提交;Esc / Ctrl+C(运行中)取消当前 turn;Ctrl+Q 退出。

use std::sync::Arc;

use aimux_core::language_model::LanguageModel;
use rutis::Ctx;
use rutis_agent::{
    llm_key, minimal_persona, minimal_tools, AgentDriverPlugin, ToolDef, ToolsPlugin, TuiPlugin,
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
    let provider = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    let model_id = std::env::var("AIMUX_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
    let model = match aimux_providers::provider(&provider, None, &model_id, None) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("failed to build {provider}/{model_id}: {e}");
            if provider == "deepseek" && std::env::var_os("DEEPSEEK_API_KEY").is_none() {
                eprintln!(
                    "hint: export DEEPSEEK_API_KEY=... ; or offline: cargo run -p rutis-agent --example tui_scripted"
                );
            }
            std::process::exit(1);
        }
    };
    let llm: Arc<dyn LanguageModel> = Arc::from(model);

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());
    let root = Ctx::root().expect("run inside a tokio runtime");
    root.provide_as(llm_key(), llm)
        .expect("provide llm service");
    // minimal mode:bash + replace_text + get_weather
    let mut tools = minimal_tools();
    tools.push(weather_tool());
    let tools_view = root.plugin(ToolsPlugin::new(tools));
    let driver_view = root
        .plugin(
            AgentDriverPlugin::new(10000)
                .with_system_prompt(minimal_persona(&model_id, &cwd))
                .with_default_session_path(),
        );
    (&tools_view).await.expect("tools loads");
    (&driver_view).await.expect("driver loads");

    // TUI 在 driver 装载完成后创建:apply 内 get agent 必成功(启动门控)
    let tui_view = root.plugin(TuiPlugin::new().with_intro(vec![
        format!("backend: {provider}/{model_id}"),
        format!("cwd: {cwd}"),
        "tools: bash + replace_text + get_weather | Enter 发送 | Esc 取消 | Ctrl+Q 退出".to_string(),
    ]));
    // TUI apply 即主循环:退出(或 fiber 卸载)后 settle 才完成
    match (&tui_view).await {
        Ok(()) => println!("tui exited"),
        Err(e) => eprintln!("tui failed: {e}"),
    }

    // 卸载 TUI fiber 后级联收尾
    tui_view.dispose().await.unwrap();
    driver_view.dispose().await.unwrap();
    tools_view.dispose().await.unwrap();
}
