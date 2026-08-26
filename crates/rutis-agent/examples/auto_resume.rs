//! 自动续跑演示:agent 设 todo 后,自己连续跑,不需要用户每轮输入。
//!
//! 机制:AutoResume 监听 AgentTurnEnd,turn 结束有待办 → 自动 followup。
//!
//! 运行:`cargo run -p rutis-agent --example auto_resume`

use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, AutoResume, LlmResponse, ScriptedLlm, ToolsPlugin,
};

#[tokio::main]
async fn main() {
    // 脚本模型:每次 followup 返回一段文本(turn 都成功)
    // 模拟"任务分多步"——每轮返回一行,模型"做一步"
    let scripted = ScriptedLlm::new(vec![
        LlmResponse::content("step 1 done"),
        LlmResponse::content("step 2 done"),
        LlmResponse::content("step 3 done"),
        LlmResponse::content("step 4 done"),
        LlmResponse::content("step 5 done"),
        LlmResponse::content("step 6 done"),
    ]);

    let root = rutis::Ctx::root().unwrap();
    root.provide_as(llm_key(), rutis_agent::into_service(scripted)).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::minimal_tools()));
    let driver_view = root.plugin(AgentDriverPlugin::new(20));
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();

    // 装配自动续跑(上限 3 轮自动)
    root.events().on(&root, AutoResume::new(3)).unwrap();

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // 用户输入第一轮 + 设 todo:之后 agent 自己继续
    agent.set_todo("run steps until done".to_string());
    let out1 = agent.followup("start the task, I'll stop typing").await.unwrap();
    println!("user turn -> {out1}");

    // 等自动续跑跑几轮(AutoResume 每轮 end 后自动 followup)
    // 上限 3 → 自动跑 3 轮后停
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if agent.session().messages().len() >= 8 {
            break;
        }
    }

    let msgs = agent.session().messages().len();
    println!("session messages: {msgs} (user turn + auto turns)");
    assert!(
        msgs >= 6,
        "auto-resume should have continued the task: {msgs}"
    );
    println!("✅ 自动续跑:agent 设 todo 后自己连续跑,无需用户每轮输入");
    println!("   (上限 3 轮自动续跑,之后停下)");

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
}
