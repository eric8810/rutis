//! `rutis-dsh up`:一条命令跑起正式 dsh。
//!
//! 1. 起 TCP listener(桥的专用通道);
//! 2. 构造真实 aimux provider(`AIMUX_PROVIDER`/`AIMUX_MODEL`,key 走
//!    各 provider 的 env,如 `DEEPSEEK_API_KEY`);
//! 3. spawn 官方 dsh CLI(`RUTIS_DSH_BIN` 或 PATH 的 `dsh`),经
//!    `RUTIS_BRIDGE_PORT` 告知桥端口——profile 里的 rutis-bridge 插件
//!    以此连回;stdio 继承(dsh 自己的交互面归它);
//! 4. 握手后挂 LlmSeam(模型调用过线 → aimux)+ 事件观察日志
//!    (`evt/emit` → stderr);
//! 5. dsh 退出或桥断连即收敛。

use std::sync::Arc;
use std::time::Duration;

use aimux_core::language_model::LanguageModel;
use rutis_cordis::{Bridge, BridgeConfig, ExpectedHost, InboundHooks, TcpWire};
use rutis_dsh::LlmSeam;
use serde_json::json;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("up") => up().await,
        _ => {
            eprintln!("usage: rutis-dsh up");
            eprintln!();
            eprintln!("env: AIMUX_PROVIDER / AIMUX_MODEL (default deepseek / deepseek-chat),");
            eprintln!("      RUTIS_DSH_BIN (default: dsh from PATH), provider keys per provider");
            std::process::exit(2)
        }
    }
}

/// 极简空白分割(引用段不支持;路径含空格时 RUTIS_DSH_BIN 需自身可执行)。
fn shell_split(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

async fn up() {
    let provider_name = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    let model_id = std::env::var("AIMUX_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
    let model: Arc<dyn LanguageModel> = match aimux_providers::provider(&provider_name, None, &model_id, None) {
        Ok(model) => Arc::from(model),
        Err(e) => {
            eprintln!("[rutis-dsh] build {provider_name}/{model_id}: {e}");
            std::process::exit(1)
        }
    };
    let dsh_bin = std::env::var("RUTIS_DSH_BIN").unwrap_or_else(|_| "dsh".into());
    // RUTIS_DSH_BIN 支持空格分隔的多段命令(如 "node --import tsx bin.js"):
    // 开发机上官方 CLI 常以 node 直跑其 bin 入口。
    let mut dsh_command: Vec<String> = shell_split(&dsh_bin);
    let dsh_display = dsh_command.first().cloned().unwrap_or_else(|| dsh_bin.clone());
    dsh_command.extend(std::env::args().skip(2));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind bridge channel");
    let port = listener.local_addr().expect("addr").port();
    eprintln!("[rutis-dsh] bridge channel on 127.0.0.1:{port} (llm: {provider_name}/{model_id})");

    let mut dsh = match tokio::process::Command::new(&dsh_display)
        .env("RUTIS_BRIDGE_PORT", port.to_string())
        .args(dsh_command.iter().skip(1))
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("[rutis-dsh] cannot spawn {dsh_display}: {e}");
            eprintln!("[rutis-dsh] install the official CLI (npm i -g @deepseek-ai/dsh) or set RUTIS_DSH_BIN");
            std::process::exit(1)
        }
    };
    eprintln!("[rutis-dsh] dsh started (pid {:?}) — stdio is the app's own", dsh.id());

    let (stream, _) = tokio::time::timeout(Duration::from_secs(60), listener.accept())
        .await
        .expect("dsh connects within 60s (is the rutis-bridge plugin in the profile?)")
        .expect("accept");

    // 事件观察:evt/emit 回流打 stderr(stdout 属于 dsh 的交互面)。
    let mut hooks = InboundHooks::default();
    hooks.on_notify = Some(Arc::new(|method, params| {
        Box::pin(async move {
            if method == "evt/emit" {
                eprintln!("[rutis-dsh] evt {}", params["event"]);
            }
        })
    }));

    let seam = LlmSeam::new(model);
    let mut bridge = Bridge::start(
        Box::new(TcpWire::from_stream(stream)),
        BridgeConfig::default(),
        seam.hooks(),
        ExpectedHost::protocol(1),
        json!({ "services": ["llm"], "wfKinds": [], "scopes": [] }),
    );
    seam.attach(bridge.clone());
    match bridge.ready().await {
        Ok(hello) => {
            eprintln!("[rutis-dsh] handshake ok: {} — bridged", hello["base"].as_str().unwrap_or("?"))
        }
        Err(e) => {
            eprintln!("[rutis-dsh] handshake failed: {e}");
            let _ = dsh.kill().await;
            std::process::exit(1)
        }
    }

    // 收敛:dsh 退出即结束(桥断连由状态机收敛;Ctrl-C 归 dsh 的 stdio)。
    let status = dsh.wait().await.expect("wait dsh");
    eprintln!("[rutis-dsh] dsh exited: {status}");
    drop(bridge);
}
