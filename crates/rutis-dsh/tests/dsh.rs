//! dsh 面(设计 v3.2 §三之二):hello 的 `dsh` 节——两级握手、dshSemver
//! pin、纯 cordis 宿主(无 dsh 节)的基座路径。

use rutis_cordis::{
    Bridge, BridgeConfig, ExpectedHost, Frame, InboundHooks, MemoryWire, ProtoError, Wire,
};
use rutis_dsh::{parse_dsh_node, ExpectedDsh};
use serde_json::{json, Value};

struct TestHost {
    wire: MemoryWire,
}

impl TestHost {
    async fn hello(&self, declaration: Value) -> Value {
        self.wire
            .send(Frame::Req {
                id: 1,
                method: "hello".into(),
                params: declaration,
                scope_id: None,
                session_id: None,
                turn_id: None,
            })
            .await
            .expect("send hello");
        let Frame::Res { ok, result, error, .. } =
            self.wire.recv().await.expect("bridge alive")
        else {
            panic!("expected hello res")
        };
        assert!(ok, "hello rejected: {:?}", error);
        result.expect("payload")
    }
}

fn dsh_host_hello(dsh_semver: &str, services: Value) -> Value {
    json!({
        "protocol": 1,
        "base": "min-cordis",
        "baseSemver": "0.1.0",
        "stack": ["node"],
        "caps": { "services": ["tools"], "wfKinds": [], "scopes": [] },
        "dsh": { "dshSemver": dsh_semver, "services": services },
    })
}

#[tokio::test]
async fn dsh_node_validated_during_handshake_with_semver_pin() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedDsh::strict("0.1.1-rc.2").base_expectation(),
        json!({}),
    );
    let host = TestHost { wire: host_wire };
    // dshSemver 漂移:握手期报错(两级任一失败都算握手失败)。
    host.wire
        .send(Frame::Req {
            id: 1,
            method: "hello".into(),
            params: dsh_host_hello("0.2.0-rc.1", json!(["tools"])),
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send hello");
    let Frame::Res { ok, error, .. } = host.wire.recv().await.expect("res") else {
        panic!("expected res")
    };
    assert!(!ok);
    assert!(error.expect("payload").message.contains("dshSemver mismatch"));
    match bridge.ready().await {
        Err(ProtoError::Handshake(reason)) => assert!(reason.contains("dshSemver mismatch")),
        e => panic!("expected Handshake, got {e:?}"),
    }
}

#[tokio::test]
async fn dsh_node_parsed_from_ready_and_capability_diff() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedDsh::any().base_expectation(),
        json!({}),
    );
    let host = TestHost { wire: host_wire };
    let reply = host.hello(dsh_host_hello("0.1.1-rc.2", json!(["tools", "shell"]))).await;
    // dsh 节的 dshSemver 在回包里回显。
    assert_eq!(reply["dshSemver"], "0.1.1-rc.2");
    let params = bridge.ready().await.expect("handshake");
    let node = parse_dsh_node(&params).expect("dsh node");
    assert_eq!(node.dsh_semver, "0.1.1-rc.2");
    // dsh 服务集求差:装载期 injects ⊆ dsh.services。
    assert_eq!(node.missing_services(["tools", "approval"]), vec!["approval".to_string()]);
    assert!(node.missing_services(["tools", "shell"]).is_empty());
}

#[tokio::test]
async fn pure_cordis_host_without_dsh_node_passes_base_handshake() {
    // 纯 cordis 宿主(语料/社区插件):基座路径,不带 dsh 节,无 dsh 期望。
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedHost::protocol(1),
        json!({}),
    );
    let host = TestHost { wire: host_wire };
    let reply = host
        .hello(json!({
            "protocol": 1,
            "base": "cordis",
            "baseSemver": "4.0.1",
            "caps": { "services": ["logger"], "wfKinds": [], "scopes": [] },
        }))
        .await;
    assert_eq!(reply["base"], "rutis");
    let params = bridge.ready().await.expect("handshake");
    // 基座桥不解释 dsh 节:无 dsh 节的 hello 对 dsh 解析就是"非 dsh 部署"。
    assert!(parse_dsh_node(&params).is_err());
}

#[tokio::test]
async fn dsh_expectation_rejects_host_without_dsh_node() {
    // 反向:桥侧有 dsh 期望、宿主却没带 dsh 节 → 握手失败(不是静默降级)。
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedDsh::any().base_expectation(),
        json!({}),
    );
    let host = TestHost { wire: host_wire };
    host.wire
        .send(Frame::Req {
            id: 1,
            method: "hello".into(),
            params: json!({
                "protocol": 1,
                "base": "cordis",
                "baseSemver": "4.0.1",
                "caps": { "services": [], "wfKinds": [], "scopes": [] },
            }),
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send hello");
    let Frame::Res { ok, error, .. } = host.wire.recv().await.expect("res") else {
        panic!("expected res")
    };
    assert!(!ok);
    assert!(error.expect("payload").message.contains("no dsh node"));
    match bridge.ready().await {
        Err(ProtoError::Handshake(reason)) => assert!(reason.contains("no dsh node")),
        e => panic!("expected Handshake, got {e:?}"),
    }
}
