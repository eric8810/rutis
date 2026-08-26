//! wire 层分发验证(内存 wire,业务无关桥 + llm face):
//! - `svc/call stream` 的 part 流按 `svc/part` ntf 回传,dispatchId 关联,
//!   发射序 = 到达序,res 以 parts 计数收尾;
//! - 未注册服务 / 未知方法按 RemoteError 分类;
//! - hello 能力集从注册表推导。

use std::pin::Pin;
use std::sync::Arc;

use aimux_core::stream_part::StreamPart;
use aimux_llm::{AimuxLlm, LlmService, LlmServiceError};
use rutis_cordis::{
    Bridge, BridgeConfig, CordisService, ExpectedHost, Frame, MemoryWire, ServiceDispatch, Wire,
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
    let bridge = Bridge::start(
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


/// 固定的最小 LlmService:stream 回单条文本,list_models 回固定模型。
/// 用于稳定测 LlmFace 边界(不依赖 aimux_providers 的真实 provider 路由)。
struct FakeLlm;
#[async_trait::async_trait]
impl aimux_llm::LlmService for FakeLlm {
    async fn stream(
        &self,
        _req: aimux_llm::StreamRequest,
    ) -> Result<PartStream, LlmServiceError> {
        let stream: PartStream = Box::pin(async_stream::stream! {
            use aimux_core::stream_part::StreamPart;
            use aimux_core::types::{FinishReason, FinishReasonUnified, Usage};
            yield Ok(StreamPart::StreamStart { warnings: Vec::new() });
            yield Ok(StreamPart::TextDelta { id: "text-0".into(), delta: "hi".into(), provider_metadata: None });
            yield Ok(StreamPart::Finish { finish_reason: FinishReason { unified: FinishReasonUnified::Stop, raw: None }, usage: Usage::default(), provider_metadata: None });
        });
        Ok(stream)
    }
    async fn list_models(
        &self,
        _provider: &str,
        _api_key: Option<&str>,
    ) -> Result<Vec<aimux_llm::ModelBrief>, LlmServiceError> {
        Ok(vec![aimux_llm::ModelBrief { id: "mock-1".into(), owned_by: Some("rutis".into()), created: Some(1) }])
    }
}
type PartStream = Pin<Box<dyn futures::Stream<Item = Result<StreamPart, LlmServiceError>> + Send>>;

/// LlmFace 边界:listModels 形状转换 + stream 非法 params 的清晰错误(而非 panic)。
#[tokio::test]
async fn llm_face_list_models_and_illegal_stream_params_are_handled() {
    let face = LlmFace::new(Arc::new(FakeLlm));

    // listModels 正常:provider 默认 & apiKey 可选,返回 models 数组
    // FakeLlm 返回固定 1 个模型,且后端 id=provider, from_value 保留形状
    let v = match face.call("listModels", json!({})).await {
        Ok(rutis_cordis::ServiceReply::Value(v)) => v,
        _ => panic!("listModels should return Value(models)"),
    };
    let models = v.get("models").and_then(serde_json::Value::as_array);
    assert!(models.is_some(), "models key present: got {v}");
    assert_eq!(models.unwrap().len(), 1, "one mock model");

    let v2 = match face
        .call("listModels", json!({ "provider": "local", "apiKey": "k" }))
        .await
    {
        Ok(rutis_cordis::ServiceReply::Value(v)) => v,
        _ => panic!("listModels(provider/apiKey) should return Value(models)"),
    };
    assert!(v2.get("models").and_then(serde_json::Value::as_array).is_some());

    // stream:空 params 因 serde(default) 成功构造 → Stream reply
    let reply = match face.call("stream", json!({})).await {
        Ok(r) => r,
        Err(e) => panic!("empty stream params use serde(default), should succeed: {:?}", e),
    };
    assert!(matches!(reply, rutis_cordis::ServiceReply::Stream(_)));

    // stream:类型不匹配的 params → from_value 反序列化失败 → 清晰 RemoteError,不 panic
    let err = match face.call("stream", json!({ "options": { "messages": "not-an-array" } })).await {
        Err(e) => e,
        Ok(_) => panic!("type-mismatched stream params must err"),
    };
    assert!(!err.code.is_empty(), "error code should be non-empty, got {:?}", err);

    // 未知方法仍是 unhandled 分类
    let err = match face.call("nope", json!({})).await {
        Err(e) => e,
        Ok(_) => panic!("unknown method must err"),
    };
    assert_eq!(err.code, "unhandled");
}
