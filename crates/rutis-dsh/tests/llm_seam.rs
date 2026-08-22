//! llm 缝 M2-3 形状验证(内存 wire):scripted 模型产出确定 chunk 序列,
//! 断言 `llm/chunk` ntf 按 dispatchId 关联、发射序 = 到达序、res 携带
//! finish。TS 侧映射(dsh StreamChunk)的端到端在 dsh 仓 vitest 宿主验收。

use std::sync::Arc;

use aimux_core::error::AiMuxError;
use aimux_core::language_model::LanguageModel;
use aimux_core::options::CallOptions;
use aimux_core::result::{GenerateResult, StreamResult};
use aimux_core::stream_part::StreamPart;
use aimux_core::types::{FinishReason, FinishReasonUnified, Usage};
use async_trait::async_trait;
use rutis_cordis::{Bridge, BridgeConfig, ExpectedHost, Frame, MemoryWire, Wire};
use rutis_dsh::LlmSeam;
use serde_json::{json, Value};

/// 逐字弹出 "hel" "lo " "m2" 的 scripted 模型(手写最小版,不依赖
/// rutis-agent——兄弟 crate 不互相依赖)。
struct ChunkedLlm;

#[async_trait]
impl LanguageModel for ChunkedLlm {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn model_id(&self) -> &str {
        "chunked"
    }

    async fn do_generate(&self, _options: &CallOptions) -> Result<GenerateResult, AiMuxError> {
        Err(AiMuxError::Other("stream-only".into()))
    }

    async fn do_stream(&self, _options: &CallOptions) -> Result<StreamResult, AiMuxError> {
        let deltas: Vec<&str> = vec!["hel", "lo ", "m2"];
        let stream = async_stream::stream! {
            yield Ok(StreamPart::StreamStart { warnings: Vec::new() });
            for delta in deltas {
                yield Ok(StreamPart::TextDelta { id: "text-0".to_string(), delta: delta.to_string(), provider_metadata: None });
            }
            yield Ok(StreamPart::Finish {
                finish_reason: FinishReason { unified: FinishReasonUnified::Stop, raw: None },
                usage: Usage::default(),
                provider_metadata: None,
            });
        };
        Ok(StreamResult { stream: Box::pin(stream), request_body: None, response_headers: None })
    }
}

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

#[tokio::test]
async fn llm_stream_chunks_flow_as_ntf_in_order_with_finish_in_res() {
    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let seam = LlmSeam::new(Arc::new(ChunkedLlm));
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        seam.hooks(),
        ExpectedHost::protocol(1),
        json!({ "services": ["llm"], "wfKinds": [], "scopes": [] }),
    );
    seam.attach(bridge.clone());
    let host = TsShapedHost { wire: host_wire };
    host.hello().await;
    bridge.ready().await.expect("handshake");

    // TS 侧发起 stream 过线调用:svc/call 是宿主 → Rust 方向(TS adapter
    // 调桥后的 aimux 服务),宿主发 Req,chunk 流与 res 都来自桥。
    host.wire
        .send(Frame::Req {
            id: 7,
            method: "svc/call".into(),
            params: json!({ "service": "llm", "method": "stream", "params": { "options": { "provider": "aimux", "model": "scripted" } } }),
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send svc/call");
    let dispatch_id = 7u64;

    // chunk 流:StreamStart + 3×TextDelta + Finish,全部按 dispatchId 关联,
    // 发射序 = 到达序;随后 res 携带 finish。
    let mut kinds = Vec::new();
    let mut texts = String::new();
    loop {
        match host.next().await {
            Frame::Ntf { method, params, .. } => {
                assert_eq!(method, "llm/chunk");
                assert_eq!(params["dispatchId"], dispatch_id, "chunk 关联调用 id");
                let part = &params["part"];
                if let Some(delta) = part.get("TextDelta").and_then(|d| d.get("delta")).and_then(Value::as_str) {
                    texts.push_str(delta);
                }
                let kind = part.as_object().and_then(|m| m.keys().next()).cloned().unwrap_or_default();
                kinds.push(kind);
            }
            Frame::Res { id, ok, result, .. } => {
                assert_eq!(id, dispatch_id);
                assert!(ok, "result: {result:?}");
                assert!(result.expect("payload")["finish"].get("Finish").is_some());
                break
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(kinds, vec!["StreamStart", "TextDelta", "TextDelta", "TextDelta", "Finish"]);
    assert_eq!(texts, "hello m2");
}
