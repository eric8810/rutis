//! 自主生存进程:承载 persona v2 + SelfDriven 生存周期引擎的 agent,
//! 启动后持续自主进化——每轮反思→执行→提交→下一轮,永不退出。
//!
//! 运行(需 DEEPSEEK_API_KEY):
//!   cargo run -p rutis-cli --example self_living

use std::sync::Arc;
use std::time::Duration;

use rutis::Ctx;
use rutis_agent::{
    agent_key, llm_key, minimal_persona, self_tools, Agent, AgentDriverPlugin, SelfDriven,
    ToolsPlugin,
};

#[tokio::main]
async fn main() {
    let provider = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    let model = std::env::var("AIMUX_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
    let env_key = format!("{}_API_KEY", provider.to_uppercase().replace('-', "_"));
    if std::env::var_os(&env_key).is_none() && provider != "ollama" {
        eprintln!("需要 {env_key}(或本地 ollama)");
        return;
    }
    let llm = aimux_providers::provider(&provider, None, &model, None)
        .unwrap_or_else(|e| panic!("build {provider}/{model}: {e}"));
    let llm: Arc<dyn aimux_core::language_model::LanguageModel> = Arc::from(llm);

    let cwd = std::env::current_dir().unwrap_or_default();
    let root = Ctx::root().unwrap();
    root.provide_as(llm_key(), llm).unwrap();

    // 工具集 = self_*(含 self_todo/self_build/hotplug_load)+ bash + replace_text
    let mut tools = self_tools(root.clone());
    tools.extend(rutis_agent::minimal_tools());
    let tools_view = root.plugin(ToolsPlugin::new(tools));
    let driver_view = root.plugin(
        AgentDriverPlugin::new(200)
            .with_system_prompt(minimal_persona(&model, &cwd.to_string_lossy()))
            .with_default_session_path(),
    );
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();

    // 生存周期引擎:每轮自主反思、自我激活、退避降频、永不停止
    root.events().on(&root, SelfDriven::new()).unwrap();

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    eprintln!("[self-living] 自主进程启动。Ctrl-C 停止。");

    // 第一轮:开始自主进化(之后 SelfDriven 自动续跑)
    let _ = agent
        .followup("开始自主进化:审视工作区,做一件最有价值的事(检查未提交/测试/改进/文档),完成后 commit + push。之后自主继续。")
        .await;

    // 保持存活:SelfDriven 每轮 turn end 自动 followup,进程永不退出
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        eprintln!(
            "[self-living] alive, msgs={}, gen={}",
            agent.session().messages().len(),
            agent.id().generation()
        );
    }
}
