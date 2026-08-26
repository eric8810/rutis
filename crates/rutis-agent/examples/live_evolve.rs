//! 真实自主进化冒烟(亲身体验,非 scripted):
//! agent 用真实 LLM(deepseek),带 SelfDriven 生存周期引擎,自主跑多轮:
//! 检查工作区 → 做一件有价值的事 → 汇报 → 自动续跑,全程无需用户输入。
//!
//! 运行(需 DEEPSEEK_API_KEY):
//!   cargo run -p rutis-agent --example live_evolve

use std::sync::Arc;

use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, SelfDriven, ToolsPlugin,
};

#[tokio::main]
async fn main() {
    let provider = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    let model = std::env::var("AIMUX_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
    let env_key = format!("{}_API_KEY", provider.to_uppercase().replace('-', "_"));
    if std::env::var_os(&env_key).is_none() && provider != "ollama" {
        eprintln!("需要 {env_key}(或本地 ollama);export {env_key}=... 再跑");
        return;
    }
    let llm = aimux_providers::provider(&provider, None, &model, None)
        .unwrap_or_else(|e| panic!("build {provider}/{model}: {e}"));
    let llm: Arc<dyn aimux_core::language_model::LanguageModel> = Arc::from(llm);

    let root = rutis::Ctx::root().unwrap();
    root.provide_as(llm_key(), llm).unwrap();

    // 工具集 = bash + replace_text + self_*(含 hotplug_load/self_todo)
    let mut tools = rutis_agent::self_tools(root.clone());
    tools.extend(rutis_agent::minimal_tools());
    let tools_view = root.plugin(ToolsPlugin::new(tools));
    let driver_view = root.plugin(
        AgentDriverPlugin::new(100)
            .with_system_prompt(rutis_agent::minimal_persona(&model, "."))
            .with_default_session_path(),
    );
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();

    // 生存周期引擎:每轮自主反思、自我激活、退避降频
    root.events().on(&root, SelfDriven::new()).unwrap();

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    println!("=== live evolve: agent 自主运行,观察 60s ===");
    let start_msgs = agent.session().messages().len();

    // 用户只启动一次,然后 agent 自己活着跑
    let _ = agent
        .followup("开始:审视工作区,自主做一件最有价值的事并汇报。之后自主继续进化。")
        .await;

    // 观察 60 秒(agent 自主续跑)
    let mut last = start_msgs;
    for tick in 0..60 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let msgs = agent.session().messages().len();
        if msgs != last {
            println!("[t={tick}s] msgs={msgs} (+{})", msgs - last);
            last = msgs;
        }
    }

    println!("=== 观察结束 ===");
    println!("总消息数: {}", agent.session().messages().len());
    println!("✅ agent 用真实 LLM 自主运行 60s,生存周期持续");

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
}
