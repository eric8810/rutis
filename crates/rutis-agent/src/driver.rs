//! AgentDriver——循环本体 + `AgentDriverPlugin` 装配(设计 §三.4/§三.5)。
//!
//! 循环写在 `followup` 内部:感知(`session.messages()`)→ 思考
//! (`llm.do_stream`,流式)→ 行动(`tools.execute`)→ 观察(回写
//! session + `agent/*` 事件),逐步检查取消。session 是唯一事实源:
//! 流式 TextDelta 一边转发前端、一边累积进 assistant 消息回写 session。

use std::sync::{Arc, Mutex};

use aimux_core::content::ContentPart;
use aimux_core::error::AiMuxError;
use aimux_core::language_model::LanguageModel;
use aimux_core::language_model_message::convert_to_language_model_prompt;
use aimux_core::message::{MessageContent, ModelMessage, Role};
use aimux_core::options::CallOptions;
use aimux_core::stream_part::StreamPart;
use aimux_core::tool::ToolCall;
use futures::stream::BoxStream;
use futures::StreamExt;
use rutis::{BoxFuture, CordisError, Ctx, Effect, Event, Plugin, TypeKey};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, AgentError, AgentStatus, SessionSnapshot, StatusCell, TurnEvent};
use crate::session::{Session, SessionId};
use crate::tools::{ToolOutput, ToolRegistry};
use crate::{agent_key, tools_key};

/// llm 服务键(aimux `LanguageModel` 直接 provide,无插件空壳)。
pub fn llm_key() -> TypeKey {
    TypeKey::of::<dyn LanguageModel>()
}

// ── 事件(总线可观测性;监听器归注册方 fiber 所有,D28)──────────────

/// 每步模型响应后发布:步号、内容、工具调用数。
#[derive(Debug, Clone)]
pub struct AgentStepEvent {
    pub step: usize,
    pub content: Option<String>,
    pub tool_calls: usize,
}

impl Event for AgentStepEvent {
    const NAME: &'static str = "agent/step";
    type Value = ();
}

/// 每次工具调用后发布:名称、参数、成败、输出。
#[derive(Debug, Clone)]
pub struct AgentToolEvent {
    pub name: String,
    pub arguments: serde_json::Value,
    pub ok: bool,
    pub output: String,
}

impl Event for AgentToolEvent {
    const NAME: &'static str = "agent/tool";
    type Value = ();
}

// ── driver 本体 ────────────────────────────────────────────────────

/// agent 循环 driver:实现 [`Agent`] 接口,由 [`AgentDriverPlugin`] 装配。
///
/// `cancel` 是 turn 级令牌:每次 `followup` 换新,`Agent::cancel` 与
/// fiber 卸载(watcher)都打在"当前 turn"上;session(history)不受影响。
pub struct AgentDriver {
    llm: Arc<dyn LanguageModel>,
    tools: Arc<ToolRegistry>,
    ctx: Ctx,
    session: Mutex<Session>,
    status: StatusCell,
    cancel: Mutex<CancellationToken>,
    max_steps: usize,
}

impl AgentDriver {
    pub(crate) fn new(
        llm: Arc<dyn LanguageModel>,
        tools: Arc<ToolRegistry>,
        ctx: Ctx,
        max_steps: usize,
    ) -> Self {
        Self {
            llm,
            tools,
            ctx,
            session: Mutex::new(Session::new()),
            status: StatusCell::idle(),
            cancel: Mutex::new(CancellationToken::new()),
            max_steps,
        }
    }

    fn fresh_turn_token(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.cancel.lock().unwrap() = token.clone();
        token
    }

    fn emit_step(&self, step: usize, content: &str, tool_calls: usize) {
        self.ctx.events().emit(
            &self.ctx,
            Arc::new(AgentStepEvent {
                step,
                content: if content.is_empty() {
                    None
                } else {
                    Some(content.to_string())
                },
                tool_calls,
            }),
        );
    }

