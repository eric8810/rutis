//! wire 层分发验证(内存 wire,业务无关桥 + llm face):
//! - `svc/call stream` 的 part 流按 `svc/part` ntf 回传,dispatchId 关联,
//!   发射序 = 到达序,res 以 parts 计数收尾;
//! - 未注册服务 / 未知方法按 RemoteError 分类;
//! - hello 能力集从注册表推导。

use std::sync::Arc;

use aimux_llm::{AimuxLlm, LlmService};
use rutis_cordis::{
    Bridge, BridgeConfig, ExpectedHost, Frame, MemoryWire, ServiceDispatch, Wire,
};
use rutis_dsh::LlmFace;
use serde_json::json;

struct TsShapedHost {
    wire: MemoryWire,
}

impl TsShapedHost {
    async fn next(&self) -> Frame {
        self.wire.recv().await.expect("bridge alive")
    }

    async fn hello(&self) {
        self.wire
            .send(Frame::Req {
                id: 1,
                method: "hello".into(),
                params: json!({
                    "protocol": 1, "base": "min-cordis", "baseSemver": "0.1.0",
                    "stack": ["node"], "caps": { "services": [], "wfKinds": [], "scopes": [] },
                }),
                scope_id: None,
                session_id: None,
                turn_id: None,
            })
            .await
            .expect("send hello");
        let Frame::Res { ok, .. } = self.next().await else {
            panic!("expected hello res")
        };
        assert!(ok);
    }
}

/// scripted 服务:逐字 "hel" "lo " "m2"(经 AimuxLlm 的回落路径)。
fn chunked_service() -> Arc<dyn LlmService> {
    Arc::new(AimuxLlm::new(Arc::new(ChunkedLlm), "scripted", "chunked"))
}

struct ChunkedLlm;

#[async_trait::async_trait]
impl aimux_core::language_model::LanguageModel for ChunkedLlm {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn model_id(&self) -> &str {
        "chunked"
    }

    async fn do_generate(
        &self,
        _o: &aimux_core::options::CallOptions,
    ) -> Result<aimux_core::result::GenerateResult, aimux_core::error::AiMuxError> {
        Err(aimux_core::error::AiMuxError::Other("stream-only".into()))
    }

    async fn do_stream(
        &self,
        _o: &aimux_core::options::CallOptions,
    ) -> Result<aimux_core::result::StreamResult, aimux_core::error::AiMuxError> {
        let stream = async_stream::stream! {
            use aimux_core::stream_part::StreamPart;
            use aimux_core::types::{FinishReason, FinishReasonUnified, Usage};
            yield Ok(StreamPart::StreamStart { warnings: Vec::new() });
            for delta in ["hel", "lo ", "m2"] {
                yield Ok(StreamPart::TextDelta { id: "text-0".to_string(), delta: delta.to_string(), provider_metadata: None });
            }
            yield Ok(StreamPart::Finish {
                finish_reason: FinishReason { unified: FinishReasonUnified::Stop, raw: None },
                usage: Usage::default(),
                provider_metadata: None,
            });
        };
        Ok(aimux_core::result::StreamResult {
            stream: Box::pin(stream),
            request_body: None,
            response_headers: None,
        })
    }
}

async fn start(dispatch: &Arc<ServiceDispatch>, wire: MemoryWire) -> Bridge {
    let mut bridge = Bridge::start(
        Box::new(wire),
        BridgeConfig::default(),
        dispatch.hooks(),
        ExpectedHost::protocol(1),
        json!({ "services": dispatch.names(), "wfKinds": [], "scopes": [] }),
    );
    dispatch.attach(bridge.clone());
    bridge
}

#[tokio::test]
async fn stream_parts_flow_as_svc_part_ntfs_in_order_with_res_tail() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let dispatch = ServiceDispatch::new(vec![LlmFace::new(chunked_service())]);
    let mut bridge = start(&dispatch, bridge_wire).await;
    let host = TsShapedHost { wire: host_wire };
    host.hello().await;
    bridge.ready().await.expect("handshake");

    host.wire
        .send(Frame::Req {
            id: 7,
            method: "svc/call".into(),
            params: json!({ "service": "llm", "method": "stream", "params": { "provider": "scripted", "model": "chunked", "options": {} } }),
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send svc/call");
    let dispatch_id = 7u64;

    let mut kinds = Vec::new();
    let mut texts = String::new();
    loop {
        match host.next().await {
            Frame::Ntf { method, params, .. } => {
                assert_eq!(method, "svc/part", "通用 part ntf(不再有 llm/chunk)");
                assert_eq!(params["dispatchId"], dispatch_id, "part 关联调用 id");
                let part = &params["part"];
                if let Some(delta) = part.get("TextDelta").and_then(|d| d.get("delta")).and_then(|v| v.as_str()) {
                    texts.push_str(delta);
                }
                let kind = part.as_object().and_then(|m| m.keys().next()).cloned().unwrap_or_default();
                kinds.push(kind);
            }
            Frame::Res { id, ok, result, .. } => {
                assert_eq!(id, dispatch_id);
                assert!(ok, "result: {result:?}");
                let result = result.expect("res payload");
                assert_eq!(result["parts"], 5, "StreamStart + 3×TextDelta + Finish");
                break
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(kinds, vec!["StreamStart", "TextDelta", "TextDelta", "TextDelta", "Finish"]);
    assert_eq!(texts, "hello m2");
    drop(bridge);
}

#[tokio::test]
async fn unknown_service_and_method_are_classified_remote_errors() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let dispatch = ServiceDispatch::new(vec![LlmFace::new(chunked_service())]);
    let mut bridge = start(&dispatch, bridge_wire).await;
    let host = TsShapedHost { wire: host_wire };
    host.hello().await;
    bridge.ready().await.expect("handshake");

    for (params, code) in [
        (json!({ "service": "nope", "method": "stream" }), "noService"),
        (json!({ "service": "llm", "method": "generate" }), "unhandled"),
    ] {
        host.wire
            .send(Frame::Req {
                id: 9,
                method: "svc/call".into(),
                params,
                scope_id: None,
                session_id: None,
                turn_id: None,
            })
            .await
            .expect("send");
        match host.next().await {
            Frame::Res { ok, error, .. } => {
                assert!(!ok);
                assert_eq!(error.expect("res error").code, code);
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    drop(bridge);
}

/// hello 能力集来自注册表:注册两个服务(第二个是假名服务)即声明两名。
#[tokio::test]
async fn hello_caps_derive_from_the_registry() {
    struct Echo;
    #[async_trait::async_trait]
    impl rutis_cordis::CordisService for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        async fn call(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<rutis_cordis::ServiceReply, rutis_cordis::RemoteError> {
            Err(rutis_cordis::RemoteError { code: "unhandled".into(), message: format!("echo/{method}") })
        }
    }
    let dispatch = ServiceDispatch::new(vec![LlmFace::new(chunked_service()), Arc::new(Echo)]);
    assert_eq!(dispatch.names(), vec!["echo".to_string(), "llm".to_string()]);
}
