//! 自我控制工具包测试(设计 session-persist §二 验证):
//!
//! 每个工具一条测试:
//! - `self_status` 读身份/代际/状态/消息数
//! - `self_persist` 手动落盘 session(需 with_session_path)
//! - `self_build` / `self_check` 复用 bash 跑 cargo
//! - `self_reload` 写意图文档 + 广播 SelfReloadRequested(核心)
//! - `self_rollback` 版本台账 dry-run / apply
//! - 集成:6 工具进 registry,ScriptedLlm 驱动一个自控 turn

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimux_core::language_model::LanguageModel;
use rutis::{Ctx, Listener};
use rutis_agent::{
    agent_key, llm_key, tool_call, Agent, AgentDriverPlugin, LlmResponse,
    ScriptedLlm, SelfReloadRequested, ToolDef, ToolsPlugin, VERSION_LEDGER_PATH,
};
use serde_json::{json, Value};

async fn soon<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(20), f)
        .await
        .expect("timed out")
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rutis-self-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn join(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 直接执行 ToolDef runner,返回 (ok, output 文本)。
async fn run(def: &ToolDef, args: Value) -> (bool, String) {
    match (def.run.clone())(args).await {
        Ok(Value::String(s)) => (true, s),
        Ok(other) => (true, other.to_string()),
        Err(e) => (false, e),
    }
}

/// 装载完整驱动(带 self 工具集 + session path),返回各 view 与 llm。
async fn load_self_driver(
    root: &Ctx,
    session_path: Option<&str>,
    responses: Vec<LlmResponse>,
) -> (rutis::FiberView, rutis::FiberView, Arc<ScriptedLlm>) {
    let llm = Arc::new(ScriptedLlm::new(responses));
    let service: Arc<dyn LanguageModel> = llm.clone();
    root.provide_as(llm_key(), service).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::self_tools(root.clone())));
    let mut p = AgentDriverPlugin::new(8);
    if let Some(pth) = session_path {
        p = p.with_session_path(pth);
    }
    let driver_view = root.plugin(p);
    soon(async {
        (&tools_view).await.expect("tools loads");
        (&driver_view).await.expect("driver loads");
    })
    .await;
    (tools_view, driver_view, llm)
}

// ── self_status ─────────────────────────────────────────────────────