    fn emit_tool(&self, call: &ToolCall, out: &ToolOutput) {
        self.ctx.events().emit(
            &self.ctx,
            Arc::new(AgentToolEvent {
                name: call.tool_name.clone(),
                arguments: call.input.clone(),
                ok: out.ok,
                output: out.output.clone(),
            }),
        );
    }
}

/// assistant 消息回写:文本(可空)+ 工具调用 parts。
fn assistant_message(text: &str, calls: &[ToolCall]) -> ModelMessage {
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(ContentPart::text(text));
    }
    for c in calls {
        parts.push(ContentPart::tool_call(
            c.tool_call_id.clone(),
            c.tool_name.clone(),
            c.input.clone(),
        ));
    }
    ModelMessage {
        role: Role::Assistant,
        content: MessageContent::Parts(parts),
    }
}

/// 工具结果消息回喂(role=tool)。
fn tool_result_message(call: &ToolCall, out: &ToolOutput) -> ModelMessage {
    ModelMessage {
        role: Role::Tool,
        content: MessageContent::Parts(vec![ContentPart::ToolResult {
            tool_call_id: call.tool_call_id.clone(),
            result: serde_json::Value::String(out.output.clone()),
            tool_name: Some(call.tool_name.clone()),
            is_error: Some(!out.ok),
            preliminary: None,
            dynamic: None,
            provider_options: None,
        }]),
    }
}

/// 流消费期间 select 的结果(cancel 不能驻留在 select 臂内 yield)。
/// select 瞬时值,无热循环驻留,大变体不值得装箱分配。
#[allow(clippy::large_enum_variant)]
enum Chunk {
    Cancelled,
    End,
    Part(Result<StreamPart, AiMuxError>),
}

impl Agent for AgentDriver {
    fn id(&self) -> SessionId {
        self.session.lock().unwrap().id()
    }

    fn status(&self) -> AgentStatus {
        self.status.get()
    }

    fn session(&self) -> SessionSnapshot {
        let session = self.session.lock().unwrap();
        SessionSnapshot::new(session.id(), session.messages())
    }

    fn cancel(&self) {
        self.cancel.lock().unwrap().cancel();
    }

