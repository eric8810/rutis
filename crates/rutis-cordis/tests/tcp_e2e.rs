//! M2-1 端到端:Rust 桥 ↔ 真 Node 进程,经 loopback TCP 的行帧 JSON。
//! 验收:握手 → 请求往返(宿主在请求路径上 `console.log`,帧流不损坏)
//! → 事件反射 → 杀宿主 → 在飞/新调用以 HostGone 收敛。零 npm 依赖
//! (node >=22 原生)。node 不可用时跳过(CI linux lane 与本机 fnm 均有)。


use std::time::Duration;

use rutis_cordis::{
    Bridge, BridgeConfig, ExpectedHost, InboundHooks, ProtoError, TcpWire,
};
use serde_json::json;

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

/// 起 node echo 宿主、accept 其拨号、完成握手;返回(子进程, 就绪的桥)。
async fn setup() -> Option<(tokio::process::Child, Bridge)> {
    let node = node_binary()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/echo-host.mjs");
    let child = tokio::process::Command::new(&node)
        .arg(script)
        .env("BRIDGE_PORT", port.to_string())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn node");
    let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("accept within 10s")
        .expect("accept");
    let wire = TcpWire::from_stream(stream);
    let mut bridge = Bridge::start(
        Box::new(wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedHost::protocol(1),
        json!({ "services": ["observe"], "wfKinds": [], "scopes": [] }),
    );
    bridge.ready().await.expect("handshake with real node");
    Some((child, bridge))
}

#[tokio::test]
async fn tcp_end_to_end_with_real_node_process() {
    // 环境缺失 = 显式失败而非静默通过(M2-1 的教训:skip 被算成 pass 制造
    // 了假绿)。设置 RUTIS_SKIP_NODE_E2E=1 才跳过。
    if std::env::var("RUTIS_SKIP_NODE_E2E").as_deref() == Ok("1") {
        eprintln!("RUTIS_SKIP_NODE_E2E=1 — skipping tcp e2e");
        return
    }
    let Some((mut child, mut bridge)) = setup().await else {
        panic!("node not found on PATH (set NODE to override, or RUTIS_SKIP_NODE_E2E=1 to skip)")
    };

    // 请求往返:宿主在请求路径上 console.log(stdout 留给日志,专用通道
    // 的帧流不受影响——这是评审 §C.1 裁决的行为面验证)。
    let result = bridge
        .request("svc/call", json!({ "hello": "m2" }), Some(5_000))
        .await
        .expect("roundtrip");
    assert_eq!(result["echoed"]["hello"], "m2");

    // 第二次往返确认通道在日志之后仍然完好。
    let again = bridge
        .request("svc/call", json!({ "n": 2 }), Some(5_000))
        .await
        .expect("second roundtrip after host console.log");
    assert_eq!(again["echoed"]["n"], 2);

    // 杀宿主:在飞调用 HostGone,新调用立即 HostGone,断连留痕。
    let pending = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.request("svc/call", json!({ "slow": true }), Some(500)).await }
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let _ = child.kill().await;
    match pending.await.unwrap() {
        Err(ProtoError::HostGone) | Err(ProtoError::Timeout { .. }) => {}
        other => panic!("expected HostGone/Timeout after killing host, got {other:?}"),
    }
    let record = bridge.wait_disconnect().await;
    assert!(record.frames_received >= 2, "record: {record:?}");
    assert!(
        matches!(bridge.request("svc/call", json!({}), None).await, Err(ProtoError::HostGone))
    );
}
