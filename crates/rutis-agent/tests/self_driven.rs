//! 自主驱动(SelfDriven)验收:
//! - 无 todo 也自我激活(核心:agent 不停下,自主继续)
//! - 空转检测:模型不产出(消息不增长)时停止(防浪费)

use std::time::Duration;

use rutis::Ctx;
use rutis_agent::{
    agent_key, llm_key, Agent, AgentDriverPlugin, LlmResponse, ScriptedLlm, SelfDriven, ToolsPlugin,
};

async fn soon<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(10), f)
        .await
        .expect("timed out")
}

fn load(
    root: &Ctx,
    responses: Vec<LlmResponse>,
) -> (rutis::FiberView, rutis::FiberView) {
    root.provide_as(llm_key(), rutis_agent::into_service(ScriptedLlm::new(responses)))
        .unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::minimal_tools()));
    let driver_view = root.plugin(AgentDriverPlugin::new(20));
    (tools_view, driver_view)
}

/// 无 todo:用户一轮后,agent 自我激活持续跑(消息数增长),直到响应耗尽。
#[tokio::test]
async fn self_activates_without_todo() {
    let root = Ctx::root().unwrap();
    let (tv, dv) = load(
        &root,
        (0..4).map(|i| LlmResponse::content(format!("round {i}"))).collect(),
    );
    (&tv).await.unwrap();
    (&dv).await.unwrap();
    root.events().on(&root, SelfDriven::new()).unwrap();

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let _ = soon(agent.followup("start")).await.unwrap();
    let start_msgs = agent.session().messages().len();

    // 等自我激活跑(最多 1s)
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let msgs = agent.session().messages().len();
    assert!(
        msgs > start_msgs,
        "无 todo 也应自我激活: start={start_msgs} now={msgs}"
    );

    let _ = dv.dispose().await;
    let _ = tv.dispose().await;
}

/// 空转检测:模型不产出(消息不增长)时,SelfDriven 停止(不无限循环)。
#[tokio::test]
async fn stops_when_no_progress() {
    let root = Ctx::root().unwrap();
    // 脚本模型:第一轮成功,后续无响应(耗尽 → Err → 失败 turn)
    let (tv, dv) = load(&root, vec![LlmResponse::content("only once")]);
    (&tv).await.unwrap();
    (&dv).await.unwrap();
    root.events().on(&root, SelfDriven::new()).unwrap();

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let _ = soon(agent.followup("start")).await.unwrap();
    let msgs_after_first = agent.session().messages().len();

    // 等 1s:如果 SelfDriven 不防浪费,会无限续跑(但脚本耗尽会失败);
    // 这里断言:不会无限增长(失败 turn 不续跑,空转检测兜底)
    tokio::time::sleep(Duration::from_millis(500)).await;
    let msgs = agent.session().messages().len();
    // 脚本只有 1 个响应 → 最多再失败几次;消息数不应无限涨
    assert!(
        msgs < msgs_after_first + 20,
        "空转应停止: after_first={msgs_after_first} now={msgs}"
    );

    let _ = dv.dispose().await;
    let _ = tv.dispose().await;
}
