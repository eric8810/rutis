//! 自主驱动(SelfDriven)验收:
//! - 无 todo 也自我激活(核心:agent 不停下,自主继续)
//! - 空转检测:模型不产出(消息不增长)时停止(防浪费)

use std::sync::Arc;
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

/// 关键修复验证:SelfDriven 自主激活的 turn 不应被取消(cancelled)。
/// 之前的 bug:AgentTurnEnd 在 driver followup 内部 emit,SelfDriven 在
/// 事件栈内同步调 followup → status 仍 Running → 冲突取消。
/// 修复:turn_lock 互斥 + spawn 延迟,自主续跑的 turn 应全部成功。
#[tokio::test]
async fn auto_activation_turns_are_not_cancelled() {
    let root = Ctx::root().unwrap();
    // 3 个响应:用户 1 轮 + 自主 2 轮
    let responses: Vec<LlmResponse> = (0..3)
        .map(|i| LlmResponse::content(format!("round {i}")))
        .collect();
    root.provide_as(llm_key(), rutis_agent::into_service(ScriptedLlm::new(responses)))
        .unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(rutis_agent::minimal_tools()));
    let driver_view = root.plugin(AgentDriverPlugin::new(20));
    (&tools_view).await.unwrap();
    (&driver_view).await.unwrap();

    // 捕获 turn 结果:ok vs cancelled
    let results: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    struct Track(Arc<std::sync::Mutex<Vec<String>>>);
    impl rutis::Listener<rutis_agent::AgentTurnEnd> for Track {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a rutis_agent::AgentTurnEnd,
        ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
            let r = self.0.clone();
            let ok = e.ok;
            let err = e.error.clone();
            Box::pin(async move {
                r.lock().unwrap().push(if ok { "ok".to_string() } else { format!("fail: {err}") });
                Ok(None)
            })
        }
    }
    root.events().on(&root, Track(results.clone())).unwrap();

    // 装配 SelfDriven 后,用户启动 1 轮 → 自主续跑
    root.events().on(&root, SelfDriven::new()).unwrap();
    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();
    let _ = soon(agent.followup("start")).await.unwrap();

    // 等自主续跑 2 轮(响应耗尽后停止)
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    let results = results.lock().unwrap();
    eprintln!("turn results: {results:?}");
    // 所有 turn 都应成功,**没有任何 cancelled**
    assert!(
        results.iter().all(|r| r == "ok"),
        "自主激活的 turn 不应 cancelled: {results:?}"
    );

    let _ = driver_view.dispose().await;
    let _ = tools_view.dispose().await;
}
