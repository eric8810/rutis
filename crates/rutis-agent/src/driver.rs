//! AgentDriver——循环本体 + `AgentDriverPlugin` 装配(设计 §三.4/§三.5)。
//!
//! 循环写在 `run_loop` 内部:感知(`session.messages()`,前置
//! `agent/pre-step` waterfall 可改写/拒绝)→ 思考(`llm.do_stream`,流式)
//! → 行动(工具三段 `tools/pre-execute` 门控 → 执行 → `tools/post-execute`
//! 结果决策)→ 观察(增量 emit 到 `agent/*` 事件广播 + 回写 session),
//! 逐步检查取消。session 是唯一事实源;过程增量经事件广播,不独占
//! stream——`followup` 只触发 turn + 回传终态。

use std::sync::{Arc, Mutex};

use aimux_core::content::ContentPart;
use aimux_core::error::AiMuxError;
use aimux_core::language_model::LanguageModel;
use aimux_core::language_model_message::convert_to_language_model_prompt;
use aimux_core::message::{MessageContent, ModelMessage, Role};
use aimux_core::options::CallOptions;
use aimux_core::stream_part::StreamPart;
use aimux_core::tool::ToolCall;
use futures::StreamExt;
use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, TypeKey};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, AgentError, AgentStatus, SessionSnapshot, StatusCell};
use crate::events::{
    AgentPreStep, AgentStepEvent, AgentTextDelta, AgentToolCall, AgentToolResult, AgentTurnEnd,
    PreStepDecision, ToolPostExecute, ToolPreExecute,
};
use crate::session::{Session, SessionId};
use crate::tools::{ToolOutput, ToolRegistry};
use crate::{agent_key, tools_key};

/// llm 服务键(aimux `LanguageModel` 直接 provide,无插件空壳)。
pub fn llm_key() -> TypeKey {
    TypeKey::of::<dyn LanguageModel>()
}

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
    /// system prompt(minimal mode persona 等);None = 无 system 消息。
    system_prompt: Option<String>,
}

/// `tools/pre-execute` 终态续延:放行。
fn pre_execute_default<'a>(
    _ctx: &'a Ctx,
    _e: &'a ToolPreExecute,
) -> BoxFuture<'a, Result<Option<String>, CordisError>> {
    Box::pin(async { Ok(None) })
}

/// `tools/post-execute` 终态续延:原样 accept。
fn post_execute_default<'a>(
    _ctx: &'a Ctx,
    e: &'a ToolPostExecute,
) -> BoxFuture<'a, Result<ToolOutput, CordisError>> {
    Box::pin(async {
        Ok(ToolOutput {
            ok: e.ok,
            output: e.output.clone(),
        })
    })
}

/// `agent/pre-step` 终态续延:原样放行。
fn pre_step_default<'a>(
    _ctx: &'a Ctx,
    e: &'a AgentPreStep,
) -> BoxFuture<'a, Result<PreStepDecision, CordisError>> {
    Box::pin(async { Ok(Ok((e.prompt.clone(), e.tools.clone()))) })
}

impl AgentDriver {
    pub(crate) fn new(
        llm: Arc<dyn LanguageModel>,
        tools: Arc<ToolRegistry>,
        ctx: Ctx,
        max_steps: usize,
        system_prompt: Option<String>,
    ) -> Self {
        Self {
            llm,
            tools,
            ctx,
            session: Mutex::new(Session::new()),
            status: StatusCell::idle(),
            cancel: Mutex::new(CancellationToken::new()),
            max_steps,
            system_prompt,
        }
    }

    fn fresh_turn_token(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.cancel.lock().unwrap() = token.clone();
        token
    }

    fn emit_delta(&self, session: SessionId, step: usize, delta: String) {
        self.ctx.events().emit(
            &self.ctx,
            Arc::new(AgentTextDelta {
                session,
                step,
                delta,
            }),
        );
    }

    fn emit_step(&self, session: SessionId, step: usize, content: Option<String>, calls: usize) {
        self.ctx.events().emit(
            &self.ctx,
            Arc::new(AgentStepEvent {
                session,
                step,
                content,
                tool_calls: calls,
            }),
        );
    }

    fn emit_tool_call(&self, session: SessionId, name: &str, args: &serde_json::Value) {
        self.ctx.events().emit(
            &self.ctx,
            Arc::new(AgentToolCall {
                session,
                name: name.to_string(),
                args: args.clone(),
            }),
        );
    }

    fn emit_tool_result(&self, session: SessionId, name: &str, out: &ToolOutput) {
        self.ctx.events().emit(
            &self.ctx,
            Arc::new(AgentToolResult {
                session,
                name: name.to_string(),
                ok: out.ok,
                output: out.output.clone(),
            }),
        );
    }

    fn emit_turn_end(&self, session: SessionId, result: &Result<String, AgentError>) {
        self.ctx.events().emit(
            &self.ctx,
            Arc::new(AgentTurnEnd {
                session,
                ok: result.is_ok(),
                error: match result {
                    Ok(_) => String::new(),
                    Err(e) => e.to_string(),
                },
            }),
        );
    }

