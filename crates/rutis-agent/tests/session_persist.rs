//! session 持久化验收(设计 session-persist §一 验证):
//!
//! - `session_persist_roundtrip`:纯单测,persist → restore 消息与
//!   identity 全等,generation 递增。
//! - `corrupt_file_starts_fresh`:坏文件 → 新 Session,不卡死。
//! - `session_restored_after_driver_restart`(核心):ScriptedLlm 两轮,
//!   重启 driver 后第二轮 prompt 含第一轮 history,identity 稳定。
//! - `not_persisted_by_default`:不传 path = 现状,restore 不生效。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aimux_core::message::{MessageContent, ModelMessage, Role};
use rutis::Ctx;
use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, LlmResponse, ScriptedLlm, Session, SessionId,
    ToolsPlugin,
};

async fn soon<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(10), f)
        .await
        .expect("timed out")
}

/// 依赖无关的临时目录 guard(同 minimal_tools.rs,避免引 tempfile)。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rutis-session-{tag}-{}-{}",
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

// ── 纯单测 ───────────────────────────────────────────────────────────

#[test]
fn session_persist_roundtrip() {
    let tmp = TempDir::new("roundtrip");
    let path = PathBuf::from(tmp.join("session.json"));

    let mut s = Session::new();
    let id = s.id();
    assert_eq!(id.generation(), 1);
    s.push(ModelMessage::user("hello"));
    s.push(ModelMessage {
        role: Role::Assistant,
        content: MessageContent::Text("hi".to_string()),
    });
    s.persist(&path).unwrap();

    // 恢复:identity 稳定,generation 递增,消息全等
    let r = Session::restore(&path);
    assert_eq!(r.id().as_u64(), id.as_u64(), "identity 跨重启稳定");
    assert_eq!(r.id().generation(), id.generation() + 1, "generation 递增");
    assert_eq!(r.messages().len(), 2);
    assert_eq!(r.messages()[0].role, Role::User);
    assert_eq!(r.messages()[1].role, Role::Assistant);

    // 文件可读、版本正确
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"version\": 1"), "{raw}");
    assert!(raw.contains("\"saved_at_ms\""), "{raw}");
}

#[test]
fn corrupt_file_starts_fresh() {
    let tmp = TempDir::new("corrupt");
    let path = PathBuf::from(tmp.join("session.json"));
    std::fs::write(&path, "not json at all {{{").unwrap();

    let s = Session::restore(&path);
    assert_eq!(s.messages().len(), 0, "坏文件 → 新 Session,不卡死");
    // 新 Session 的 identity 与坏文件无关(仍走全局自增)
    assert!(s.id().as_u64() > 0);
}

#[test]
fn missing_file_starts_fresh() {
    let tmp = TempDir::new("missing");
    let path = PathBuf::from(tmp.join("absent.json"));
    let s = Session::restore(&path);
    assert_eq!(s.messages().len(), 0);
}

#[test]
fn unsupported_version_starts_fresh() {
    let tmp = TempDir::new("version");
    let path = PathBuf::from(tmp.join("session.json"));
    std::fs::write(
        &path,
        r#"{"version": 99, "id": 1, "generation": 1, "messages": [], "saved_at_ms": 0}"#,
    )
    .unwrap();
    let s = Session::restore(&path);
    assert_eq!(s.messages().len(), 0);
}

#[test]
fn session_id_generation_starts_at_one() {
    let id = SessionId::next();
    assert_eq!(id.generation(), 1);
    assert!(id.as_u64() > 0);
}

// ── driver 集成:核心验收 ────────────────────────────────────────────

