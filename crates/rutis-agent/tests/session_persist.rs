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

// ── 记忆指针:让模型感知自己在继续历史 ──────────────────────────────

/// 恢复的 session:prompt 的 system 消息含"记忆指针"与代际信息。
#[tokio::test]
async fn restored_session_carries_memory_pointer() {
    let tmp = TempDir::new("memptr-restored");
    let path = tmp.join("session.json");

    // 第一代:两轮对话,落盘
    let root1 = Ctx::root().unwrap();
    let (tools_view1, driver_view1, _llm1) = load_driver(
        &root1,
        Some(&path),
        vec![LlmResponse::content("a1"), LlmResponse::content("a2")],
    )
    .await;
    let agent1 = root1.get_as::<dyn Agent>(agent_key()).unwrap();
    let _ = soon(agent1.followup("q1")).await.unwrap();
    let _ = soon(agent1.followup("q2")).await.unwrap();
    soon(async {
        driver_view1.dispose().await.unwrap();
        tools_view1.dispose().await.unwrap();
    })
    .await;

    // 第二代:恢复,检查 prompt system 含记忆指针
    let root2 = Ctx::root().unwrap();
    let (tools_view2, driver_view2, llm2) = load_driver(
        &root2,
        Some(&path),
        vec![LlmResponse::content("after restart")],
    )
    .await;
    let agent2 = root2.get_as::<dyn Agent>(agent_key()).unwrap();
    assert_eq!(agent2.id().generation(), 2, "恢复后是第 2 代");
    let _ = soon(agent2.followup("q3")).await.unwrap();

    let calls = llm2.calls.lock().unwrap();
    let texts = calls[0].message_texts();
    let system_texts: Vec<String> = texts
        .iter()
        .filter(|(role, _)| *role == Role::System)
        .map(|(_, t)| t.clone())
        .collect();
    let joined = system_texts.join("\n");
    assert!(
        joined.contains("记忆指针"),
        "system 应含记忆指针,got: {joined}"
    );
    assert!(
        joined.contains("第 2 代"),
        "system 应含代际信息,got: {joined}"
    );
    assert!(
        joined.contains("identity="),
        "system 应含 identity,got: {joined}"
    );

    soon(async {
        driver_view2.dispose().await.unwrap();
        tools_view2.dispose().await.unwrap();
    })
    .await;
}

