//! rutis-dsh:入口与组合根(决策:docs/decision-aimux-llm-plugin-2026-08-23.md
//! v2 对象 B/D)。
//!
//! 层次纪律(v2):本 crate **零 dsh 知识**——不认识 dsh 的任何形状;它只做
//! 两件事:
//! 1. [`LlmFace`]:把 [`aimux_llm::LlmService`](aimux 原生形状)适配为
//!    [`rutis_cordis::CordisService`](wire 的 JSON 面)——纯形状转换,
//!    无业务;唯一懂 dsh 的地方在 TS 侧 rutis-bridge 插件。
//! 2. runner bin(见 `src/bin/rutis_dsh.rs`):起 rutis 运行时,装
//!    aimux-llm 插件,把注册表中的服务经业务无关桥供给宿主。
//!
//! 原 M1 的 dsh 节校验/dshSemver pin 已随 v2 决策删除(无消费者)。

use std::sync::Arc;

use aimux_llm::{LlmService, LlmServiceError, StreamRequest};
use futures::StreamExt;
use rutis_cordis::{CordisService, RemoteError, ServiceReply};
use serde_json::{json, Value};

/// 把 `LlmService`(aimux 形状)接到 wire 的 JSON 面。服务名恒 `llm`;
/// 方法名与 DTO 都是 aimux-llm 声明的中性形状,本层只转换不解释。
pub struct LlmFace {
    service: Arc<dyn LlmService>,
}

impl LlmFace {
    pub fn new(service: Arc<dyn LlmService>) -> Arc<Self> {
        Arc::new(Self { service })
    }
}

fn remote(e: LlmServiceError) -> RemoteError {
    RemoteError { code: e.code, message: e.message }
}

#[async_trait::async_trait]
impl CordisService for LlmFace {
    fn name(&self) -> &str {
        "llm"
    }

    async fn call(&self, method: &str, params: Value) -> Result<ServiceReply, RemoteError> {
        match method {
            "stream" => {
                let req = StreamRequest::from_value(&params).map_err(remote)?;
                let parts = self.service.stream(req).await.map_err(remote)?;
                Ok(ServiceReply::Stream(Box::pin(parts.map(|part| {
                    part.map_err(remote)
                        .and_then(|p| serde_json::to_value(&p).map_err(|e| RemoteError { code: "encode".into(), message: e.to_string() }))
                }))))
            }
            "listModels" => {
                let provider = params
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("llm")
                    .to_owned();
                let api_key = params
                    .get("apiKey")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let models = self
                    .service
                    .list_models(&provider, api_key.as_deref())
                    .await
                    .map_err(remote)?;
                let models: Vec<Value> = models
                    .into_iter()
                    .map(|m| json!({ "id": m.id, "owned_by": m.owned_by, "created": m.created }))
                    .collect();
                Ok(ServiceReply::Value(json!({ "models": models })))
            }
            other => Err(RemoteError {
                code: "unhandled".into(),
                message: format!("llm face only serves stream/listModels, got {other:?}"),
            }),
        }
    }
}
