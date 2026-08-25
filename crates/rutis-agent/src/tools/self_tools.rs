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
use crate::tools::{bash::bash_tool, ToolDef};

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
        self_build(ctx.clone()),
        self_check(ctx.clone()),
        self_reload(ctx),
        self_rollback_tool(),
    ]
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
