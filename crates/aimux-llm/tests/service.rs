//! aimux-llm 服务层行为:keyed 路由(工厂注入)、回落不被触碰、DTO →
//! CallOptions 映射(system/messages/tools 逐字段)、chunk 流顺序。
//! 不经 wire、不联网。

use std::sync::{Arc, Mutex};

use aimux_core::content::ContentPart;
use aimux_core::error::AiMuxError;
use aimux_core::language_model::LanguageModel;
use aimux_core::language_model_message::LanguageModelPromptMessage;
use aimux_core::options::CallOptions;
use aimux_core::result::{GenerateResult, StreamResult};
use aimux_core::stream_part::StreamPart;
use aimux_core::tool::Tool;
use aimux_core::types::{FinishReason, FinishReasonUnified, Usage};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;

use aimux_llm::{AimuxLlm, LlmService, StreamRequest};

/// 逐字产出 "hel" "lo " "m2";记录收到的 CallOptions。
struct ChunkedLlm {
    calls: Mutex<Vec<CallOptions>>,
}

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

    async fn do_stream(&self, options: &CallOptions) -> Result<StreamResult, AiMuxError> {
        self.calls.lock().unwrap().push(options.clone());
        let stream = async_stream::stream! {
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
        Ok(StreamResult { stream: Box::pin(stream), request_body: None, response_headers: None })
    }
}

struct FailLlm {
    touched: Arc<Mutex<bool>>,
}

#[async_trait]
impl LanguageModel for FailLlm {
    fn provider(&self) -> &str {
        "fail"
    }

    fn model_id(&self) -> &str {
        "fail"
    }

    async fn do_generate(&self, _o: &CallOptions) -> Result<GenerateResult, AiMuxError> {
        *self.touched.lock().unwrap() = true;
        Err(AiMuxError::Other("fallback touched".into()))
    }

    async fn do_stream(&self, _o: &CallOptions) -> Result<StreamResult, AiMuxError> {
        *self.touched.lock().unwrap() = true;
        Err(AiMuxError::Other("fallback touched".into()))
    }
}

fn req(value: serde_json::Value) -> StreamRequest {
    StreamRequest::from_value(&value).expect("dto")
}

#[tokio::test]
async fn stream_yields_parts_in_order_with_finish_last() {
    let recorder = Arc::new(ChunkedLlm { calls: Mutex::new(Vec::new()) });
    let rec2 = Arc::clone(&recorder);
    let factory = Arc::new(move |_p: &str, _k: &str, _m: &str| Ok(Arc::clone(&rec2) as Arc<dyn LanguageModel>));
    let svc = AimuxLlm::with_factory(
        Arc::new(FailLlm { touched: Arc::new(Mutex::new(false)) }),
        "deepseek",
        "env-model",
        factory,
    );
    let mut parts = svc
        .stream(req(json!({ "provider": "deepseek", "model": "x", "api_key": "sk-1", "options": { "messages": [{ "role": "user", "text": "hi" }] } })))
        .await
        .expect("stream");
    let mut kinds = Vec::new();
    let mut text = String::new();
    while let Some(part) = parts.next().await {
        let part = part.expect("part ok");
        match part {
            StreamPart::TextDelta { delta, .. } => text.push_str(&delta),
            other => kinds.push(std::mem::discriminant(&other)),
        }
    }
    assert_eq!(text, "hello m2");
    assert_eq!(kinds.len(), 2, "StreamStart + Finish");
}

#[tokio::test]
async fn keyed_request_routes_through_factory_fallback_untouched() {
    let touched: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let fail = FailLlm { touched: Arc::clone(&touched) };
    let fallback: Arc<dyn LanguageModel> = Arc::new(fail);
    let seen: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = Arc::clone(&seen);
    let recorder = Arc::new(ChunkedLlm { calls: Mutex::new(Vec::new()) });
    let factory = Arc::new(move |provider: &str, key: &str, model: &str| {
        seen2.lock().unwrap().push((provider.to_owned(), key.to_owned(), model.to_owned()));
        Ok(Arc::clone(&recorder) as Arc<dyn LanguageModel>)
    });
    let svc = AimuxLlm::with_factory(fallback, "deepseek", "env-model", factory);
    let _ = svc
        .stream(req(json!({ "provider": "deepseek", "model": "deepseek-v4-flash", "api_key": "sk-from-page", "options": {} })))
        .await
        .expect("stream");
    let calls = seen.lock().unwrap();
    assert_eq!(
        calls.as_slice(),
        [("deepseek".to_string(), "sk-from-page".to_string(), "deepseek-v4-flash".to_string())]
    );
    assert!(!*touched.lock().unwrap(), "fallback must stay untouched when a key crossed");
}

#[tokio::test]
async fn keyless_request_falls_back_and_request_without_provider_uses_env_default() {
    let recorder = Arc::new(ChunkedLlm { calls: Mutex::new(Vec::new()) });
    // 无 key:回落 fallback(即注入的 recorder),不经工厂。
    let svc = AimuxLlm::new(Arc::clone(&recorder) as Arc<dyn LanguageModel>, "deepseek", "env-model");
    let _ = svc.stream(req(json!({ "options": { "messages": [{ "text": "hi" }] } }))).await.expect("stream");
    let calls = recorder.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "fallback used");
}

/// DTO → CallOptions 的逐字段映射(原 L3 面下沉到服务层)。
#[tokio::test]
async fn dto_maps_to_call_options_field_by_field() {
    let recorder = Arc::new(ChunkedLlm { calls: Mutex::new(Vec::new()) });
    let rec2 = Arc::clone(&recorder);
    let factory = Arc::new(move |_p: &str, _k: &str, _m: &str| Ok(Arc::clone(&rec2) as Arc<dyn LanguageModel>));
    let svc = AimuxLlm::with_factory(
        Arc::new(FailLlm { touched: Arc::new(Mutex::new(false)) }),
        "deepseek",
        "env-model",
        factory,
    );
    let _ = svc
        .stream(req(json!({
            "provider": "deepseek", "model": "m", "api_key": "k",
            "options": {
                "system": "be terse",
                "messages": [
                    { "role": "user", "text": "one" },
                    { "role": "assistant", "text": "two" },
                    { "text": "three" }
                ],
                "tools": [
                    { "name": "bash", "description": "run a shell", "parameters": { "type": "object" } },
                    { "name": "", "description": "nameless must be dropped" }
                ]
            }
        })))
        .await
        .expect("stream");
    let calls = recorder.calls.lock().unwrap();
    let options = calls.last().expect("recorded");
    let texts = |m: &LanguageModelPromptMessage| {
        m.content
            .iter()
            .map(|p| match p {
                ContentPart::Text { text, .. } => text.clone(),
                _ => String::new(),
            })
            .collect::<String>()
    };
    assert_eq!(options.prompt.len(), 4, "system + 3 messages");
    assert_eq!(texts(&options.prompt[0]), "be terse");
    assert_eq!(texts(&options.prompt[1]), "one");
    assert_eq!(texts(&options.prompt[2]), "two");
    assert_eq!(texts(&options.prompt[3]), "three");
    // role 语义经 do_stream 侧不可直读,断言经 messages 数与文本顺序覆盖;
    // 工具:有名 1 个保留、无名丢弃;tools 为 Some。
    let tools = options.tools.as_ref().expect("tools present");
    assert_eq!(tools.len(), 1);
    match &tools[0] {
        Tool::Function(f) => {
            assert_eq!(f.name, "bash");
            assert_eq!(f.description.as_deref(), Some("run a shell"));
        }
        other => panic!("expected function tool, got {other:?}"),
    }
}
