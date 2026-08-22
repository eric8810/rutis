//! M1 验收(设计 §九 M1 行):内存 wire 下内核原语全套——往返、并发、
//! 取消(含迟到 res 丢弃)、超时、握手错配与能力集求差、同名仲裁、重入、
//! 杀宿主 → 仅失 llm 缝且观察连续性记录在案。零 Node。

use std::sync::{Arc, Mutex};

use rutis_dsh::{
    Bridge, BridgeConfig, CancelTarget, EvtDeclaration, EvtMode, ExpectedHost, Frame, InboundHooks,
    MemoryWire, PeerCaps, PluginLedger, ProtoError, Wire,
};
use serde_json::{json, Value};

/// 测试侧宿主:手写帧收发,精确控制应答时机与顺序。
struct TestHost {
    wire: MemoryWire,
}

impl TestHost {
    async fn next(&self) -> Frame {
        self.wire.recv().await.expect("bridge side still alive")
    }

    async fn reply_ok(&self, id: u64, result: Value) {
        self.wire
            .send(Frame::Res { id, ok: true, result: Some(result), error: None })
            .await
            .expect("send res ok");
    }

    async fn reply_err(&self, id: u64, code: &str, message: &str) {
        self.wire
            .send(Frame::Res {
                id,
                ok: false,
                result: None,
                error: Some(rutis_dsh::RemoteError {
                    code: code.to_owned(),
                    message: message.to_owned(),
                }),
            })
            .await
            .expect("send res err");
    }

    /// 宿主发起一次 hello 握手(§三 规则 1:宿主首发),返回桥的回包。
    async fn hello(&self, declaration: Value) -> Value {
        self.wire
            .send(Frame::Req { id: 1, method: "hello".into(), params: declaration, scope_id: None })
            .await
            .expect("send hello");
        let frame = self.next().await;
        let Frame::Res { ok, result, error, .. } = frame else {
            panic!("expected hello res, got {frame:?}")
        };
        assert!(ok, "hello rejected: {:?}", error);
        result.expect("hello result payload")
    }

    /// 完成一次合法握手:发 `hello` 声明,收桥的对称能力集。
    async fn hello_ok(&self, caps: Value) -> Value {
        self.hello(json!({
            "protocol": 1,
            "base": "min-cordis",
            "baseSemver": "0.1.0",
            "dshSemver": "0.1.1-rc.2",
            "stack": ["node"],
            "caps": caps,
        }))
        .await
    }
}

fn caps(services: &[&str]) -> Value {
    json!({ "services": services, "wfKinds": [], "scopes": [] })
}

/// 标准会话:握手完成,桥侧 Ready。
async fn setup() -> (Bridge, TestHost) {
    setup_with(ExpectedHost::protocol(1)).await
}

async fn setup_with(expected: ExpectedHost) -> (Bridge, TestHost) {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        expected,
        PeerCaps::default(),
    );
    let host = TestHost { wire: host_wire };
    host.hello_ok(caps(&["tools", "shell", "systemPrompt"])).await;
    bridge.ready().await.expect("handshake");
    (bridge, host)
}

// ---------------------------------------------------------------------------
// 往返
// ---------------------------------------------------------------------------

#[tokio::test]
async fn roundtrip_ok_and_remote_error() {
    let (bridge, host) = setup().await;

    let ok = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.request("svc/call", json!({ "a": 1 }), None).await }
    });
    let Frame::Req { id: id_ok, method, params, .. } = host.next().await else {
        panic!("expected req")
    };
    assert_eq!(method, "svc/call");
    assert_eq!(params["a"], 1);
    host.reply_ok(id_ok, json!({ "answer": 42 })).await;
    assert_eq!(ok.await.unwrap().unwrap()["answer"], 42);

    let err = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.request("svc/call", json!({} ), None).await }
    });
    let Frame::Req { id: id_err, .. } = host.next().await else {
        panic!("expected req")
    };
    host.reply_err(id_err, "notFound", "service missing").await;
    match err.await.unwrap().unwrap_err() {
        ProtoError::Remote { code, message } => {
            assert_eq!(code, "notFound");
            assert_eq!(message, "service missing");
        }
        e => panic!("expected Remote, got {e:?}"),
    }
}

