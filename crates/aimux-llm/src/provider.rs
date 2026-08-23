//! [`LlmService`] 的 aimux 实现:工厂/缓存/回落/流循环本体
//! (自 rutis-dsh 的 LlmSeam 迁入,决策文档 v2 对象 A)。

use std::sync::{Arc, Mutex};

use aimux_core::content::ContentPart;
use aimux_core::language_model::LanguageModel;
use aimux_core::language_model_message::LanguageModelPromptMessage;
use aimux_core::message::Role;
use aimux_core::options::{CallOptions, FunctionTool};
use futures::StreamExt;
use serde_json::{json, Value};

use crate::service::{
    LlmService, LlmServiceError, ModelBrief, PartStream, PromptSpec, StreamRequest,
};

/// per-key provider 工厂:(provider, api_key, model) → model。默认实现走
/// aimux;测试注入 fake 以断言 keyed 路由(不联网)。
pub type ProviderFactory =
    Arc<dyn Fn(&str, &str, &str) -> Result<Arc<dyn LanguageModel>, String> + Send + Sync>;

/// 未配置的模型占位:构造失败(如缺 key)不阻止宿主启动;真正的调用
/// 发生时错误才产生。
struct UnconfiguredModel {
    reason: String,
}

#[async_trait::async_trait]
impl LanguageModel for UnconfiguredModel {
    fn provider(&self) -> &str {
        "unconfigured"
    }

    fn model_id(&self) -> &str {
        "unconfigured"
    }

    async fn do_generate(
        &self,
        _options: &CallOptions,
    ) -> Result<aimux_core::result::GenerateResult, aimux_core::error::AiMuxError> {
        Err(aimux_core::error::AiMuxError::Other(self.reason.clone()))
    }

    async fn do_stream(
        &self,
        _options: &CallOptions,
    ) -> Result<aimux_core::result::StreamResult, aimux_core::error::AiMuxError> {
        Err(aimux_core::error::AiMuxError::Other(self.reason.clone()))
    }
}

/// llm 服务本体。
///
/// 凭据双路:请求携带 key 时按 (key, provider, model) 构造并缓存 provider;
/// 无 key 时回落 `fallback`(构造期由 env 兜底或 [`UnconfiguredModel`] 占位)。
/// provider/model 以请求值为准,兜底仅补缺。
pub struct AimuxLlm {
    fallback: Arc<dyn LanguageModel>,
    provider_name: String,
    fallback_model: String,
    factory: ProviderFactory,
    keyed: Mutex<std::collections::HashMap<String, Arc<dyn LanguageModel>>>,
    list_cache: Mutex<std::collections::HashMap<String, Value>>,
}

