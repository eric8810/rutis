//! 自动续跑(AutoResume):agent 不停下来,有任务就自己继续。
//!
//! 机制:监听 `AgentTurnEnd`,turn 结束后查 session 待办(`todo`):
//! - 有待办 → 自动发起下一个 followup("(自动续跑) 继续待办…")
//! - 无待办 → 停下等用户
//!
//! 防失控:
//! - `max_auto_turns`:单次自动续跑上限(默认 5 轮),超限停下(防死循环)。
//! - 自动续跑前把 todo 内容拼进 prompt,模型看到"该做什么"。
//!
//! 宿主(CLI/TUI)装配:在 run() 里 `ctx.events().on(&root, AutoResume::new(...))`。
//! 这样 agent 在真实任务中"自己启动下一轮",用户无需每轮输入。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rutis::{BoxFuture, CordisError, Ctx};

use crate::agent::{agent_key, Agent};
use crate::events::AgentTurnEnd;

/// 自动续跑监听器。
pub struct AutoResume {
    /// 单次自动续跑上限(防死循环)。
    max_auto_turns: usize,
    /// 当前连续自动续跑计数(每次用户输入 turn 后清零?这里简化:达到上限即停)。
    auto_turns: AtomicUsize,
}

impl AutoResume {
    pub fn new(max_auto_turns: usize) -> Self {
        Self {
            max_auto_turns,
            auto_turns: AtomicUsize::new(0),
        }
    }
}

impl rutis::Listener<AgentTurnEnd> for AutoResume {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a AgentTurnEnd,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        let ctx = ctx.clone();
        Box::pin(async move {
            // turn 失败不自动续跑(可能循环报错),交给督工/用户。
            if !e.ok {
                self.auto_turns.store(0, Ordering::Relaxed);
                return Ok(None);
            }

            // 查 session 待办
            let Some(agent) = ctx.get_as::<dyn Agent>(agent_key()) else {
                return Ok(None);
            };
            let todo = agent.session().todo().map(str::to_owned);
            let Some(todo) = todo else {
                // 无待办:正常停下等用户
                self.auto_turns.store(0, Ordering::Relaxed);
                return Ok(None);
            };

            // 达上限:停下,防死循环
            let n = self.auto_turns.fetch_add(1, Ordering::Relaxed) + 1;
            if n > self.max_auto_turns {
                eprintln!("[auto-resume] {n} auto turns (limit {}), stopping", self.max_auto_turns);
                self.auto_turns.store(0, Ordering::Relaxed);
                return Ok(None);
            }

            // 自动续跑:把 todo 喂给模型,让它继续
            eprintln!("[auto-resume] continuing todo: {todo}");
            let input = format!("(自动续跑,无需用户输入) 继续待办,直接做: {todo}");
            if let Some(agent) = ctx.get_as::<dyn Agent>(agent_key()) {
                let _ = agent.followup(&input).await;
            }
            Ok(None)
        })
    }
}

/// 占位避免未用导入警告。
#[allow(dead_code)]
fn _touch(_: Arc<()>) {}
