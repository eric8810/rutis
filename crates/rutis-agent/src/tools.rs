//! 工具集——`ToolRegistry` 服务 + `ToolsPlugin`(设计 §三.5)。
//!
//! schema 直接用 aimux [`FunctionTool`](进 `CallOptions.tools`);
//! runner 失败转 `error: ...` 文本回喂模型,panic 任务边界兜底,不崩循环。

use std::collections::HashMap;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use aimux_core::options::Tool;
use aimux_core::tool::{FunctionTool, ToolCall};
use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, TypeKey};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// tools 服务键。
pub fn tools_key() -> TypeKey {
    TypeKey::of::<ToolRegistry>()
}

/// 一次工具执行的输出(失败已转为模型可见的 `error: ...` 文本)。
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub ok: bool,
    pub output: String,
}

/// 声明式工具:aimux `FunctionTool`(schema,直接进 `CallOptions.tools`)
/// 加异步 runner。runner 收参数对象,返回值字符串化进 tool 结果消息
/// (`Value::String` 原样,其余 JSON 序列化)。
#[derive(Clone)]
pub struct ToolDef {
    pub tool: FunctionTool,
    pub run: Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>,
}

impl ToolDef {
    pub fn new<F, Fut>(name: &str, description: &str, parameters: Value, run: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        let tool = FunctionTool::new(name, parameters).with_description(description);
        Self::from_function_tool(tool, run)
    }

    pub fn from_function_tool<F, Fut>(tool: FunctionTool, run: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        Self {
            tool,
            run: Arc::new(move |args: Value| Box::pin(run(args))),
        }
    }

    pub fn name(&self) -> &str {
        &self.tool.name
    }
}

/// 工具注册表服务:统一注册、schema 汇入 prompt、按名执行。
/// 真装配单元——由 [`ToolsPlugin`] 提供,fiber 管生命周期,可热替换。
pub struct ToolRegistry {
    tools: HashMap<String, ToolDef>,
    handle: tokio::runtime::Handle,
}

impl ToolRegistry {
    pub(crate) fn new(handle: tokio::runtime::Handle, defs: Vec<ToolDef>) -> Self {
        Self {
            tools: defs.into_iter().map(|d| (d.tool.name.clone(), d)).collect(),
            handle,
        }
    }

    /// 每步交给模型的工具 schema(aimux `Tool`,直接进 `CallOptions.tools`)。
    pub fn schemas(&self) -> Vec<Tool> {
        self.tools
            .values()
            .map(|d| Tool::Function(d.tool.clone()))
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.tools.get(name)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 执行一次工具调用;失败转为模型可见的 `error: ...` 结果,不崩循环。
    /// 工具执行在任务边界:runner 创建或 poll 中的 panic 同样转为模型
    /// 可见错误(评审 #13,对齐 python `except Exception` 兜底)。
    /// `cancel` 在工具执行期间生效(取消 → `error: cancelled` 回喂,
    /// 循环在下一边界以 `Stopped` 收尾)。
    pub async fn execute(&self, call: &ToolCall, cancel: &CancellationToken) -> ToolOutput {
        let Some(def) = self.tools.get(&call.tool_name) else {
            return ToolOutput::err(format!("error: unknown tool '{}'", call.tool_name));
        };
        let run = def.run.clone();
        let input = call.input.clone();
        let fut = match catch_unwind(AssertUnwindSafe(move || run(input))) {
            Ok(fut) => fut,
            Err(p) => return ToolOutput::err(format!("error: {}", panic_message(&p))),
        };
        let join = self.handle.spawn(fut);
        let outcome = tokio::select! {
            _ = cancel.cancelled() => {
                return ToolOutput::err("error: tool execution cancelled".to_string())
            }
            out = join => out,
        };
        match outcome {
            Ok(Ok(value)) => ToolOutput {
                ok: true,
                output: match value {
                    Value::String(s) => s,
                    other => other.to_string(),
                },
            },
            Ok(Err(e)) => ToolOutput::err(format!("error: {e}")),
            // 取消不是 panic:into_panic 在取消场景会二次 panic(评审 P2)
            Err(join_err) => ToolOutput::err(if join_err.is_panic() {
                format!("error: {}", panic_message(&join_err.into_panic()))
            } else {
                "error: tool task cancelled".to_string()
            }),
        }
    }
}

impl ToolOutput {
    fn err(output: String) -> Self {
        Self { ok: false, output }
    }
}

/// 任务边界捕获的 panic 转消息(与核心 panic_error 同构,crate 私有)。
fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "tool panicked".to_string()
    }
}

/// 工具集插件:装配 `ToolRegistry` 服务(统一注册 / 门控 / 可热替换)。
pub struct ToolsPlugin {
    defs: Vec<ToolDef>,
}

impl ToolsPlugin {
    pub fn new(defs: Vec<ToolDef>) -> Self {
        Self { defs }
    }
}

impl Plugin for ToolsPlugin {
    fn name(&self) -> &str {
        "tools"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let registry = Arc::new(ToolRegistry::new(ctx.handle().clone(), self.defs.clone()));
            ctx.provide_as(tools_key(), registry)?;
            Ok(Effect::Done)
        })
    }
}
