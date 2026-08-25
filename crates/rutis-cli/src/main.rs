//! rutis-cli——最小 coding agent 的命令行形态(minimal mode)。
//!
//! ```text
//! export DEEPSEEK_API_KEY=... && rutis-cli                    # deepseek-chat
//! rutis-cli --provider ollama --model qwen3:8b                # 本地模型
//! AIMUX_PROVIDER=ollama AIMUX_MODEL=qwen3:8b rutis-cli        # 环境变量等价
//! rutis-cli --scripted                                        # 无 key 离线演示
//! ```
//!
//! 工具集 = `bash` + `replace_text`(能改文件、能跑命令);交互见 TUI 界面
//! 底栏:Enter 提交,Esc / Ctrl+C(运行中)取消当前 turn,Ctrl+Q 退出。

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use aimux_core::language_model::LanguageModel;
use rutis::Ctx;
use rutis_agent::{
    agent_key, llm_key, minimal_persona, minimal_tools, self_tools, Agent,
    AgentDriverPlugin, AgentTurnEnd, SelfReloadRequested, ToolsPlugin, TuiPlugin,
};

const USAGE: &str = "\
rutis-cli — minimal coding agent (bash + replace_text) on the rutis framework

USAGE:
    rutis-cli [OPTIONS]

OPTIONS:
    -p, --provider <ID>   aimux provider id [env: AIMUX_PROVIDER] [default: deepseek]
    -m, --model <ID>      model id [env: AIMUX_MODEL] [default: deepseek-chat]
        --scripted        offline demo backend (no API key needed)
        --reload-demo     (scripted) first turn calls self_reload to demo hot-restart
        --reload-handoff <PATH>
                          self_reload handoff path for --reload-demo
                          [default: /tmp/rutis-smoke/reload-intent.md]
    -h, --help            print this help
    -V, --version         print version
";

