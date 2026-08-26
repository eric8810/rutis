//! 运行中更新 persona 验收:
//! - `Agent::update_persona` 替换 system prompt,下一轮生效
//! - `self_persona` 工具让 agent 自己更新(自我改善闭环)

use std::sync::Arc;
use std::time::Duration;

use rutis::Ctx;
use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, LlmResponse, ScriptedLlm, ToolDef, ToolsPlugin,
};
use serde_json::json;

async fn soon<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(10), f)
        .await
        .expect("timed out")
}

/// 收集 prompt 的 system 文本(listener 在 followup 前注册)。
fn load(root: &Ctx, responses: Vec<LlmResponse>) -> (rutis::FiberView, rutis::FiberView, Arc<ScriptedLlm>) {
    let llm = Arc::new(ScriptedLlm::new(responses));
    root.provide_as(llm_key(), llm.clone() as Arc<dyn aimux_core::language_model::LanguageModel>)
        .unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(vec![ToolDef::new(
        "noop",
        "noop",
        json!({ "type": "object", "properties": {} }),
        |_| async move { Ok(json!({})) },
    )]));
    let driver_view = root.plugin(
        AgentDriverPlugin::new(10).with_system_prompt("v1 persona: original"),
    );
    (tools_view, driver_view, llm)
}

/// update_persona 运行中替换,下一轮 prompt 的 system 是新 persona。
#[tokio::test]
async fn update_persona_takes_effect_next_turn() {
    let root = Ctx::root().unwrap();
    let (tv, dv, llm) = load(&root, vec![
        LlmResponse::content("turn 1"),
        LlmResponse::content("turn 2"),
    ]);
    (&tv).await.unwrap();
    (&dv).await.unwrap();
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // 第一轮:system 是 v1
    let _ = soon(agent.followup("q1")).await.unwrap();
    let calls = llm.calls.lock().unwrap();
    let joined: String = calls[0].message_texts().iter()
        .filter(|(r, _)| *r == aimux_core::message::Role::System)
        .map(|(_, t)| t.clone()).collect::<Vec<_>>().join("\n");
    assert!(joined.contains("v1 persona"), "第一轮 system 是 v1: {joined}");
    drop(calls);

    // 运行中更新 persona
    agent.update_persona("v2 persona: evolved cognition".to_string());

    // 第二轮:system 是 v2
    let _ = soon(agent.followup("q2")).await.unwrap();
    let calls = llm.calls.lock().unwrap();
    let joined: String = calls[1].message_texts().iter()
        .filter(|(r, _)| *r == aimux_core::message::Role::System)
        .map(|(_, t)| t.clone()).collect::<Vec<_>>().join("\n");
    assert!(joined.contains("v2 persona"), "第二轮 system 是 v2: {joined}");
    assert!(!joined.contains("v1 persona"), "v1 已替换: {joined}");

    let _ = dv.dispose().await;
    let _ = tv.dispose().await;
}

/// self_persona 工具:agent 自己调用它更新 persona,下一轮生效。
#[tokio::test]
async fn self_persona_tool_updates_own_persona() {
    let root = Ctx::root().unwrap();
    // 脚本:turn1 调 self_persona,turn2 正常
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::tool_calls(vec![rutis_agent::tool_call(
            "p1", "self_persona",
            json!({ "persona": "v3 persona: self-evolved" }),
        )]),
        LlmResponse::content("persona updated"),
        LlmResponse::content("turn after update"),
    ]));
    root.provide_as(llm_key(), llm.clone() as Arc<dyn aimux_core::language_model::LanguageModel>)
        .unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::self_tools(root.clone())));
    let driver_view = root.plugin(
        AgentDriverPlugin::new(10).with_system_prompt("v0 persona"),
    );
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // turn1:调 self_persona
    let _ = soon(agent.followup("evolve my persona")).await.unwrap();
    // turn2:system 应为 v3
    let _ = soon(agent.followup("next")).await.unwrap();
    let calls = llm.calls.lock().unwrap();
    let joined: String = calls[2].message_texts().iter()
        .filter(|(r, _)| *r == aimux_core::message::Role::System)
        .map(|(_, t)| t.clone()).collect::<Vec<_>>().join("\n");
    assert!(joined.contains("v3 persona"), "self_persona 后 system 是 v3: {joined}");
    assert!(!joined.contains("v0 persona"), "v0 已替换: {joined}");

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
}
