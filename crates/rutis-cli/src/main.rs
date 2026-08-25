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
//! 底栏:Enter 提交,Esc / Ctrl+C(运行中)取消当前 turn,Ctrl+Q 退出。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aimux_core::language_model::LanguageModel;
use rutis::Ctx;
use rutis_agent::{
    llm_key, minimal_persona, minimal_tools, self_tools, AgentDriverPlugin,
    SelfReloadRequested, ToolsPlugin, TuiPlugin,
};

const USAGE: &str = "\
rutis-cli — minimal coding agent (bash + replace_text) on the rutis framework

USAGE:
    rutis-cli [OPTIONS]

OPTIONS:
    -p, --provider <ID>   aimux provider id [env: AIMUX_PROVIDER] [default: deepseek]
    -m, --model <ID>      model id [env: AIMUX_MODEL] [default: deepseek-chat]
        --scripted        offline demo backend (no API key needed)
        --reload-demo     (scripted) first turn calls self_reload to demo hot-restart
    -h, --help            print this help
    -V, --version         print version
";

#[tokio::main]
async fn main() {
    let mut provider = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    let mut model = std::env::var("AIMUX_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
    let mut scripted = false;
    let mut reload_demo = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--provider" => provider = value(&mut args, &arg),
            "-m" | "--model" => model = value(&mut args, &arg),
            "--scripted" => scripted = true,
            "--reload-demo" => reload_demo = true,
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
        Arc::new(rutis_agent::ScriptedLlm::new(scripted_responses(reload_demo)))
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

    if let Err(e) = run(llm, &provider, &model_id, reload_demo).await {
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

/// 宿主侧 `SelfReloadRequested` 监听器:收到 self_reload 请求后
/// 标记重启意图并 dispose root fiber(TUI 循环经 ctx.cancelled()
/// 优雅退出),run() 收尾时 exec 重启进程(保留环境与参数)。
struct ReloadHandler {
    root: Ctx,
    requested: Arc<AtomicBool>,
}

impl rutis::Listener<SelfReloadRequested> for ReloadHandler {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a SelfReloadRequested,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        let root = self.root.clone();
        let requested = self.requested.clone();
        Box::pin(async move {
            requested.store(true, Ordering::SeqCst);
            // 触发 root fiber 卸载 → TUI 循环 ctx.cancelled() 优雅退出
            let _ = root.root_view().dispose().await;
            Ok(None)
        })
    }
}

async fn run(
    llm: Arc<dyn LanguageModel>,
    provider: &str,
    model: &str,
    _reload_demo: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());
    let root = Ctx::root()?;
    root.provide_as(llm_key(), llm)?;

    // session 持久化路径(默认 cwd/.rutis/session.json)+ 自我控制工具包
    let session_path = PathBuf::from(&cwd).join(".rutis").join("session.json");
    let mut tools = self_tools(root.clone());
    tools.extend(minimal_tools());
    let tools_view = root.plugin(ToolsPlugin::new(tools));
    let driver_view = root.plugin(
        AgentDriverPlugin::new(10000)
            .with_system_prompt(minimal_persona(model, &cwd))
            .with_session_path(&session_path),
    );

    // 宿主侧重载监听:self_reload 请求 → 优雅退出 → exec 重启
    let reload_requested = Arc::new(AtomicBool::new(false));
    root.events().on(&root, ReloadHandler {
        root: root.clone(),
        requested: reload_requested.clone(),
    })?;

    let tui_view = root.plugin(TuiPlugin::new().with_intro(vec![
        format!("backend: {provider}/{model}"),
        format!("cwd: {cwd}"),
        "tools: bash + replace_text + self_* | Enter 发送 | Esc 取消 | Ctrl+Q 退出".to_string(),
    ]));
    (&tools_view).await?;
    (&driver_view).await?;
    // TUI apply 即主循环:退出(或 fiber 卸载)后 settle 才完成
    let _ = (&tui_view).await;

    // 卸载 TUI fiber 后级联收尾
    tui_view.dispose().await?;
    driver_view.dispose().await?;
    tools_view.dispose().await?;

    // 自我重载请求:exec 重启进程(保留环境变量与参数,自我替换)
    if reload_requested.load(Ordering::SeqCst) {
        eprintln!("self-reload: restarting process...");
        let exe = std::env::current_exe()?;
        let args: Vec<String> = std::env::args().skip(1).collect();
        // 类 Unix:exec 替换当前进程镜像,不返回;失败时 fallthrough 打印
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(&exe).args(&args).exec();
            eprintln!("self-reload: exec failed: {err}");
        }
        #[cfg(not(unix))]
        {
            let status = std::process::Command::new(exe).args(&args).status()?;
            if !status.success() {
                eprintln!("self-reload: restart failed (status {status})");
            }
        }
    }
    Ok(())
}

/// 离线演示脚本:一轮工具调用(建文件 + cat)+ 一轮终答。
/// `reload_demo` 时第一轮调用 `self_reload` 演示宿主侧热重启闭环。
fn scripted_responses(reload_demo: bool) -> Vec<rutis_agent::LlmResponse> {
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
    let mut out = vec![
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
    ];
    if reload_demo {
        // 第一轮:调用 self_reload(写 handoff 意图 + 广播事件 → 宿主重启)
        out.insert(
            0,
            rutis_agent::LlmResponse::tool_calls(vec![mk(
                "reload1",
                "self_reload",
                json!({
                    "handoff": "/tmp/rutis-smoke/reload-intent.md",
                }),
            )]),
        );
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// 宿主侧 ReloadHandler:收到 SelfReloadRequested → 标记重启意图
    /// → dispose root fiber(TUI 循环将优雅退出)。
    #[tokio::test]
    async fn reload_handler_marks_request_and_disposes_root() {
        let root = Ctx::root().unwrap();
        let requested = Arc::new(AtomicBool::new(false));

        // 注册宿主监听器
        root.events()
            .on(
                &root,
                ReloadHandler {
                    root: root.clone(),
                    requested: requested.clone(),
                },
            )
            .unwrap();

        // 模拟 self_reload 工具广播事件(载荷来自 session id)
        let session = rutis_agent::SessionId::next();
        root.events().emit(
            &root,
            Arc::new(SelfReloadRequested {
                session,
                reason: "test".to_string(),
                intent_path: "/tmp/test-intent.md".to_string(),
            }),
        );

        // 事件异步派发:等待标记置位
        for _ in 0..200 {
            if requested.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            requested.load(Ordering::SeqCst),
            "reload flag should be set after SelfReloadRequested"
        );

        // dispose 触发后,root fiber 应进入 Disposed/退出流程
        for _ in 0..200 {
            let st = root.root_view().state();
            if st.state == rutis::FiberState::Disposed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(
            root.root_view().state().state,
            rutis::FiberState::Disposed,
            "root fiber should be disposed after reload request"
        );
    }
}