    fn followup<'a>(&'a self, input: &'a str) -> BoxStream<'a, TurnEvent> {
        Box::pin(async_stream::stream! {
            // 感知起点:用户消息进 session,turn 用全新取消令牌
            self.session.lock().unwrap().push(ModelMessage::user(input));
            let token = self.fresh_turn_token();
            self.status.set(AgentStatus::Running);

            let mut step = 0usize;
            let outcome = loop {
                if token.is_cancelled() {
                    break Err(AgentError::Stopped);
                }
                if step >= self.max_steps {
                    break Err(AgentError::MaxSteps(self.max_steps));
                }
                step += 1;

                // 思考:从 session 取全量 history,流式调 aimux
                let prompt =
                    convert_to_language_model_prompt(self.session.lock().unwrap().messages(), None);
                let mut result = match self.llm.do_stream(&CallOptions {
                    prompt,
                    tools: Some(self.tools.schemas()),
                    ..CallOptions::default()
                }).await {
                    Ok(r) => r,
                    Err(e) => break Err(AgentError::Llm(e.to_string())),
                };

                // 观察:逐块收 TextDelta 转发前端,同时累积 assistant 内容
                let mut text = String::new();
                let mut calls: Vec<ToolCall> = Vec::new();
                let mut failure: Option<AgentError> = None;
                loop {
                    let chunk = tokio::select! {
                        _ = token.cancelled() => Chunk::Cancelled,
                        part = result.stream.next() => match part {
                            None => Chunk::End,
                            Some(p) => Chunk::Part(p),
                        },
                    };
                    match chunk {
                        Chunk::Cancelled => {
                            failure = Some(AgentError::Stopped);
                            break;
                        }
                        Chunk::End => break,
                        Chunk::Part(Ok(StreamPart::TextDelta { delta, .. })) => {
                            text.push_str(&delta);
                            yield TurnEvent::TextDelta(delta);
                        }
                        Chunk::Part(Ok(StreamPart::ToolCall {
                            tool_call_id, tool_name, input, ..
                        })) => {
                            calls.push(ToolCall {
                                tool_call_id,
                                tool_name,
                                input,
                                provider_executed: None,
                                dynamic: None,
                                thought_signature: None,
                            });
                        }
                        Chunk::Part(Ok(StreamPart::Error { error })) => {
                            failure = Some(AgentError::Llm(error.to_string()));
                            break;
                        }
                        Chunk::Part(Ok(_)) => {}
                        Chunk::Part(Err(e)) => {
                            failure = Some(AgentError::Llm(e.to_string()));
                            break;
                        }
                    }
                }
                if let Some(e) = failure {
                    break Err(e);
                }

                // assistant 回写 session(流是视图,session 是事实源)
                self.session
                    .lock()
                    .unwrap()
                    .push(assistant_message(&text, &calls));
                self.emit_step(step, &text, calls.len());

                if calls.is_empty() {
                    break Ok(text); // 终答
                }

                // 行动 + 观察:执行工具,失败回喂,panic 任务边界
                for call in calls {
                    yield TurnEvent::ToolCall {
                        name: call.tool_name.clone(),
                        args: call.input.clone(),
                    };
                    let out = self.tools.execute(&call, &token).await;
                    self.emit_tool(&call, &out);
                    yield TurnEvent::ToolResult {
                        name: call.tool_name.clone(),
                        ok: out.ok,
                        output: out.output.clone(),
                    };
                    self.session
                        .lock()
                        .unwrap()
                        .push(tool_result_message(&call, &out));
                }
            };

            self.status.set(AgentStatus::Idle);
            yield TurnEvent::Done(outcome);
        })
    }
}

/// agent driver 插件:`injects = [llm, tools]` 双门控,就绪后提供
/// `dyn Agent` 服务;fiber 卸载经 `ctx.cancelled()` watcher 级联取消
/// 当前 turn(session 保留,重载即新 driver 新 session)。
pub struct AgentDriverPlugin {
    max_steps: usize,
    inject_keys: Vec<TypeKey>,
}

impl AgentDriverPlugin {
    pub fn new(max_steps: usize) -> Self {
        Self {
            max_steps,
            inject_keys: vec![llm_key(), tools_key()],
        }
    }
}

impl Default for AgentDriverPlugin {
    fn default() -> Self {
        Self::new(16)
    }
}

impl Plugin for AgentDriverPlugin {
    fn name(&self) -> &str {
        "agent-driver"
    }

    fn injects(&self) -> &[TypeKey] {
        &self.inject_keys
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            // 双门控保证此处 llm + tools 必在
            let llm = ctx
                .get_as::<dyn LanguageModel>(llm_key())
                .ok_or_else(|| CordisError::InjectUnsatisfied(vec!["llm".to_string()]))?;
            let tools = ctx
                .get_as::<ToolRegistry>(tools_key())
                .ok_or_else(|| CordisError::InjectUnsatisfied(vec!["tools".to_string()]))?;
            let driver = Arc::new(AgentDriver::new(llm, tools, ctx.clone(), self.max_steps));
            ctx.provide_as::<dyn Agent>(agent_key(), driver.clone())?;

            // fiber 卸载 → cancel 当前 turn(driver 内 token 级联停止)
            let watcher_ctx = ctx.clone();
            ctx.effect(move || {
                let watcher_ctx = watcher_ctx.clone();
                let driver = driver.clone();
                let handle = watcher_ctx.handle().clone();
                handle.spawn(async move {
                    watcher_ctx.cancelled().await;
                    driver.cancel();
                });
                Effect::Done
            })?;
            Ok(Effect::Done)
        })
    }
}