    /// 工具三段管线(设计 §四.1):`tools/pre-execute` 门控 → 执行 →
    /// `tools/post-execute` 结果决策。失败(含拒绝)都转模型可见的
    /// `error: ...` 结果回喂,循环继续。
    async fn run_tool(
        &self,
        session: SessionId,
        call: &ToolCall,
        token: &CancellationToken,
    ) -> ToolOutput {
        self.emit_tool_call(session, &call.tool_name, &call.input);

        // ① tools/pre-execute:执行前门控(拒绝或放行;默认放行;
        //    管线自身失败按拒绝处理——门控 fail closed)
        let gate: Option<String> = self
            .ctx
            .events()
            .waterfall(
                &self.ctx,
                &ToolPreExecute {
                    session,
                    call: call.clone(),
                },
                pre_execute_default,
            )
            .await
            .unwrap_or_else(|e| Some(e.to_string()));
        if let Some(reason) = gate {
            return ToolOutput::err(format!("error: tool execution rejected: {reason}"));
        }

        // ② 执行:ToolRegistry 内置 panic 任务边界与取消(评审 #13/P2)
        let out = self.tools.execute(call, token).await;

        // ③ tools/post-execute:结果决策(accept/replace;失败也到这,
        //    默认原样 accept)
        let out = self
            .ctx
            .events()
            .waterfall(
                &self.ctx,
                &ToolPostExecute {
                    session,
                    call: call.clone(),
                    ok: out.ok,
                    output: out.output,
                },
                post_execute_default,
            )
            .await
            .unwrap_or_else(|e| ToolOutput::err(format!("error: {e}")));

        self.emit_tool_result(session, &call.tool_name, &out);
        out
    }

    /// 一个 turn 的完整循环;过程增量经事件广播,返回终态。
    async fn run_loop(&self, token: &CancellationToken) -> Result<String, AgentError> {
        let session = self.id();
        let mut step = 0usize;
        loop {
            if token.is_cancelled() {
                return Err(AgentError::Stopped);
            }
            if step >= self.max_steps {
                return Err(AgentError::MaxSteps(self.max_steps));
            }
            step += 1;

            // 思考:从 session 取全量 history(persona 经 instructions 前置)
            let prompt = convert_to_language_model_prompt(
                self.session.lock().unwrap().messages(),
                self.system_prompt.as_deref(),
            );
            let tools = self.tools.schemas();

            // ── agent/pre-step waterfall:改写/拒绝进入这步(默认原样)──
            let (prompt, tools) = self
                .ctx
                .events()
                .waterfall(
                    &self.ctx,
                    &AgentPreStep {
                        session,
                        step,
                        prompt: prompt.clone(),
                        tools: tools.clone(),
                    },
                    pre_step_default,
                )
                .await
                .map_err(|e| AgentError::Pipeline(e.to_string()))?
                .map_err(AgentError::Pipeline)?;

            let mut result = self
                .llm
                .do_stream(&CallOptions {
                    prompt,
                    tools: Some(tools),
                    ..CallOptions::default()
                })
                .await
                .map_err(|e| AgentError::Llm(e.to_string()))?;

            // 观察:逐块收 TextDelta emit 广播,同时累积 assistant 内容
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
                        self.emit_delta(session, step, delta);
                    }
                    Chunk::Part(Ok(StreamPart::ToolCall {
                        tool_call_id,
                        tool_name,
                        input,
                        ..
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
                return Err(e);
            }

            // assistant 回写 session(session 是事实源,事件是广播)
            self.session
                .lock()
                .unwrap()
                .push(assistant_message(&text, &calls));
            self.emit_step(
                session,
                step,
                if text.is_empty() {
                    None
                } else {
                    Some(text.clone())
                },
                calls.len(),
            );

            if calls.is_empty() {
                return Ok(text); // 终答
            }

            // 行动 + 观察:工具三段管线,失败回喂,循环继续
            for call in calls {
                let out = self.run_tool(session, &call, token).await;
                self.session
                    .lock()
                    .unwrap()
                    .push(tool_result_message(&call, &out));
            }
        }
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

/// 流消费期间 select 的结果。
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

    fn followup<'a>(&'a self, input: &'a str) -> BoxFuture<'a, Result<String, AgentError>> {
        Box::pin(async move {
            // 感知起点:用户消息进 session,turn 用全新取消令牌
            self.session.lock().unwrap().push(ModelMessage::user(input));
            let token = self.fresh_turn_token();
            self.status.set(AgentStatus::Running);
            let out = self.run_loop(&token).await;
            self.status.set(AgentStatus::Idle);
            self.emit_turn_end(self.id(), &out);
            out
        })
    }
}

/// agent driver 插件:`injects = [llm, tools]` 双门控,就绪后提供
/// `dyn Agent` 服务;fiber 卸载经 `ctx.cancelled()` watcher 级联取消
/// 当前 turn(session 保留,重载即新 driver 新 session)。
pub struct AgentDriverPlugin {
    max_steps: usize,
    system_prompt: Option<String>,
    inject_keys: Vec<TypeKey>,
}

impl AgentDriverPlugin {
    pub fn new(max_steps: usize) -> Self {
        Self {
            max_steps,
            system_prompt: None,
            inject_keys: vec![llm_key(), tools_key()],
        }
    }

    /// 静态 system prompt(minimal mode persona);每步作为 instructions 前置。
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
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
            let driver = Arc::new(AgentDriver::new(
                llm,
                tools,
                ctx.clone(),
                self.max_steps,
                self.system_prompt.clone(),
            ));
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
