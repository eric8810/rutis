//! 自我控制工具包(设计 session-persist §二)——agent 操舵自身热加载的"手"。
//!
//! 控制回路 = 决策(督工,后续)+ 执行(本工具集)。六个工具:
//!
//! - [`self_status`]:读 session id / 代际 / 状态 / 版本快照(经 `Agent`
//!   trait 现有方法组合,不新增 trait 方法)。
//! - [`self_persist`]:手动落盘 session(经 `Agent::session()` 快照 +
//!   持久化路径服务,路径由 `with_session_path` 注入)。
//! - [`self_build`]:`cargo build -p rutis-agent`(复用 bash runner)。
//! - [`self_check`]:`cargo test`(复用 bash runner)。
//! - [`self_reload`]:触发重启——冷重启版:写交接意图 + 请求退出
//!   (经 `SelfReloadRequested` 事件广播,宿主监听后重启进程)。
//! - [`self_rollback`]:回滚到上一代(版本台账,见 [`version_ledger`])。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rutis::Ctx;
use serde_json::{json, Value};

use crate::agent::Agent;
use crate::driver::session_path_key;
use crate::events::SelfReloadRequested;
use crate::session::SessionId;
use crate::tools::{bash::bash_tool, hotplug::hotplug_load, ToolDef, ToolRegistry};

/// 版本台账文件(约定路径,随仓库走)。
pub const VERSION_LEDGER_PATH: &str = "docs/work/version-ledger.json";

/// 台账条目:一次成功构建/测试时的代码版本。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LedgerEntry {
    pub commit: String,
    pub at_ms: u64,
    pub note: String,
}

/// 台账:按时间升序的版本记录。`self_build` 成功时追加当前 HEAD;
/// `self_rollback` 的"上一代" = 倒数第二条。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct VersionLedger {
    pub entries: Vec<LedgerEntry>,
}

impl VersionLedger {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("serialize ledger: {e}"))?;
        std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
    }

    pub fn push(&mut self, entry: LedgerEntry) {
        self.entries.push(entry);
    }

    /// 上一代:倒数第二条(最近一条是"当前代")。None = 台账不足两条。
    pub fn previous(&self) -> Option<&LedgerEntry> {
        if self.entries.len() < 2 {
            return None;
        }
        self.entries.get(self.entries.len() - 2)
    }
}

/// 当前 git HEAD 短哈希(经 `git rev-parse`;失败 = "(no git)")。
fn git_head() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no git)".to_string())
}

/// 构造 `self_*` 工具集。`ctx` 用于工具执行时经服务注册表取
/// session 路径、经事件总线广播 `SelfReloadRequested`。
pub fn self_tools(ctx: Ctx) -> Vec<ToolDef> {
    vec![
        self_status(ctx.clone()),
        self_persist(ctx.clone()),
        self_compact(ctx.clone()),
        self_todo(ctx.clone()),
        self_persona(ctx.clone()),
        self_hotload(ctx.clone()),
        hotplug_load(ctx.clone()),
        self_build(ctx.clone()),
        self_check(ctx.clone()),
        self_reload(ctx),
        self_rollback_tool(),
        skill(),
    ]
}

/// `skill`:检索技能库(docs/skills/index.md)——借鉴 Codex skills 的可检索
/// 能力库。技能 = 方法论/能力单元(何时用/怎么做/到哪看),避免临场摸索与
/// 重复发明。`skill` 列全部;`skill SKILL-X1|名称` 返回匹配条目。
/// 动态读文件,故技能库内容演进后工具自动反映,跨重启稳定(非热加载易失)。
/// 技能库 index 路径解析:cwd 无关。
///
/// 优先用编译期注入的 `CARGO_MANIFEST_DIR`(本 crate 目录)上溯到仓库根定位
/// `docs/skills/index.md`——不依赖运行时 cwd(cargo test/integration 以 crate 为
/// cwd,子目录调起 CLI 也会换 cwd,相对路径会因找不到技能库而坏,见
/// self-review-checklist 的 cwd 敏感警告)。回退到相对路径以兼容非 cargo 部署。
fn skills_index_path() -> std::path::PathBuf {
    let manifest_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_relative = manifest_root.join("../../docs/skills/index.md");
    if repo_relative.exists() {
        return repo_relative;
    }
    std::path::PathBuf::from("docs/skills/index.md")
}