#[tokio::test]
async fn self_status_reports_identity_generation_and_history() {
    let tmp = TempDir::new("status");
    let path = tmp.join("session.json");
    let root = Ctx::root().unwrap();
    let (tv, dv, _llm) = load_self_driver(
        &root,
        Some(&path),
        vec![LlmResponse::content("ok")],
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let _ = soon(agent.followup("hello")).await.unwrap();

    let def = rutis_agent::self_status(root.clone());
    let (ok, out) = run(&def, json!({})).await;
    assert!(ok, "{out}");
    let id = agent.id();
    assert!(out.contains(&format!("identity: {}", id.identity())), "{out}");
    assert!(out.contains(&format!("generation: {}", id.generation())), "{out}");
    assert!(out.contains("status: Idle"), "{out}");
    assert!(out.contains("session messages: 2"), "{out}");
    assert!(out.contains(&path), "{out}");

    soon(async {
        dv.dispose().await.unwrap();
        tv.dispose().await.unwrap();
    })
    .await;
}

// ── self_persist ────────────────────────────────────────────────────

#[tokio::test]
async fn self_persist_writes_session_file() {
    let tmp = TempDir::new("persist");
    let path = tmp.join("session.json");
    let root = Ctx::root().unwrap();
    let (tv, dv, _llm) = load_self_driver(
        &root,
        Some(&path),
        vec![LlmResponse::content("saved")],
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let _ = soon(agent.followup("remember this")).await.unwrap();

    // 删除文件(模拟手动落盘场景),再 self_persist
    std::fs::remove_file(&path).unwrap();
    let def = rutis_agent::self_persist(root.clone());
    let (ok, out) = run(&def, json!({})).await;
    assert!(ok, "{out}");
    assert!(out.contains("persisted session"), "{out}");
    assert!(std::path::Path::new(&path).exists(), "文件被写入");

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"version\": 1"), "{raw}");
    assert!(raw.contains("remember this"), "{raw}");

    soon(async {
        dv.dispose().await.unwrap();
        tv.dispose().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn self_persist_without_path_errors() {
    let root = Ctx::root().unwrap();
    let (tv, dv, _llm) = load_self_driver(&root, None, vec![LlmResponse::content("x")])
        .await;
    let def = rutis_agent::self_persist(root.clone());
    let (ok, out) = run(&def, json!({})).await;
    assert!(!ok, "未配置路径应报错: {out}");
    assert!(out.contains("no session persist path"), "{out}");

    soon(async {
        dv.dispose().await.unwrap();
        tv.dispose().await.unwrap();
    })
    .await;
}

// ── self_reload(核心)────────────────────────────────────────────────

#[tokio::test]
async fn self_reload_writes_intent_and_emits_event() {
    // 用临时 handoff 路径隔离测试(不污染仓库 docs/work/handoff.md)
    let tmp = TempDir::new("reload");
    let handoff = tmp.join("handoff.md");
    std::fs::write(&handoff, "# prior\n").unwrap();

    let root = Ctx::root().unwrap();
    let (tv, dv, _llm) = load_self_driver(&root, None, vec![LlmResponse::content("x")])
        .await;

    // 监听 SelfReloadRequested
    let got: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    struct L(Arc<Mutex<Option<String>>>);
    impl Listener<SelfReloadRequested> for L {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a SelfReloadRequested,
        ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
            let got = self.0.clone();
            let reason = e.reason.clone();
            let intent = e.intent_path.clone();
            Box::pin(async move {
                got.lock().unwrap().replace(format!("{reason}|{intent}"));
                Ok(None)
            })
        }
    }
    let _d = root.events().on(&root, L(got.clone())).unwrap();

    let def = rutis_agent::self_reload(root.clone());
    let (ok, out) = run(&def, json!({ "handoff": handoff })).await;
    assert!(ok, "{out}");

    // 意图文档被写(原内容保留 + 追加 intent)
    let text = std::fs::read_to_string(&handoff).unwrap();
    assert!(text.contains("# prior"), "{text}");
    assert!(text.contains("## reload intent"), "{text}");
    assert!(text.contains("Next instance"), "{text}");

    // 事件被广播
    soon(async {
        while got.lock().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let got = got.lock().unwrap().clone().unwrap();
    assert!(got.contains("self_reload"), "{got}");

    soon(async {
        dv.dispose().await.unwrap();
        tv.dispose().await.unwrap();
    })
    .await;
}

// ── self_build / self_check(复用 bash)───────────────────────────────

#[tokio::test]
async fn self_check_runs_command_via_bash() {
    let root = Ctx::root().unwrap();
    let def = rutis_agent::self_check(root.clone());
    // 轻命令验证"复用 bash 执行";默认 cargo test 太重且并行不稳
    let (ok, out) = run(&def, json!({ "command": "echo self-check-ok" })).await;
    assert!(ok, "{out}");
    assert!(out.contains("self-check-ok"), "{out}");
}

#[tokio::test]
async fn self_build_runs_command_and_records_ledger() {
    // 台账写 crate 目录下的约定路径(相对 cwd),测试后清理
    let ledger_path = PathBuf::from(VERSION_LEDGER_PATH);
    if let Some(parent) = ledger_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let _ = std::fs::remove_file(&ledger_path);

    let root = Ctx::root().unwrap();
    let def = rutis_agent::self_build(root.clone());
    // 轻命令模拟成功构建(输出含 Finished 且无 error)→ 记台账
    let (ok, out) = run(
        &def,
        json!({ "command": "echo Finished ok" }),
    )
    .await;
    assert!(ok, "{out}");
    assert!(out.contains("Finished"), "{out}");
    assert!(out.contains("[ledger] recorded"), "{out}");

    // 台账被记录
    let raw = std::fs::read_to_string(&ledger_path).unwrap();
    assert!(raw.contains("self_build"), "{raw}");

    // 幂等:同 commit 再跑不重复记
    let (_, out2) = run(
        &def,
        json!({ "command": "echo Finished ok" }),
    )
    .await;
    assert!(!out2.contains("[ledger] recorded"), "幂等: {out2}");

    let _ = std::fs::remove_file(&ledger_path);
}

// ── self_rollback(版本台账)──────────────────────────────────────────

#[tokio::test]
async fn self_rollback_dry_run_reports_previous_generation() {
    let tmp = TempDir::new("rollback");
    let ledger = tmp.join("version-ledger.json");
    std::fs::write(
        &ledger,
        r#"{"entries":[
            {"commit":"abc123","at_ms":1,"note":"first"},
            {"commit":"def456","at_ms":2,"note":"second"}
        ]}"#,
    )
    .unwrap();

    // 用临时台账路径测:直接构造台账读逻辑(工具读固定路径,这里验证
    // VersionLedger 语义 + 工具 dry-run 输出)
    let v: rutis_agent::VersionLedger =
        serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap()).unwrap();
    let prev = v.previous().unwrap();
    assert_eq!(prev.commit, "abc123", "上一代 = 倒数第二条");

    // 工具本身:先把测试台账写到工具约定路径(相对 crate 目录),跑完清理
    let real = PathBuf::from(VERSION_LEDGER_PATH);
    if let Some(parent) = real.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&real, std::fs::read_to_string(&ledger).unwrap()).unwrap();

    let _root = Ctx::root().unwrap();
    let def = rutis_agent::self_rollback_tool();
    let (ok, out) = run(&def, json!({})).await;
    assert!(ok, "{out}");
    assert!(out.contains("abc123"), "dry-run 报告上一代: {out}");
    assert!(out.contains("dry-run"), "{out}");
    assert!(!out.contains("rolled back"), "默认不执行: {out}");

    // 清理测试台账
    let _ = std::fs::remove_file(&real);
}