#[tokio::test]
async fn concurrent_calls_complete_out_of_order() {
    let (bridge, host) = setup().await;
    let mut calls = Vec::new();
    for name in ["a", "b", "c"] {
        calls.push((
            name.to_string(),
            tokio::spawn({
                let bridge = bridge.clone();
                let name = name.to_string();
                async move { bridge.request("svc/call", json!({ "name": name }), None).await }
            }),
        ));
    }
    // 收齐三帧,按 c → a → b 乱序回。
    let mut ids = Vec::new();
    for (i, (name, _)) in calls.iter().enumerate() {
        let Frame::Req { id, params, .. } = host.next().await else {
            panic!("expected req {i}")
        };
        assert_eq!(params["name"], *name);
        ids.push(id);
    }
    host.reply_ok(ids[2], json!({ "v": "c" })).await;
    host.reply_ok(ids[0], json!({ "v": "a" })).await;
    host.reply_ok(ids[1], json!({ "v": "b" })).await;
    for (name, call) in calls {
        assert_eq!(call.await.unwrap().unwrap()["v"], name.as_str());
    }
}

// ---------------------------------------------------------------------------
// 取消与超时(§三 规则 4:迟到 res 按孤儿应答丢弃并计数)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_drops_call_and_late_res_counts_orphan() {
    let (bridge, host) = setup().await;
    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.request("svc/call", json!({}), None).await }
    });
    let Frame::Req { id, .. } = host.next().await else {
        panic!("expected req")
    };

    bridge.cancel(CancelTarget::call(id)).await.expect("cancel notify");
    match call.await.unwrap().unwrap_err() {
        ProtoError::Remote { code, .. } => assert_eq!(code, "cancelled"),
        e => panic!("expected cancelled, got {e:?}"),
    }
    // 取消通知过线(target 带类型前缀)。
    let Frame::Ntf { method, params, .. } = host.next().await else {
        panic!("expected cancel ntf")
    };
    assert_eq!(method, "cancel");
    assert_eq!(params["target"], format!("call:{id}"));

    // 迟到的 res:无在飞匹配 → 孤儿计数,不占位、不 panic。
    host.reply_ok(id, json!({ "late": true })).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(bridge.stats().orphan_responses, 1);
}

#[tokio::test]
async fn caller_declared_timeout_fails_without_waiting() {
    let config = BridgeConfig { default_timeout_ms: 25, max_timeout_ms: 500 };
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        config,
        InboundHooks::default(),
        ExpectedHost::protocol(1),
        PeerCaps::default(),
    );
    let host = TestHost { wire: host_wire };
    host.hello_ok(caps(&[])).await;
    bridge.ready().await.expect("handshake");

    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.request("svc/call", json!({}), Some(40)).await }
    });
    let Frame::Req { id, method: _, .. } = host.next().await else {
        panic!("expected req")
    };
    match call.await.unwrap().unwrap_err() {
        ProtoError::Timeout { id: tid, method: tmethod, timeout_ms } => {
            assert_eq!(tid, id);
            assert_eq!(tmethod, "svc/call");
            assert_eq!(timeout_ms, 40);
        }
        e => panic!("expected Timeout, got {e:?}"),
    }
    // 超时后迟到的 res 同样落孤儿。
    host.reply_ok(id, json!({})).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(bridge.stats().orphan_responses, 1);

    // 声明超过全局上限:显式拒绝,不静默截断。
    match bridge.request("svc/call", json!({}), Some(10_000)).await {
        Err(ProtoError::TimeoutTooLarge { requested, max }) => {
            assert_eq!((requested, max), (10_000, 500));
        }
        e => panic!("expected TimeoutTooLarge, got {e:?}"),
    }
    // 未声明时用配置默认值。
    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.request("svc/call", json!({}), None).await }
    });
    let Frame::Req { id, .. } = host.next().await else {
        panic!("expected req")
    };
    let started = std::time::Instant::now();
    match call.await.unwrap().unwrap_err() {
        ProtoError::Timeout { timeout_ms, .. } => assert_eq!(timeout_ms, 25),
        e => panic!("expected default Timeout, got {e:?}"),
    }
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    host.reply_ok(id, json!({})).await;
}

// ---------------------------------------------------------------------------
// 握手(§三 规则 1:版本错配在握手期报错;能力集求差装载期生效)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handshake_protocol_mismatch_fails_at_handshake() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedHost::protocol(1),
        PeerCaps::default(),
    );
    let host = TestHost { wire: host_wire };
    // 宿主声明了桥不认的协议版本:握手期报错(显式 error res + 会话失败)。
    host.wire
        .send(Frame::Req {
            id: 1,
            method: "hello".into(),
            params: json!({
                "protocol": 2,
                "base": "min-cordis",
                "baseSemver": "0.1.0",
                "dshSemver": "0.1.1-rc.2",
                "caps": caps(&[]),
            }),
            scope_id: None,
        })
        .await
        .expect("send hello");
    let Frame::Res { ok, error, .. } = host.next().await else {
        panic!("expected error res")
    };
    assert!(!ok);
    let error = error.expect("error payload");
    assert_eq!(error.code, "handshake");
    assert!(error.message.contains("protocol version mismatch"), "{}", error.message);
    match bridge.ready().await {
        Err(ProtoError::Handshake(reason)) => {
            assert!(reason.contains("protocol version mismatch"), "{reason}")
        }
        e => panic!("expected Handshake, got {e:?}"),
    }
}