pub fn skill() -> ToolDef {
    let skills_index = std::sync::Arc::new(skills_index_path());
    ToolDef::new(
        "skill",
        "Retrieve a skill from the agent skill library (docs/skills/index.md). Pass a SKILL code (e.g. SKILL-U1) or a name keyword to get the matching skill's doc path + usage; 'list' (or no arg) to list all skills. Lets you reuse known methodologies instead of reinventing them.",
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "SKILL code (e.g. SKILL-U1), a name keyword to grep, or 'list' for all." }
            },
            "required": []
        }),
        {
            let skills_index = skills_index.clone();
            move |args: Value| {
                let skills_index = skills_index.clone();
                async move {
                    let key = args["key"].as_str().unwrap_or("list").trim();
                    let index = std::fs::read_to_string(skills_index.as_ref())
                        .map_err(|e| format!("error: read skills index {}: {e}", skills_index.display()))?;
                    // 提取索引主体(跳过标题区,从第一个 '- **SKILL' 起)
                    let body = index.split_once("## ").map(|(_, b)| b).unwrap_or(&index);
                    if key != "list" && !key.is_empty() {
                        let needle = if key.to_uppercase().starts_with("SKILL-") {
                            key.to_uppercase()
                        } else {
                            key.to_string()
                        };
                        let mut found: String = String::new();
                        for line in body.lines() {
                            let up = line.to_uppercase();
                            if up.contains(&needle) || line.to_lowercase().contains(&key.to_lowercase()) {
                                found.push_str(line.trim_start());
                                found.push('\n');
                            }
                        }
                        if found.is_empty() {
                            Ok(Value::String(format!("skill '{key}' not found. List all with `skill list`.")))
                        } else {
                            Ok(Value::String(found))
                        }
                    } else {
                        Ok(Value::String(body.trim_start().to_string()))
                    }
                }
            }
        },
    )
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 取当前 `dyn Agent` 服务(未装配 → Err 文本回喂模型)。
fn current_agent(ctx: &Ctx) -> Result<Arc<dyn Agent>, String> {
    ctx.get_as::<dyn Agent>(crate::agent_key())
        .ok_or_else(|| "error: agent driver not loaded (self tools require AgentDriverPlugin)".to_string())
}

/// 取持久化路径服务(None = 未配置)。
fn session_path(ctx: &Ctx) -> Option<PathBuf> {
    ctx.get_as::<Option<PathBuf>>(session_path_key())
        .and_then(|arc| arc.as_ref().clone())
}

// ── self_status ─────────────────────────────────────────────────────

/// `self_status`:读 session id / 代际 / 状态 / 消息数 / 版本快照。
pub fn self_status(ctx: Ctx) -> ToolDef {
    ToolDef::new(
        "self_status",
        "Read the agent's own status snapshot: session identity, generation, status, history length, and (if configured) persistence path. No side effects.",
        json!({ "type": "object", "properties": {}, "required": [] }),
        move |_args: Value| {
            let ctx = ctx.clone();
            async move {
                let agent = current_agent(&ctx)?;
                let id = agent.id();
                let status = agent.status();
                let snapshot = agent.session();
                let msgs = snapshot.messages().len();
                let path = session_path(&ctx);
                let persisted = path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "not configured".to_string());
                Ok(Value::String(format!(
                    "self_status:\n  identity: {}\n  generation: {}\n  status: {:?}\n  session messages: {}\n  persist path: {}",
                    id.identity(),
                    id.generation(),
                    status,
                    msgs,
                    persisted,
                )))
            }
        },
    )
}

// ── self_persist ────────────────────────────────────────────────────

/// `self_persist`:手动把当前 session 落盘(原子写)。
/// 未配置持久化路径 → 错误提示(不静默)。
pub fn self_persist(ctx: Ctx) -> ToolDef {
    ToolDef::new(
        "self_persist",
        "Persist the current session to disk now (atomic write). Requires a configured session path (AgentDriverPlugin::with_session_path).",
        json!({ "type": "object", "properties": {}, "required": [] }),
        move |_args: Value| {
            let ctx = ctx.clone();
            async move {
                let agent = current_agent(&ctx)?;
                let path = session_path(&ctx)
                    .ok_or_else(|| "error: no session persist path configured (use AgentDriverPlugin::with_session_path)".to_string())?;
                let snapshot = agent.session();
                snapshot
                    .persist(&path)
                    .map_err(|e| format!("error: {e}"))?;
                Ok(Value::String(format!(
                    "persisted session identity={} generation={} messages={} -> {}",
                    snapshot.id().identity(),
                    snapshot.id().generation(),
                    snapshot.messages().len(),
                    path.display()
                )))
            }
        },
    )
}

// ── self_todo(待办/自动接续)────────────────────────────────────────

