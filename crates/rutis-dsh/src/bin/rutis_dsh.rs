//! `rutis-dsh up`:入口与组合根(决策文档 v2 对象 D)。
//!
//! 1. 起 rutis 运行时,装载 aimux-llm 插件(apply → 注册 llm 服务);
//! 2. 注册表中的服务经业务无关桥(rutis-cordis)供给宿主——hello 能力集
//!    从注册表推导,不硬编码;
//! 3. spawn 官方 dsh CLI(`RUTIS_DSH_BIN` 或 PATH 的 `dsh`),经
//!    `RUTIS_BRIDGE_PORT` 告知桥端口;stdio 继承;
//! 4. 事件观察日志(`evt/emit` → stderr);
//! 5. dsh 退出且桥断连即收敛。
//!
//! 本文件零 dsh 知识:dsh 只是它拉起的一个进程。

use std::sync::Arc;
use std::time::Duration;

use aimux_llm::{AimuxLlmPlugin, LlmService, llm_service_key};
use rutis::{Ctx, FiberState, FiberView};
use rutis_cordis::{Bridge, BridgeConfig, ExpectedHost, ServiceDispatch, TcpWire};
use serde_json::json;

#[tokio::main]
async fn main() {
    // 无声退出取证:panic 与正常返回都必须留下最后一行日志。
    std::panic::set_hook(Box::new(|info| {
        eprintln!("[rutis-dsh] PANIC: {info}");
    }));
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("up") => up().await,
        _ => {
            eprintln!("usage: rutis-dsh up");
            eprintln!();
            eprintln!("env: RUTIS_DSH_BIN (default: dsh from PATH); model keys per provider");
            std::process::exit(2)
        }
    }
    eprintln!("[rutis-dsh] runner exiting");
}

/// 极简空白分割(引用段不支持;路径含空格时 RUTIS_DSH_BIN 需自身可执行)。
fn shell_split(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

/// 按 char 边界截断(日志摘要用;`floor_char_boundary` 尚未稳定)。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned()
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// 等插件到达目标态(装载是异步的;服务就绪以 fiber Active 为准)。
async fn wait_state(view: &FiberView, want: FiberState) {
    let mut rx = view.watch();
    loop {
        if rx.borrow().state == want {
            return
        }
        rx.changed().await.expect("fiber driver alive");
    }
}

async fn up() {
    // ── rutis 运行时:装载 aimux-llm,llm 服务进注册表 ──
    let ctx = Ctx::root().expect("rutis runtime root (needs tokio)");
    let plugin = AimuxLlmPlugin::from_env();
    let view = ctx.plugin(plugin);
    wait_state(&view, FiberState::Active).await;
    let llm: Arc<dyn LlmService> = ctx
        .get_as::<dyn LlmService>(llm_service_key())
        .expect("aimux-llm registered the llm service");

    // ── 宿主进程:官方 dsh(stdio 归它自己)──
    let dsh_bin = std::env::var("RUTIS_DSH_BIN").unwrap_or_else(|_| "dsh".into());
    // RUTIS_DSH_BIN 支持空格分隔的多段命令(如 "node --import tsx bin.js")。
    let mut dsh_command: Vec<String> = shell_split(&dsh_bin);
    let dsh_display = dsh_command.first().cloned().unwrap_or_else(|| dsh_bin.clone());
    dsh_command.extend(std::env::args().skip(2));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind bridge channel");
    let port = listener.local_addr().expect("addr").port();
    eprintln!("[rutis-dsh] bridge channel on 127.0.0.1:{port} (services from registry)");

    let mut dsh = match tokio::process::Command::new(&dsh_display)
        .env("RUTIS_BRIDGE_PORT", port.to_string())
        .args(dsh_command.iter().skip(1))
        .spawn()
    {
        Ok(child) => child,
        Err(direct) => {
            let fail_msg = || {
                eprintln!("[rutis-dsh] cannot spawn {dsh_display}: {direct}");
                eprintln!("[rutis-dsh] install the official CLI (npm i -g @deepseek-ai/dsh) or set RUTIS_DSH_BIN");
                std::process::exit(1)
            };
            // Windows:npm 全局命令是 .cmd shim(CreateProcess 只认 .exe),
            // 经 cmd /c 解析 PATH 里的 shim。RUTIS_DSH_BIN 显式多段
            // (node bin.js)不经此层。
            #[cfg(windows)]
            {
                let mut via_cmd = tokio::process::Command::new("cmd");
                via_cmd.arg("/c").args(&dsh_command);
                match via_cmd.env("RUTIS_BRIDGE_PORT", port.to_string()).spawn() {
                    Ok(child) => child,
                    Err(_) => fail_msg(),
                }
            }
            #[cfg(not(windows))]
            {
                fail_msg()
            }
        }
    };
    eprintln!("[rutis-dsh] dsh started (pid {:?}) — stdio is the app's own", dsh.id());

    let (stream, _) = tokio::time::timeout(Duration::from_secs(60), listener.accept())
        .await
        .expect("dsh connects within 60s (is the rutis-bridge plugin in the profile?)")
        .expect("accept");

    // ── 桥:注册表驱动的服务分发 + 事件观察,合并为一套钩子 ──
    let face = rutis_dsh::LlmFace::new(llm);
    let dispatch = ServiceDispatch::new(vec![face]);
    let mut hooks = dispatch.hooks();
    hooks.on_notify = Some(Arc::new(|method, params| {
        Box::pin(async move {
            if method == "evt/emit" {
                // 载荷摘要(截断):形状级可见性;保真断言在测试里做。
                let summary = serde_json::to_string(&params["params"]).unwrap_or_else(|_| "?".into());
                let summary = truncate_chars(&summary, 160);
                eprintln!("[rutis-dsh] evt {} {}", params["event"], summary);
            }
        })
    }));
    let mut bridge = Bridge::start(
        Box::new(TcpWire::from_stream(stream)),
        BridgeConfig::default(),
        hooks,
        ExpectedHost::protocol(1),
        json!({ "services": dispatch.names(), "wfKinds": [], "scopes": [] }),
    );
    dispatch.attach(bridge.clone());
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

    // 收敛以**桥断连**为权威信号(时序不可信的包装层存在时,子进程退出
    // 可能先于真实进程);两侧都等齐才退;退出**不杀 dsh**——桥断了宿主
    // 继续跑(§十.4),模型调用由插件按 bridgeDisconnected 拒绝。
    let mut dsh_exited = false;
    let mut bridge_closed = false;
    while !(dsh_exited && bridge_closed) {
        tokio::select! {
            status = dsh.wait(), if !dsh_exited => {
                eprintln!("[rutis-dsh] dsh exited: {}", status.expect("wait dsh"));
                dsh_exited = true;
            }
            _ = bridge.wait_disconnect(), if !bridge_closed => {
                eprintln!("[rutis-dsh] bridge channel closed");
                bridge_closed = true;
            }
        }
    }
}