#[tokio::test]
async fn handshake_dsh_semver_pin_rejects_drift() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedHost { protocol: 1, dsh_semver: Some("0.1.1-rc.2".into()), base: None },
        PeerCaps::default(),
    );
    let host = TestHost { wire: host_wire };
    host.wire
        .send(Frame::Req {
            id: 1,
            method: "hello".into(),
            params: json!({
                "protocol": 1,
                "base": "min-cordis",
                "baseSemver": "0.1.0",
                "dshSemver": "0.2.0-rc.1",
                "caps": caps(&[]),
            }),
            scope_id: None,
        })
        .await
        .expect("send hello");
    let Frame::Res { ok, error, .. } = host.next().await else {
        panic!("expected error res")
    };
    assert!(!ok);
    let error = error.expect("error payload");
    assert!(error.message.contains("dshSemver mismatch"), "{}", error.message);
    match bridge.ready().await {
        Err(ProtoError::Handshake(reason)) => assert!(reason.contains("dshSemver")),
        e => panic!("expected Handshake, got {e:?}"),
    }
}

#[tokio::test]
async fn handshake_replies_symmetric_capability_set() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedHost::protocol(1),
        PeerCaps {
            services: ["llm"].into_iter().map(str::to_owned).collect(),
            wf_kinds: vec!["decide".into()],
            scopes: vec!["session".into()],
        },
    );
    let host = TestHost { wire: host_wire };
    let reply = host.hello_ok(caps(&["tools"])).await;
    assert_eq!(reply["base"], "rutis");
    assert_eq!(reply["dshSemver"], "0.1.1-rc.2");
    assert_eq!(reply["caps"]["services"], json!(["llm"]));
    assert_eq!(reply["caps"]["wfKinds"], json!(["decide"]));
    let peer = bridge.ready().await.expect("handshake");
    assert_eq!(peer.services, ["tools"].into_iter().map(str::to_owned).collect());
}

#[tokio::test]
async fn first_frame_must_be_hello() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedHost::protocol(1),
        PeerCaps::default(),
    );
    let host = TestHost { wire: host_wire };
    host.wire
        .send(Frame::Req { id: 1, method: "evt/on".into(), params: json!({}), scope_id: None })
        .await
        .expect("send non-hello first frame");
    match bridge.ready().await {
        Err(ProtoError::Handshake(reason)) => {
            assert!(reason.contains("first frame must be hello"), "{reason}")
        }
        e => panic!("expected Handshake, got {e:?}"),
    }
}

#[tokio::test]
async fn capability_diff_rejects_load_with_missing_services() {
    let (mut bridge, _host) = setup().await;
    // 握手声明的宿主能力集(services = tools/shell/systemPrompt)。
    let peer = bridge.ready().await.expect("still ready");
    // plugin/load 申报 injects 与宿主能力集求差,差集非空即显式拒绝。
    let missing = peer.missing_services(["tools", "approval", "jobs"]);
    assert_eq!(missing, vec!["approval".to_string(), "jobs".to_string()]);
    // 求差为空 = 可装载。
    assert!(peer.missing_services(["tools", "shell"]).is_empty());
}

// ---------------------------------------------------------------------------
// 同名仲裁(§三 规则 3)
// ---------------------------------------------------------------------------

#[test]
fn plugin_ledger_arbitration() {
    let mut ledger = PluginLedger::default();
    ledger.load("tool-bash", "npm:@deepseek-ai/dsh-tool-bash").expect("first load");
    // 同 id 同 entry:幂等重载。
    ledger.load("tool-bash", "npm:@deepseek-ai/dsh-tool-bash").expect("idempotent reload");
    // 同 id 异 entry:拒绝后者,指名已有者。
    match ledger.load("tool-bash", "file:/local/tool-bash") {
        Err(ProtoError::DuplicatePlugin { plugin_id, existing_entry, attempted_entry }) => {
            assert_eq!(plugin_id, "tool-bash");
            assert_eq!(existing_entry, "npm:@deepseek-ai/dsh-tool-bash");
            assert_eq!(attempted_entry, "file:/local/tool-bash");
        }
        e => panic!("expected DuplicatePlugin, got {e:?}"),
    }
    // 卸载后可换 entry。
    assert!(ledger.unload("tool-bash"));
    ledger.load("tool-bash", "file:/local/tool-bash").expect("load after unload");
    assert_eq!(ledger.entry("tool-bash"), Some("file:/local/tool-bash"));
    assert!(!ledger.unload("absent"));
}