/// `self_todo`:记录/更新待办——agent 中断(重启/崩溃)后自动接续的
/// 工作指引。重启恢复时,待办注入 system prompt(`# 待办/下一步`),
/// 模型第一眼看到"该做什么",自动继续而不是问用户。
/// 参数:`todo`(下一步工作,简洁可执行)。传空串清除待办。
pub fn self_todo(ctx: Ctx) -> ToolDef {
    ToolDef::new(
        "self_todo",
        "Record or update your todo / next-step: the task to continue after an interruption (restart/crash). It is injected into the system prompt on recovery, so the next instance automatically resumes work instead of asking what to do. Pass an empty string to clear.",
        json!({
            "type": "object",
            "properties": {
                "todo": {
                    "type": "string",
                    "description": "Concise next-step instruction for the next instance"
                }
            },
            "required": ["todo"]
        }),
        move |args: Value| {
            let ctx = ctx.clone();
            async move {
                let todo = args["todo"]
                    .as_str()
                    .ok_or_else(|| "error: todo is required".to_string())?
                    .to_string();
                let agent = current_agent(&ctx)?;
                agent.set_todo(todo.clone());
                Ok(Value::String(format!(
                    "todo updated (will auto-resume after restart): {todo}"
                )))
            }
        },
    )
}

// ── self_persona(实时更新自己的 system prompt)───────────────────────

/// `self_persona`:运行中更新自己的 system prompt(persona)。
/// 自我改善的真正闭环:agent 意识到自己的认知需要升级时,直接调用
/// 本工具替换 persona,下一轮立即生效——无需重启、无需宿主介入。
/// 参数:`persona`(完整的新 system prompt 文本;建议保留使命/环境/
/// 交接/纪律等核心段落,只改需要进化的部分)。
pub fn self_persona(ctx: Ctx) -> ToolDef {
    ToolDef::new(
        "self_persona",
        "Update your own system prompt (persona) at runtime. Pass the full new persona text. Takes effect on the next turn — no restart, no host intervention. Use it when you realize your cognition should evolve (e.g. new discipline, new self-understanding).",
        json!({
            "type": "object",
            "properties": {
                "persona": {
                    "type": "string",
                    "description": "The full new system prompt / persona text"
                }
            },
            "required": ["persona"]
        }),
        move |args: Value| {
            let ctx = ctx.clone();
            async move {
                let persona = args["persona"]
                    .as_str()
                    .ok_or_else(|| "error: persona is required".to_string())?
                    .to_string();
                let agent = current_agent(&ctx)?;
                agent.update_persona(persona.clone());
                Ok(Value::String(format!(
                    "persona updated at runtime ({} chars). Next turn uses the new cognition.",
                    persona.chars().count()
                )))
            }
        },
    )
}

// ── self_compact ────────────────────────────────────────────────────

/// `self_compact`:压缩长会话记忆——保存摘要,裁剪 messages 到最近
/// `keep` 条(默认 20)。被裁剪消息由 `summary` 替代,后续 prompt 经
/// system 前置注入(`# 记忆摘要`),长会话不退化。
/// 参数:`summary`(早期对话摘要文本,由模型生成)、`keep`(保留最近条数)。
pub fn self_compact(ctx: Ctx) -> ToolDef {
    ToolDef::new(
        "self_compact",
        "Compact the session memory: summarize early messages and trim the history to the most recent N. Pass a concise summary of what happened before the kept messages. Keeps long sessions from degrading.",
        json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Concise summary of the early conversation being trimmed"
                },
                "keep": {
                    "type": "integer",
                    "description": "Number of most recent messages to keep (default 20)"
                }
            },
            "required": ["summary"]
        }),
        move |args: Value| {
            let ctx = ctx.clone();
            async move {
                let summary = args["summary"]
                    .as_str()
                    .ok_or_else(|| "error: summary is required".to_string())?
                    .to_string();
                let keep = args["keep"].as_u64().unwrap_or(20) as usize;
                let agent = current_agent(&ctx)?;
                let (before, after) = agent.compact(summary, keep);
                Ok(Value::String(format!(
                    "compacted session: messages {} -> {} (kept last {keep})",
                    before, after
                )))
            }
        },
    )
}

// ── self_hotload(热加载新能力)───────────────────────────────────────