impl AimuxLlm {
    pub fn new(fallback: Arc<dyn LanguageModel>, provider_name: impl Into<String>, fallback_model: impl Into<String>) -> Self {
        let provider_name = provider_name.into();
        Self::with_factory(fallback, provider_name.clone(), fallback_model, Arc::new(move |provider, key, model| {
            aimux_providers::provider(provider, Some(key.to_owned()), model, None)
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
    ) -> Self {
        Self {
            fallback,
            provider_name: provider_name.into(),
            fallback_model: fallback_model.into(),
            factory,
            keyed: Mutex::new(std::collections::HashMap::new()),
            list_cache: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// env 兜底构造:`AIMUX_PROVIDER`/`AIMUX_MODEL`(默认 deepseek/
    /// deepseek-chat),key 走各 provider 的 env。构造失败不阻塞——
    /// 占位模型把错误留到调用时。
    pub fn from_env() -> Self {
        let provider_name = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
        let model_id = std::env::var("AIMUX_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
        match aimux_providers::provider(&provider_name, None, &model_id, None) {
            Ok(model) => Self::new(Arc::from(model), provider_name, model_id),
            Err(e) => {
                eprintln!("[aimux-llm] model not configured ({provider_name}/{model_id}: {e}) —");
                eprintln!("[aimux-llm] the host still boots; model calls will surface this error.");
                eprintln!("[aimux-llm] set the provider key (e.g. DEEPSEEK_API_KEY) and restart to enable them.");
                Self::new(
                    Arc::new(UnconfiguredModel { reason: format!("provider {provider_name}/{model_id} not configured: {e}") }),
                    provider_name,
                    model_id,
                )
            }
        }
    }

    fn model_for(&self, req: &StreamRequest) -> Result<Arc<dyn LanguageModel>, LlmServiceError> {
        let wire_provider = req
            .provider
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| self.provider_name.clone());
        let wire_model = req
            .model
            .clone()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| self.fallback_model.clone());
        match &req.api_key {
            Some(api_key) => {
                let cache_key = format!("{api_key}\u{0}{wire_provider}\u{0}{wire_model}");
                let mut keyed = self.keyed.lock().unwrap();
                if let Some(model) = keyed.get(&cache_key) {
                    return Ok(Arc::clone(model))
                }
                match (self.factory)(&wire_provider, api_key, &wire_model) {
                    Ok(model) => {
                        keyed.insert(cache_key, Arc::clone(&model));
                        Ok(model)
                    }
                    Err(e) => Err(LlmServiceError::new("llmProvider", e)),
                }
            }
            None => Ok(Arc::clone(&self.fallback)),
        }
    }
}

/// DTO → aimux `CallOptions`:system → System 消息;逐消息按 role 映射
/// (非文本块是长尾,不进入本形状);工具 `{name,description,parameters}`
/// → `FunctionTool`。
fn to_call_options(spec: &PromptSpec) -> CallOptions {
    let mut prompt: Vec<LanguageModelPromptMessage> = Vec::new();
    if let Some(system) = &spec.system {
        prompt.push(text_message(Role::System, system));
    }
    for message in &spec.messages {
        let role = match message.role.as_deref() {
            Some("assistant") => Role::Assistant,
            Some("system") => Role::System,
            _ => Role::User,
        };
        prompt.push(text_message(role, &message.text));
    }
    let tools = spec
        .tools
        .iter()
        .filter(|t| !t.name.is_empty())
        .map(|t| {
            aimux_core::tool::Tool::Function(FunctionTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.parameters.clone().unwrap_or_else(|| json!({})),
                strict: None,
                provider_options: None,
                input_examples: None,
            })
        })
        .collect::<Vec<_>>();
    CallOptions { prompt, tools: (!tools.is_empty()).then_some(tools), ..CallOptions::default() }
}

fn text_message(role: Role, text: &str) -> LanguageModelPromptMessage {
    LanguageModelPromptMessage {
        role,
        content: vec![ContentPart::Text { text: text.to_owned(), provider_options: None }],
        provider_options: None,
    }
}

#[async_trait::async_trait]
impl LlmService for AimuxLlm {
    async fn stream(&self, req: StreamRequest) -> Result<PartStream, LlmServiceError> {
        let provider = req.provider.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| "-".into());
        let model = req.model.clone().filter(|m| !m.is_empty()).unwrap_or_else(|| "-".into());
        // 入口/出口日志(调用方可观察性;stderr 不占用任何宿主的 stdout)。
        eprintln!(
            "[aimux-llm] stream provider={provider} model={model} msgs={} tools={} system={} key={}",
            req.options.messages.len(),
            req.options.tools.len(),
            if req.options.system.is_some() { "yes" } else { "no" },
            if req.api_key.is_some() { "request" } else { "fallback" },
        );
        let model_impl = self.model_for(&req)?;
        let options = to_call_options(&req.options);
        let result = model_impl
            .do_stream(&options)
            .await
            .map_err(|e| LlmServiceError::new("llmStream", e.to_string()))?;
        let mut stream = result.stream;
        let out = async_stream::stream! {
            let mut chunks: u64 = 0;
            let mut finish = Value::Null;
            while let Some(part) = stream.next().await {
                let part = match part {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[aimux-llm] stream part error after {chunks} chunks: {e}");
                        yield Err(LlmServiceError::new("llmStreamPart", e.to_string()));
                        return
                    }
                };
                chunks += 1;
                let encoded = serde_json::to_value(&part)
                    .map_err(|e| LlmServiceError::new("encode", e.to_string()))?;
                if encoded.get("Finish").is_some() {
                    finish = encoded.get("Finish").cloned().unwrap_or(Value::Null);
                }
                yield Ok(part);
            }
            let finish_kind = finish
                .get("finish_reason")
                .and_then(|r| r.get("unified"))
                .and_then(Value::as_str)
                .unwrap_or("-");
            eprintln!("[aimux-llm] stream done provider={provider} model={model} chunks={chunks} finish={finish_kind}");
        };
        Ok(Box::pin(out))
    }

    async fn list_models(
        &self,
        provider: &str,
        api_key: Option<&str>,
    ) -> Result<Vec<ModelBrief>, LlmServiceError> {
        let key = api_key.unwrap_or_default().to_owned();
        let cache_key = format!("list\u{0}{provider}\u{0}{key}");
        if let Some(cached) = self.list_cache.lock().unwrap().get(&cache_key).cloned() {
            return Ok(serde_json::from_value(cached).unwrap_or_default())
        }
        let handle = aimux_providers::provider_handle(
            provider,
            if key.is_empty() { None } else { Some(key.clone()) },
            None,
        )
        .map_err(|e| LlmServiceError::new("llmProvider", e.to_string()))?;
        let listed = handle
            .list_models()
            .await
            .map_err(|e| LlmServiceError::new("llmListModels", e.to_string()))?;
        let models: Vec<ModelBrief> = listed
            .into_iter()
            .map(|m| ModelBrief { id: m.id, owned_by: m.owned_by, created: m.created.map(|c| c as i64) })
            .collect();
        let cached = serde_json::to_value(&models).unwrap_or(Value::Null);
        self.list_cache.lock().unwrap().insert(cache_key, cached);
        eprintln!("[aimux-llm] listModels provider={provider} models={}", models.len());
        Ok(models)
    }
}
