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

use aimux_core::content::ContentPart;
use aimux_core::language_model::LanguageModel;
use aimux_core::language_model_message::LanguageModelPromptMessage;
use aimux_core::message::Role;
use aimux_core::options::{CallOptions, FunctionTool};
use futures::StreamExt;
use rutis_cordis::{Bridge, InboundHooks, RemoteError};
use serde_json::{json, Value};

/// dsh `GenerateOptions`(TS JSON)→ aimux `CallOptions` 的 L3 映射面:
/// `system` → System 消息;`messages` 的 text 块按角色映射(非文本块
/// 图片/文件是长尾清单项);`tools` 的 `{name,description,parameters}` →
/// `FunctionTool`。工具结果消息以 user 文本回喂(宿主侧组装)。
pub fn map_generate_options(generate: &Value) -> Result<CallOptions, RemoteError> {
    let mut prompt: Vec<LanguageModelPromptMessage> = Vec::new();
    if let Some(system) = generate.get("system").and_then(Value::as_str) {
        prompt.push(text_message(Role::System, system));
    }
    if let Some(messages) = generate.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = match message.get("role").and_then(Value::as_str) {
                Some("assistant") => Role::Assistant,
                Some("system") => Role::System,
                _ => Role::User,
            };
            let text = message
                .get("content")
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            prompt.push(text_message(role, &text));
        }
    }
    let tools = generate
        .get("tools")
        .and_then(Value::as_array)
        .map(|schemas| {
            schemas
                .iter()
                .filter_map(|schema| {
                    let name = schema.get("name")?.as_str()?.to_owned();
                    Some(
                        FunctionTool {
                            name,
                            description: schema
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            input_schema: schema
                                .get("parameters")
                                .cloned()
                                .unwrap_or_else(|| json!({})),
                            strict: None,
                            provider_options: None,
                            input_examples: None,
                        }
                        .into(),
                    )
                })
                .collect::<Vec<_>>()
        });
    Ok(CallOptions { prompt, tools, ..CallOptions::default() })
}

fn text_message(role: Role, text: &str) -> LanguageModelPromptMessage {
    LanguageModelPromptMessage {
        role,
        content: vec![ContentPart::Text { text: text.to_owned(), provider_options: None }],
        provider_options: None,
    }
}

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
        let generate = params
            .pointer("/params/options")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let options = map_generate_options(&generate)?;
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