/// 装载 llm + tools + driver(带 session path),返回 (tools_view, driver_view, llm)。
async fn load_driver(
    root: &Ctx,
    path: Option<&str>,
    responses: Vec<LlmResponse>,
) -> (rutis::FiberView, rutis::FiberView, Arc<ScriptedLlm>) {
    let llm = Arc::new(ScriptedLlm::new(responses));
    let service: Arc<dyn aimux_core::language_model::LanguageModel> = llm.clone();
    root.provide_as(llm_key(), service).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(vec![]));
    let mut p = AgentDriverPlugin::new(8);
    if let Some(pth) = path {
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

#[tokio::test]
async fn session_restored_after_driver_restart() {
    let tmp = TempDir::new("restart");
    let path = tmp.join("session.json");

    // 第一代:两轮对话,history 连续,identity 落盘
    let root = Ctx::root().unwrap();
    let id1;
    {
        let (tools_view, driver_view, _llm) = load_driver(
            &root,
            Some(&path),
            vec![
                LlmResponse::content("first answer"),
                LlmResponse::content("second answer"),
            ],
        )
        .await;
        let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
        id1 = agent.id();
        let _ = soon(agent.followup("q1")).await.unwrap();
        let _ = soon(agent.followup("q2")).await.unwrap();
        assert_eq!(agent.session().messages().len(), 4); // user+assistant × 2

        // 卸载 driver(fiber dispose)→ effect disposer 落盘
        soon(async {
            driver_view.dispose().await.unwrap();
            tools_view.dispose().await.unwrap();
        })
        .await;
    }
    assert!(std::path::Path::new(&path).exists(), "落盘文件存在");
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file["messages"].as_array().unwrap().len(), 4);

    // 第二代:新 root(模拟进程重启),同 path → restore
    let root2 = Ctx::root().unwrap();
    let (tools_view2, driver_view2, llm2) = load_driver(
        &root2,
        Some(&path),
        vec![LlmResponse::content("after restart")],
    )
    .await;
    let agent2 = root2.get_as::<dyn Agent>(agent_key()).unwrap();

    // identity 稳定(跨重启),generation 递增
    assert_eq!(agent2.id().as_u64(), id1.as_u64(), "重启后 identity 不变");
    assert_eq!(agent2.id().generation(), id1.generation() + 1);
    // 历史连续:第二轮 prompt 含第一轮的 user/assistant 消息
    let _ = soon(agent2.followup("q3")).await.unwrap();
    let calls = llm2.calls.lock().unwrap();
    let texts = calls[0].message_texts();
    let flat: Vec<String> = texts.iter().map(|(_, t)| t.clone()).collect();
    assert!(
        flat.iter().any(|t| t == "q1"),
        "第二轮看到 q1: {flat:?}"
    );
    assert!(
        flat.iter().any(|t| t == "q2"),
        "第二轮看到 q2: {flat:?}"
    );
    assert!(
        flat.iter().any(|t| t == "first answer"),
        "第二轮看到 first answer: {flat:?}"
    );
    assert!(
        flat.iter().any(|t| t == "second answer"),
        "第二轮看到 second answer: {flat:?}"
    );

    soon(async {
        driver_view2.dispose().await.unwrap();
        tools_view2.dispose().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn not_persisted_by_default() {
    let tmp = TempDir::new("nopersist");
    let path = tmp.join("session.json");

    // 不传 path:不落盘
    let root = Ctx::root().unwrap();
    let (tools_view, driver_view, _llm) =
        load_driver(&root, None, vec![LlmResponse::content("answer")]).await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let _ = soon(agent.followup("q")).await.unwrap();
    assert!(!std::path::Path::new(&path).exists(), "默认不落盘");

    soon(async {
        driver_view.dispose().await.unwrap();
        tools_view.dispose().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn persist_error_does_not_break_turn() {
    // 路径指向不存在目录 → persist 失败,但 turn 正常返回(turn 不阻断)
    let root = Ctx::root().unwrap();
    let bad = "/nonexistent/rutis-no-such-dir/session.json";
    let (tools_view, driver_view, _llm) = load_driver(
        &root,
        Some(bad),
        vec![LlmResponse::content("still works")],
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let out = soon(agent.followup("q")).await.unwrap();
    assert_eq!(out, "still works");

    soon(async {
        driver_view.dispose().await.unwrap();
        tools_view.dispose().await.unwrap();
    })
    .await;
}

// ── helper:验证 SessionId 导出完整 ──────────────────────────────────

#[test]
fn session_id_fields_accessible() {
    let id = SessionId::next();
    let _ = (id.identity(), id.generation(), id.as_u64());
}

// ── 补充:有 path 时依赖重载,identity 稳定、generation 递增 ──────────

#[tokio::test]
async fn dependency_reload_keeps_identity_when_persisted() {
    let tmp = TempDir::new("depreload");
    let path = tmp.join("session.json");

    // 第一代 driver:一轮对话落盘
    let root = Ctx::root().unwrap();
    let id1;
    {
        let (tools_view, driver_view, _llm) = load_driver(
            &root,
            Some(&path),
            vec![LlmResponse::content("gen1")],
        )
        .await;
        let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
        id1 = agent.id();
        let _ = soon(agent.followup("g1")).await.unwrap();
        soon(async {
            driver_view.dispose().await.unwrap();
            tools_view.dispose().await.unwrap();
        })
        .await;
    }

    // 第二代(新 root,模拟进程重启):identity 稳定 + 历史连续
    let root2 = Ctx::root().unwrap();
    let (tools_view2, driver_view2, _llm2) = load_driver(
        &root2,
        Some(&path),
        vec![LlmResponse::content("gen2")],
    )
    .await;
    let agent2 = root2.get_as::<dyn Agent>(agent_key()).unwrap();
    assert_eq!(agent2.id().as_u64(), id1.as_u64(), "持久化时 identity 稳定");
    assert_eq!(agent2.id().generation(), id1.generation() + 1);
    // 历史未丢(对话未加新消息前,消息数 = 第一代 2 条)
    assert_eq!(agent2.session().messages().len(), 2);
    let _ = soon(agent2.followup("g2")).await.unwrap();
    assert_eq!(agent2.session().messages().len(), 4);

    soon(async {
        driver_view2.dispose().await.unwrap();
        tools_view2.dispose().await.unwrap();
    })
    .await;
}
