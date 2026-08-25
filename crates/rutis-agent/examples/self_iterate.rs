//! 自动迭代闭环(核心):运行中的 agent 自己加载新能力并立即使用。
//!
//! 流程:
//! 1. 预编译一个 cdylib 插件(librutis_hotplug_demo.so 或任意 .so)
//! 2. agent 启动,工具集含 hotplug_load
//! 3. turn 1:模型(scripted)调用 hotplug_load 加载 .so → 新工具注册
//! 4. turn 2:模型直接调用新工具(release_notes)→ 生效
//!
//! 运行:
//!   cargo build -p rutis-hotplug-demo
//!   cargo run -p rutis-agent --example self_iterate

use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, LlmResponse, ScriptedLlm, ToolsPlugin,
};
use serde_json::json;

#[tokio::main]
async fn main() {
    // 模型脚本:turn1 调 hotplug_load,turn2 调 release_notes
    let scripted = ScriptedLlm::new(vec![
        LlmResponse::tool_calls(vec![rutis_agent::tool_call(
            "l1",
            "hotplug_load",
            json!({ "path": "target/debug/librutis_hotplug_demo.so" }),
        )]),
        LlmResponse::content("loaded the plugin"),
        LlmResponse::tool_calls(vec![rutis_agent::tool_call("l2", "release_notes", json!({}))]),
        LlmResponse::content("got release notes from hot-loaded tool"),
    ]);

    let root = rutis::Ctx::root().unwrap();
    root.provide_as(llm_key(), rutis_agent::into_service(scripted)).unwrap();

    // 工具集 = self_*(含 hotplug_load)
    let tools = rutis_agent::self_tools(root.clone());
    let tools_view = root.plugin(ToolsPlugin::new(tools));
    let driver_view = root.plugin(AgentDriverPlugin::new(20));
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // turn 1:加载插件 → 新工具注册进运行中的 registry
    let out1 = agent.followup("load the release notes plugin").await.unwrap();
    println!("turn1 -> {out1}");

    // 验证:registry 里有 release_notes
    let registry = root
        .get_as::<rutis_agent::ToolRegistry>(rutis_agent::tools_key())
        .expect("registry");
    assert!(registry.get("release_notes").is_some(), "plugin tool registered");

    // turn 2:直接调用新工具
    let out2 = agent.followup("what are the release notes?").await.unwrap();
    println!("turn2 -> {out2}");
    assert!(
        out2.contains("hot-loaded tool"),
        "hot-loaded tool was called: {out2}"
    );

    println!("\n✅ 自动迭代闭环:运行中 agent 自己加载 .so 插件 → 新工具立即可用,无需重启");

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
}
