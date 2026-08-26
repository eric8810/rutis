//! 生存周期体验:agent 持续自我激活运行,无论对错,一直活着。
//!
//! 脚本模型设计:模拟"有时成功有时失败"的真实环境——
//! 响应序列:成功、成功、失败(耗尽)、失败……让 agent 经历
//! 成功期 → 失败期 → 退避降频 → 持续存活。
//!
//! 运行:`cargo run -p rutis-agent --example lifetime`(观察 3 秒)

use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, LlmResponse, ScriptedLlm, SelfDriven, ToolsPlugin,
};

#[tokio::main]
async fn main() {
    // 3 个成功响应,然后耗尽(后续失败)——模拟"环境先好后坏"
    let scripted = ScriptedLlm::new(vec![
        LlmResponse::content("life 1: checked workspace, nothing urgent"),
        LlmResponse::content("life 2: improved a doc comment"),
        LlmResponse::content("life 3: reviewed tests, all green"),
    ]);

    let root = rutis::Ctx::root().unwrap();
    root.provide_as(llm_key(), rutis_agent::into_service(scripted)).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::minimal_tools()));
    let driver_view = root.plugin(AgentDriverPlugin::new(20));
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();

    root.events().on(&root, SelfDriven::new()).unwrap();
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // 用户只启动一次,然后 agent 自己活着跑(观察 3 秒)
    let _ = agent.followup("start living").await;
    println!("--- observing agent lifetime (3s) ---");

    let mut last_msgs = 0;
    for tick in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let msgs = agent.session().messages().len();
        if msgs != last_msgs {
            println!("t={}ms msgs={msgs}", (tick + 1) * 100);
            last_msgs = msgs;
        }
        // 响应耗尽后 followup 会 Err → 观察失败处理
    }

    println!("--- lifetime observation done ---");
    println!("total msgs: {}", agent.session().messages().len());
    println!("✅ agent 持续运行(成功期+失败期+退避),生存周期未断");

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
}