// ---------------------------------------------------------------------------
// 重入(§十.6:llm 缝执行中 evt 回流)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reentrant_events_flow_while_call_in_flight() {
    let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let notify_seen = Arc::clone(&seen);
    let hooks = InboundHooks {
        on_notify: Some(Arc::new(move |method, _params| {
            Box::pin({
                let notify_seen = Arc::clone(&notify_seen);
                async move {
                    if method == "evt/emit" {
                        notify_seen.lock().unwrap().push("evt");
                    }
                }
            })
        })),
        on_request: None,
    };
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        hooks,
        ExpectedHost::protocol(1),
        PeerCaps::default(),
    );
    let host = TestHost { wire: host_wire };
    host.hello_ok(caps(&[])).await;
    bridge.ready().await.expect("handshake");

    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.request("svc/call", json!({ "m": "llm" }), None).await }
    });
    let Frame::Req { id, .. } = host.next().await else {
        panic!("expected req")
    };
    // 调用在飞期间,宿主回流事件:泵不因等待 res 而阻塞。
    host.wire
        .send(Frame::Ntf { method: "evt/emit".into(), params: json!({ "event": "agent/tool-call" }), scope_id: None })
        .await
        .expect("send evt");
    host.reply_ok(id, json!({ "turn": "done" })).await;
    assert_eq!(call.await.unwrap().unwrap()["turn"], "done");
    for _ in 0..50 {
        if seen.lock().unwrap().len() == 1 {
            break
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(*seen.lock().unwrap(), vec!["evt"]);
}

// ---------------------------------------------------------------------------
// 宿主死亡(§九 M1:仅失 llm 缝,观察连续性记录在案)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn host_death_fails_pending_calls_and_records_continuity() {
    let (mut bridge, host) = setup().await;
    let mut calls = Vec::new();
    for name in ["llm-call", "llm-call-2"] {
        calls.push(tokio::spawn({
            let bridge = bridge.clone();
            let name = name.to_string();
            async move { bridge.request("svc/call", json!({ "name": name }), None).await }
        }));
    }
    for _ in 0..2 {
        let Frame::Req { .. } = host.next().await else {
            panic!("expected req")
        };
    }
    // 杀宿主:对端消失。
    drop(host);

    for call in calls {
        match call.await.unwrap() {
            Err(ProtoError::HostGone) => {}
            e => panic!("expected HostGone, got {e:?}"),
        }
    }
    let record = bridge.wait_disconnect().await;
    assert_eq!(record.pending.len(), 2);
    assert!(record.pending.iter().all(|(_, m)| m == "svc/call"));
    assert!(record.frames_received >= 1); // 宿主发的 hello(两个 svc/call 是桥发出去的)

    // 死亡后的新调用立即 HostGone,不挂起。
    assert!(matches!(bridge.request("svc/call", json!({}), None).await, Err(ProtoError::HostGone)));
}

// ---------------------------------------------------------------------------
// 协议字段(§三:scopeId 预留、evt mode)
// ---------------------------------------------------------------------------

#[test]
fn frame_scope_id_reserved_roundtrip() {
    let frame = Frame::Req {
        id: 7,
        method: "svc/call".into(),
        params: json!({}),
        scope_id: Some("session-42".into()),
    };
    let wire = serde_json::to_string(&frame).expect("serialize");
    assert!(wire.contains("\"scopeId\":\"session-42\""), "{wire}");
    let back: Frame = serde_json::from_str(&wire).expect("deserialize");
    assert_eq!(back, frame);
    // 未声明时字段不出现。
    let bare = serde_json::to_string(&Frame::Ntf {
        method: "evt/emit".into(),
        params: json!({}),
        scope_id: None,
    })
    .expect("serialize");
    assert!(!bare.contains("scopeId"), "{bare}");
}

#[test]
fn evt_mode_accepts_three_dispatch_semantics_and_rejects_others() {
    let emit: EvtDeclaration =
        serde_json::from_value(json!({ "name": "agent/tool-call", "mode": "emit" })).expect("emit");
    assert_eq!(emit.mode, EvtMode::Emit);
    let parallel: EvtDeclaration =
        serde_json::from_value(json!({ "name": "session/flush", "mode": "parallel" })).expect("parallel");
    assert_eq!(parallel.mode, EvtMode::Parallel);
    let serial: EvtDeclaration =
        serde_json::from_value(json!({ "name": "agent/turn-stopping", "mode": "serial" })).expect("serial");
    assert_eq!(serial.mode, EvtMode::Serial);
    // waterfall 不是 evt 分发模式(它在 wf/register 的 kind 里)。
    assert!(serde_json::from_value::<EvtDeclaration>(json!({ "name": "x", "mode": "waterfall" })).is_err());
}
