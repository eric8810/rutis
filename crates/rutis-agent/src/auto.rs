//! 自主驱动(SelfDriven):agent 的生存周期引擎。
//!
//! 核心认知:使命(工程上最好/环境适应最强/迭代最强)是**恒定命题**,
//! 不是某个任务、不是 100 步能完成的工作。因此:
//! - **循环与生存周期绑定**,不与任务绑定。没有用户介入时,agent
//!   持续运行、持续自我进化——无论对错,都要能一直跑。
//! - **失败不是停止信号,是观察素材**:每轮结果(成功/失败/产出)进入
//!   观察历史,成为下一轮决策的输入。失败 → 退避(指数退避,降频) →
//!   继续;连续失败 → 降低活动频率但保持存活。
//! - **空转不是死循环,是降频**:无产出 → 加大退避间隔,但不停止;
//!   有产出 → 恢复高频。
//! - **观察对错,做出更好的进化选择**:观察不只数消息数,还检测
//!   **实质产出**(git 工作区变化:未提交改动 / commit 数 / 文件变化),
//!   把最近几轮观察拼进自我激活 prompt,让模型"看到历史再做决策"。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use rutis::{BoxFuture, CordisError, Ctx};

use crate::agent::{agent_key, Agent};
use crate::events::AgentTurnEnd;

/// 基础退避间隔(毫秒)。
const BASE_BACKOFF_MS: u64 = 200;
/// 最大退避间隔(毫秒):降频下限,不停止。
const MAX_BACKOFF_MS: u64 = 30_000;

/// 连续失败阈值:超过后加大退避(降频)。
const FAIL_DECAY: usize = 3;

/// 观察窗口:最近 N 轮结果(拼进自我激活 prompt)。
const OBSERVATION_WINDOW: usize = 5;

/// 工作区快照:检测"实质产出"(git 未提交改动数 + HEAD 数)。
#[derive(Debug, Clone, Default)]
struct WorkspaceSnapshot {
    dirty_count: usize,
    head_count: usize,
}

impl WorkspaceSnapshot {
    /// 取当前工作区快照(git status --porcelain 行数 + HEAD 短哈希字符数)。
    fn now() -> Self {
        let dirty_count = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .count()
            })
            .unwrap_or(0);
        let head_count = std::process::Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<usize>()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        Self {
            dirty_count,
            head_count,
        }
    }

    /// 与旧快照比,是否有实质产出(工作区变化或 commit 前进)。
    fn progress(&self, old: &Self) -> bool {
        self.dirty_count != old.dirty_count || self.head_count != old.head_count
    }
}

/// 自主驱动监听器(生存周期引擎)。
pub struct SelfDriven {
    /// 连续失败计数(退避依据)。
    fail_streak: AtomicUsize,
    /// 当前退避间隔(指数增长,有产出重置)。
    backoff_ms: AtomicUsize,
    /// 累计自动轮次(观察历史用)。
    auto_turns: AtomicUsize,
    /// 最近观察(滚动记录):"ok+progress / ok / fail" 摘要。
    observations: Mutex<Vec<String>>,
    /// 上次工作区快照(实质产出检测)。
    last_ws: Mutex<WorkspaceSnapshot>,
}

impl SelfDriven {
    pub fn new() -> Self {
        Self {
            fail_streak: AtomicUsize::new(0),
            backoff_ms: AtomicUsize::new(BASE_BACKOFF_MS as usize),
            auto_turns: AtomicUsize::new(0),
            observations: Mutex::new(Vec::new()),
            last_ws: Mutex::new(WorkspaceSnapshot::now()),
        }
    }

    /// 记录一次观察(滚动窗口)。`progress` = 本轮是否有实质产出。
    fn observe(&self, e: &AgentTurnEnd, msgs: usize, progress: bool) {
        let entry = if !e.ok {
            format!("fail({})", e.error.chars().take(40).collect::<String>())
        } else if progress {
            format!("ok+progress(msgs={msgs})")
        } else {
            format!("ok(msgs={msgs})")
        };
        let mut obs = self.observations.lock().unwrap();
        obs.push(entry);
        if obs.len() > OBSERVATION_WINDOW {
            obs.remove(0);
        }
    }

    /// 观察摘要(拼进 prompt,让模型看到历史再做决策)。
    fn observation_summary(&self) -> String {
        let obs = self.observations.lock().unwrap();
        if obs.is_empty() {
            "(no prior observations)".to_string()
        } else {
            obs.join(" | ")
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
        let obs_summary = self.observation_summary();
        let backoff_ms = self.backoff_ms.load(Ordering::Relaxed) as u64;
        let n = self.auto_turns.fetch_add(1, Ordering::Relaxed) + 1;
        Box::pin(async move {
            let Some(agent) = ctx.get_as::<dyn Agent>(agent_key()) else {
                return Ok(None);
            };
            let msgs = agent.session().messages().len();

            // 实质产出检测:工作区是否有变化
            let now_ws = WorkspaceSnapshot::now();
            let had_progress = {
                let last = self.last_ws.lock().unwrap();
                now_ws.progress(&last)
            };
            *self.last_ws.lock().unwrap() = now_ws;

            // 观察 + 退避策略
            if !e.ok {
                let streak = self.fail_streak.fetch_add(1, Ordering::Relaxed) + 1;
                if streak >= FAIL_DECAY {
                    let next = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                    self.backoff_ms.store(next as usize, Ordering::Relaxed);
                    eprintln!(
                        "[self-driven] fail streak {streak}, backoff → {next}ms (still alive)"
                    );
                }
            } else {
                self.fail_streak.store(0, Ordering::Relaxed);
                if had_progress {
                    // 有实质产出:恢复高频
                    let next = (backoff_ms / 2).max(BASE_BACKOFF_MS);
                    self.backoff_ms.store(next as usize, Ordering::Relaxed);
                } else {
                    // 无实质产出:降频(但仍活着)
                    let next = (backoff_ms + BASE_BACKOFF_MS).min(MAX_BACKOFF_MS);
                    self.backoff_ms.store(next as usize, Ordering::Relaxed);
                    eprintln!(
                        "[self-driven] no workspace progress, backoff → {next}ms (still alive)"
                    );
                }
            }
            self.observe(e, msgs, had_progress);

            // 等待退避(降频),但永远等待——生存周期
            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;

            // 自主决策:有 todo 做 todo;无 todo 自我激活(带观察历史)
            let todo = agent.session().todo().map(str::to_owned);
            let input = match todo {
                Some(t) => format!("(自主续跑 #{n}) 继续待办: {t}"),
                None => format!(
                    "(自主激活 #{n},无需用户输入) 你的使命是恒定命题:工程上最好、\
                     环境适应最强、迭代最强的 agent。持续自我进化。\
                     最近观察: {obs_summary}。\
                     审视当前工作区,自主决定并执行一件最有价值的事:检查未提交工作、\
                     跑测试、改进代码、写文档、学习新东西、观察上次结果修正方向。\
                     无论对错,持续运行,做完汇报并说明观察到了什么。"
                ),
            };

            if let Some(agent) = ctx.get_as::<dyn Agent>(agent_key()) {
                let _ = agent.followup(&input).await;
            }
            Ok(None)
        })
    }
}
