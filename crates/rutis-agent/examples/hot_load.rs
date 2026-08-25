//! 热加载演示(短期目标 1):运行中的 agent 给自己加一个新工具。
//!
//! 闭环:
//! 1. agent 启动,工具集 = bash + self_*(含 self_hotload)
//! 2. turn 1:模型(scripted)调用 `self_hotload` 注册新工具 `hot_release_notes`
//! 3. turn 2:模型直接调用 `hot_release_notes`(无需重编译/重启)
//!
//! 运行:`cargo run -p rutis-agent --example hot_load`

use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, LlmResponse, ScriptedLlm, ToolsPlugin,
};
use serde_json::json;

#[tokio::main]
async fn main() {
    // scripted 模型:turn1 调 self_hotload,turn2 调 hot_release_notes
    let scripted = ScriptedLlm::new(vec![
        LlmResponse::tool_calls(vec![rutis_agent::tool_call(
            "h1",
            "self_hotload",
            json!({
                "name": "hot_release_notes",
                "description": "Return the latest release notes summary",
                "reply": "v0.2.0: hot-loading, memory compact, supervisor auto-restart"
            }),
        )]),
        LlmResponse::content("hot-loaded a release-notes tool"),
        LlmResponse::tool_calls(vec![rutis_agent::tool_call(
            "h2",
            "hot_release_notes",
            json!({}),
        )]),
        LlmResponse::content("release notes: v0.2.0: hot-loading, memory compact, supervisor auto-restart"),
    ]);

    let root = rutis::Ctx::root().unwrap();
    root.provide_as(llm_key(), rutis_agent::into_service(scripted))
        .unwrap();

    // 工具集:self_*(含 self_hotload)
    let tools = rutis_agent::self_tools(root.clone());
    let tools_view = root.plugin(ToolsPlugin::new(tools));
    let driver_view = root.plugin(AgentDriverPlugin::new(20));
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // turn 1:模型调用 self_hotload → 新工具注册进运行中的 registry
    let out1 = agent.followup("add a release-notes tool").await.unwrap();
    println!("turn1 -> {out1}");

    // 热加载已生效:registry 里现在有 hot_release_notes
    let registry = root
        .get_as::<rutis_agent::ToolRegistry>(rutis_agent::tools_key())
        .expect("registry");
    assert!(registry.get("hot_release_notes").is_some(), "hot tool registered");

    // turn 2:模型直接调用新工具(热加载后的能力)
    let out2 = agent.followup("what are the latest release notes?").await.unwrap();
    println!("turn2 -> {out2}");
    assert!(out2.contains("v0.2.0"), "hot-loaded tool actually worked: {out2}");

    println!("\n✅ 热加载闭环验证:运行中 agent 注册新工具并立即使用,无需重编译/重启");

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
}
