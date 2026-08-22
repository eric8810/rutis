//! M1 验收(设计 §九 M1 行),机制层:内存 wire 下内核原语全套——往返、
//! 并发、取消(含迟到 res 丢弃)、超时、握手错配与能力集求差、同名仲裁、
//! 重入、杀宿主 → 仅失 llm 缝且观察连续性记录在案。零 Node,零 dsh 知识
//! (dsh 节校验的测试在 rutis-dsh crate)。

use std::sync::{Arc, Mutex};

use rutis_cordis::{
    Bridge, BridgeConfig, CancelTarget, EvtDeclaration, EvtMode, ExpectedHost, Frame, InboundHooks,
    MemoryWire, PeerCaps, PluginLedger, ProtoError, WfDeclaration, WfKind, Wire,
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
            .send(Frame::Res {
                id,
                ok: true,
                result: Some(result),
                error: None,
                scope_id: None,
                session_id: None,
                turn_id: None,
            })
            .await
            .expect("send res ok");
    }

    /// 宿主发起一次 hello 握手(§三 规则 1:宿主首发),返回桥的回包。
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
        let frame = self.next().await;
        let Frame::Res { ok, result, error, .. } = frame else {
            panic!("expected hello res, got {frame:?}")
        };
        assert!(ok, "hello rejected: {:?}", error);
        result.expect("hello result payload")
    }

    /// 完成一次合法握手:发 `hello` 声明,收桥的对称回包。
    async fn hello_ok(&self, caps: Value) -> Value {
        self.hello(json!({
            "protocol": 1,
            "base": "min-cordis",
            "baseSemver": "0.1.0",
            "stack": ["node"],
            "caps": caps,
        }))
        .await
    }
}

fn caps(services: &[&str]) -> Value {
    json!({ "services": services, "wfKinds": [], "scopes": [] })
}

/// 标准会话:握手完成,桥侧 Ready。返回(桥, 宿主, 宿主 hello 原始参数)。
async fn setup() -> (Bridge, TestHost, Value) {
    setup_with(ExpectedHost::protocol(1)).await
}

async fn setup_with(expected: ExpectedHost) -> (Bridge, TestHost, Value) {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        expected,
        json!({ "services": ["observe"], "wfKinds": [], "scopes": [] }),
    );
    let host = TestHost { wire: host_wire };
    host.hello_ok(caps(&["tools", "shell", "systemPrompt"])).await;
    let params = bridge.ready().await.expect("handshake");
    (bridge, host, params)
}

// ---------------------------------------------------------------------------
// 往返
// ---------------------------------------------------------------------------

#[tokio::test]
async fn roundtrip_ok_and_remote_error() {
    let (bridge, host, _) = setup().await;

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
        async move { bridge.request("svc/call", json!({}), None).await }
    });
    let Frame::Req { id: id_err, .. } = host.next().await else {
        panic!("expected req")
    };
    host.wire
        .send(rutis_cordis::Frame::Res {
            id: id_err,
            ok: false,
            result: None,
            error: Some(rutis_cordis::RemoteError {
                code: "notFound".into(),
                message: "service missing".into(),
            }),
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send res err");
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
    let (bridge, host, _) = setup().await;
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
async fn cancel_settles_as_cancelled_and_late_res_counts_orphan() {
    let (bridge, host, _) = setup().await;
    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.request("svc/call", json!({}), None).await }
    });
    let Frame::Req { id, .. } = host.next().await else {
        panic!("expected req")
    };

    bridge.cancel(CancelTarget::call(id)).await.expect("cancel notify");
    // 取消以 Cancelled 结算,与远端错误可区分(F2)。
    match call.await.unwrap().unwrap_err() {
        ProtoError::Cancelled { id: cid, method } => {
            assert_eq!(cid, id);
            assert_eq!(method, "svc/call");
        }
        e => panic!("expected Cancelled, got {e:?}"),
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
        serde_json::json!({}),
    );
    let host = TestHost { wire: host_wire };
    host.hello_ok(caps(&[])).await;
    bridge.ready().await.expect("handshake");

    let call = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.request("svc/call", json!({}), Some(40)).await }
    });
    let Frame::Req { id, .. } = host.next().await else {
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
        serde_json::json!({}),
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
                "caps": caps(&[]),
            }),
            scope_id: None,
            session_id: None,
            turn_id: None,
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
async fn handshake_base_pin_rejects_wrong_base() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedHost { protocol: 1, base: Some("min-cordis".into()), verify: None },
        serde_json::json!({}),
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
                "caps": caps(&[]),
            }),
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send hello");
    let Frame::Res { ok, error, .. } = host.next().await else {
        panic!("expected error res")
    };
    assert!(!ok);
    assert!(error.expect("error payload").message.contains("base mismatch"));
    match bridge.ready().await {
        Err(ProtoError::Handshake(reason)) => assert!(reason.contains("base mismatch")),
        e => panic!("expected Handshake, got {e:?}"),
    }
}

