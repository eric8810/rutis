//! AgentDriver——循环本体 + `AgentDriverPlugin` 装配(设计 §三.4/§三.5)。
//!
//! 循环写在 `run_loop` 内部:感知(`session.messages()`,前置
//! `agent/pre-step` waterfall 可改写/拒绝)→ 思考(`llm.do_stream`,流式)
//! → 行动(工具三段 `tools/pre-execute` 门控 → 执行 → `tools/post-execute`
//! 结果决策)→ 观察(增量 emit 到 `agent/*` 事件广播 + 回写 session),
//! 逐步检查取消。session 是唯一事实源;过程增量经事件广播,不独占
//! stream——`followup` 只触发 turn + 回传终态。

use std::path::PathBuf;
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
    AgentPreStep, AgentReasoning, AgentStepEvent, AgentTextDelta, AgentToolCall, AgentToolResult,
    AgentTurnEnd,
    PreStepDecision, ToolPostExecute, ToolPreExecute,
};
use crate::session::{Session, SessionId};
use crate::tools::{ToolOutput, ToolRegistry};
use crate::{agent_key, tools_key};

/// llm 服务键(aimux `LanguageModel` 直接 provide,无插件空壳)。
pub fn llm_key() -> TypeKey {
    TypeKey::of::<dyn LanguageModel>()
}

/// session 持久化路径服务键(`AgentDriverPlugin` 在 apply 时 provide;
/// 供 `self_persist` 等自我控制工具经服务注册表读取,不污染 `Agent` trait)。
pub fn session_path_key() -> TypeKey {
    TypeKey::of::<Option<PathBuf>>()
}

