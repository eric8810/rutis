//! 自我迭代闭环验收:agent 运行中 ① 更新认知(self_persona)→
//! ② 挂载新工具(hotplug_load)→ ③ 自主续跑(SelfDriven)。
//! 这是"自我迭代"三件套的联动验证。

use std::sync::Arc;
use std::time::Duration;

use rutis::Ctx;
use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, LlmResponse, ScriptedLlm, ToolsPlugin,
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

/// 完整闭环:先 hotplug_load 挂载新工具(从 .so),再 self_persona 更新认知,
/// 再确认后续 turn 能看到新工具 + 新 persona。
#[tokio::test]
async fn self_iteration_loop_persona_plus_hotplug() {
    // 先构建插件 .so(测试前置:需要 librutis_hotplug_demo.so)。
    // 产物在**仓库根** target/ 下,CARGO_MANIFEST_DIR 是本期 crate 的绝对路径
    // (/…/crates/rutis-agent),往上级两级即仓库根(/…/rutis)——cwd 无关,
    // CI/任意目录都正确定位,不会像原相对路径那样在别处 cwd 下误 skip。
    let repo_root = {
        let m = env!("CARGO_MANIFEST_DIR");
        let p = std::path::Path::new(m);
        p.parent().unwrap().parent().unwrap()
    };
    let so = repo_root
        .join("target/debug/librutis_hotplug_demo.so")
        .to_string_lossy()
        .into_owned();
    if !std::path::Path::new(&so).exists() {
        eprintln!("skip: {so} not built (run cargo build -p rutis-hotplug-demo)");
        return;
    }

    let root = Ctx::root().unwrap();
    let llm = Arc::new(ScriptedLlm::new(vec![
        // turn1:调 self_persona 更新认知
        LlmResponse::tool_calls(vec![rutis_agent::tool_call(
            "p1", "self_persona",
            json!({ "persona": "evolved persona v4" }),
        )]),
        LlmResponse::content("persona updated"),
        // turn2:调 hotplug_load 挂载新工具
        LlmResponse::tool_calls(vec![rutis_agent::tool_call(
            "h1", "hotplug_load",
            json!({ "path": so }),
        )]),
        LlmResponse::content("hotplug loaded"),
        // turn3:终态
        LlmResponse::content("done"),
    ]));
    root.provide_as(llm_key(), llm.clone() as Arc<dyn aimux_core::language_model::LanguageModel>)
        .unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::self_tools(root.clone())));
    let driver_view = root.plugin(
        AgentDriverPlugin::new(20).with_system_prompt("v0 persona"),
    );
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // turn1:更新认知
    let _ = soon(agent.followup("evolve")).await.unwrap();
    // turn2:挂载新工具
    let _ = soon(agent.followup("load plugin")).await.unwrap();
    // turn3:终态
    let _ = soon(agent.followup("done")).await.unwrap();

    // 验证 1:新工具已挂载(release_notes 在 registry)
    let registry = root
        .get_as::<rutis_agent::ToolRegistry>(rutis_agent::tools_key())
        .unwrap();
    assert!(registry.get("release_notes").is_some(), "hotplug 挂载了新工具");

    // 验证 2:认知已更新(第 3 轮 prompt 的 system 是 evolved persona v4)
    let calls = llm.calls.lock().unwrap();
    let joined: String = calls[2].message_texts().iter()
        .filter(|(r, _)| *r == aimux_core::message::Role::System)
        .map(|(_, t)| t.clone()).collect::<Vec<_>>().join("\n");
    assert!(joined.contains("evolved persona v4"), "第3轮 system 是 v4: {joined}");
    assert!(!joined.contains("v0 persona"), "v0 已替换");

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
}
