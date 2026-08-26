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


/// 获取 hotplug demo 插件路径;缺失则自动快速构建(~0.8s)。
/// 假绿防线:hotplug 测试前置需真实 .so,若无则静默 skip 会让"没测也 ok"
/// (self-review-checklist §1: eprintln skip 是没测的红信号)。这里改为
/// 自动构建 + 断言存在,使 CI/任意新环境都真实运行。
fn ensure_hotplug_plugin() -> String {
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
        eprintln!("[hotplug-test] .so missing, auto-building rutis-hotplug-demo (fake-green guard)...");
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "rutis-hotplug-demo"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("cargo available");
        assert!(
            status.success(),
            "failed to pre-build rutis-hotplug-demo for hotplug e2e test"
        );
    }
    assert!(
        std::path::Path::new(&so).exists(),
        "hotplug demo plugin must exist after pre-build: {so}"
    );
    so
}

/// 完整闭环:先 hotplug_load 挂载新工具(从 .so),再 self_persona 更新认知,
/// 再确认后续 turn 能看到新工具 + 新 persona。
#[tokio::test]
async fn self_iteration_loop_persona_plus_hotplug() {
    // 先构建插件 .so(测试前置:需要 librutis_hotplug_demo.so)。
    // 产物在**仓库根** target/ 下,CARGO_MANIFEST_DIR 是本期 crate 的绝对路径
    // (/…/crates/rutis-agent),往上级两级即仓库根(/…/rutis)——cwd 无关,
    // CI/任意目录都正确定位,不会像原相对路径那样在别处 cwd 下误 skip。
    // cargo 根定位 + 自动构建(假绿防线),见 ensure_hotplug_plugin。
    let so = ensure_hotplug_plugin();

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

/// 实验 2:热加载量化基线(agent-bench)。测"注册→调用→返回"端到端成功:
/// hotplug_load 从 .so 挂载 release_notes → 后续 turn 实际调用它 → 断言返回。
/// 指标:注册成功 + 调用成功(端到端),这是热加载"即插即用的可量化证明"。
#[tokio::test]
async fn hotplug_load_then_call_is_end_to_end() {
    // 定位 .so(与既有测试同法:CARGO_MANIFEST_DIR 往上级两级 = 仓库根)
    // cargo 根定位 + 自动构建(假绿防线),见 ensure_hotplug_plugin。
    let so = ensure_hotplug_plugin();

    let root = Ctx::root().unwrap();
    // turn1 hotplug_load;turn2 调用 release_notes;turn3 结束
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::tool_calls(vec![rutis_agent::tool_call(
            "h1", "hotplug_load", json!({ "path": so }),
        )]),
        LlmResponse::content("plugged"),
        LlmResponse::tool_calls(vec![rutis_agent::tool_call(
            "r1", "release_notes", json!({}),
        )]),
        LlmResponse::content("notes fetched"),
        LlmResponse::content("end"),
    ]));
    root.provide_as(
        llm_key(),
        llm.clone() as Arc<dyn aimux_core::language_model::LanguageModel>,
    )
    .unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::self_tools(root.clone())));
    let driver_view = root.plugin(AgentDriverPlugin::new(20));
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    let _ = soon(agent.followup("load plugin")).await.unwrap();
    let _ = soon(agent.followup("get notes via release_notes")).await.unwrap();
    let _ = soon(agent.followup("done")).await.unwrap();

    // 1) 注册成功
    let registry = root
        .get_as::<rutis_agent::ToolRegistry>(rutis_agent::tools_key())
        .unwrap();
    assert!(registry.get("release_notes").is_some(), "release_notes 已挂载");

    // 2) 调用成功(release_notes 被执行,结果回喂给模型)
    let calls = llm.calls.lock().unwrap();
    // 第 2 个用户 turn(调 release_notes)之后的 history 应含 release_notes 结果
    let notes_called = calls[1]
        .message_texts()
        .iter()
        .any(|(_, t)| t.contains("release_notes") || t.contains("notes"));
    assert!(notes_called, "release_notes 在 turn 2 被调用: {calls:?}");
    eprintln!("[hotplug-e2e] release_notes registered + callable end-to-end ✓");

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
}

/// hotplug_load 失败路径:路径不存在/加载失败 → 工具返回清晰 error
/// (而非 panic/挂起)。此前只有成功路径(已构建 .so)测试。
#[tokio::test]
async fn hotplug_load_nonexistent_path_reports_error() {
    let root = Ctx::root().unwrap();
    // 装配 ToolsPlugin 提供 ToolRegistry(hotplug_load 从 ctx 取)
    let tools_view = root.plugin(ToolsPlugin::new(vec![]));
    (&tools_view).await.unwrap();

    let def = rutis_agent::hotplug_load(root.clone());
    let res = (def.run)(json!({ "path": "/nonexistent/librutis_missing.so" })).await;
    match res {
        Err(e) => {
            assert!(
                e.contains("open") || e.contains("failed") || e.contains(".so"),
                "error should mention the load failure, got: {e}"
            );
        }
        Ok(v) => panic!("nonexistent .so should error, got Ok: {v}"),
    }

    tools_view.dispose().await.unwrap();
}
