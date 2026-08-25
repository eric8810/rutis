//! 脚本后端(验证文档 §一 单元层):按序弹出 `LlmResponse`,
//! 实现真 aimux [`LanguageModel`] 接口(`do_generate` + `do_stream`),
//! 并记录每次调用的完整 prompt 供多轮 history 断言。

use std::sync::{Arc, Mutex};

use aimux_core::error::AiMuxError;
use aimux_core::language_model::LanguageModel;
use aimux_core::language_model_message::LanguageModelPrompt;
use aimux_core::message::Role;
use aimux_core::options::{CallOptions, Tool};
use aimux_core::result::{GenerateContent, GenerateResult, StreamResult};
use aimux_core::stream_part::StreamPart;
use aimux_core::tool::ToolCall;
use aimux_core::types::{FinishReason, FinishReasonUnified, ResponseMetadata, Usage};
use async_trait::async_trait;
use serde_json::Value;

/// 测试用模型请求的一次工具调用便捷构造。
pub fn tool_call(id: impl Into<String>, name: impl Into<String>, input: Value) -> ToolCall {
    ToolCall {
        tool_call_id: id.into(),
        tool_name: name.into(),
        input,
        provider_executed: None,
        dynamic: None,
        thought_signature: None,
    }
}

/// 一次脚本响应:最终内容、请求的工具调用,或两者。
#[derive(Debug, Clone, Default)]
pub struct LlmResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// 可选:推理/思考内容(reasoning)。演示时用于产出 `StreamPart::ReasoningDelta`。
    pub reasoning: Option<String>,
}

impl LlmResponse {
    pub fn content(s: impl Into<String>) -> Self {
        Self {
            content: Some(s.into()),
            tool_calls: Vec::new(),
            reasoning: None,
        }
    }

    pub fn tool_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            content: None,
            tool_calls: calls,
            reasoning: None,
        }
    }

    /// 在已有响应上附加 reasoning(演示用),返回自身。
    pub fn with_reasoning(mut self, r: impl Into<String>) -> Self {
        self.reasoning = Some(r.into());
        self
    }
}

/// ScriptedLlm 记录的一次调用快照(测试断言模型看到了什么)。
#[derive(Debug, Clone)]
pub struct ScriptedCall {
    pub prompt: LanguageModelPrompt,
    pub tools: Vec<Tool>,
}

impl ScriptedCall {
    /// prompt 的 (role, 首段文本) 摘要,断言用。
    pub fn message_texts(&self) -> Vec<(Role, String)> {
        self.prompt
            .iter()
            .map(|m| {
                let text = m
                    .content
                    .iter()
                    .map(|p| match p {
                        aimux_core::content::ContentPart::Text { text, .. } => text.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    })
                    .collect::<String>();
                (m.role, text)
            })
            .collect()
    }
}

/// 测试用确定性后端:每次弹出一个脚本响应,并记录全部对话。
pub struct ScriptedLlm {
    responses: Mutex<Vec<LlmResponse>>,
    pub calls: Mutex<Vec<ScriptedCall>>,
}

impl ScriptedLlm {
    pub fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn record_and_pop(&self, options: &CallOptions) -> Result<LlmResponse, AiMuxError> {
        self.calls.lock().unwrap().push(ScriptedCall {
            prompt: options.prompt.clone(),
            tools: options.tools.clone().unwrap_or_default(),
        });
        let mut responses = self.responses.lock().unwrap();
        responses.pop_front_or_err()
    }
}

trait PopFront<T> {
    fn pop_front_or_err(&mut self) -> Result<T, AiMuxError>;
}

impl PopFront<LlmResponse> for Vec<LlmResponse> {
    fn pop_front_or_err(&mut self) -> Result<LlmResponse, AiMuxError> {
        if self.is_empty() {
            return Err(AiMuxError::InvalidArgument(
                "ScriptedLLM has no responses left".into(),
            ));
        }
        Ok(self.remove(0))
    }
}

fn finish_reason(response: &LlmResponse) -> FinishReason {
    FinishReason {
        unified: if response.tool_calls.is_empty() {
            FinishReasonUnified::Stop
        } else {
            FinishReasonUnified::ToolCalls
        },
        raw: None,
    }
}

/// 文本按 4 字符分块:一条内容以多个 `TextDelta` 到达,钉死流式累积路径。
fn text_chunks(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars.chunks(4).map(|c| c.iter().collect()).collect()
}

#[async_trait]
impl LanguageModel for ScriptedLlm {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn model_id(&self) -> &str {
        "scripted-model"
    }

    async fn do_generate(&self, options: &CallOptions) -> Result<GenerateResult, AiMuxError> {
        let response = self.record_and_pop(options)?;
        let mut content = Vec::new();
        if let Some(text) = &response.content {
            content.push(GenerateContent::Text {
                text: text.clone(),
                provider_metadata: None,
            });
        }
        for call in &response.tool_calls {
            content.push(GenerateContent::ToolCall {
                tool_call_id: call.tool_call_id.clone(),
                tool_name: call.tool_name.clone(),
                input: call.input.clone(),
                provider_executed: None,
                dynamic: None,
                thought_signature: None,
                provider_metadata: None,
            });
        }
        Ok(GenerateResult {
            content,
            finish_reason: finish_reason(&response),
            usage: Usage::default(),
            warnings: Vec::new(),
            provider_metadata: None,
            response: ResponseMetadata::default(),
            request_body: None,
            response_headers: None,
        })
    }

    async fn do_stream(&self, options: &CallOptions) -> Result<StreamResult, AiMuxError> {
        let response = self.record_and_pop(options)?;
        let finish = finish_reason(&response);
        let stream = async_stream::stream! {
            yield Ok(StreamPart::StreamStart { warnings: Vec::new() });
            if let Some(r) = &response.reasoning {
                yield Ok(StreamPart::ReasoningStart {
                    id: "reason-0".to_string(),
                    provider_metadata: None,
                });
                for delta in text_chunks(r) {
                    yield Ok(StreamPart::ReasoningDelta {
                        id: "reason-0".to_string(),
                        delta,
                        provider_metadata: None,
                    });
                }
                yield Ok(StreamPart::ReasoningEnd {
                    id: "reason-0".to_string(),
                    provider_metadata: None,
                });
            }
            if let Some(text) = &response.content {
                for delta in text_chunks(text) {
                    yield Ok(StreamPart::TextDelta {
                        id: "text-0".to_string(),
                        delta,
                        provider_metadata: None,
                    });
                }
            }
            for call in &response.tool_calls {
                yield Ok(StreamPart::ToolCall {
                    tool_call_id: call.tool_call_id.clone(),
                    tool_name: call.tool_name.clone(),
                    input: call.input.clone(),
                    provider_executed: None,
                    dynamic: None,
                    thought_signature: None,
                    provider_metadata: None,
                });
            }
            yield Ok(StreamPart::Finish {
                finish_reason: finish,
                usage: Usage::default(),
                provider_metadata: None,
            });
        };
        Ok(StreamResult {
            stream: Box::pin(stream),
            request_body: None,
            response_headers: None,
        })
    }
}

/// 便捷转换:任何 `LanguageModel` 装箱进 `Arc`(provide_as 直用)。
pub fn into_service(model: impl LanguageModel + 'static) -> Arc<dyn LanguageModel> {
    Arc::new(model)
}