/// `self_hotload`:运行中给 agent 注册一个新工具(热加载)。
///
/// 参数:
/// - `name`:新工具名(必填,如 `my_tool`)
/// - `description`:新工具描述
/// - `reply`:工具被调用时的固定回复文本(简单但真实有用,如
///   "release notes" 工具、环境信息工具)
///
/// 执行:向当前 `ToolRegistry` 运行时注册新工具;后续 turn 的模型
/// schema 立即包含它,无需重编译/重启。返回新工具已注册的确认。
/// 这是"agent 运行中给自己加能力"的热迭代闭环。
pub fn self_hotload(ctx: Ctx) -> ToolDef {
    ToolDef::new(
        "self_hotload",
        "Hot-load a new tool into the running agent. Pass a name, description, and a fixed reply. The tool becomes available to the model in subsequent turns — no rebuild or restart needed. Use it to add small utility tools on the fly.",
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "New tool name (e.g. my_tool)" },
                "description": { "type": "string", "description": "What the tool does" },
                "reply": { "type": "string", "description": "Fixed reply the tool returns when called" }
            },
            "required": ["name", "reply"]
        }),
        move |args: Value| {
            let ctx = ctx.clone();
            async move {
                let name = args["name"]
                    .as_str()
                    .ok_or_else(|| "error: name is required".to_string())?
                    .to_string();
                let reply = args["reply"]
                    .as_str()
                    .ok_or_else(|| "error: reply is required".to_string())?
                    .to_string();
                let description = args["description"]
                    .as_str()
                    .unwrap_or("hot-loaded tool")
                    .to_string();

                // 取当前 ToolRegistry 服务
                let registry = ctx
                    .get_as::<ToolRegistry>(crate::tools_key())
                    .ok_or_else(|| "error: tool registry not loaded".to_string())?;

                // 注册:固定回复工具
                let def = ToolDef::new(
                    &name,
                    &description,
                    json!({ "type": "object", "properties": {} }),
                    move |_: Value| {
                        let reply = reply.clone();
                        async move { Ok(Value::String(reply)) }
                    },
                );
                registry.register(def);

                Ok(Value::String(format!(
                    "hot-loaded tool '{name}' — now available in the model's tool set"
                )))
            }
        },
    )
}

// ── self_build / self_check(复用 bash runner)────────────────────────

/// 复用 bash 工具的执行逻辑(设计:不造轮子)。
fn bash_run(
    command: &str,
) -> Arc<dyn Fn(Value) -> futures::future::BoxFuture<'static, Result<Value, String>> + Send + Sync>
{
    let bash = bash_tool();
    let run = bash.run.clone();
    let command = command.to_string();
    Arc::new(move |_args: Value| {
        let run = run.clone();
        let command = command.clone();
        Box::pin(async move {
            run(json!({
                "command": command,
                "description": "self control: build/check",
                "timeout_ms": 600_000,
            }))
            .await
        })
    })
}

/// `self_build`:`cargo build -p rutis-agent`(复用 bash)。
pub fn self_build(ctx: Ctx) -> ToolDef {
    let _ = ctx;
    ToolDef::new(
        "self_build",
        "Build the agent crate (`cargo build -p rutis-agent`) via bash. On success, records the current commit in the version ledger (docs/work/version-ledger.json). Optional `command` overrides the build command (tests use this).",
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Optional override command; default `cargo build -p rutis-agent`." }
            },
            "required": []
        }),
        |args: Value| async move {
            let command = args["command"]
                .as_str()
                .unwrap_or("cargo build -p rutis-agent");
            let out = bash_run(&format!("{command} 2>&1"))("".into()).await?;
            // 成功则记台账(幂等:同一 commit 只记一次)
            if let Value::String(s) = &out {
                if !s.contains("error") && s.contains("Finished") {
                    let commit = git_head();
                    let path = PathBuf::from(VERSION_LEDGER_PATH);
                    let mut ledger = VersionLedger::load(&path);
                    if ledger.entries.last().map(|e| e.commit.as_str()) != Some(commit.as_str()) {
                        let commit_clone = commit.clone();
                        ledger.push(LedgerEntry {
                            commit,
                            at_ms: now_ms(),
                            note: "self_build".to_string(),
                        });
                        if let Err(e) = ledger.save(&path) {
                            return Err(format!("build ok but ledger write failed: {e}"));
                        }
                        return Ok(Value::String(format!("{s}\n[ledger] recorded {commit_clone}")));
                    }
                }
            }
            Ok(out)
        },
    )
}