/// F1:首帧纪律对三类帧全覆盖——Ntf 开场即终态拒绝,不静默吞掉。
#[tokio::test]
async fn first_frame_ntf_rejected() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedHost::protocol(1),
        serde_json::json!({}),
    );
    let host = TestHost { wire: host_wire };
    host.wire
        .send(Frame::Ntf {
            method: "evt/emit".into(),
            params: json!({}),
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send ntf first frame");
    match bridge.ready().await {
        Err(ProtoError::Handshake(reason)) => {
            assert!(reason.contains("first frame must be hello"), "{reason}")
        }
        e => panic!("expected Handshake, got {e:?}"),
    }
}

/// F1:Res 开场同样终态拒绝。
#[tokio::test]
async fn first_frame_res_rejected() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedHost::protocol(1),
        serde_json::json!({}),
    );
    let host = TestHost { wire: host_wire };
    host.wire
        .send(Frame::Res {
            id: 9,
            ok: true,
            result: Some(json!({})),
            error: None,
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send res first frame");
    match bridge.ready().await {
        Err(ProtoError::Handshake(reason)) => {
            assert!(reason.contains("first frame must be hello"), "{reason}")
        }
        e => panic!("expected Handshake, got {e:?}"),
    }
}

#[tokio::test]
async fn first_frame_req_must_be_hello() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        InboundHooks::default(),
        ExpectedHost::protocol(1),
        serde_json::json!({}),
    );
    let host = TestHost { wire: host_wire };
    host.wire
        .send(Frame::Req {
            id: 1,
            method: "evt/on".into(),
            params: json!({}),
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send non-hello first frame");
    let Frame::Res { ok, error, .. } = host.next().await else {
        panic!("expected error res")
    };
    assert!(!ok);
    assert!(error.expect("error payload").message.contains("first frame must be hello"));
    match bridge.ready().await {
        Err(ProtoError::Handshake(reason)) => {
            assert!(reason.contains("first frame must be hello"), "{reason}")
        }
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
        json!({ "services": ["llm"], "wfKinds": ["decide"], "scopes": ["session"] }),
    );
    let host = TestHost { wire: host_wire };
    let reply = host.hello_ok(caps(&["tools"])).await;
    // 回显 protocol 供宿主对称验证(D2)。
    assert_eq!(reply["protocol"], 1);
    assert_eq!(reply["base"], "rutis");
    assert_eq!(reply["caps"]["services"], json!(["llm"]));
    assert_eq!(reply["caps"]["wfKinds"], json!(["decide"]));
    let params = bridge.ready().await.expect("handshake");
    let peer = PeerCaps::from_hello_params(&params).expect("parse caps");
    assert_eq!(peer.services, ["tools"].into_iter().map(str::to_owned).collect());
}

#[tokio::test]
async fn capability_diff_rejects_load_with_missing_services() {
    let (_, _, params) = setup().await;
    // 握手声明的宿主能力集(services = tools/shell/systemPrompt)。
    let peer = PeerCaps::from_hello_params(&params).expect("parse caps");
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

/// F3 强化:res 被**扣住**,直到 evt 实际抵达钩子才放行——证明泵没有
/// 等待在飞调用,事件在应答未到时已被分发。
#[tokio::test]
async fn reentrant_events_processed_before_call_settles() {
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::channel::<()>(1);
    let notify_seen = Arc::new(Mutex::new(seen_tx));
    let hooks = InboundHooks {
        on_notify: Some(Arc::new(move |method, _params| {
            Box::pin({
                let notify_seen = Arc::clone(&notify_seen);
                async move {
                    if method == "evt/emit" {
                        let signal = notify_seen.lock().unwrap().clone();
                        signal.send(()).await.expect("signal test");
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
        serde_json::json!({}),
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
    // 调用在飞期间宿主回流事件,且**先不回应答**。
    host.wire
        .send(Frame::Ntf {
            method: "evt/emit".into(),
            params: json!({ "event": "agent/tool-call" }),
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send evt");
    // res 仍在扣住状态:evt 必须已经抵达钩子,否则这里 5s 超时失败——
    // 这就是"泵不阻塞"的直接证明。
    tokio::time::timeout(std::time::Duration::from_secs(5), seen_rx.recv())
        .await
        .expect("evt processed while res withheld")
        .expect("hook alive");
    // 现在才放行 res,调用正常完成。
    host.reply_ok(id, json!({ "turn": "done" })).await;
    assert_eq!(call.await.unwrap().unwrap()["turn"], "done");
}

// ---------------------------------------------------------------------------
// 宿主死亡(§九 M1:仅失 llm 缝,观察连续性记录在案)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn host_death_fails_pending_calls_and_records_continuity() {
    let (mut bridge, host, _) = setup().await;
    let mut calls = Vec::new();
    for _ in 0..2 {
        calls.push(tokio::spawn({
            let bridge = bridge.clone();
            async move { bridge.request("svc/call", json!({}), None).await }
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

    // 死亡后的新调用立即 HostGone,不挂起(F4:notify 同样有终态门)。
    assert!(
        matches!(bridge.request("svc/call", json!({}), None).await, Err(ProtoError::HostGone))
    );
    assert!(matches!(bridge.notify("evt/emit", json!({})).await, Err(ProtoError::HostGone)));
    assert!(matches!(
        bridge.cancel(CancelTarget::call(1)).await,
        Err(ProtoError::HostGone)
    ));
}

// ---------------------------------------------------------------------------
// 协议字段(§三:三预留字段、evt mode、wf kind)
// ---------------------------------------------------------------------------

#[test]
fn frame_reserved_fields_roundtrip() {
    let frame = Frame::Req {
        id: 7,
        method: "svc/call".into(),
        params: json!({}),
        scope_id: Some("scope-9".into()),
        session_id: Some("session-42".into()),
        turn_id: Some("turn-3".into()),
    };
    let wire = serde_json::to_string(&frame).expect("serialize");
    assert!(wire.contains("\"scopeId\":\"scope-9\""), "{wire}");
    assert!(wire.contains("\"sessionId\":\"session-42\""), "{wire}");
    assert!(wire.contains("\"turnId\":\"turn-3\""), "{wire}");
    let back: Frame = serde_json::from_str(&wire).expect("deserialize");
    assert_eq!(back, frame);
    // 未声明时字段不出现。
    let bare = serde_json::to_string(&Frame::Ntf {
        method: "evt/emit".into(),
        params: json!({}),
        scope_id: None,
        session_id: None,
        turn_id: None,
    })
    .expect("serialize");
    assert!(!bare.contains("scopeId") && !bare.contains("sessionId") && !bare.contains("turnId"), "{bare}");
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

/// F5:wf/register 的 kind 三型是 v1.1 冻结字段,与 evt mode 对称。
#[test]
fn wf_kind_three_shapes_frozen() {
    let decide: WfDeclaration =
        serde_json::from_value(json!({ "name": "tools/pre-execute", "kind": "decide" }))
            .expect("decide");
    assert_eq!(decide.kind, WfKind::Decide);
    let around: WfDeclaration =
        serde_json::from_value(json!({ "name": "tools/execute", "kind": "around" }))
            .expect("around");
    assert_eq!(around.kind, WfKind::Around);
    let stream: WfDeclaration =
        serde_json::from_value(json!({ "name": "agent/text-delta", "kind": "stream" }))
            .expect("stream");
    assert_eq!(stream.kind, WfKind::Stream);
    // kind 值域封闭:未知拒绝,不静默字符串化。
    assert!(serde_json::from_value::<WfDeclaration>(json!({ "name": "x", "kind": "waterfall" })).is_err());
    assert!(serde_json::from_value::<WfDeclaration>(json!({ "name": "x", "kind": "emit" })).is_err());
}