/// 默认 session 持久化路径约定:`<cwd>/.rutis/session.json`。
///
/// 所有"人用"宿主(CLI / examples)应统一用它,使 agent 跨进程/跨会话
/// 自动恢复模型历史(`Session::restore`)。`AgentDriverPlugin::new()`
/// 保持"默认关闭"(None)——框架语义不变,由宿主显式启用。
pub fn default_session_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".rutis")
        .join("session.json")
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
    /// Mutex:运行中可更新(self_persona 工具 / update_persona 方法)。
    system_prompt: Mutex<Option<String>>,
    /// 可选 session 持久化路径;None = 不持久化(默认,现状不变)。
    session_path: Option<PathBuf>,
    /// turn 互斥:保证同一时刻只有一个 followup 在改 session。
    /// 并发 turn(TUI 提交 vs SelfDriven 自主续跑)会在同一把锁上排队,
    /// 避免 user/assistant/tool 的 push 交错导致历史乱序(悬空 tool_call)。
    turn_lock: tokio::sync::Mutex<()>,
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
        session_path: Option<PathBuf>,
    ) -> Self {
        let session = match &session_path {
            Some(p) => Session::restore(p), // 失败静默降级为新 Session
            None => Session::new(),
        };
        Self {
            llm,
            tools,
            ctx,
            session: Mutex::new(session),
            status: StatusCell::idle(),
            cancel: Mutex::new(CancellationToken::new()),
            max_steps,
            system_prompt: Mutex::new(system_prompt),
            session_path,
            turn_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 落盘当前 session;未配置路径 = 静默 no-op。
    /// 错误路由 ErrorSink(turn 不阻断,但可观测)。
    fn persist_session(&self) {
        let Some(path) = self.session_path.clone() else {
            return;
        };
        let result = {
            let session = self.session.lock().unwrap();
            session.persist(&path)
        };
        if let Err(e) = result {
            let err: Box<dyn std::error::Error + Send + Sync> = format!("session persist failed: {e}").into();
            self.ctx.error_sink()(Arc::new(CordisError::PluginFailed(err)));
        }
    }

    /// 上下文超限兜底(Fix 3):保留最近 `keep` 条消息,早期历史折叠进
    /// summary。摘要优先沿用既有 summary(跨次 compact 累加信息),否则用
    /// 一句标记;compact() 内部会自动落盘。有界重试在 run_loop 里控制。
    fn auto_compact(&self, keep: usize) {
        let summary = {
            let session = self.session.lock().unwrap();
            session
                .summary()
                .map(str::to_string)
                .unwrap_or_else(|| "（早期对话因超出模型上下文窗口被自动裁剪,细节不可恢复）".to_string())
        };
        let (before, after) = self.compact(summary, keep);
        eprintln!("[driver] auto-compact: messages {before} -> {after} (kept last {keep})");
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

    fn emit_reasoning(&self, session: SessionId, step: usize, delta: String) {
        self.ctx.events().emit(
            &self.ctx,
            Arc::new(AgentReasoning {
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
            //
            // 记忆指针:恢复的 session(generation > 1 或已有历史)在 system
            // prompt 尾部附加一段显式说明,让模型"感知"自己在继续一段历史
            // 对话而非全新会话——历史在输入里但模型不自动引用,这是
            // session 恢复"机制生效、认知不生效"的根因(见
            // docs/work/lesson-confabulation-2026-08-25.md)。
            let system = {
                let session = self.session.lock().unwrap();
                // 保持"无附加内容时传 None"的语义(不引入空 system 消息,
                // 不破坏 MockReplayModel 等对 prompt 结构的断言)。
                let mut base = self.system_prompt.lock().unwrap().clone().unwrap_or_default();
                let mut has_extra = false;
                // 1) 记忆摘要(长会话压缩后):前置到 system prompt,
                //    替代被裁剪的历史,保持长会话不退化。
                if let Some(summary) = session.summary() {
                    base.push_str(&format!(
                        "\n\n# 记忆摘要\n\n以下是你早期对话的摘要(原始消息已被压缩裁剪):\n{summary}\n"
                    ));
                    has_extra = true;
                }
                // 2) 记忆指针:仅恢复的 session(generation > 1,跨进程/代际)
                //    附加,告知模型在继续历史;全新 session 自然连续,无需提示。
                if session.id().generation() > 1 {
                    let gen = session.id().generation();
                    base.push_str(&format!(
                        "\n\n# 记忆指针\n\n\
                         你正在继续一段历史对话(第 {gen} 代,identity={})。\
                         以上是之前的完整消息,视为你的记忆。\
                         回答时引用它,不要重新询问已经问过/答过的事,不要假装失忆。",
                        session.id().identity()
                    ));
                    has_extra = true;
                }
                // 3) 待办/下一步:agent 中断后自动接续的工作指引。
                //    重启恢复后模型第一眼就看到"该做什么",自动继续,
                //    而不是问用户"我从哪开始"。
                if let Some(todo) = session.todo() {
                    base.push_str(&format!(
                        "\n\n# 待办/下一步\n\n以下是你(上一代实例)未完成的工作,继续做:\n{todo}\n"
                    ));
                    has_extra = true;
                }
                if has_extra {
                    Some(base)
                } else {
                    self.system_prompt.lock().unwrap().clone()
                }
            };
            // 每步重建 prompt:Fix 4 先用 sanitize_history 规整 session(去掉
            // 因并发/历史损坏导致的悬空 tool_call / 孤儿 tool_result),再配合
            // Fix 3 的上下文超限自动 compact + 有界重试。
            let mut ctx_retry = 0u32;
            let mut result = 'llm: loop {
                // Fix 4:规整历史(确保 assistant/tool 配对、无孤儿)后再转 prompt
                let prompt = {
                    let session = self.session.lock().unwrap();
                    let sanitized = sanitize_history(session.messages());
                    convert_to_language_model_prompt(&sanitized, system.as_deref())
                };
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

                match self
                    .llm
                    .do_stream(&CallOptions {
                        prompt,
                        tools: Some(tools),
                        ..CallOptions::default()
                    })
                    .await
                {
                    Ok(r) => break 'llm r,
                    // Fix 3:上下文超限 → 自动 compact 后重试(有界),避免拿越来
                    // 越大的 prompt 硬试、越滚越挂的死循环。
                    Err(e) => {
                        let msg = e.to_string();
                        if ctx_retry < 2 && is_context_overflow(&msg) {
                            ctx_retry += 1;
                            eprintln!("[driver] context overflow, auto-compact & retry ({ctx_retry}/2)");
                            self.auto_compact(if ctx_retry == 1 { 40 } else { 8 });
                            continue 'llm;
                        }
                        return Err(AgentError::Llm(msg));
                    }
                }
            };

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
                    Chunk::Part(Ok(StreamPart::ReasoningDelta { delta, .. })) => {
                        // reasoning 增量单独广播,不进入 assistant 文本/会话回写
                        self.emit_reasoning(session, step, delta);
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
/// 长工具输出 soft-trim(对标 grok compaction.pruning 的 soft_trim):
/// 超过 `TOOL_RESULT_KEEP` 字符的输出,裁剪为「头 + 裁剪标记 + 尾」,
/// 防止超大 tool result 永久撑爆会话上下文(长会话退化的根因之一)。
/// 只在真正超长时裁剪,正常路径原样 —— 不破坏现有行为与测试。
fn tool_result_message(call: &ToolCall, out: &ToolOutput) -> ModelMessage {
    // 保留区:前后各保留的字符(裁剪中间)。
    const TOOL_RESULT_KEEP: usize = 1500;
    let mut output = out.output.clone();
    if output.chars().count() > 2 * TOOL_RESULT_KEEP + 80 {
        let head: String = output.chars().take(TOOL_RESULT_KEEP).collect();
        let tail: String = output
            .chars()
            .rev()
            .take(TOOL_RESULT_KEEP)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let total = output.chars().count();
        output = format!(
            "{head}\n…[trimmed {} chars…]\n{tail}",
            total - 2 * TOOL_RESULT_KEEP
        );
    }
    ModelMessage {
        role: Role::Tool,
        content: MessageContent::Parts(vec![ContentPart::ToolResult {
            tool_call_id: call.tool_call_id.clone(),
            result: serde_json::Value::String(output),
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

/// 判断 LLM 错误信息是否属于"上下文超限"类(Fix 3)。
/// 启发式:误判最多触发一次多余的 compact + 重试(有界),漏判则退化为
/// 普通失败(回滚 + SelfDriven 退避),两个方向都安全。
pub(crate) fn is_context_overflow(err: &str) -> bool {
    let s = err.to_lowercase();
    [
        "context",
        "too long",
        "too many tokens",
        "exceed",
        "input length",
        "prompt is too",
        "longer than",
        "maximum context",
        "token limit",
    ]
    .iter()
    .any(|k| s.contains(k))
}

fn tool_call_ids(m: &ModelMessage) -> Vec<String> {
    match &m.content {
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolCall {
                    tool_call_id, ..
                } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn tool_result_ids(m: &ModelMessage) -> Vec<String> {
    match &m.content {
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolResult {
                    tool_call_id, ..
                } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// 规整历史消息,保证发给 LLM 的序列合法(Fix 4)。
/// 以"assistant + 紧随其后的连续 tool 消息"作为一个 tool block:
///   1) 保留 assistant 里"block 内有对应 tool_result"的 tool_call,剥离其余悬空 call;
///   2) 只保留 block 内被保留 call 覆盖的 tool_result,丢弃孤儿结果;
///   3) 剥离后为空的 assistant 丢弃;user/system 及纯文本 assistant 原样保留;
///   4) 无前置 assistant 的顶层 tool 视为孤儿丢弃。
/// 纯函数,不修改 session 本身。
pub(crate) fn sanitize_history(msgs: &[ModelMessage]) -> Vec<ModelMessage> {
    let n = msgs.len();
    let mut out: Vec<ModelMessage> = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let m = &msgs[i];
        if m.role != Role::Assistant {
            // user/system 原样保留;无前置 assistant 的顶层 tool 是孤儿,丢弃
            if m.role != Role::Tool {
                out.push(m.clone());
            }
            i += 1;
            continue;
        }

        // assistant:收集紧随其后的连续 tool 消息(一个 tool block)
        let mut block: Vec<&ModelMessage> = Vec::new();
        let mut j = i + 1;
        while j < n && msgs[j].role == Role::Tool {
            block.push(&msgs[j]);
            j += 1;
        }
        let block_results: std::collections::HashSet<String> = block
            .iter()
            .map(|t| tool_result_ids(t))
            .flatten()
            .collect();
        let kept_calls: std::collections::HashSet<String> = tool_call_ids(m)
            .into_iter()
            .filter(|c| block_results.contains(c))
            .collect();

        // 重写 assistant:只保留被结果覆盖的 call,其余 part 原样
        let new_parts: Vec<ContentPart> = match &m.content {
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::ToolCall {
                        tool_call_id, ..
                    } => kept_calls.contains(tool_call_id).then(|| p.clone()),
                    other => Some(other.clone()),
                })
                .collect(),
            _ => Vec::new(),
        };
        if matches!(m.content, MessageContent::Text(ref t) if !t.is_empty()) {
            out.push(m.clone());
        } else if !new_parts.is_empty() {
            out.push(ModelMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(new_parts),
            });
        }

        // 输出 block 内被保留 call 覆盖的 tool 消息,丢弃孤儿结果
        for t in block {
            if tool_result_ids(t).iter().any(|r| kept_calls.contains(r)) {
                out.push(t.clone());
            }
        }

        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolsPlugin;
    use crate::{LlmResponse, ScriptedLlm};
    use std::sync::atomic::{AtomicBool, Ordering};

    async fn soon<F, T>(f: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        tokio::time::timeout(std::time::Duration::from_secs(5), f)
            .await
            .expect("timed out")
    }

    /// Fix 4:规整真实损坏序列——孤儿 tool_result 丢弃、无紧邻结果的 call 剥离。
    #[test]
    fn sanitize_drops_orphan_tool_result_and_unpaired_call() {
        let msgs = vec![
            ModelMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "t1".into(),
                        provider_options: None,
                    },
                    ContentPart::tool_call("A", "bash", serde_json::json!({})),
                ]),
            },
            ModelMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "t2".into(),
                        provider_options: None,
                    },
                    ContentPart::tool_call("B", "bash", serde_json::json!({})),
                ]),
            },
            ModelMessage {
                role: Role::Tool,
                content: MessageContent::Parts(vec![ContentPart::tool_result(
                    "B",
                    serde_json::Value::String("ok".into()),
                )]),
            },
            ModelMessage::user("继续"),
            ModelMessage::user("自主续跑"),
            // 孤儿:A 的结果排在两条 user 之后,前面已无"在飞"的 A call
            ModelMessage {
                role: Role::Tool,
                content: MessageContent::Parts(vec![ContentPart::tool_result(
                    "A",
                    serde_json::Value::String("late".into()),
                )]),
            },
        ];
        let out = sanitize_history(&msgs);
        let roles: Vec<Role> = out.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![Role::Assistant, Role::Assistant, Role::Tool, Role::User, Role::User]
        );
        // 只有 B 的结果留下,A(孤儿)被丢弃
        let tool_ids: Vec<String> = out
            .iter()
            .filter(|m| m.role == Role::Tool)
            .flat_map(tool_result_ids)
            .collect();
        assert_eq!(tool_ids, vec!["B".to_string()]);
        // 第一条 assistant 的 call A 被剥离,只剩文本
        assert!(matches!(
            &out[0].content,
            MessageContent::Parts(p) if p.len() == 1
        ));
    }

    #[test]
    fn sanitize_keeps_valid_tool_roundtrip() {
        let msgs = vec![
            ModelMessage::user("hi"),
            ModelMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![ContentPart::tool_call(
                    "X",
                    "bash",
                    serde_json::json!({}),
                )]),
            },
            ModelMessage {
                role: Role::Tool,
                content: MessageContent::Parts(vec![ContentPart::tool_result(
                    "X",
                    serde_json::Value::String("done".into()),
                )]),
            },
        ];
        let out = sanitize_history(&msgs);
        assert_eq!(out.len(), 3, "合法往返不应被改动");
    }

    /// 长工具输出 soft-trim:超长保头/保尾 + 标记;正常长度原样不动。
    #[test]
    fn tool_result_long_output_is_trimmed() {
        use crate::tools::ToolOutput;
        let call = crate::scripted::tool_call("t1", "big", serde_json::json!({}));
        // 短输出:原样
        let short = ToolOutput { ok: true, output: "hello".into() };
        let m = tool_result_message(&call, &short);
        match &m.content {
            MessageContent::Parts(parts) => match &parts[0] {
                ContentPart::ToolResult { result, .. } => {
                    assert_eq!(result, &serde_json::Value::String("hello".into()));
                }
                other => panic!("expected ToolResult, got {other:?}"),
            },
            other => panic!("expected Parts, got {other:?}"),
        }
        // 超长输出:保头 + marker + 保尾,总长 < 输入
        let long_body: String = "x".repeat(5000);
        let long = ToolOutput { ok: true, output: long_body.clone() };
        let m2 = tool_result_message(&call, &long);
        match &m2.content {
            MessageContent::Parts(parts) => match &parts[0] {
                ContentPart::ToolResult { result, .. } => {
                    let s = result.as_str().expect("string result");
                    assert!(s.len() < long_body.len(), "被裁剪: {s:.0?}");
                    assert!(s.contains("…[trimmed"), "含裁剪标记");
                    assert!(s.starts_with("x".repeat(1500).as_str()), "保头");
                    assert!(s.ends_with("x".repeat(1500).as_str()), "保尾");
                }
                other => panic!("expected ToolResult, got {other:?}"),
            },
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    /// 一条 assistant 发起多个 call、由多条 tool 消息回答:必须全部保留(此前会误剥)。
    #[test]
    fn sanitize_keeps_multi_call_multi_result() {
        let msgs = vec![
            ModelMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![
                    ContentPart::tool_call("A", "bash", serde_json::json!({})),
                    ContentPart::tool_call("B", "read", serde_json::json!({})),
                ]),
            },
            ModelMessage {
                role: Role::Tool,
                content: MessageContent::Parts(vec![ContentPart::tool_result(
                    "A",
                    serde_json::Value::String("ra".into()),
                )]),
            },
            ModelMessage {
                role: Role::Tool,
                content: MessageContent::Parts(vec![ContentPart::tool_result(
                    "B",
                    serde_json::Value::String("rb".into()),
                )]),
            },
        ];
        let out = sanitize_history(&msgs);
        assert_eq!(out.len(), 3, "多 call 多 result 的合法往返应原样保留");
        // assistant 的两个 call 都在
        let calls = tool_call_ids(&out[0]);
        assert!(calls.contains(&"A".to_string()) && calls.contains(&"B".to_string()));
    }

    #[test]
    fn context_overflow_detection() {
        assert!(is_context_overflow("context length exceeded: 350000 > 128000"));
        assert!(is_context_overflow("prompt is too long: 50000 tokens"));
        assert!(is_context_overflow("This model's maximum context window is 128K"));
        assert!(!is_context_overflow("tool execution rejected"));
        assert!(!is_context_overflow("network timeout"));
    }

    /// Fix 2 原语:truncate_to 把 session 截回指定长度(幂等)。
    #[test]
    fn session_truncate_to_rolls_back() {
        let mut s = Session::new();
        for i in 0..10 {
            s.push(ModelMessage::user(format!("m{i}")));
        }
        s.truncate_to(4);
        assert_eq!(s.messages().len(), 4);
        s.truncate_to(8); // 目标 > 当前 → 不变
        assert_eq!(s.messages().len(), 4);
    }

    /// Session::sanitize(一次性修复):丢孤儿 tool_result + 裁尾部悬挂 user。
    #[test]
    fn session_sanitize_repair() {
        let mut s = Session::new();
        s.push(ModelMessage::user("hi"));
        s.push(ModelMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![ContentPart::tool_call(
                "A",
                "bash",
                serde_json::json!({}),
            )]),
        });
        s.push(ModelMessage {
            role: Role::Tool,
            content: MessageContent::Parts(vec![ContentPart::tool_result(
                "A",
                serde_json::Value::String("ok".into()),
            )]),
        });
        // 孤儿 B:前面无配对 call
        s.push(ModelMessage {
            role: Role::Tool,
            content: MessageContent::Parts(vec![ContentPart::tool_result(
                "B",
                serde_json::Value::String("late".into()),
            )]),
        });
        // 尾部悬挂 user(死循环残留)
        s.push(ModelMessage::user("续跑1"));
        s.push(ModelMessage::user("续跑2"));

        let (before, after) = s.sanitize();
        assert_eq!(before, 6);
        assert_eq!(after, 3);
        let roles: Vec<Role> = s.messages().iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
    }

    /// Fix 2:失败 turn 不留悬挂 user——空脚本 LLM 必然失败,回滚后 session 长度不变。
    #[tokio::test]
    async fn failed_turn_rolls_back_session() {
        let root = Ctx::root().unwrap();
        let llm: Arc<dyn LanguageModel> = Arc::new(ScriptedLlm::new(Vec::new()));
        let llm_d = root.provide_as(llm_key(), llm).unwrap();
        let tools_v = root.plugin(ToolsPlugin::new(Vec::new()));
        let driver_v = root.plugin(AgentDriverPlugin::new(16));
        (&tools_v).await.expect("tools loads");
        (&driver_v).await.expect("driver loads");
        let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

        let before = agent.session().messages().len();
        let res = soon(agent.followup("hi")).await;
        assert!(res.is_err(), "空脚本 LLM 必然失败");
        assert_eq!(
            agent.session().messages().len(),
            before,
            "失败 turn 应回滚,不留悬挂 user"
        );

        llm_d.dispose().await.unwrap();
        tools_v.dispose().await.unwrap();
        driver_v.dispose().await.unwrap();
    }

    /// Fix 3:上下文超限 → 自动 compact 后重试成功(超大 session 自愈)。
    #[tokio::test]
    async fn context_overflow_triggers_auto_compact_then_succeeds() {
        struct OverflowOnce {
            inner: ScriptedLlm,
            hit: AtomicBool,
        }
        #[async_trait::async_trait]
        impl LanguageModel for OverflowOnce {
            fn provider(&self) -> &str {
                self.inner.provider()
            }
            fn model_id(&self) -> &str {
                self.inner.model_id()
            }
            async fn do_generate(
                &self,
                o: &CallOptions,
            ) -> Result<aimux_core::result::GenerateResult, AiMuxError> {
                self.inner.do_generate(o).await
            }
            async fn do_stream(
                &self,
                o: &CallOptions,
            ) -> Result<aimux_core::result::StreamResult, AiMuxError> {
                if !self.hit.swap(true, Ordering::SeqCst) {
                    return Err(AiMuxError::InvalidArgument(
                        "400: prompt is too long, context length exceeded".into(),
                    ));
                }
                self.inner.do_stream(o).await
            }
        }

        // 预置 50 条历史的 session 文件(> 40,确保触发裁剪)
        let path = std::env::temp_dir().join(format!("rutis-overflow-test-{}.json", std::process::id()));
        {
            let mut s = Session::new();
            for i in 0..50 {
                s.push(if i % 2 == 0 {
                    ModelMessage::user(format!("q{i}"))
                } else {
                    ModelMessage::assistant(format!("a{i}"))
                });
            }
            s.persist(&path).unwrap();
        }

        let root = Ctx::root().unwrap();
        let llm: Arc<dyn LanguageModel> = Arc::new(OverflowOnce {
            inner: ScriptedLlm::new(vec![LlmResponse::content("done")]),
            hit: AtomicBool::new(false),
        });
        let llm_d = root.provide_as(llm_key(), llm).unwrap();
        let tools_v = root.plugin(ToolsPlugin::new(Vec::new()));
        let driver_v = root
            .plugin(AgentDriverPlugin::new(16).with_session_path(path.clone()));
        (&tools_v).await.expect("tools loads");
        (&driver_v).await.expect("driver loads");
        let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
        assert!(agent.session().messages().len() > 40, "应恢复了 50 条历史");

        let res = soon(agent.followup("go")).await;
        assert!(res.is_ok(), "compact 后应成功: {res:?}");
        assert!(agent.session().summary().is_some(), "超限应触发自动 compact");
        assert!(
            agent.session().messages().len() <= 42,
            "compact 后应大幅缩减,实际={}",
            agent.session().messages().len()
        );

        llm_d.dispose().await.unwrap();
        tools_v.dispose().await.unwrap();
        driver_v.dispose().await.unwrap();
        let _ = std::fs::remove_file(&path);
    }
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
        SessionSnapshot::new(
            session.id(),
            session.messages(),
            session.summary().map(str::to_owned),
            session.todo().map(str::to_owned),
        )
    }

    fn compact(&self, summary: String, keep: usize) -> (usize, usize) {
        let result = {
            let mut session = self.session.lock().unwrap();
            session.compact(summary, keep)
        };
        // 压缩后立即落盘(路径已配置时),让摘要跨代保留
        self.persist_session();
        result
    }

    fn set_todo(&self, todo: String) {
        {
            let mut session = self.session.lock().unwrap();
            session.set_todo(todo);
        }
        // 立即落盘:中断后恢复时待办可用
        self.persist_session();
    }

    fn update_persona(&self, persona: String) {
        *self.system_prompt.lock().unwrap() = Some(persona);
    }

    fn cancel(&self) {
        self.cancel.lock().unwrap().cancel();
    }

    fn followup<'a>(&'a self, input: &'a str) -> BoxFuture<'a, Result<String, AgentError>> {
        Box::pin(async move {
            // 防重入(Fix 1):整个 turn(push user + run_loop)持锁串行,
            // 并发 followup(TUI 提交 vs SelfDriven 自主续跑)排队执行,
            // 不会交错写 session 造成历史乱序/悬空 tool_call。
            let _guard = self.turn_lock.lock().await;

            // 原子 turn(Fix 2):记录 turn 前长度,失败时回滚本轮所有 push。
            let before = self.session.lock().unwrap().messages().len();
            self.session.lock().unwrap().push(ModelMessage::user(input));
            let token = self.fresh_turn_token();
            self.status.set(AgentStatus::Running);
            let out = self.run_loop(&token).await;
            self.status.set(AgentStatus::Idle);
            // 失败回滚:移除本轮 push 的悬挂 user + 半成品 assistant/tool,
            // 恢复 turn 前状态,避免坏历史累积(否则每次失败都留一条 user,
            // 连排 + 越滚越长,最终每次请求都非法/超长)。
            if out.is_err() {
                self.session.lock().unwrap().truncate_to(before);
            }
            self.emit_turn_end(self.id(), &out);
            // 保存时机 ①:每 turn 结束原子落盘(未配置路径 = no-op)
            self.persist_session();
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
    session_path: Option<PathBuf>,
    inject_keys: Vec<TypeKey>,
}

impl AgentDriverPlugin {
    pub fn new(max_steps: usize) -> Self {
        Self {
            max_steps,
            system_prompt: None,
            session_path: None,
            inject_keys: vec![llm_key(), tools_key()],
        }
    }

    /// 静态 system prompt(minimal mode persona);每步作为 instructions 前置。
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// 启用 session 持久化:`apply` 时从 path 恢复(失败静默降级为新
    /// Session),此后每 turn 结束 + fiber 卸载原子落盘。
    pub fn with_session_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.session_path = Some(path.into());
        self
    }

    /// 启用默认 session 持久化路径(`<cwd>/.rutis/session.json`)。
    /// 人用宿主的推荐入口:一行启用跨会话记忆。
    pub fn with_default_session_path(mut self) -> Self {
        self.session_path = Some(default_session_path());
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
                self.session_path.clone(),
            ));
            ctx.provide_as::<dyn Agent>(agent_key(), driver.clone())?;
            // 持久化路径服务:自我控制工具(`self_status`/`self_persist`)读取
            ctx.provide_as::<Option<PathBuf>>(session_path_key(), Arc::new(self.session_path.clone()))?;

            // fiber 卸载 → 落盘 session(持久化路径已配置时)。
            // 保存时机 ②:挂 effect disposer,后注册→先清理(LIFO),
            // 保证卸载时 session 是最后一代快照。
            let persist_driver = driver.clone();
            ctx.effect(move || {
                let driver = persist_driver.clone();
                Effect::AsyncDisposer(Box::new(move || {
                    let driver = driver.clone();
                    Box::pin(async move {
                        driver.persist_session();
                        Ok(())
                    })
                }))
            })?;

            // fiber 卸载 → cancel 当前 turn(driver 内 token 级联停止)
            let watcher_ctx = ctx.clone();
            let watcher_driver = driver.clone();
            ctx.effect(move || {
                let watcher_ctx = watcher_ctx.clone();
                let driver = watcher_driver.clone();
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