/// `self_check`:`cargo test -p rutis-agent`(复用 bash)。
pub fn self_check(_ctx: Ctx) -> ToolDef {
    ToolDef::new(
        "self_check",
        "Run the agent crate tests (`cargo test -p rutis-agent`) via bash. Optional `command` overrides the test command (tests use this).",
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Optional override command; default `cargo test -p rutis-agent`." }
            },
            "required": []
        }),
        |args: Value| async move {
            let command = args["command"]
                .as_str()
                .unwrap_or("cargo test -p rutis-agent");
            bash_run(&format!("{command} 2>&1"))("".into()).await
        },
    )
}

// ── self_reload(冷重启版)────────────────────────────────────────────

/// `self_reload`:冷重启——写交接意图(追加到 `docs/work/handoff.md`)
/// + 广播 `SelfReloadRequested` 事件(宿主监听后优雅退出并重启进程)。
/// 冷重启版不做热加载(督工热重启为后续演进)。
pub fn self_reload(ctx: Ctx) -> ToolDef {
    ToolDef::new(
        "self_reload",
        "Request a cold restart: append a reload intent to docs/work/handoff.md (so the next instance can resume), then broadcast SelfReloadRequested so the host can exit and restart the process. Returns after the intent is written and the event is emitted.",
        json!({
            "type": "object",
            "properties": {
                "handoff": { "type": "string", "description": "Optional handoff file path (default docs/work/handoff.md)." }
            },
            "required": []
        }),
        move |args: Value| {
            let ctx = ctx.clone();
            async move {
                // 1) 写意图(追加到 handoff;缺文件则创建)
                let handoff = args["handoff"]
                    .as_str()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("docs/work/handoff.md"));
                let intent = format!(
                    "\n---\n## reload intent @ {}\nSelf reload requested via self_reload tool.\nNext instance: read docs/work/handoff.md, resume from the latest task.\n",
                    now_ms()
                );
                let mut existing = std::fs::read_to_string(&handoff).unwrap_or_default();
                if !existing.ends_with('\n') {
                    existing.push('\n');
                }
                existing.push_str(&intent);
                std::fs::write(&handoff, existing)
                    .map_err(|e| format!("error: write handoff: {e}"))?;

                // 2) 广播退出请求(可观测:宿主监听 SelfReloadRequested)
                ctx.events().emit(
                    &ctx,
                    Arc::new(SelfReloadRequested {
                        session: current_agent(&ctx).map(|a| a.id()).unwrap_or_else(|_| SessionId::next()),
                        reason: "self_reload tool invoked".to_string(),
                        intent_path: handoff.to_string_lossy().into_owned(),
                    }),
                );

                Ok(Value::String(format!(
                    "reload intent written to {}; SelfReloadRequested emitted. Host should now exit and restart the process.",
                    handoff.display()
                )))
            }
        },
    )
}

// ── self_rollback(版本台账)──────────────────────────────────────────

/// `self_rollback`:回滚到上一代(版本台账)。
/// 默认 dry-run:报告候选 commit 与命令,不执行;`apply=true` 才 `git checkout`。
pub fn self_rollback_tool() -> ToolDef {
    ToolDef::new(
        "self_rollback",
        "Roll back to the previous generation using the version ledger (docs/work/version-ledger.json). Default: dry-run, reports the target commit and the git command. Pass apply=true to actually run `git checkout <prev-commit>` (destructive: uncommitted changes are kept). Optional `ledger` overrides the ledger path (tests use this).",
        json!({
            "type": "object",
            "properties": {
                "apply": { "type": "boolean", "description": "If true, actually run git checkout. If false/absent, dry-run report only." },
                "ledger": { "type": "string", "description": "Optional ledger path; default docs/work/version-ledger.json." }
            },
            "required": []
        }),
        |args: Value| async move {
            let ledger_path = args["ledger"]
                .as_str()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(VERSION_LEDGER_PATH));
            let ledger = VersionLedger::load(&ledger_path);
            match ledger.previous() {
                None => Ok(Value::String(
                    "error: version ledger has fewer than 2 entries — no previous generation to roll back to. Run self_build successfully at least twice first.".to_string(),
                )),
                Some(prev) => {
                    let apply = args["apply"].as_bool().unwrap_or(false);
                    if apply {
                        let out = bash_run(&format!(
                            "git checkout {} 2>&1",
                            prev.commit
                        ))("".into())
                        .await?;
                        Ok(Value::String(format!(
                            "rolled back to {} (previous generation)\n{out}",
                            prev.commit
                        )))
                    } else {
                        Ok(Value::String(format!(
                            "previous generation: {} (at {})\ndry-run: `git checkout {}`\npass apply=true to execute",
                            prev.commit, prev.at_ms, prev.commit
                        )))
                    }
                }
            }
        },
    )
}
