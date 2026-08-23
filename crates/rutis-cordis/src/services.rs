//! 通用服务分发(决策文档 2026-08-23 v2 对象 C):svc/call 按名查注册表,
//! 请求/应答纯 JSON 透传,流式应答以 `svc/part` ntf(按调用 id 关联)
//! 回传。**零业务**:不认识任何具体服务;能力集(hello 的 services)由
//! 注册表推导。

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use futures::{Stream, StreamExt};
use serde_json::{json, Value};

use crate::rpc::{Bridge, InboundHooks, RemoteError};

/// 一次服务调用的应答:单值,或流(part 逐个回传,调用 res 收尾)。
pub enum ServiceReply {
    Value(Value),
    Stream(Pin<Box<dyn Stream<Item = Result<Value, RemoteError>> + Send>>),
}

/// 过线服务:名 + JSON 调用面。实现方持有全部业务;本 crate 只透传。
#[async_trait::async_trait]
pub trait CordisService: Send + Sync {
    fn name(&self) -> &str;
    async fn call(&self, method: &str, params: Value) -> Result<ServiceReply, RemoteError>;
}

/// 注册表驱动的 svc/call 分发器。
pub struct ServiceDispatch {
    services: HashMap<String, Arc<dyn CordisService>>,
    bridge: OnceLock<Bridge>,
}

impl ServiceDispatch {
    pub fn new(services: Vec<Arc<dyn CordisService>>) -> Arc<Self> {
        Arc::new(Self {
            services: services.into_iter().map(|s| (s.name().to_owned(), s)).collect(),
            bridge: OnceLock::new(),
        })
    }

    /// hello 能力集的 services 清单(注册了什么声明什么)。
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.services.keys().cloned().collect();
        names.sort();
        names
    }

    /// `Bridge::start` 之后注入句柄——流式应答需要它发 part ntf。
    pub fn attach(&self, bridge: Bridge) {
        let _ = self.bridge.set(bridge);
    }

    /// 组装入站钩子(与事件观察等其它钩子由调用方合并)。
    pub fn hooks(self: &Arc<Self>) -> InboundHooks {
        let dispatch = Arc::clone(self);
        let mut hooks = InboundHooks::default();
        hooks.on_request = Some(Arc::new(move |id, method, params| {
            let dispatch = Arc::clone(&dispatch);
            Box::pin(async move { dispatch.dispatch(id, &method, params).await })
        }));
        hooks
    }

    async fn dispatch(&self, id: u64, method: &str, params: Value) -> Result<Value, RemoteError> {
        if method != "svc/call" {
            return Err(RemoteError {
                code: "unhandled".into(),
                message: format!("service dispatch only serves svc/call, got {method}"),
            })
        }
        let (service, op) = (params["service"].as_str(), params["method"].as_str());
        let service = service.ok_or_else(|| RemoteError {
            code: "badRequest".into(),
            message: "svc/call missing service".into(),
        })?;
        let service = self.services.get(service).ok_or_else(|| RemoteError {
            code: "noService".into(),
            message: format!("no service named {service:?} registered"),
        })?;
        let op = op.ok_or_else(|| RemoteError {
            code: "badRequest".into(),
            message: "svc/call missing method".into(),
        })?;
        let inner = params.get("params").cloned().unwrap_or_else(|| json!({}));
        let bridge = self.bridge.get().ok_or_else(|| RemoteError {
            code: "notAttached".into(),
            message: "ServiceDispatch::attach was not called after Bridge::start".into(),
        })?;
        match service.call(op, inner).await? {
            ServiceReply::Value(value) => Ok(value),
            ServiceReply::Stream(mut parts) => {
                let mut count: u64 = 0;
                while let Some(part) = parts.next().await {
                    let part = part?;
                    count += 1;
                    bridge
                        .notify("svc/part", json!({ "dispatchId": id, "part": part }))
                        .await
                        .map_err(|e| RemoteError { code: "hostGone".into(), message: e.to_string() })?;
                }
                Ok(json!({ "parts": count }))
            }
        }
    }
}