#[tokio::main]
async fn main() {
    let mut provider = std::env::var("AIMUX_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    let mut model = std::env::var("AIMUX_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
    let mut scripted = false;
    let mut reload_demo = false;
    let mut reload_handoff = "/tmp/rutis-smoke/reload-intent.md".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--provider" => provider = value(&mut args, &arg),
            "-m" | "--model" => model = value(&mut args, &arg),
            "--scripted" => scripted = true,
            "--reload-demo" => reload_demo = true,
            "--reload-handoff" => reload_handoff = value(&mut args, &arg),
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            "-V" | "--version" => {
                println!("rutis-cli {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => {
                eprintln!("unknown argument: {other}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let llm: Arc<dyn LanguageModel> = if scripted {
        Arc::new(rutis_agent::ScriptedLlm::new(scripted_responses(
            reload_demo,
            &reload_handoff,
        )))
    } else {
        match aimux_providers::provider(&provider, None, &model, None) {
            Ok(m) => Arc::from(m),
            Err(e) => {
                eprintln!("failed to build {provider}/{model}: {e}");
                if provider == "deepseek" && std::env::var_os("DEEPSEEK_API_KEY").is_none() {
                    eprintln!(
                        "hint: export DEEPSEEK_API_KEY=... ; or offline demo: rutis-cli --scripted"
                    );
                }
                std::process::exit(1);
            }
        }
    };
    let model_id = if scripted {
        "scripted".to_string()
    } else {
        model.clone()
    };

    if let Err(e) = run(llm, &provider, &model_id, reload_demo).await {
        eprintln!("rutis-cli failed: {e}");
        std::process::exit(1);
    }
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| {
            eprintln!("missing value for {flag}\n\n{USAGE}");
            std::process::exit(2);
        })
        .trim_matches('"')
        .to_string()
}

/// 宿主侧 `SelfReloadRequested` 监听器:收到 self_reload 请求后
/// **fiber 级热重启**——只重装配 agent-driver fiber:
/// - 进程保留(不 exec)、LLM 连接保留、TTY 保留
/// - TUI 不声明 agent 依赖(driver 重启不驱逐 UI,保持运行)
/// - 每次提交/取消经 ctx 重新 get 最新 agent(不缓存旧 driver)
/// - session 从 disk restore:identity 稳定、generation+1、历史连续
struct ReloadHandler {
    driver: rutis::FiberView,
}

impl rutis::Listener<SelfReloadRequested> for ReloadHandler {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a SelfReloadRequested,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        let driver = self.driver.clone();
        Box::pin(async move {
            // 短暂延迟:让 self_reload 工具结果回喂、当前 turn 收尾落盘,
            // 再重启 driver(避免取消进行中的 turn 造成 Turn failed 噪音)
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            // fiber 级热重启:干净卸载 → 重装配(apply 内 Session::restore)
            let _ = driver.restart().await;
            Ok(None)
        })
    }
}

/// 宿主侧督工:`AgentTurnEnd` 后自动评估,超阈值即触发 driver 热重启。
///
/// 与 `ReloadHandler` 的关系:
/// - ReloadHandler = 被动(agent 通过 self_reload 请求重启)
/// - Supervisor = 主动(宿主监听 turn 结果,失败率/消息数超限自动重启)
///
/// 两者共享 driver_view,重启路径一致(fiber 级,进程/TUI/LLM 保留)。
struct Supervisor {
    driver: rutis::FiberView,
    /// 连续失败阈值:超过即重启(默认 3)。
    max_failures: usize,
    /// 消息数阈值:超过即重启(默认 500,避免长会话退化)。
    max_messages: usize,
    /// 当前连续失败数(turn 成功清零)。
    fail_streak: AtomicUsize,
    /// 累计自动重启次数(可观测;Arc 便于测试持有计数副本)。
    restarts: Arc<AtomicUsize>,
}

impl Supervisor {
    fn new(
        driver: rutis::FiberView,
        max_failures: usize,
        max_messages: usize,
    ) -> Self {
        Self {
            driver,
            max_failures,
            max_messages,
            fail_streak: AtomicUsize::new(0),
            restarts: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 共享的累计重启计数器(测试/外部诊断可持有副本)。
    pub fn restarts_shared(&self) -> Arc<AtomicUsize> {
        self.restarts.clone()
    }

    /// 记录一次 turn 结果,返回连续失败是否已达阈值(触发重启)。
    /// 纯逻辑,不依赖 ctx——消息数检查在 listener 层做(见 [`Supervisor::call`])。
    fn record_turn(&self, e: &AgentTurnEnd) -> bool {
        let streak = if e.ok {
            // 成功 turn:清零连续失败,绝不触发
            self.fail_streak.store(0, Ordering::Relaxed);
            0
        } else {
            self.fail_streak.fetch_add(1, Ordering::Relaxed) + 1
        };
        if streak >= self.max_failures {
            eprintln!(
                "[supervisor] {streak} consecutive failed turns (limit {}). restarting driver",
                self.max_failures
            );
            true
        } else {
            false
        }
    }
}

impl rutis::Listener<AgentTurnEnd> for Supervisor {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a AgentTurnEnd,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        let fail_limit = self.record_turn(e);

        // 消息数超限:turn 结束时查 agent 快照(独立于失败计数)
        let mut too_many = false;
        if let Some(agent) = ctx.get_as::<dyn Agent>(agent_key()) {
            let n = agent.session().messages().len();
            too_many = n > self.max_messages;
            if too_many {
                eprintln!(
                    "[supervisor] session messages {} > {}. restarting driver",
                    n, self.max_messages
                );
            }
        }

        if !fail_limit && !too_many {
            return Box::pin(async { Ok(None) });
        }
        self.restarts.fetch_add(1, Ordering::Relaxed);
        let driver = self.driver.clone();
        Box::pin(async move {
            // 与 ReloadHandler 相同:短暂延迟让当前 turn 收尾落盘
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let _ = driver.restart().await;
            Ok(None)
        })
    }
}

async fn run(
    llm: Arc<dyn LanguageModel>,
    provider: &str,
    model: &str,
    _reload_demo: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());
    let root = Ctx::root()?;
    root.provide_as(llm_key(), llm)?;

    // session 持久化(默认 <cwd>/.rutis/session.json)+ 自我控制工具包
    let mut tools = self_tools(root.clone());
    tools.extend(minimal_tools());
    let tools_view = root.plugin(ToolsPlugin::new(tools));
    let driver_view = root.plugin(
        AgentDriverPlugin::new(10000)
            .with_system_prompt(minimal_persona(model, &cwd))
            .with_default_session_path(),
    );

    (&tools_view).await?;
    (&driver_view).await?;

    // 宿主侧重载监听:self_reload 请求 → fiber 级热重启 driver
    // (TUI 不声明 agent 依赖,重启时 UI 保持运行)
    root.events().on(&root, ReloadHandler {
        driver: driver_view.clone(),
    })?;

    // 宿主侧督工:AgentTurnEnd 后自动评估,失败/消息数超限自动重启
    // (阈值:连续 3 次失败,或 session 消息数 > 500)
    root.events().on(&root, Supervisor::new(driver_view.clone(), 3, 500))?;

    // TUI 在 driver 装载完成后创建:apply 内 get agent 必成功(启动门控)
    let tui_view = root.plugin(TuiPlugin::new().with_intro(vec![
        format!("backend: {provider}/{model}"),
        format!("cwd: {cwd}"),
        "tools: bash + replace_text + self_* | Enter 发送 | Esc 取消 | Ctrl+Q 退出".to_string(),
    ]));
    // TUI apply 即主循环:退出(或 fiber 卸载)后 settle 才完成
    let _ = (&tui_view).await;

    // 卸载 TUI fiber 后级联收尾
    tui_view.dispose().await?;
    driver_view.dispose().await?;
    tools_view.dispose().await?;

    Ok(())
}

/// 离线演示脚本:一轮工具调用(建文件 + cat)+ 一轮终答。
/// `reload_demo` 时第一轮调用 `self_reload` 演示宿主侧热重启闭环。
/// handoff 路径可经 `--reload-handoff` 参数化(默认 `/tmp/rutis-smoke/reload-intent.md`)。
fn scripted_responses(reload_demo: bool, reload_handoff: &str) -> Vec<rutis_agent::LlmResponse> {
    use aimux_core::tool::ToolCall;
    use serde_json::json;

    let mk = |id: &str, name: &str, input: serde_json::Value| ToolCall {
        tool_call_id: id.into(),
        tool_name: name.into(),
        input,
        provider_executed: None,
        dynamic: None,
        thought_signature: None,
    };
    let mut out = vec![
        rutis_agent::LlmResponse::tool_calls(vec![
            mk(
                "c1",
                "replace_text",
                json!({
                    "command": "create",
                    "path": "rutis-cli-demo.txt",
                    "file_text": "hello from the scripted backend\n"
                }),
            ),
            mk(
                "c2",
                "bash",
                json!({
                    "command": "cat rutis-cli-demo.txt",
                    "description": "Show the file just created"
                }),
            ),
        ]),
        rutis_agent::LlmResponse::content(
            "demo done: created rutis-cli-demo.txt and read it back. Ask me to edit real files with a real backend key.",
        ),
    ];
    if reload_demo {
        // 第一轮:调用 self_reload(写 handoff 意图 + 广播事件 → 宿主重启)
        out.insert(
            0,
            rutis_agent::LlmResponse::tool_calls(vec![mk(
                "reload1",
                "self_reload",
                json!({
                    "handoff": reload_handoff,
                }),
            )]),
        );
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;

    /// 宿主侧 ReloadHandler:收到 SelfReloadRequested → driver fiber
    /// 热重启(干净卸载→重装配)。验证:
    /// - driver fiber 状态回到 Active(非 Disposed,进程保留)
    /// - agent 服务被重新 provide(新 driver 实例)
    /// - session identity 稳定、generation+1、历史连续
    #[tokio::test]
    async fn reload_handler_fiber_restarts_driver() {
        let tmp = std::env::temp_dir().join(format!("rutis-cli-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let session_path = tmp.join("session.json");

        let root = Ctx::root().unwrap();
        let llm = rutis_agent::into_service(rutis_agent::ScriptedLlm::new(vec![
            rutis_agent::LlmResponse::content("first"),
        ]));
        root.provide_as(llm_key(), llm).unwrap();

        let tools_view = root.plugin(ToolsPlugin::new(minimal_tools()));
        let driver_view = root.plugin(
            AgentDriverPlugin::new(10)
                .with_system_prompt(minimal_persona("scripted", "."))
                .with_session_path(&session_path),
        );
        (&tools_view).await.unwrap();
        (&driver_view).await.unwrap();

        // 第一代 driver
        let agent1 = root.get_as::<dyn rutis_agent::Agent>(rutis_agent::agent_key()).unwrap();
        let id1 = agent1.id();
        agent1.followup("hello").await.unwrap();
        let msgs1 = agent1.session().messages().len();

        // 注册宿主监听器(持 driver_view)
        root.events()
            .on(
                &root,
                ReloadHandler {
                    driver: driver_view.clone(),
                },
            )
            .unwrap();

        // 模拟 self_reload 工具广播事件
        root.events().emit(
            &root,
            Arc::new(SelfReloadRequested {
                session: id1,
                reason: "test".to_string(),
                intent_path: tmp.join("intent.md").to_string_lossy().into_owned(),
            }),
        );

        // 等待 driver restart 完成:状态回到 Active(非 Disposed)
        for _ in 0..400 {
            let st = driver_view.state();
            if st.state == rutis::FiberState::Active {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        // 等待 restart 完成(等 generation 变化,不只状态)
        let start_gen = driver_view.state().generation;
        for _ in 0..400 {
            let st = driver_view.state();
            if st.generation > start_gen {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            driver_view.state().generation > start_gen,
            "driver generation should advance after fiber restart"
        );

        // agent 服务被重新 provide(新实例)
        let agent2 = root
            .get_as::<dyn rutis_agent::Agent>(rutis_agent::agent_key())
            .unwrap();
        assert!(
            !Arc::ptr_eq(&agent1, &agent2),
            "driver should be a new instance after restart"
        );

        // session:identity 稳定、generation+1、历史连续
        let id2 = agent2.id();
        assert_eq!(id1.identity(), id2.identity(), "identity stable");
        assert_eq!(id1.generation() + 1, id2.generation(), "generation +1");
        assert!(
            agent2.session().messages().len() >= msgs1,
            "history preserved after fiber restart"
        );

        let _ = driver_view.dispose().await;
        let _ = tools_view.dispose().await;
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 督工 record_turn 纯逻辑:连续失败计数与清零(不依赖 ctx/fiber)。
    #[tokio::test]
    async fn supervisor_record_turn_streak_and_reset() {
        let driver_view = dummy_fiber_view();
        let sup = Supervisor::new(driver_view, 3, 500);
        let sess = rutis_agent::SessionId::next();

        let e_fail = AgentTurnEnd {
            session: sess,
            ok: false,
            error: "boom".into(),
        };
        let e_ok = AgentTurnEnd {
            session: sess,
            ok: true,
            error: String::new(),
        };

        assert!(!sup.record_turn(&e_fail)); // 1
        assert!(!sup.record_turn(&e_fail)); // 2
        assert!(sup.record_turn(&e_fail));  // 3 → 触发
        assert!(!sup.record_turn(&e_ok));   // 成功清零
        assert!(!sup.record_turn(&e_fail)); // 重新计数 1
    }

    /// 督工 record_turn 纯逻辑:消息数阈值不由 record_turn 判定(listener 层做)。
    #[tokio::test]
    async fn supervisor_record_turn_ok_never_triggers() {
        let driver_view = dummy_fiber_view();
        let sup = Supervisor::new(driver_view, 3, 1); // 即使消息数阈值 1
        let sess = rutis_agent::SessionId::next();
        let e_ok = AgentTurnEnd {
            session: sess,
            ok: true,
            error: String::new(),
        };
        // 成功 turn 不因消息数触发(消息数由 listener 层检查)
        assert!(!sup.record_turn(&e_ok));
    }

    /// 督工端到端:连续 3 次失败 turn → driver 热重启(generation+1)。
    /// 用独立 runtime + root_with 隔离(避免全局 root 污染)。
    #[tokio::test]
    async fn supervisor_restarts_after_failure_streak() {
        use tokio::runtime::Handle;

        let handle = Handle::current();
        let root = Ctx::root_with(handle);
        let tmp = std::env::temp_dir().join(format!("rutis-cli-sup-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let session_path = tmp.join("session.json");

        // 一个会失败的后端:空 ScriptedLlm,每次 do_generate 弹完即 Err
        let llm = rutis_agent::ScriptedLlm::new(Vec::new());
        root.provide_as(llm_key(), rutis_agent::into_service(llm)).unwrap();

        let tools_view = root.plugin(ToolsPlugin::new(minimal_tools()));
        let driver_view = root.plugin(
            AgentDriverPlugin::new(10)
                .with_system_prompt(minimal_persona("scripted", "."))
                .with_session_path(&session_path),
        );
        (&tools_view).await.unwrap();
        (&driver_view).await.unwrap();

        let agent1 = root.get_as::<dyn rutis_agent::Agent>(rutis_agent::agent_key()).unwrap();
        let start_gen = driver_view.state().generation;

        // 注册督工:连续 3 次失败触发
        let supervisor = Supervisor::new(driver_view.clone(), 3, 500);
        let restart_counter = supervisor.restarts_shared();
        root.events().on(&root, supervisor).unwrap();

        // 3 次失败 turn
        for i in 0..3 {
            let _ = agent1.followup(&format!("turn {i}")).await;
        }

        // 等待 generation 前进(触发重启)
        for _ in 0..400 {
            if driver_view.state().generation > start_gen {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            driver_view.state().generation > start_gen,
            "supervisor should restart driver after 3 failed turns"
        );
        assert!(
            restart_counter.load(Ordering::Relaxed) >= 1,
            "supervisor should count the restart"
        );

        let _ = driver_view.dispose().await;
        let _ = tools_view.dispose().await;
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 督工:成功 turn 不触发重启(独立 runtime 隔离)。
    #[tokio::test]
    async fn supervisor_no_restart_on_success() {
        use tokio::runtime::Handle;

        let handle = Handle::current();
        let root = Ctx::root_with(handle);
        let llm = rutis_agent::into_service(rutis_agent::ScriptedLlm::new(vec![
            rutis_agent::LlmResponse::content("ok1"),
            rutis_agent::LlmResponse::content("ok2"),
            rutis_agent::LlmResponse::content("ok3"),
        ]));
        root.provide_as(llm_key(), llm).unwrap();
        let tools_view = root.plugin(ToolsPlugin::new(minimal_tools()));
        let driver_view = root.plugin(
            AgentDriverPlugin::new(10).with_session_path(
                std::env::temp_dir().join(format!("rutis-sup-ok-{}.json", std::process::id())),
            ),
        );
        (&tools_view).await.unwrap();
        (&driver_view).await.unwrap();

        let sup = Supervisor::new(driver_view.clone(), 3, 500);

        // 注册督工
        root.events().on(&root, sup).unwrap();

        let agent = root.get_as::<dyn rutis_agent::Agent>(rutis_agent::agent_key()).unwrap();
        let start_gen = driver_view.state().generation;
        for i in 0..3 {
            let _ = agent.followup(&format!("ok {i}")).await;
        }

        // 等一小段(让事件派发跑完),generation 不应前进
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            driver_view.state().generation,
            start_gen,
            "successful turns must not restart driver"
        );

        let _ = driver_view.dispose().await;
    }
}

/// 纯逻辑测试用的 dummy FiberView:不挂到任何 fiber,仅提供类型。
/// (FiberView 是 Clone + Send + Sync;不 await 它就不会真正跑 fiber。)
fn dummy_fiber_view() -> rutis::FiberView {
    struct DummyPlugin;
    impl rutis::Plugin for DummyPlugin {
        fn name(&self) -> &str {
            "dummy-supervisor-test"
        }
        fn apply<'a>(
            &'a self,
            _ctx: &'a Ctx,
        ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
            Box::pin(async { Ok(rutis::Effect::Done) })
        }
    }
    let root = Ctx::root_with(tokio::runtime::Handle::current());
    root.plugin(DummyPlugin)
}