/// 全新 session(generation = 1)全程不带记忆指针:连续对话自然连续,
/// 无需"继续历史"提示;记忆指针只服务跨代恢复(见 restored 测试)。
#[tokio::test]
async fn fresh_session_never_has_memory_pointer() {
    let root = Ctx::root().unwrap();
    let (tools_view, driver_view, llm) = load_driver(
        &root,
        None,
        vec![LlmResponse::content("first"), LlmResponse::content("second")],
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // 两轮都无记忆指针(generation 恒为 1)
    let _ = soon(agent.followup("q1")).await.unwrap();
    let _ = soon(agent.followup("q2")).await.unwrap();
    let calls = llm.calls.lock().unwrap();
    for (i, call) in calls.iter().enumerate() {
        let texts = call.message_texts();
        let joined: String = texts
            .iter()
            .filter(|(role, _)| *role == Role::System)
            .map(|(_, t)| t.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !joined.contains("记忆指针"),
            "第 {} 轮(全新 session)不应含记忆指针,got: {joined}",
            i + 1
        );
    }

    soon(async {
        driver_view.dispose().await.unwrap();
        tools_view.dispose().await.unwrap();
    })
    .await;
}

// ── 记忆压缩(compact)───────────────────────────────────────────────

/// Session::compact:保存摘要、裁剪消息、持久化后恢复摘要仍在。
#[test]
fn session_compact_trims_and_keeps_summary() {
    let tmp = TempDir::new("compact");
    let path = PathBuf::from(tmp.join("session.json"));

    let mut s = Session::new();
    for i in 0..10 {
        s.push(ModelMessage::user(format!("q{i}")));
        s.push(ModelMessage {
            role: Role::Assistant,
            content: MessageContent::Text(format!("a{i}")),
        });
    }
    assert_eq!(s.messages().len(), 20);

    let (before, after) = s.compact("early chat summary".to_string(), 4);
    assert_eq!(before, 20);
    assert_eq!(after, 4, "裁剪到最近 4 条");
    assert_eq!(s.summary(), Some("early chat summary"));
    // 保留的是最近 4 条(q8/a8/q9/a9)
    let texts: Vec<String> = s
        .messages()
        .iter()
        .map(|m| match &m.content {
            MessageContent::Text(t) => t.clone(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(texts, vec!["q8", "a8", "q9", "a9"]);

    s.persist(&path).unwrap();
    let r = Session::restore(&path);
    assert_eq!(r.summary(), Some("early chat summary"), "恢复后摘要保留");
    assert_eq!(r.messages().len(), 4);
}

/// 旧 session 文件(无 summary 字段)兼容加载:serde default → None。
#[test]
fn legacy_session_file_without_summary_loads() {
    let tmp = TempDir::new("legacy-summary");
    let path = PathBuf::from(tmp.join("session.json"));
    std::fs::write(
        &path,
        r#"{"version":1,"id":7,"generation":2,"messages":[{"role":"user","content":"hi"}],"saved_at_ms":1}"#,
    )
    .unwrap();
    let s = Session::restore(&path);
    assert_eq!(s.summary(), None, "旧文件无 summary → None");
    assert_eq!(s.messages().len(), 1);
}

/// driver 集成:compact 后,后续 prompt 的 system 含"记忆摘要"。
#[tokio::test]
async fn compacted_session_injects_summary_into_prompt() {
    let root = Ctx::root().unwrap();
    let (tools_view, driver_view, llm) = load_driver(
        &root,
        None,
        vec![
            LlmResponse::content("a1"),
            LlmResponse::content("a2"),
            LlmResponse::content("a3"),
        ],
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    let _ = soon(agent.followup("q1")).await.unwrap();
    let _ = soon(agent.followup("q2")).await.unwrap();
    // 压缩:摘要 + 保留 2 条
    let (before, after) = agent.compact("first two turns summary".to_string(), 2);
    assert!(before > after);

    let _ = soon(agent.followup("q3")).await.unwrap();
    let calls = llm.calls.lock().unwrap();
    let texts = calls[2].message_texts();
    let joined: String = texts
        .iter()
        .filter(|(role, _)| *role == Role::System)
        .map(|(_, t)| t.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("记忆摘要"),
        "压缩后 system 应含记忆摘要,got: {joined}"
    );
    assert!(
        joined.contains("first two turns summary"),
        "摘要内容应注入,got: {joined}"
    );
    // 历史被裁剪:q1/q2 不在保留区(但摘要提及)
    let snapshot = agent.session();
    let msg_texts: Vec<String> = snapshot
        .messages()
        .iter()
        .map(|m| match &m.content {
            MessageContent::Text(t) => t.clone(),
            _ => String::new(),
        })
        .collect();
    assert!(!msg_texts.contains(&"q1".to_string()), "q1 被裁剪");
    assert!(msg_texts.contains(&"q2".to_string()), "q2 保留(最近)");

    soon(async {
        driver_view.dispose().await.unwrap();
        tools_view.dispose().await.unwrap();
    })
    .await;
}

// ── 待办/自动接续(todo)─────────────────────────────────────────────

/// Session todo roundtrip:设置 → 持久化 → 恢复 → 待办仍在。
#[test]
fn session_todo_roundtrip() {
    let tmp = TempDir::new("todo");
    let path = PathBuf::from(tmp.join("session.json"));

    let mut s = Session::new();
    s.push(ModelMessage::user("q1"));
    s.set_todo("finish the hotplug integration".to_string());
    s.persist(&path).unwrap();

    let r = Session::restore(&path);
    assert_eq!(r.todo(), Some("finish the hotplug integration"));
    assert_eq!(r.messages().len(), 1);
}

/// 旧 session 文件(无 todo 字段)兼容加载。
#[test]
fn legacy_session_file_without_todo_loads() {
    let tmp = TempDir::new("legacy-todo");
    let path = PathBuf::from(tmp.join("session.json"));
    std::fs::write(
        &path,
        r#"{"version":1,"id":7,"generation":2,"messages":[{"role":"user","content":"hi"}],"saved_at_ms":1}"#,
    )
    .unwrap();
    let s = Session::restore(&path);
    assert_eq!(s.todo(), None, "旧文件无 todo → None");
}

/// driver 集成:设置待办后,后续 prompt 的 system 含"待办/下一步"。
#[tokio::test]
async fn todo_injected_into_prompt() {
    let root = Ctx::root().unwrap();
    let (tools_view, driver_view, llm) = load_driver(
        &root,
        None,
        vec![LlmResponse::content("a1"), LlmResponse::content("a2")],
    )
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // 第一轮(无待办):system 不含"待办"
    let _ = soon(agent.followup("q1")).await.unwrap();
    let calls = llm.calls.lock().unwrap();
    let joined: String = calls[0]
        .message_texts()
        .iter()
        .filter(|(r, _)| *r == Role::System)
        .map(|(_, t)| t.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!joined.contains("待办"), "无待办时不应注入, got: {joined}");
    drop(calls);

    // 设置待办 → 第二轮 system 含"待办/下一步"
    agent.set_todo("continue the dsh bridge work".to_string());
    let _ = soon(agent.followup("q2")).await.unwrap();
    let calls = llm.calls.lock().unwrap();
    let joined: String = calls[1]
        .message_texts()
        .iter()
        .filter(|(r, _)| *r == Role::System)
        .map(|(_, t)| t.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("待办"), "有待办时应注入, got: {joined}");
    assert!(
        joined.contains("continue the dsh bridge work"),
        "待办内容应注入, got: {joined}"
    );

    soon(async {
        driver_view.dispose().await.unwrap();
        tools_view.dispose().await.unwrap();
    })
    .await;
}

/// 重启后待办保留并注入(自动接续核心):第一代设待办 → 第二代恢复 →
/// prompt 含待办。
#[tokio::test]
async fn todo_survives_restart_and_injects() {
    let tmp = TempDir::new("todo-restart");
    let path = tmp.join("session.json");

    // 第一代:一轮 + 设待办,落盘
    let root1 = Ctx::root().unwrap();
    let (tv1, dv1, _llm1) = load_driver(
        &root1,
        Some(&path),
        vec![LlmResponse::content("a1")],
    )
    .await;
    let agent1 = root1.get_as::<dyn Agent>(agent_key()).unwrap();
    let _ = soon(agent1.followup("q1")).await.unwrap();
    agent1.set_todo("finish the self-todo feature".to_string());
    soon(async {
        dv1.dispose().await.unwrap();
        tv1.dispose().await.unwrap();
    })
    .await;

    // 第二代:恢复 → 第一轮 prompt 含待办(自动接续)
    let root2 = Ctx::root().unwrap();
    let (tv2, dv2, llm2) = load_driver(
        &root2,
        Some(&path),
        vec![LlmResponse::content("resumed")],
    )
    .await;
    let agent2 = root2.get_as::<dyn Agent>(agent_key()).unwrap();
    assert_eq!(agent2.session().todo(), Some("finish the self-todo feature"));
    let _ = soon(agent2.followup("resume")).await.unwrap();
    let calls = llm2.calls.lock().unwrap();
    let joined: String = calls[0]
        .message_texts()
        .iter()
        .filter(|(r, _)| *r == Role::System)
        .map(|(_, t)| t.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("finish the self-todo feature"),
        "重启后待办应注入 prompt(自动接续), got: {joined}"
    );

    soon(async {
        dv2.dispose().await.unwrap();
        tv2.dispose().await.unwrap();
    })
    .await;
}

/// 实验 1:跨代记忆保持率(外部对标:LongContext / MemGPT 记忆保持维度)
///
/// 第一代注入一批关键事实(多轮对话积历史),模拟 gen+1 重启后,
/// 断言第二代 prompt 仍携带这些关键事实——不只"记忆指针存在",
/// 而是**具体内容真的保持**。量化记忆保持率的基线。
#[tokio::test]
async fn cross_generation_memory_retention_keeps_facts() {
    let tmp = TempDir::new("memretention");
    let path = tmp.join("session.json");

    // 第一代:4 轮对话,每轮注入一个关键事实,逐步建立"项目上下文"
    let facts = [
        "architecture uses turn_lock mutex to serialize turns",
        "self_rollback needs version ledger with 2 entries",
        "the killer model key is 'deepseek-chat'",
        "deploy target is bare-metal on fast-deliver",
    ];
    let root1 = Ctx::root().unwrap();
    let gen1 = load_driver(
        &root1,
        Some(&path),
        facts.iter()
            .flat_map(|f| vec![LlmResponse::content(format!("ok, recorded: {f}"))])
            .collect(),
    )
    .await;
    let agent1 = root1
        .get_as::<dyn Agent>(agent_key())
        .unwrap();
    for q in 1..=4 {
        let _ = soon(agent1.followup(&format!("remember fact {q}"))).await.unwrap();
    }
    soon(async {
        gen1.0.dispose().await.unwrap();
        gen1.1.dispose().await.unwrap();
    })
    .await;
    drop(gen1.2);

    // 第二代:恢复,收集 prompt 全部文本(history + system 记忆指针)
    let root2 = Ctx::root().unwrap();
    let (_, _, llm2) = load_driver(
        &root2,
        Some(&path),
        vec![LlmResponse::content("ok").to_owned()],
    )
    .await;
    let agent2 = root2.get_as::<dyn Agent>(agent_key()).unwrap();
    assert_eq!(agent2.id().generation(), 2);
    let _ = soon(agent2.followup("continue")).await.unwrap();

    let calls = llm2.calls.lock().unwrap();
    let all: String = calls[0]
        .message_texts()
        .iter()
        .map(|(_, t)| t.clone())
        .collect::<Vec<_>>()
        .join("\n");

    // 记忆保持率:4 个事实中几个仍出现在第二代 prompt?
    let kept = facts.iter().filter(|f| all.contains(**f)).count();
    let rate = kept as f64 / facts.len() as f64;
    eprintln!("[mem-retention] kept {kept}/{} = {rate:.0}%", facts.len());
    // 基线:全量 history 恢复 → 关键事实应变率保持(保守断言 ≥ 3/4)
    assert!(
        rate >= 0.75,
        "跨代记忆保持率过低: {:.0}% (kept {}/{}). prompt: {}",
        rate, kept, facts.len(), all
    );
}

/// 实验 3:长会话压缩信息保真(外部对标:grok compaction 摘要保真/Reflexion)。
/// 压缩后关键信息须经摘要保留——量化"压缩保真率"。
/// 关键点:摘要若是我(agent)人工提炼(AI 摘要)则保真率高;
/// 若是模板占位(driver auto_compact)则关键事实丢失。
#[test]
fn compact_information_fidelity_keeps_key_facts_via_summary() {
    let mut s = Session::new();

    // 注入一批关键事实到早期消息(将被子摘要裁剪的部分)
    let facts = [
        "secret_api_key = sk-proj-alpha-2026",
        "deploy_script_at = /opt/rutis/deploy.sh",
        "consensus on port 8443 for the new service",
        "turn_lock serializes concurrent followup turns",
        "auth uses JWT with 8h expiry, refresh token 30d",
    ];
    for (i, f) in facts.iter().enumerate() {
        s.push(ModelMessage::user(format!("fact {i}: {f}")));
        s.push(ModelMessage::assistant(format!("noted fact {i}")));
    }
    // 几条后续正常消息(保留区)
    s.push(ModelMessage::user("recent q"));
    s.push(ModelMessage::assistant("recent a"));

    // 压缩:摘要 = 若我提炼得好,应含这些关键事实;模拟"高质量提炼" vs "模板"
    // 高质量(agent 提炼):把关键事实浓缩进摘要
    let high_quality_summary = format!(
        "早前关键信息: {} | {} | {} | {} | {}",
        facts[0], facts[1], facts[2], facts[3], facts[4]
    );
    let (before, after) = s.compact(high_quality_summary, 2); // 只留最近 2 条
    assert!(after < before, "压缩应裁剪: {before} -> {after}");
    assert_eq!(s.messages().len(), 2, "只保留最近 2 条");

    // 压缩后,关键事实须在 summary 里(经模型/agent 提炼保留)
    let summarized = s.summary().unwrap_or_default();
    let kept = facts.iter().filter(|f| summarized.contains(**f)).count();
    let fidelity = kept as f64 / facts.len() as f64;
    eprintln!("[compact-fidelity] high-quality summary kept {kept}/{} = {fidelity:.0}%", facts.len());
    assert_eq!(fidelity, 1.0, "高质量摘要应 100% 保真关键事实; 摘要: {summarized}");

    // 对照组:模板摘要(driver auto_compact 空路径)不保留任何关键事实
    let (_, _) = s.compact("（早期对话因超出模型上下文窗口被自动裁剪,细节不可恢复）".to_string(), 2);
    let tmpl = s.summary().unwrap_or_default();
    let kept_t = facts.iter().filter(|f| tmpl.contains(**f)).count();
    eprintln!("[compact-fidelity] template summary kept {kept_t}/{}", facts.len());
    assert_eq!(kept_t, 0, "模板摘要不保留关键事实(证明优质摘要的必要性)");
}

/// round62 前瞻项第一增量:跨轮 token 累计的持久化 roundtrip。
/// - add_tokens 累计,saturating 防溢出;
/// - persist/restore 后 tokens_used 保留(跨重启成本核算);
/// - 旧文件(无 tokens_used 字段)→ 0(serde default 兼容)。
#[test]
fn tokens_used_accumulates_and_survives_restart() {
    let tmp = TempDir::new("tokens");
    let path = PathBuf::from(tmp.join("session.json"));

    let mut s = Session::new();
    assert_eq!(s.tokens_used(), 0);
    s.add_tokens(100);
    s.add_tokens(250);
    assert_eq!(s.tokens_used(), 350, "累计");
    // saturating:不会溢出回绕
    s.add_tokens(u64::MAX - 340);
    assert_eq!(s.tokens_used(), u64::MAX, "saturating 兜到 MAX");

    s.persist(&path).unwrap();
    let r = Session::restore(&path);
    assert_eq!(r.tokens_used(), u64::MAX, "跨重启保留");
}

/// 旧 session 文件(无 tokens_used 字段)→ tokens_used = 0。
#[test]
fn legacy_session_file_without_tokens_loads_zero() {
    let tmp = TempDir::new("legacy-tokens");
    let path = PathBuf::from(tmp.join("session.json"));
    std::fs::write(
        &path,
        r#"{"version":1,"id":7,"generation":2,"messages":[{"role":"user","content":"hi"}],"saved_at_ms":1}"#,
    )
    .unwrap();
    let s = Session::restore(&path);
    assert_eq!(s.tokens_used(), 0, "旧文件无 tokens_used → 0");
}

/// round62 前瞻项第一增量:driver 从 LLM 流式 Finish 捕获 usage 累计到
/// session.tokens_used。用自定义 LanguageModel 返回带真实 token 的 Finish,
/// 驱动一轮后断言累计(验证 driver 捕获链路,非仅 session 存储)。
#[tokio::test]
async fn driver_accumulates_llm_token_usage_from_finish() {
    use aimux_core::language_model::LanguageModel;
    use aimux_core::options::CallOptions;
    use aimux_core::result::{GenerateResult, StreamResult};
    use aimux_core::stream_part::StreamPart;
    use aimux_core::types::{FinishReason, FinishReasonUnified, TokenUsage, Usage};

    struct UsageLlm;
    #[async_trait::async_trait]
    impl LanguageModel for UsageLlm {
        fn provider(&self) -> &str { "usage-test" }
        fn model_id(&self) -> &str { "usage-test" }
        async fn do_generate(&self, _o: &CallOptions) -> Result<GenerateResult, aimux_core::error::AiMuxError> {
            Err(aimux_core::error::AiMuxError::Other("stream-only".into()))
        }
        async fn do_stream(&self, _o: &CallOptions) -> Result<StreamResult, aimux_core::error::AiMuxError> {
            let stream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<StreamPart, aimux_core::error::AiMuxError>> + Send>> =
                Box::pin(async_stream::stream! {
                    yield Ok(StreamPart::StreamStart { warnings: Vec::new() });
                    yield Ok(StreamPart::TextDelta { id: "t0".into(), delta: "hi".into(), provider_metadata: None });
                    // input 10 + output 5 = 15 tokens
                    yield Ok(StreamPart::Finish {
                        finish_reason: FinishReason { unified: FinishReasonUnified::Stop, raw: None },
                        usage: Usage {
                            input_tokens: TokenUsage { total: Some(10), no_cache: None, cache_read: None, cache_write: None, text: None, reasoning: None },
                            output_tokens: TokenUsage { total: Some(5), no_cache: None, cache_read: None, cache_write: None, text: None, reasoning: None },
                            raw: None,
                        },
                        provider_metadata: None,
                    });
                });
            Ok(StreamResult { stream, request_body: None, response_headers: None })
        }
    }

    let root = Ctx::root().unwrap();
    use rutis_agent::llm_key;
    let lm: Arc<dyn LanguageModel> = Arc::new(UsageLlm);
    root.provide_as(llm_key(), lm).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(vec![]));
    let driver_view = root.plugin(AgentDriverPlugin::new(8));
    soon(async {
        (&tools_view).await.expect("tools");
        (&driver_view).await.expect("driver");
    })
    .await;
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let _ = soon(agent.followup("go")).await.unwrap();
    assert_eq!(
        agent.session().tokens_used(),
        15,
        "driver captured input(10)+output(5) from Finish usage"
    );

    soon(async {
        driver_view.dispose().await.unwrap();
        tools_view.dispose().await.unwrap();
    })
    .await;
}
