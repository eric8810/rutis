//! llm 缝(设计 §四.1):桥在 TS 侧以 dsh llm adapter 形态注册,
//! `stream` 过线 → aimux,**chunk 以 ntf 流回传**(`llm/chunk`,按调用
//! 关联 id 即 dispatchId 关联),流终结以该调用的 res 交付。
//!
//! M2-3 范围:scripted 模型下的形状打通——`StreamPart` 经 serde JSON 原样
//! 过线,TS 侧映射 dsh `StreamChunk`(见 dsh 仓 experiments/m2-host 的
//! bridge-adapter)。`GenerateOptions` → `CallOptions` 的完整 prompt 映射
//! 与 chunk/finish/usage 逐字段保真是 L3(M2-4)的验收范围,当前透传
//! 最小面(scripted 模型不读 prompt)。

use std::sync::{Arc, Mutex, OnceLock};

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

/// per-key provider 工厂:(api_key, model) → provider。默认实现走 aimux;
/// 测试注入 fake 以断言 keyed 路由(不联网)。
pub type ProviderFactory =
    Arc<dyn Fn(&str, &str) -> Result<Arc<dyn LanguageModel>, String> + Send + Sync>;

/// llm 缝本体:挂进桥的入站钩子,服务 `svc/call {service:"llm",
/// method:"stream"}`。
///
/// 凭据双路:dsh 侧 per-request 解析的 key 随请求过线时(§web Models 页
/// 配置的 key 即此路径),按 (key, model) 构造并缓存 provider;无 key 时
/// 回落 `fallback`(runner 以 env 构造,或 UnconfiguredModel 占位)。模型
/// 名以过线值为准(页面/组合里的选择),fallback 仅兜底。
pub struct LlmSeam {
    fallback: Arc<dyn LanguageModel>,
    provider_name: String,
    fallback_model: String,
    factory: ProviderFactory,
    keyed: Mutex<std::collections::HashMap<String, Arc<dyn LanguageModel>>>,
    bridge: OnceLock<Bridge>,
}

impl LlmSeam {
    pub fn new(
        fallback: Arc<dyn LanguageModel>,
        provider_name: impl Into<String>,
        fallback_model: impl Into<String>,
    ) -> Arc<LlmSeam> {
        let provider_name = provider_name.into();
        Self::with_factory(fallback, provider_name.clone(), fallback_model, Arc::new(move |key, model| {
            aimux_providers::provider(&provider_name, Some(key.to_owned()), model, None)
                .map(|m| Arc::from(m) as Arc<dyn LanguageModel>)
                .map_err(|e| e.to_string())
        }))
    }

    /// 工厂注入版(测试)。
    pub fn with_factory(
        fallback: Arc<dyn LanguageModel>,
        provider_name: impl Into<String>,
        fallback_model: impl Into<String>,
        factory: ProviderFactory,
    ) -> Arc<LlmSeam> {
        Arc::new(LlmSeam {
            fallback,
            provider_name: provider_name.into(),
            fallback_model: fallback_model.into(),
            factory,
            keyed: Mutex::new(std::collections::HashMap::new()),
            bridge: OnceLock::new(),
        })
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
        // 请求侧入口日志(proxy 可观察性):每次过线调用一行入口 + 一行
        // 出口,stderr 不占 dsh 的 stdout 交互面。
        eprintln!(
            "[rutis-dsh] llm/stream id={id} provider={} model={} msgs={} tools={} system={} key={}",
            generate.get("provider").and_then(Value::as_str).unwrap_or("-"),
            generate.get("model").and_then(Value::as_str).unwrap_or("-"),
            generate.get("messages").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            generate.get("tools").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            if generate.get("system").is_some() { "yes" } else { "no" },
            if params.pointer("/params/credentials/apiKey").is_some() { "dsh" } else { "env" },
        );
        // 模型选择:过线值(页面/组合)优先,fallback 仅兜底;凭据:dsh 侧
        // 解析的 key 随请求过线时按 (key, model) 构造并缓存 provider。
        let wire_model = generate
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .unwrap_or(&self.fallback_model)
            .to_owned();
        let model: Arc<dyn LanguageModel> = match params
            .pointer("/params/credentials/apiKey")
            .and_then(Value::as_str)
        {
            Some(api_key) => {
                let cache_key = format!("{api_key}\u{0}{wire_model}");
                let mut keyed = self.keyed.lock().unwrap();
                if let Some(model) = keyed.get(&cache_key) {
                    Arc::clone(model)
                } else {
                    match (self.factory)(api_key, &wire_model) {
                        Ok(model) => {
                            keyed.insert(cache_key, Arc::clone(&model));
                            model
                        }
                        Err(e) => {
                            eprintln!("[rutis-dsh] llm/stream id={id} provider build failed: {e}");
                            return Err(RemoteError { code: "llmProvider".into(), message: e })
                        }
                    }
                }
            }
            None => Arc::clone(&self.fallback),
        };
        let result = match model.do_stream(&options).await {
            Ok(result) => result,
            Err(e) => {
                eprintln!("[rutis-dsh] llm/stream id={id} failed: {e}");
                return Err(RemoteError { code: "llmStream".into(), message: e.to_string() })
            }
        };
        let mut stream = result.stream;
        let mut finish = Value::Null;
        let mut chunks: u64 = 0;
        while let Some(part) = stream.next().await {
            chunks += 1;
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
        let finish_kind = finish
            .get("Finish")
            .and_then(|f| f.get("finish_reason"))
            .and_then(|r| r.get("unified"))
            .and_then(Value::as_str)
            .unwrap_or("-");
        eprintln!("[rutis-dsh] llm/stream id={id} done chunks={chunks} finish={finish_kind}");
        Ok(json!({ "finish": finish }))
    }
}
