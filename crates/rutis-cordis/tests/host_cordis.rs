//! M2-2 事件缝端到端:Rust 拉起真 cordis 基座(min-cordis)的 TS 桥端
//! 宿主(dsh 仓 experiments/m2-host,经 tsx 运行),装载一个装载即 emit
//! 的测试插件,断言事件经 `evt/emit` ntf 回流 Rust 观察者。
//!
//! env:`DSH_ROOT`(deepseek-harness 检出路径)、`MIN_CORDIS_ROOT`
//! (min-cordis 检出路径)、`NODE`(可选,node 二进制覆盖)。缺失即跳过。

use std::sync::Arc;
use std::time::Duration;

use rutis_cordis::{Bridge, BridgeConfig, ExpectedHost, InboundHooks, TcpWire};
use serde_json::{json, Value};

fn env_or_skip(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn node_binary() -> Option<String> {
    if let Ok(node) = std::env::var("NODE") {
        return Some(node)
    }
    let output = std::process::Command::new("where")
        .arg("node")
        .output()
        .or_else(|_| std::process::Command::new("which").arg("node").output())
        .ok()?;
    if !output.status.success() {
        return None
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::to_owned)
}

#[tokio::test]
#[ignore = "min-cordis host e2e: needs DSH_ROOT + MIN_CORDIS_ROOT checkouts; run with -- --ignored (set RUTIS_SKIP_NODE_E2E=1 to skip this e2e)"]
async fn event_seam_end_to_end_with_min_cordis_host() {
    // 环境缺失 = 显式失败而非静默通过(M2-1 的教训)。RUTIS_SKIP_NODE_E2E=1
    // 提供显式逃生门。
    if std::env::var("RUTIS_SKIP_NODE_E2E").as_deref() == Ok("1") {
        eprintln!("RUTIS_SKIP_NODE_E2E=1 — skipping min-cordis host e2e");
        return
    }
    let Some(dsh_root) = env_or_skip("DSH_ROOT") else {
        panic!("DSH_ROOT not set (deepseek-harness checkout) — required for min-cordis host e2e")
    };
    let Some(min_cordis_root) = env_or_skip("MIN_CORDIS_ROOT") else {
        panic!("MIN_CORDIS_ROOT not set (min-cordis checkout) — required for min-cordis host e2e")
    };
    let Some(node) = node_binary() else {
        panic!("node not found on PATH (set NODE to override)")
    };

    // 事件回流面:notify 钩子收 evt/emit;evt/on 申报由 request 钩子确认。
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<(String, Value)>(8);
    let mut hooks = InboundHooks::default();
    let tx = Arc::new(event_tx);
    hooks.on_notify = Some(Arc::new(move |method, params| {
        Box::pin({
            let tx = Arc::clone(&tx);
            async move {
                if method == "evt/emit" {
                    let _ = tx.send((method, params)).await;
                }
            }
        })
    }));
    let (declare_tx, mut declare_rx) = tokio::sync::mpsc::channel::<Value>(4);
    let declare = Arc::new(declare_tx);
    hooks.on_request = Some(Arc::new(move |_id, method, params| {
        Box::pin({
            let declare = Arc::clone(&declare);
            async move {
                if method == "evt/on" {
                    let _ = declare.send(params).await;
                    Ok(json!({ "ok": true }))
                } else {
                    Ok(json!({ "ok": true, "note": "accepted" }))
                }
            }
        })
    }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let script = format!("{dsh_root}/experiments/m2-host/bridge-host.mjs");
    let mut child = tokio::process::Command::new(node)
        .arg("--import")
        .arg("tsx")
        .arg(&script)
        .env("BRIDGE_PORT", port.to_string())
        .env("MIN_CORDIS_ROOT", &min_cordis_root)
        .current_dir(&dsh_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn bridge host");

    let (stream, _) = tokio::time::timeout(Duration::from_secs(20), listener.accept())
        .await
        .expect("host connects within 20s")
        .expect("accept");
    let mut bridge = Bridge::start(
        Box::new(TcpWire::from_stream(stream)),
        BridgeConfig::default(),
        hooks,
        ExpectedHost::protocol(1),
        json!({ "services": ["observe"], "wfKinds": [], "scopes": [] }),
    );
    let hello = bridge.ready().await.expect("handshake with min-cordis host");
    assert_eq!(hello["base"], "min-cordis");

    // plugin/load:装载即 emit 的测试插件,声明转发 test/fired。
    let entry = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/emit-plugin.mjs");
    let loaded = bridge
        .request(
            "plugin/load",
            json!({
                "pluginId": "test-emit",
                "entry": entry,
                "config": { "tag": "m2-seam" },
                "events": ["test/fired"],
            }),
            Some(15_000),
        )
        .await
        .expect("plugin/load res");
    assert_eq!(loaded["ok"], true, "load result: {loaded}");

    // evt/on 申报到达(宿主 → Rust 的事件缝申报)。
    let declaration = tokio::time::timeout(Duration::from_secs(5), declare_rx.recv())
        .await
        .expect("evt/on declaration within 5s")
        .expect("declaration channel alive");
    assert_eq!(declaration["pluginId"], "test-emit");
    assert_eq!(declaration["events"][0]["name"], "test/fired");
    assert_eq!(declaration["events"][0]["mode"], "emit");

    // 插件装载期间的同步 emit 已过线回流。
    let (method, payload) = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .expect("evt/emit within 5s")
        .expect("event channel alive");
    assert_eq!(method, "evt/emit");
    assert_eq!(payload["event"], "test/fired");
    assert_eq!(payload["params"]["ok"], true);
    assert_eq!(payload["params"]["from"], "m2-seam");

    let _ = child.kill().await;
}
