//! rutis-cli——最小 coding agent 的命令行形态(minimal mode)。
//!
//! ```text
//! export DEEPSEEK_API_KEY=... && rutis-cli                    # deepseek-chat
//! rutis-cli --provider ollama --model qwen3:8b                # 本地模型
//! AIMUX_PROVIDER=ollama AIMUX_MODEL=qwen3:8b rutis-cli        # 环境变量等价
//! rutis-cli --scripted                                        # 无 key 离线演示
//! ```
//!
//! 工具集 = `bash` + `replace_text`(能改文件、能跑命令);交互见 TUI 界面
//! 底栏:Enter 提交,Esc / Ctrl+C(运行中)取消当前 turn,Ctrl+q 退出。
//! session 默认持久化到 `<cwd>/.rutis/session.json`,重启恢复历史。

use std::sync::Arc;

use aimux_core::language_model::LanguageModel;
use rutis::Ctx;
use rutis_agent::{
    llm_key, minimal_persona, minimal_tools, AgentDriverPlugin, ToolsPlugin, TuiPlugin,
};

const USAGE: &str = "\
rutis-cli — minimal coding agent (bash + replace_text) on the rutis framework

USAGE:
    rutis-cli [OPTIONS]

OPTIONS:
    -p, --provider <ID>   aimux provider id [env: AIMUX_PROVIDER] [default: deepseek]
    -m, --model <ID>      model id [env: AIMUX_MODEL] [default: deepseek-chat]
        --scripted        offline demo backend (no API key needed)
    -h, --help            print this help
    -V, --version         print the version
";

#[tokio::main]
async fn main() {
    let mut provider = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    let mut model = std::env::var("AIMUX_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
    let mut scripted = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--provider" => provider = value(&mut args, &arg),
            "-m" | "--model" => model = value(&mut args, &arg),
            "--scripted" => scripted = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            "-V" | "--version" => {
                println!("rutis-cli {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => {
                eprintln!("unknown argument: {other}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let llm: Arc<dyn LanguageModel> = if scripted {
        Arc::new(rutis_agent::ScriptedLlm::new(scripted_responses()))
    } else {
        match aimux_providers::provider(&provider, None, &model, None) {
            Ok(m) => Arc::from(m),
            Err(e) => {
                eprintln!("failed to build {provider}/{model}: {e}");
                if provider == "deepseek" && std::env::var_os("DEEPSEEK_API_KEY").is_none() {
                    eprintln!(
                        "hint: export DEEPSEEK_API_KEY=... ; or offline demo: rutis-cli --scripted"
                    );
                }
                std::process::exit(1);
            }
        }
    };
    let model_id = if scripted {
        "scripted".to_string()
    } else {
        model.clone()
    };

    if let Err(e) = run(llm, &provider, &model_id).await {
        eprintln!("rutis-cli failed: {e}");
        std::process::exit(1);
    }
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| {
            eprintln!("missing value for {flag}\n\n{USAGE}");
            std::process::exit(2);
        })
        .trim_matches('"')
        .to_string()
}

async fn run(
    llm: Arc<dyn LanguageModel>,
    provider: &str,
    model: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());
    let root = Ctx::root()?;
    root.provide_as(llm_key(), llm)?;

    // session 持久化(默认 <cwd>/.rutis/session.json,重启恢复历史)
    let tools_view = root.plugin(ToolsPlugin::new(minimal_tools()));
    let driver_view = root.plugin(
        AgentDriverPlugin::new(10000)
            .with_system_prompt(minimal_persona(model, &cwd))
            .with_default_session_path(),
    );

    (&tools_view).await?;
    (&driver_view).await?;

    // TUI 在 driver 装载完成后创建:apply 内 get agent 必成功(启动门控)
    let tui_view = root.plugin(TuiPlugin::new().with_intro(vec![
        format!("backend: {provider}/{model}"),
        format!("cwd: {cwd}"),
        "tools: bash + replace_text | Enter 发送 | Esc 取消 | Ctrl+Q 退出".to_string(),
    ]));
    // TUI apply 即主循环:退出(或 fiber 卸载)后 settle 才完成
    let _ = (&tui_view).await;

    // 卸载 TUI fiber 后级联收尾
    tui_view.dispose().await?;
    driver_view.dispose().await?;
    tools_view.dispose().await?;

    Ok(())
}

/// 离线演示脚本:一轮工具调用(建文件 + cat)+ 一轮终答。
fn scripted_responses() -> Vec<rutis_agent::LlmResponse> {
    use aimux_core::tool::ToolCall;
    use serde_json::json;

    let mk = |id: &str, name: &str, input: serde_json::Value| ToolCall {
        tool_call_id: id.into(),
        tool_name: name.into(),
        input,
        provider_executed: None,
        dynamic: None,
        thought_signature: None,
    };
    vec![
        rutis_agent::LlmResponse::tool_calls(vec![
            mk(
                "c1",
                "replace_text",
                json!({
                    "command": "create",
                    "path": "rutis-cli-demo.txt",
                    "file_text": "hello from the scripted backend\n"
                }),
            ),
            mk(
                "c2",
                "bash",
                json!({
                    "command": "cat rutis-cli-demo.txt",
                    "description": "Show the file just created"
                }),
            ),
        ]),
        rutis_agent::LlmResponse::content(
            "demo done: created rutis-cli-demo.txt and read it back. Ask me to edit real files with a real backend key.",
        ),
    ]
}
