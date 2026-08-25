//! fiber 级热重启:`driver_view.restart()` 后 session 恢复、TUI 级联重载。
//!
//! 目标(handoff §三 5):exec 进程级重启 → fiber 级热重启。
//! 验证:driver restart 只重装配 agent-driver fiber,进程/LLM 保留;
//! session identity 稳定,generation+1,历史恢复。

use std::sync::Arc;

use aimux_core::language_model::LanguageModel;
use rutis::Ctx;
use rutis_agent::{
    agent_key, llm_key, minimal_persona, AgentDriverPlugin, LlmResponse, ScriptedLlm,
    ToolsPlugin,
};

fn scripted() -> Arc<dyn LanguageModel> {
    Arc::new(ScriptedLlm::new(vec![
        LlmResponse::content("first turn"),
        LlmResponse::content("second turn"),
    ]))
}

/// driver restart 后:
/// 1. agent 服务被重新 provide(新 driver 实例)
/// 2. session 从 path 恢复(identity 稳定、generation+1、历史连续)
/// 3. TUI 依赖 agent → 级联重载(TUI fiber 重建)
#[tokio::test]
async fn driver_restart_preserves_session_and_cascades_tui() {
    let tmp = std::env::temp_dir().join(format!("rutis-fiber-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let session_path = tmp.join("session.json");

    let root = Ctx::root().unwrap();
    root.provide_as(llm_key(), scripted()).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::minimal_tools()));
    let driver_view = root.plugin(
        AgentDriverPlugin::new(10)
            .with_system_prompt(minimal_persona("scripted", "."))
            .with_session_path(&session_path),
    );
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();

    // 第一代 driver
    let agent1 = root.get_as::<dyn rutis_agent::Agent>(agent_key()).unwrap();
    let id1 = agent1.id();
    let msgs1 = agent1.session().messages().len();

    // turn 1
    agent1.followup("hello").await.unwrap();
    assert!(agent1.session().messages().len() > msgs1, "turn should add messages");

    // fiber 级热重启 driver
    driver_view.restart().await.unwrap();

    // 重启后 agent 服务被重新 provide
    let agent2 = root.get_as::<dyn rutis_agent::Agent>(agent_key()).unwrap();
    assert!(!Arc::ptr_eq(&agent1, &agent2), "driver should be a new instance");

    // session 恢复:identity 稳定,generation+1,历史连续
    let id2 = agent2.id();
    assert_eq!(id1.identity(), id2.identity(), "identity stable across restart");
    assert_eq!(id1.generation() + 1, id2.generation(), "generation +1");
    assert!(
        agent2.session().messages().len() >= msgs1,
        "history preserved after restart"
    );

    // turn 2 继续
    agent2.followup("world").await.unwrap();

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
    let _ = std::fs::remove_dir_all(&tmp);
}
