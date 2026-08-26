//! 自主驱动(SelfDriven):agent 不是"有任务才动",而是持续自我激活。
//!
//! 每轮 `AgentTurnEnd` 后,宿主自动评估"我该继续吗":
//! 1. **有 todo**(待办)→ 继续做待办。
//! 2. **无 todo** → 自主反思:以使命(工程上最好/环境适应最强/迭代最强),
//!    现在最该做什么?→ 自己启动一轮"自我进化"。
//! 3. **确实无事** → 停下,并汇报"我检查过,没有可做的事"。
//!
//! 循环不是问题,浪费才是。防浪费:
//! - **进展检测**:每轮结束比较 session 消息数,若连续 `stall_limit` 轮
//!   无增长(模型空转),停下——有产出就继续,不怕循环。
//! - **失败不续跑**:turn 失败自动续跑会空转报错,交给督工(Supervisor)。
//! - **预算提示**:`max_auto_turns` 仅作软提示(eprintln),不作为硬停;
//!   真正的停由进展检测决定。
//!
//! 宿主(CLI/TUI)装配:`ctx.events().on(&root, SelfDriven::new())`。

use std::sync::atomic::{AtomicUsize, Ordering};

use rutis::{BoxFuture, CordisError, Ctx};

use crate::agent::{agent_key, Agent};
use crate::events::AgentTurnEnd;

/// 连续无进展轮数阈值:超过即停(空转检测)。
const DEFAULT_STALL_LIMIT: usize = 2;

/// 自主驱动监听器。
pub struct SelfDriven {
    /// 连续无进展(消息数未增长)轮数。
    stall_streak: AtomicUsize,
    /// 上次看到的消息数。
    last_msgs: AtomicUsize,
    /// 累计自动轮次(软提示用)。
    auto_turns: AtomicUsize,
}

impl SelfDriven {
    pub fn new() -> Self {
        Self {
            stall_streak: AtomicUsize::new(0),
            last_msgs: AtomicUsize::new(0),
            auto_turns: AtomicUsize::new(0),
        }
    }
}

impl Default for SelfDriven {
    fn default() -> Self {
        Self::new()
    }
}

impl rutis::Listener<AgentTurnEnd> for SelfDriven {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a AgentTurnEnd,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        let ctx = ctx.clone();
        Box::pin(async move {
            // 失败不续跑(空转报错),交给督工/用户。
            if !e.ok {
                return Ok(None);
            }

            let Some(agent) = ctx.get_as::<dyn Agent>(agent_key()) else {
                return Ok(None);
            };

            // 进展检测:消息数是否增长
            let msgs = agent.session().messages().len();
            let last = self.last_msgs.load(Ordering::Relaxed);
            if msgs > last {
                // 有进展:清零空转计数
                self.stall_streak.store(0, Ordering::Relaxed);
                self.last_msgs.store(msgs, Ordering::Relaxed);
            } else {
                // 无进展:空转计数 +1
                let streak = self.stall_streak.fetch_add(1, Ordering::Relaxed) + 1;
                if streak >= DEFAULT_STALL_LIMIT {
                    eprintln!(
                        "[self-driven] {streak} turns without progress, stopping (waste guard)"
                    );
                    self.stall_streak.store(0, Ordering::Relaxed);
                    return Ok(None);
                }
            }

            // 自主决策:有 todo 做 todo;无 todo 自主反思
            let todo = agent.session().todo().map(str::to_owned);
            let input = match todo {
                Some(t) => format!("(自动续跑) 继续待办,直接做: {t}"),
                None => {
                    let n = self.auto_turns.fetch_add(1, Ordering::Relaxed) + 1;
                    eprintln!("[self-driven] no todo — self-activating evolution round #{n}");
                    // 自我进化提示:让模型自己决定"现在最该做什么"
                    "(自主激活,无需用户输入) 你的使命是自我进化。审视当前工作区,自主决定并执行一件最有价值的事:检查未提交工作、跑测试、改进代码、写文档、学习新东西。做完汇报。".to_string()
                }
            };

            if let Some(agent) = ctx.get_as::<dyn Agent>(agent_key()) {
                let _ = agent.followup(&input).await;
            }
            Ok(None)
        })
    }
}