#[tokio::test]
async fn self_rollback_too_few_entries_errors() {
    let tmp = TempDir::new("rollback2");
    let ledger = tmp.join("version-ledger.json");
    std::fs::write(
        &ledger,
        r#"{"entries":[{"commit":"abc","at_ms":1,"note":"only"}]}"#,
    )
    .unwrap();
    let v: rutis_agent::VersionLedger =
        serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap()).unwrap();
    assert!(v.previous().is_none());

    let real = PathBuf::from(VERSION_LEDGER_PATH);
    if let Some(parent) = real.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&real, std::fs::read_to_string(&ledger).unwrap()).unwrap();

    let _root = Ctx::root().unwrap();
    let def = rutis_agent::self_rollback_tool();
    let (ok, out) = run(&def, json!({})).await;
    assert!(ok, "{out}");
    assert!(out.contains("fewer than 2 entries"), "{out}");

    let _ = std::fs::remove_file(&real);
}

// ── 集成:自控 turn ─────────────────────────────────────────────────

#[tokio::test]
async fn self_tools_registered_and_driven_in_turn() {
    let tmp = TempDir::new("turn");
    let path = tmp.join("session.json");
    let root = Ctx::root().unwrap();
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::tool_calls(vec![tool_call(
            "c1",
            "self_status",
            json!({}),
        )]),
        LlmResponse::content("status reported"),
    ]));
    let service: Arc<dyn LanguageModel> = llm.clone();
    root.provide_as(llm_key(), service).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::self_tools(root.clone())));
    let driver_view = root.plugin(
        AgentDriverPlugin::new(8)
            .with_session_path(&path)
            .with_system_prompt("you are a self-aware agent"),
    );
    soon(async {
        (&tools_view).await.expect("tools loads");
        (&driver_view).await.expect("driver loads");
    })
    .await;

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let out = soon(agent.followup("what is your status?")).await.unwrap();
    assert_eq!(out, "status reported");

    // 模型收到的 schema 含全部 6 个 self 工具
    let calls = llm.calls.lock().unwrap();
    let mut names: Vec<String> = calls[0]
        .tools
        .iter()
        .map(|t| match t {
            aimux_core::options::Tool::Function(f) => f.name.clone(),
            aimux_core::options::Tool::Provider(_) => "(provider)".to_string(),
        })
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "self_build",
            "self_check",
            "self_persist",
            "self_reload",
            "self_rollback",
            "self_status",
        ],
        "{names:?}"
    );

    soon(async {
        driver_view.dispose().await.unwrap();
        tools_view.dispose().await.unwrap();
    })
    .await;
}
