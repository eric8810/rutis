//! llm 缝(设计 §四.1):桥在 TS 侧以 dsh llm adapter 形态注册,
//! `stream` 过线 → aimux,**chunk 以 ntf 流回传**(`llm/chunk`,按调用
//! 关联 id 即 dispatchId 关联),流终结以该调用的 res 交付。
//!
//! M2-3 范围:scripted 模型下的形状打通——`StreamPart` 经 serde JSON 原样
//! 过线,TS 侧映射 dsh `StreamChunk`(见 dsh 仓 experiments/m2-host 的
//! bridge-adapter)。`GenerateOptions` → `CallOptions` 的完整 prompt 映射
//! 与 chunk/finish/usage 逐字段保真是 L3(M2-4)的验收范围,当前透传
//! 最小面(scripted 模型不读 prompt)。

use std::sync::{Arc, OnceLock};

use aimux_core::language_model::LanguageModel;
use aimux_core::options::CallOptions;
use futures::StreamExt;
use rutis_cordis::{Bridge, InboundHooks, RemoteError};
use serde_json::{json, Value};

/// llm 缝本体:挂进桥的入站钩子,服务 `svc/call {service:"llm",
/// method:"stream"}`。
pub struct LlmSeam {
    model: Arc<dyn LanguageModel>,
    bridge: OnceLock<Bridge>,
}

impl LlmSeam {
    pub fn new(model: Arc<dyn LanguageModel>) -> Arc<LlmSeam> {
        Arc::new(LlmSeam { model, bridge: OnceLock::new() })
    }

    /// `Bridge::start` 之后注入句柄——钩子需要它发 chunk ntf。
    pub fn attach(&self, bridge: Bridge) {
        let _ = self.bridge.set(bridge);
    }

    /// 组装入站钩子(与事件缝共用一个 `InboundHooks` 时由调用方合并)。
    pub fn hooks(self: &Arc<Self>) -> InboundHooks {
        let seam = Arc::clone(self);
        let mut hooks = InboundHooks::default();
        hooks.on_request = Some(Arc::new(move |id, method, params| {
            let seam = Arc::clone(&seam);
            Box::pin(async move { seam.handle(id, &method, params).await })
        }));
        hooks
    }

    async fn handle(&self, id: u64, method: &str, params: Value) -> Result<Value, RemoteError> {
        if method != "svc/call" {
            return Err(RemoteError {
                code: "unhandled".into(),
                message: format!("llm seam only serves svc/call, got {method}"),
            })
        }
        let (service, op) = (params["service"].as_str(), params["method"].as_str());
        if service != Some("llm") || op != Some("stream") {
            return Err(RemoteError {
                code: "unhandled".into(),
                message: format!("llm seam only serves llm/stream, got {service:?}/{op:?}"),
            })
        }
        let bridge = self.bridge.get().ok_or_else(|| RemoteError {
            code: "notAttached".into(),
            message: "LlmSeam::attach was not called after Bridge::start".into(),
        })?;
        // M2-3:scripted 模型不读 prompt;完整映射在 M2-4(L3)。
        let options = CallOptions::default();
        let result = self
            .model
            .do_stream(&options)
            .await
            .map_err(|e| RemoteError { code: "llmStream".into(), message: e.to_string() })?;
        let mut stream = result.stream;
        let mut finish = Value::Null;
        while let Some(part) = stream.next().await {
            let part = part.map_err(|e| RemoteError {
                code: "llmStreamPart".into(),
                message: e.to_string(),
            })?;
            let encoded = serde_json::to_value(&part)
                .map_err(|e| RemoteError { code: "encode".into(), message: e.to_string() })?;
            if encoded.get("Finish").is_some() {
                finish = encoded.clone();
            }
            bridge
                .notify("llm/chunk", json!({ "dispatchId": id, "part": encoded }))
                .await
                .map_err(|e| RemoteError { code: "hostGone".into(), message: e.to_string() })?;
        }
        Ok(json!({ "finish": finish }))
    }
}
