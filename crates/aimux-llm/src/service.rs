//! llm 服务面:trait + DTO。DTO 是本服务对外声明的中性形状——调用方
//! (直接或经桥)按此构造;`serde(default)` 全开,缺省即空。

use std::pin::Pin;

use aimux_core::stream_part::StreamPart;
use futures::Stream;
use serde::Deserialize;
use serde_json::Value;

/// 服务错误:code 面向调用方分类,消息面向人。
#[derive(Debug, Clone)]
pub struct LlmServiceError {
    pub code: String,
    pub message: String,
}

impl LlmServiceError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

impl std::fmt::Display for LlmServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// stream 的 part 流(aimux `StreamPart` 原生;序列化中性)。
pub type PartStream = Pin<Box<dyn Stream<Item = Result<StreamPart, LlmServiceError>> + Send>>;

/// 模型目录条目。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ModelBrief {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
}

/// llm 服务(服务名恒为 `llm`;路由 = provider 名)。
#[async_trait::async_trait]
pub trait LlmService: Send + Sync {
    /// 一次流式调用。凭据在请求内(`api_key`),不在环境——环境只是
    /// 无凭据请求的兜底构造面。
    async fn stream(&self, req: StreamRequest) -> Result<PartStream, LlmServiceError>;

    /// 某路由(= aimux provider 名)的模型目录。
    async fn list_models(&self, provider: &str, api_key: Option<&str>)
        -> Result<Vec<ModelBrief>, LlmServiceError>;
}

/// `stream` 的中性 DTO。`provider`/`model` 缺省回落宿主构造时的兜底;
/// `api_key` 有值即走 keyed 工厂,无值走兜底模型。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StreamRequest {
    pub provider: Option<String>,
    pub model: Option<String>,
    /// 凭据随请求过线(中性契约用 camelCase,与 wire 侧一致)。
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    pub options: PromptSpec,
}

/// prompt 的中性形状:system + 逐消息(role, 纯文本) + 工具 schema。
/// 非文本块(图片/文件)是长尾,形状留位但不进入本 DTO。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PromptSpec {
    pub system: Option<String>,
    pub messages: Vec<MessageSpec>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MessageSpec {
    pub role: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ToolSpec {
    pub name: String,
    pub description: Option<String>,
    pub parameters: Option<Value>,
}

impl StreamRequest {
    pub fn from_value(value: &Value) -> Result<Self, LlmServiceError> {
        serde_json::from_value(value.clone())
            .map_err(|e| LlmServiceError::new("badRequest", format!("stream request malformed: {e}")))
    }
}
