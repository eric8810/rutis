//! 自主驱动演示:agent 没有 todo 也会自我激活持续进化。
//!
//! 两种模式:
//! - `--with-todo`:设 todo,验证按待办自动续跑。
//! - 默认:不设 todo,验证"无 todo 也自我激活"——每轮结束自动反思,
//!   自己启动下一轮(自我进化),直到空转检测停止。
//!
//! 运行:`cargo run -p rutis-agent --example self_driven [--with-todo]`

use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, LlmResponse, ScriptedLlm, SelfDriven, ToolsPlugin,
};

#[tokio::main]
async fn main() {
    let with_todo = std::env::args().any(|a| a == "--with-todo");

    // 脚本模型:每轮返回一段文本(模拟模型"做事")。消息数每轮 +2
    // (user + assistant),所以进展检测认为有进展,会持续自我激活,
    // 直到 ScriptedLlm 响应耗尽(弹完 Err → turn 失败 → 不续跑)。
    let responses: Vec<LlmResponse> = (0..6)
        .map(|i| LlmResponse::content(format!("self-driven round {i} done")))
        .collect();
    let scripted = ScriptedLlm::new(responses);

    let root = rutis::Ctx::root().unwrap();
    root.provide_as(llm_key(), rutis_agent::into_service(scripted)).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::minimal_tools()));
    let driver_view = root.plugin(AgentDriverPlugin::new(20));
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();

    root.events().on(&root, SelfDriven::new()).unwrap();
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    if with_todo {
        agent.set_todo("finish the task".to_string());
    }

    let out1 = agent.followup("start").await.unwrap();
    println!("user turn -> {out1}");

    // 等自我激活跑几轮
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if agent.status() == rutis_agent::AgentStatus::Idle && agent.session().messages().len() > 2 {
            break;
        }
    }

    let msgs = agent.session().messages().len();
    println!("session messages: {msgs}");
    assert!(msgs >= 6, "self-driven should continue: {msgs}");
    println!(
        "✅ {}:agent 不设 todo 也自我激活持续进化(消息数 {msgs})",
        if with_todo { "[with-todo] " } else { "[no-todo] " }
    );

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
}
