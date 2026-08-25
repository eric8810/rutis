//! TUI 前端插件(验证文档 §二)——`agent/*` 事件监听器,不是 loop 的一部分。
//!
//! 基于 [rutui](https://github.com/eric8810/rutui) 的 block 式 scrollback
//! 渲染管线 + 多行 prompt 编辑器,替换原全手写的对话/输入/折行逻辑。
//! 设计见 `docs/design-rutui-rewrite-2026-08-23.md`。
//!
//! - 对话区:`ScrollbackState` + `ScrollbackPane`(rutui-core)——block 式渲染、
//!   CJK 宽度、滚动、流式追加、agent 消息 markdown + 代码高亮(开箱即用)
//! - 输入框:`PromptWidget`(rutui-prompt)——多行 textarea + readline + undo
//! - 输入行:Enter 提交 → `agent.followup(text)` 触发 turn(返回终态)
//! - 过程增量:订阅 `AgentTextDelta`/`AgentToolCall`/`AgentToolResult` /
//!   `AgentTurnEnd`,翻译成 `ScrollbackState` 的 block 操作(全靠 EventBus
//!   广播,不独占 stream)
//! - 状态栏:idle | running(手写,P2 接 ShortcutsBar)
//! - Esc / Ctrl+C(运行中)取消当前 turn;Ctrl+Q 退出(卸载 TUI fiber)
//!
//! ## 线程模型
//!
//! `ScrollbackState` 内部的 markdown 渲染器含 onig_sys 的 `*mut`(非 Send),
//! 故 `App` 非 Send,不能跨 `tokio::select!` 的 await 持有(apply 返回的
//! `BoxFuture` 要求 Send)。解法:TUI 是天然单线程的,把 `App` + `Terminal`
//! 放进一个专用 OS 线程跑同步渲染循环,async `apply` 只持有 Send 的通道句柄
//! 做 IO 多路复用,转发 crossterm 事件 / agent 事件 / 取消信号进 std 通道,
//! 等 OS 线程结束。

use std::time::Duration;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, EventStream, KeyCode,
    KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use unicode_width::UnicodeWidthStr;
use rutis::{BoxFuture, CordisError, Ctx, Effect, Listener, Plugin, TypeKey};
use rutui_core::scrollback::block::RenderBlock;
use rutui_core::scrollback::blocks::{
    AgentMessageBlock, OtherToolCallBlock, SessionEvent, SessionEventBlock, SystemMessageBlock,
    ThinkingBlock, ToolCallBlock, UserPromptBlock,
};
use rutui_core::scrollback::{
    EntryId, ScrollbackPane, ScrollbackState, ScratchBuffer,
};
use rutui_prompt::prompt_widget::{PromptStyle, PromptWidget};
use tokio::sync::{mpsc, oneshot};

use crate::agent::Agent;
use crate::events::{
    AgentReasoning, AgentTextDelta, AgentToolCall, AgentToolResult, AgentTurnEnd,
};

/// TUI 前端插件:`injects = [agent]`,依赖 agent 服务(dual gating 之上再门控)。
pub struct TuiPlugin {
    inject_keys: Vec<TypeKey>,
    /// 启动时置顶的说明行(如后端类型提示,离线脚本示例用它自我标识)。
    intro: Vec<String>,
}

impl TuiPlugin {
    pub fn new() -> Self {
        Self {
            inject_keys: vec![crate::agent_key()],
            intro: Vec::new(),
        }
    }

    /// 启动时在对话区顶部插入灰色说明行。
    #[must_use]
    pub fn with_intro(mut self, intro: Vec<String>) -> Self {
        self.intro = intro;
        self
    }
}

impl Default for TuiPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// 鼠标滚轮每 notch 滚动的行数(与 Shift+↑/↓ 一致)。
const MOUSE_SCROLL_LINES: u16 = 3;

fn setup_terminal() -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    Ok(())
}

fn restore_terminal() -> std::io::Result<()> {
    execute!(
        std::io::stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    Ok(())
}

// ── UI 状态(非 Send:ScrollbackState 含 onig_sys *mut)──────────────

/// agent/* 事件经 listener 翻译成的 UI 命令(借用在监听器调用内拷贝)。
#[derive(Debug, Clone)]
enum UiCmd {
    ReasoningDelta(String),
    TextDelta(String),
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        ok: bool,
        output: String,
    },
    TurnEnd {
        ok: bool,
        error: String,
    },
}

/// 渲染线程从 std 通道收到的消息(输入事件或 agent 事件)。
enum ThreadMsg {
    Key(crossterm::event::KeyEvent),
    /// 鼠标事件(目前只处理滚轮上下滚动)。
    Mouse(MouseEvent),
    Ui(UiCmd),
    /// 取消信号(fiber 卸载)——通知渲染线程退出。
    Cancel,
}

/// 一个 turn 内,正文 / 思考 / 工具各用一个独立 cursor 保持打开。
/// reasoning 与正文在事件流里可能交错(两个监听器并发、各自保序但互相穿插),
/// 用独立 cursor 可避免"每交错一次就收尾正文开新块"导致正文被切成碎片。
/// 段边界(工具调用/结果、turn 结束)时收尾对应 cursor。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Active {
    agent: Option<EntryId>,
    thinking: Option<EntryId>,
    tool: Option<EntryId>,
}

impl Active {
    /// 收尾所有 open 块并清空。
    fn close_all(&mut self, sb: &mut ScrollbackState) {
        for id in [self.agent, self.thinking, self.tool]
            .into_iter()
            .flatten()
        {
            sb.finish_running(id);
        }
        *self = Self::default();
    }
}

/// 左键拖选区(绝对屏幕坐标)。`start` = 按下点,`end` = 当前/抬起点,均为 `(col, row)`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct Selection {
    start: (u16, u16),
    end: (u16, u16),
}

struct App {
    scrollback: ScrollbackState,
    prompt: PromptWidget,
    scratch: ScratchBuffer,
    /// turn 内各类型块的流式 cursor(正文/思考/工具),详见 [Active]。
    active: Active,
    running: bool,
    /// 左键拖选区;`None` = 无选区。
    selection: Option<Selection>,
    /// 对话区 Rect(渲染时更新),把鼠标坐标约束到对话区内。
    conv_area: Rect,
}

impl App {
    fn new(intro: &[String]) -> Self {
        // 锁定 terminal-native(透明)主题:所有块背景跟随终端,不叠加 rutui
        // 自带的深色主题底色(否则会出现非终端背景的深色块)。必须在
        // ScrollbackState::new() 之前设置——其 AppearanceConfig 在构造时
        // 就从 Theme::current() 采样背景色。
        rutui_theme::theme::cache::set_terminal_native_lock(true);
        let mut scrollback = ScrollbackState::new();
        // 关闭时间戳:它会在每个消息块第一行右端预留宽度,窄 TUI 里会让
        // CJK 文本提前换行(半句话断开)。克隆当前 appearance 只关这一个
        // 开关,保留上面锁定的透明主题配色。
        let mut appearance = scrollback.appearance().clone();
        appearance.show_timestamps = false;
        scrollback.set_appearance(appearance);
        for line in intro {
            scrollback.push_block(RenderBlock::System(SystemMessageBlock::new(line)));
        }
        Self {
            scrollback,
            prompt: PromptWidget::new(),
            scratch: ScratchBuffer::new(),
            active: Active::default(),
            running: false,
            selection: None,
            conv_area: Rect::default(),
        }
    }

    fn submit(&mut self, text: &str) {
        self.scrollback
            .push_block(RenderBlock::UserPrompt(UserPromptBlock::new(text)));
        self.running = true;
    }

    fn on_ui_cmd(&mut self, cmd: UiCmd) {
        // 内容变化(新 delta/工具/turn 结束)会使选区坐标失效,清除。
        self.selection = None;
        match cmd {
            // reasoning 段:追加到当前 thinking cursor(无则开新块)。
            // 独立 cursor:正文/思考交错时互不收尾,避免正文被切碎。
            UiCmd::ReasoningDelta(delta) => {
                let id = match self.active.thinking {
                    Some(id) => id,
                    None => {
                        let id = self
                            .scrollback
                            .push_block(RenderBlock::Thinking(ThinkingBlock::streaming()));
                        self.scrollback.set_entry_running(id, true);
                        self.active.thinking = Some(id);
                        id
                    }
                };
                self.scrollback.push_chunk_to_thinking(id, &delta);
            }
            // 正文段:追加到当前 agent cursor(无则开新块)。
            UiCmd::TextDelta(delta) => {
                let id = match self.active.agent {
                    Some(id) => id,
                    None => {
                        let id = self.scrollback.push_block(RenderBlock::AgentMessage(
                            AgentMessageBlock::streaming(),
                        ));
                        self.scrollback.set_entry_running(id, true);
                        self.active.agent = Some(id);
                        id
                    }
                };
                self.scrollback.push_chunk_to_agent(id, &delta);
            }
            // 工具调用:收尾当前所有 open 块(思考/正文),再开 tool 块。
            UiCmd::ToolCall { name, args } => {
                self.active.close_all(&mut self.scrollback);
                let id = self
                    .scrollback
                    .push_block(RenderBlock::ToolCall(ToolCallBlock::Other(
                        OtherToolCallBlock::new(name, args),
                    )));
                self.scrollback.set_entry_running(id, true);
                self.active.tool = Some(id);
            }
            // 工具结果:原地更新 tool 块(完整单元)并收尾它;
            // 不动可能仍 open 的正文/思考 cursor。
            UiCmd::ToolResult { ok, output, name } => {
                if let Some(id) = self.active.tool {
                    // 从原 tool 块还原 summary/started_at,保留调用参数与时序
                    let (summary, started_at) = self
                        .scrollback
                        .get_by_id(id)
                        .map(|e| match &e.block {
                            RenderBlock::ToolCall(ToolCallBlock::Other(t)) => {
                                (t.summary.clone(), t.started_at)
                            }
                            _ => (String::new(), None),
                        })
                        .unwrap_or_default();
                    let mut block = OtherToolCallBlock::new(name, summary);
                    if ok {
                        block = block.with_output(output);
                    } else {
                        block = block.with_error(output);
                    }
                    self.scrollback.replace_tool_block(
                        id,
                        RenderBlock::ToolCall(ToolCallBlock::Other(block)),
                        started_at,
                    );
                    self.scrollback.finish_running(id);
                    self.active.tool = None;
                }
            }
            // turn 终态:收尾所有 open 块,置回 idle。
            UiCmd::TurnEnd { ok: true, .. } => {
                self.active.close_all(&mut self.scrollback);
                self.running = false;
            }
            UiCmd::TurnEnd { ok: false, error } => {
                self.active.close_all(&mut self.scrollback);
                self.scrollback.push_block(RenderBlock::SessionEvent(SessionEventBlock::new(
                    SessionEvent::TurnFailed {
                        error,
                        elapsed: None,
                    },
                )));
                self.running = false;
            }
        }
    }

    fn status_line(&self) -> String {
        if self.running {
            " status: running | PgUp/滚轮 scroll | Esc cancel | Ctrl+Q quit".to_string()
        } else {
            " status: idle | PgUp/滚轮 scroll | Ctrl+Q quit".to_string()
        }
    }

    /// 按键处理。返回 true 表示退出。
    /// 先处理全局动作键;其余交给 PromptWidget(字符、退格、readline、undo)。
    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<HandleOutcome> {
        // 任何按键都清除拖选区(输入/滚动/取消时选区已过时)。
        self.selection = None;
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => return Some(HandleOutcome::Quit),
            (KeyCode::Char('c'), KeyModifiers::CONTROL) if !self.running => {
                return Some(HandleOutcome::Quit)
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
                return Some(HandleOutcome::Cancel);
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if !self.running {
                    let text = self.prompt.text().trim().to_string();
                    if !text.is_empty() {
                        self.prompt.set_text("");
                        self.submit(&text);
                        return Some(HandleOutcome::Submit(text));
                    }
                }
                return None;
            }
            // 滚动对话区(不占用 textarea 的普通上/下光标键):
            // PgUp/PgDn 整页,Shift+上/下 逐 3 行。scroll_down 触底后按
            // follow_by_overscroll 自动恢复跟随(新内容继续贴底)。
            (KeyCode::PageUp, _) => {
                self.scrollback.page_up();
                return None;
            }
            (KeyCode::PageDown, _) => {
                self.scrollback.page_down();
                return None;
            }
            (KeyCode::Up, KeyModifiers::SHIFT) => {
                self.scrollback.scroll_up(3);
                return None;
            }
            (KeyCode::Down, KeyModifiers::SHIFT) => {
                self.scrollback.scroll_down(3);
                return None;
            }
            _ => {}
        }
        // 其余键交给 PromptWidget(字符、退格、readline、undo 等)
        let _ = self.prompt.handle_key(&key);
        None
    }
}

/// 渲染线程回传给主调的动作:在 tokio runtime 上执行(agent 调用必须)。
enum HandleOutcome {
    Quit,
    Cancel,
    Submit(String),
}

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [conv, status, input] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(5),
    ])
    .areas(area);

    // 对话区:rutui ScrollbackPane
    // 渲染前必须 prepare_layout(计算各 entry 高度 / 布局缓存),否则 render panic
    app.scrollback.prepare_layout(conv.width, conv.height);
    ScrollbackPane::new()
        .render_with_scratch(conv, frame.buffer_mut(), &mut app.scrollback, &mut app.scratch);

    // 记录对话区位置(拖选约束鼠标坐标用),并在其上叠加选区高亮。
    app.conv_area = conv;
    if let Some(sel) = app.selection {
        let hi = Style::default().bg(Color::Indexed(24));
        let buf = frame.buffer_mut();
        for (row, c0, c1) in selection_rows(&sel, conv) {
            for col in c0..=c1 {
                if let Some(cell) = buf.cell_mut((col, row)) {
                    cell.set_style(cell.style().patch(hi));
                }
            }
        }
    }

    // 状态栏
    frame.render_widget(
        Paragraph::new(app.status_line()).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        status,
    );

    // 输入框:rutui PromptWidget
    let style = PromptStyle {
        focused: true,
        chrome: true,
        show_borders: true,
        ..PromptStyle::default()
    };
    let result = app.prompt.draw(frame.buffer_mut(), input, None, &style, None, None);
    if let Some((col, row)) = result.cursor_pos {
        frame.set_cursor_position((col, row));
    }
}

// ── 拖选:坐标约束 / 行段 / 取文本 / 剪贴板 ─────────────────────────

/// 鼠标绝对坐标是否落在对话区内;在则返回该坐标,否则 None(点击对话区外不起选)。
fn conv_point(p: (u16, u16), area: Rect) -> Option<(u16, u16)> {
    let (col, row) = p;
    ((area.y..area.bottom()).contains(&row) && (area.x..area.right()).contains(&col))
        .then_some((col, row))
}

/// 选区覆盖的行段:每行返回 `(row, col_start, col_end)`。
/// 单行取两点之间;多行首行从起点列到行尾、末行从行首到终点列、中间整行。
fn selection_rows(sel: &Selection, area: Rect) -> Vec<(u16, u16, u16)> {
    let right = area.right().saturating_sub(1);
    let (a, b) = (sel.start, sel.end);
    let r0 = a.1.min(b.1);
    let r1 = a.1.max(b.1);
    let mut out = Vec::new();
    if r0 == r1 {
        let c0 = a.0.min(b.0).clamp(area.x, right);
        let c1 = a.0.max(b.0).clamp(area.x, right);
        out.push((r0, c0, c1));
        return out;
    }
    for row in r0.max(area.y)..=r1.min(right) {
        let (c0, c1) = if row == r0 {
            (col_on_row(a, b, r0).max(area.x), right)
        } else if row == r1 {
            (area.x, col_on_row(a, b, r1).min(right))
        } else {
            (area.x, right)
        };
        out.push((row, c0, c1));
    }
    out
}

/// 返回恰好位于 `row` 行那个端点的列。
fn col_on_row(a: (u16, u16), b: (u16, u16), row: u16) -> u16 {
    if a.1 == row {
        a.0
    } else {
        b.0
    }
}

/// 从已渲染缓冲读取选区文本。CJK 宽字符占 2 格(尾格被 ratatui reset 为空、
/// `symbol()` 读回为空格),故按字形宽度跳尾格拼接,避免汉字之间多出空格;
/// 每行去行尾空白,行间以换行连接。
fn selection_text(buf: &Buffer, sel: &Selection, area: Rect) -> Option<String> {
    let mut out = String::new();
    for (row, c0, c1) in selection_rows(sel, area) {
        let mut line = String::new();
        let mut col = c0;
        while col <= c1 {
            let Some(cell) = buf.cell((col, row)) else {
                break
            };
            let sym = cell.symbol();
            let w = sym.width();
            line.push_str(sym);
            col += w.max(1) as u16;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    let out = out.trim_end();
    if out.is_empty() {
        None
    } else {
        Some(out.to_string())
    }
}

/// 最小 base64 编码器(OSC 52 剪贴板用),避免引入额外依赖。
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16
            | (ch.get(1).copied().unwrap_or(0) as u32) << 8
            | ch.get(2).copied().unwrap_or(0) as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if ch.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if ch.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// 通过 OSC 52 把文本写入系统剪贴板(多数现代终端支持,无需原生依赖)。
fn copy_to_clipboard_os52(text: &str) {
    let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let _ = execute!(std::io::stdout(), crossterm::style::Print(seq));
}

// ── 事件 → UI 通道监听器(归 TUI fiber 所有,D28)─────────────────────

/// `AgentTextDelta` → UI。
struct DeltaL(mpsc::Sender<UiCmd>);
impl Listener<AgentTextDelta> for DeltaL {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a AgentTextDelta,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        let tx = self.0.clone();
        let ev = UiCmd::TextDelta(e.delta.clone());
        Box::pin(async move {
            let _ = tx.send(ev).await;
            Ok(None)
        })
    }
}

/// `AgentToolCall` → UI。
struct ToolCallL(mpsc::Sender<UiCmd>);
impl Listener<AgentToolCall> for ToolCallL {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a AgentToolCall,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        let tx = self.0.clone();
        let ev = UiCmd::ToolCall {
            name: e.name.clone(),
            args: e.args.to_string(),
        };
        Box::pin(async move {
            let _ = tx.send(ev).await;
            Ok(None)
        })
    }
}

/// `AgentToolResult` → UI。
struct ToolResultL(mpsc::Sender<UiCmd>);
impl Listener<AgentToolResult> for ToolResultL {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a AgentToolResult,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        let tx = self.0.clone();
        let ev = UiCmd::ToolResult {
            name: e.name.clone(),
            ok: e.ok,
            output: e.output.clone(),
        };
        Box::pin(async move {
            let _ = tx.send(ev).await;
            Ok(None)
        })
    }
}

/// `AgentTurnEnd` → UI(兜底:followup 任务的返回值之外,turn 状态也经事件)。
struct TurnEndL(mpsc::Sender<UiCmd>);
impl Listener<AgentTurnEnd> for TurnEndL {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a AgentTurnEnd,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        let tx = self.0.clone();
        let ev = UiCmd::TurnEnd {
            ok: e.ok,
            error: e.error.clone(),
        };
        Box::pin(async move {
            let _ = tx.send(ev).await;
            Ok(None)
        })
    }
}

/// `AgentReasoning` → UI(推理/reasoning 增量,渲染为 Thinking 块)。
struct ReasoningL(mpsc::Sender<UiCmd>);
impl Listener<AgentReasoning> for ReasoningL {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a AgentReasoning,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        let tx = self.0.clone();
        let ev = UiCmd::ReasoningDelta(e.delta.clone());
        Box::pin(async move {
            let _ = tx.send(ev).await;
            Ok(None)
        })
    }
}

impl Plugin for TuiPlugin {
    fn name(&self) -> &str {
        "tui"
    }

    fn injects(&self) -> &[TypeKey] {
        &self.inject_keys
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let agent = ctx
                .get_as::<dyn Agent>(crate::agent_key())
                .ok_or_else(|| CordisError::InjectUnsatisfied(vec!["agent".to_string()]))?;

            // 终端恢复兜底:任何卸载路径(fiber dispose / apply 提前失败)
            ctx.effect(|| {
                Effect::Disposer(Box::new(|| {
                    let _ = restore_terminal();
                    Ok(())
                }))
            })?;
            setup_terminal()
                .map_err(|e| CordisError::PluginFailed(format!("tui setup: {e}").into()))?;

            // async 侧(.Send)持有的通道:
            //   ui_tx —— agent 事件 listener → 渲染线程
            //   (input 经 EventStream 直接读,转 key 到 thread_tx)
            let (ui_tx, mut ui_rx) = mpsc::channel::<UiCmd>(256);
            // agent/* 事件订阅:监听器归本 fiber 所有,随 fiber 卸载(D28)
            ctx.events().on(ctx, ReasoningL(ui_tx.clone()))?;
            ctx.events().on(ctx, DeltaL(ui_tx.clone()))?;
            ctx.events().on(ctx, ToolCallL(ui_tx.clone()))?;
            ctx.events().on(ctx, ToolResultL(ui_tx.clone()))?;
            ctx.events().on(ctx, TurnEndL(ui_tx.clone()))?;

            // 渲染线程 ↔ async 侧的 std 通道(Send,跨 OS 线程)
            let (thread_tx, thread_rx) = std::sync::mpsc::channel::<ThreadMsg>();
            // 渲染线程回传结果(Quit/Cancel/Submit 动作需在 tokio runtime 执行)
            let (outcome_tx, mut outcome_rx) = oneshot::channel::<Result<(), CordisError>>();

            let intro = self.intro.clone();
            let handle = ctx.handle().clone();

            // ── 渲染线程:持有非 Send 的 App + Terminal,跑同步循环 ──
            let render_thread = std::thread::Builder::new()
                .name("rutis-tui".into())
                .spawn(move || {
                    let backend = CrosstermBackend::new(std::io::stdout());
                    let mut terminal = match Terminal::new(backend) {
                        Ok(t) => t,
                        Err(e) => {
                            let _ = outcome_tx.send(Err(CordisError::PluginFailed(
                                format!("tui terminal: {e}").into(),
                            )));
                            return;
                        }
                    };
                    let mut app = App::new(&intro);
                    let outcome: Result<(), CordisError> = loop {
                        let _ = app.scrollback.tick();
                        if let Err(e) = terminal.draw(|f| render(f, &mut app)) {
                            break Err(CordisError::PluginFailed(
                                format!("tui draw: {e}").into(),
                            ));
                        }
                        // 非阻塞收取消息;无消息时短暂 park 等下一帧
                        match thread_rx.recv_timeout(Duration::from_millis(50)) {
                            Ok(ThreadMsg::Key(key)) => {
                                if key.kind == KeyEventKind::Press {
                                    match app.handle_key(key) {
                                        Some(HandleOutcome::Quit) => break Ok(()),
                                        Some(HandleOutcome::Cancel) => {
                                            let agent = agent.clone();
                                            handle.spawn(async move {
                                                agent.cancel();
                                            });
                                        }
                                        Some(HandleOutcome::Submit(text)) => {
                                            let agent = agent.clone();
                                            handle.spawn(async move {
                                                if let Err(e) = agent.followup(&text).await {
                                                    eprintln!("turn failed: {e}");
                                                }
                                            });
                                        }
                                        None => {}
                                    }
                                }
                            }
                            Ok(ThreadMsg::Mouse(m)) => match m.kind {
                                MouseEventKind::ScrollUp => {
                                    app.selection = None;
                                    app.scrollback.scroll_up(MOUSE_SCROLL_LINES)
                                }
                                MouseEventKind::ScrollDown => {
                                    app.selection = None;
                                    app.scrollback.scroll_down(MOUSE_SCROLL_LINES)
                                }
                                // 左键拖选:按下起选、拖动延伸、抬起取文本入剪贴板。
                                MouseEventKind::Down(MouseButton::Left) => {
                                    app.selection = conv_point((m.column, m.row), app.conv_area)
                                        .map(|p| Selection { start: p, end: p });
                                }
                                MouseEventKind::Drag(MouseButton::Left) => {
                                    if let (Some(sel), Some(p)) = (
                                        &mut app.selection,
                                        conv_point((m.column, m.row), app.conv_area),
                                    ) {
                                        sel.end = p;
                                    }
                                }
                                MouseEventKind::Up(MouseButton::Left) => {
                                    if let Some(sel) = app.selection {
                                        if sel.start != sel.end {
                                            if let Some(text) =
                                                selection_text(terminal.current_buffer_mut(), &sel, app.conv_area)
                                            {
                                                copy_to_clipboard_os52(&text);
                                            }
                                        } else {
                                            app.selection = None;
                                        }
                                    }
                                }
                                _ => {}
                            },
                            Ok(ThreadMsg::Ui(cmd)) => app.on_ui_cmd(cmd),
                            Ok(ThreadMsg::Cancel) => break Ok(()),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
                        }
                    };
                    let _ = restore_terminal();
                    let _ = outcome_tx.send(outcome);
                })
                .map_err(|e| CordisError::PluginFailed(format!("tui thread spawn: {e}").into()))?;

            // ── async 侧:多路复用 crossterm 事件 / agent 事件 / 取消 ──
            let (input_tx, mut input_rx) = mpsc::channel::<CrosstermEvent>(16);
            let input_task = ctx.handle().spawn(async move {
                let mut events = EventStream::new();
                while let Some(Ok(ev)) = events.next().await {
                    if input_tx.send(ev).await.is_err() {
                        break;
                    }
                }
            });

            let outcome: Result<(), CordisError> = loop {
                tokio::select! {
                    // fiber 卸载 → 通知渲染线程退出
                    _ = ctx.cancelled() => {
                        let _ = thread_tx.send(ThreadMsg::Cancel);
                        break Ok(());
                    }
                    ev = input_rx.recv() => {
                        let Some(ev) = ev else { continue };
                        let msg = match ev {
                            CrosstermEvent::Key(key) => ThreadMsg::Key(key),
                            CrosstermEvent::Mouse(m) => ThreadMsg::Mouse(m),
                            _ => continue,
                        };
                        if thread_tx.send(msg).is_err() {
                            break Ok(());
                        }
                    }
                    ev = ui_rx.recv() => {
                        let Some(ev) = ev else { continue };
                        if thread_tx.send(ThreadMsg::Ui(ev)).is_err() {
                            break Ok(());
                        }
                    }
                    r = &mut outcome_rx => {
                        // 渲染线程已结束(Quit / draw 失败)
                        match r {
                            Ok(o) => break o,
                            Err(_) => break Ok(()),
                        }
                    }
                }
            };

            input_task.abort();
            // 等渲染线程彻底退出(确保终端已恢复)
            let _ = render_thread.join();
            outcome?;
            Ok(Effect::Done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把一个 RenderBlock 归约成可断言的标签。
    fn label(block: &RenderBlock) -> String {
        match block {
            RenderBlock::AgentMessage(m) => format!("agent({})", m.text()),
            RenderBlock::ToolCall(ToolCallBlock::Other(t)) => {
                let out = t
                    .output
                    .as_deref()
                    .map(|o| format!(",out={o}"))
                    .unwrap_or_default();
                let err = t
                    .error
                    .as_deref()
                    .map(|e| format!(",err={e}"))
                    .unwrap_or_default();
                format!("tool({},{}{out}{err})", t.name, t.summary)
            }
            RenderBlock::UserPrompt(u) => format!("user({})", u.text),
            RenderBlock::System(_) => "system".to_string(),
            other => format!("{other:?}"),
        }
    }

    /// turn 内 text 与 toolcall 必须按事件顺序交替成独立 block:
    /// [user][text1][tool1][tool2][text3]——工具调用之后的文本不得
    /// 追加到之前的 text block(旧 bug:全部塞进第一个 assistant 行)。
    #[test]
    fn text_and_tool_calls_interleave_in_order() {
        let mut app = App::new(&[]);

        // 用户提交
        app.submit("hello");

        // step1: 先流文本,再工具调用(同一 step)
        app.on_ui_cmd(UiCmd::TextDelta("I'll check ".into()));
        app.on_ui_cmd(UiCmd::TextDelta("the weather".into()));
        app.on_ui_cmd(UiCmd::ToolCall {
            name: "get_weather".into(),
            args: "city=Oslo".into(),
        });
        app.on_ui_cmd(UiCmd::ToolResult {
            name: "get_weather".into(),
            ok: true,
            output: "18°".into(),
        });

        // step2: 只有工具调用
        app.on_ui_cmd(UiCmd::ToolCall {
            name: "get_weather".into(),
            args: "city=Bergen".into(),
        });
        app.on_ui_cmd(UiCmd::ToolResult {
            name: "get_weather".into(),
            ok: false,
            output: "timeout".into(),
        });

        // step3: 终答文本
        app.on_ui_cmd(UiCmd::TextDelta("Final answer.".into()));
        app.on_ui_cmd(UiCmd::TurnEnd {
            ok: true,
            error: String::new(),
        });

        let blocks: Vec<String> = (0..app.scrollback.len())
            .filter_map(|i| app.scrollback.get(i).map(|e| label(&e.block)))
            .collect();

        assert_eq!(
            blocks,
            vec![
                "user(hello)".to_string(),
                "agent(I'll check the weather)".to_string(),
                "tool(get_weather,city=Oslo,out=18°)".to_string(),
                "tool(get_weather,city=Bergen,err=timeout)".to_string(),
                "agent(Final answer.)".to_string(),
            ]
        );
    }

    /// 纯文本 turn(无工具):单一 agent block。
    #[test]
    fn plain_text_turn_is_single_block() {
        let mut app = App::new(&[]);
        app.submit("hi");
        app.on_ui_cmd(UiCmd::TextDelta("Hello ".into()));
        app.on_ui_cmd(UiCmd::TextDelta("world".into()));
        app.on_ui_cmd(UiCmd::TurnEnd {
            ok: true,
            error: String::new(),
        });
        let blocks: Vec<String> = (0..app.scrollback.len())
            .filter_map(|i| app.scrollback.get(i).map(|e| label(&e.block)))
            .collect();
        assert_eq!(
            blocks,
            vec!["user(hi)".to_string(), "agent(Hello world)".to_string()]
        );
    }

    /// turn 失败:agent block 收尾 + SessionEvent(TurnFailed)。
    #[test]
    fn failed_turn_emits_session_event() {
        let mut app = App::new(&[]);
        app.submit("hi");
        app.on_ui_cmd(UiCmd::TextDelta("partial".into()));
        app.on_ui_cmd(UiCmd::TurnEnd {
            ok: false,
            error: "boom".into(),
        });
        let blocks: Vec<String> = (0..app.scrollback.len())
            .filter_map(|i| app.scrollback.get(i).map(|e| label(&e.block)))
            .collect();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], "user(hi)");
        assert_eq!(blocks[1], "agent(partial)");
        // 最后一个 block 是 SessionEvent(TurnFailed)
        let last = app
            .scrollback
            .last()
            .expect("session event pushed")
            .block
            .clone();
        assert!(matches!(
            last,
            RenderBlock::SessionEvent(_)
        ));
    }

    /// 工具结果到达后,结果数据应合并进原 toolcall block(默认折叠,不强制展开)。
    /// 校验数据层面的"合并"——结果写入同一个 block,而非新建一个 block。
    #[test]
    fn tool_result_merged_into_block() {
        let mut app = App::new(&[]);
        app.submit("hi");
        app.on_ui_cmd(UiCmd::ToolCall {
            name: "get_weather".into(),
            args: "city=Oslo".into(),
        });
        let id = app
            .active
            .tool
            .expect("active.tool should be set after ToolCall");
        app.on_ui_cmd(UiCmd::ToolResult {
            ok: true,
            output: "18° clear".into(),
            name: "get_weather".into(),
        });
        app.on_ui_cmd(UiCmd::TurnEnd {
            ok: true,
            error: String::new(),
        });

        let block = &app
            .scrollback
            .get_by_id(id)
            .expect("tool entry should exist")
            .block;
        let merged = match block {
            RenderBlock::ToolCall(ToolCallBlock::Other(t)) => {
                t.output.as_deref() == Some("18° clear") && t.name == "get_weather"
            }
            _ => false,
        };
        assert!(merged, "tool result not merged into the original block");
    }

    /// reasoning 增量应渲染为独立 Thinking 块,且排在正文之前。
    #[test]
    fn reasoning_rendered_before_text() {
        let mut app = App::new(&[]);
        app.submit("hi");
        // 先 reasoning,再正文
        app.on_ui_cmd(UiCmd::ReasoningDelta("让我".into()));
        app.on_ui_cmd(UiCmd::ReasoningDelta("想想".into()));
        app.on_ui_cmd(UiCmd::TextDelta("答案是42".into()));
        app.on_ui_cmd(UiCmd::TurnEnd {
            ok: true,
            error: String::new(),
        });

        let blocks: Vec<RenderBlock> = (0..app.scrollback.len())
            .filter_map(|i| app.scrollback.get(i).map(|e| e.block.clone()))
            .collect();

        // 顺序应为: user -> thinking -> agent
        assert_eq!(blocks.len(), 3, "expected user/thinking/agent blocks");
        assert!(matches!(blocks[0], RenderBlock::UserPrompt(_)));
        assert!(matches!(blocks[1], RenderBlock::Thinking(_)));
        assert!(matches!(blocks[2], RenderBlock::AgentMessage(_)));

        // reasoning 内容应合并进 thinking 块
        let thinking = match &blocks[1] {
            RenderBlock::Thinking(t) => t,
            other => panic!("expected thinking block, got {other:?}"),
        };
        assert_eq!(thinking.text(), "让我想想");
    }

    /// 跨 turn:第二轮的正文应开新 block,不得合并进第一轮的历史 text 块。
    #[test]
    fn second_turn_text_is_separate_block() {
        let mut app = App::new(&[]);
        // 第一轮:reasoning + 正文
        app.submit("q1");
        app.on_ui_cmd(UiCmd::ReasoningDelta("思考1".into()));
        app.on_ui_cmd(UiCmd::TextDelta("答案1".into()));
        app.on_ui_cmd(UiCmd::TurnEnd {
            ok: true,
            error: String::new(),
        });
        // 第二轮:又一次 reasoning + 正文
        app.submit("q2");
        app.on_ui_cmd(UiCmd::ReasoningDelta("思考2".into()));
        app.on_ui_cmd(UiCmd::TextDelta("答案2".into()));
        app.on_ui_cmd(UiCmd::TurnEnd {
            ok: true,
            error: String::new(),
        });

        let blocks: Vec<RenderBlock> = (0..app.scrollback.len())
            .filter_map(|i| app.scrollback.get(i).map(|e| e.block.clone()))
            .collect();
        // 期望: user, thinking, agent(答案1), user, thinking, agent(答案2)
        assert_eq!(blocks.len(), 6, "expected 2 turns each with user/thinking/agent");
        // 第二轮正文是独立块
        let last = match &blocks[5] {
            RenderBlock::AgentMessage(a) => a,
            other => panic!("expected agent block for turn 2, got {other:?}"),
        };
        assert_eq!(last.text(), "答案2");
        // 第一轮正文仍是独立块,未被第二轮合并
        let first_agent = match &blocks[2] {
            RenderBlock::AgentMessage(a) => a,
            other => panic!("expected agent block for turn 1, got {other:?}"),
        };
        assert_eq!(first_agent.text(), "答案1");
    }

    /// 完整交错序列:reasoning → tool → reasoning → text,
    /// 单一 active 光标应产出 5 个独立块,无多余块、无合并。
    #[test]
    fn full_reasoning_tool_text_sequence() {
        let mut app = App::new(&[]);
        app.submit("q");
        // step 1: reasoning + 工具调用
        app.on_ui_cmd(UiCmd::ReasoningDelta("思考1".into()));
        app.on_ui_cmd(UiCmd::ToolCall {
            name: "get_weather".into(),
            args: "city=Oslo".into(),
        });
        app.on_ui_cmd(UiCmd::ToolResult {
            ok: true,
            output: "18° clear".into(),
            name: "get_weather".into(),
        });
        // step 2: reasoning + 终答正文
        app.on_ui_cmd(UiCmd::ReasoningDelta("思考2".into()));
        app.on_ui_cmd(UiCmd::TextDelta("最终答案".into()));
        app.on_ui_cmd(UiCmd::TurnEnd {
            ok: true,
            error: String::new(),
        });

        let blocks: Vec<RenderBlock> = (0..app.scrollback.len())
            .filter_map(|i| app.scrollback.get(i).map(|e| e.block.clone()))
            .collect();
        // user, thinking, tool, thinking, agent —— 恰好 5 个块
        assert_eq!(blocks.len(), 5, "expected 5 blocks, got {}", blocks.len());
        assert!(matches!(blocks[0], RenderBlock::UserPrompt(_)));
        assert!(matches!(blocks[1], RenderBlock::Thinking(_)));
        assert!(matches!(blocks[2], RenderBlock::ToolCall(_)));
        assert!(matches!(blocks[3], RenderBlock::Thinking(_)));
        assert!(matches!(blocks[4], RenderBlock::AgentMessage(_)));
    }

    /// 回归:reasoning 与正文在事件流里交错时(两个监听器各自保序但互相穿插),
    /// 正文必须合并进**单个** agent 块,不能被切碎成多个片段。
    /// 早期"单一 active cursor + 换 kind 就收尾"的模型会把正文碎片化。
    #[test]
    fn interleaved_reasoning_does_not_fragment_agent() {
        let mut app = App::new(&[]);
        app.submit("奥斯陆今天天气怎么样请详细描述一下");

        let chunks = |s: &str| {
            let cs: Vec<char> = s.chars().collect();
            cs.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>()
        };
        let reason1 = "用户想知道天气。我需要先查一下奥斯陆的天气,调用 get_weather 工具。";
        let reason2 = "工具已返回,现在直接回答用户。";
        let content = "奥斯陆今天 18 度,晴。较长的中文文本——用来验证 TUI 的 流式渲染与按显示宽度折行。";

        // step 1: reasoning + 工具
        for d in chunks(reason1) {
            app.on_ui_cmd(UiCmd::ReasoningDelta(d));
        }
        app.on_ui_cmd(UiCmd::ToolCall {
            name: "get_weather".into(),
            args: "{\"city\":\"Oslo\"}".into(),
        });
        app.on_ui_cmd(UiCmd::ToolResult {
            name: "get_weather".into(),
            ok: true,
            output: "18°C sunny".into(),
        });
        // step 2: reasoning,正文与零星 reasoning 交错
        for d in chunks(reason2) {
            app.on_ui_cmd(UiCmd::ReasoningDelta(d));
        }
        let cc = chunks(content);
        let n = cc.len();
        for (i, d) in cc.iter().enumerate() {
            if i == n / 2 {
                app.on_ui_cmd(UiCmd::ReasoningDelta("补充思考".into()));
            }
            app.on_ui_cmd(UiCmd::TextDelta(d.clone()));
        }
        app.on_ui_cmd(UiCmd::TurnEnd {
            ok: true,
            error: String::new(),
        });

        let blocks: Vec<RenderBlock> = (0..app.scrollback.len())
            .map(|i| app.scrollback.get(i).unwrap().block.clone())
            .collect();
        let agents: Vec<&RenderBlock> = blocks
            .iter()
            .filter(|b| matches!(b, RenderBlock::AgentMessage(_)))
            .collect();

        // 恰好一个 agent 块(未被 reasoning 交错切碎)
        assert_eq!(agents.len(), 1, "正文应合并进单个 agent 块,即使 reasoning 交错");
        // 且文本 = 完整正文(顺序正确,未被截断)
        let agent_text = match agents[0] {
            RenderBlock::AgentMessage(a) => a.text(),
            _ => unreachable!(),
        };
        assert_eq!(agent_text, content, "agent 块文本应为完整正文");
    }

    /// 选区行段:单行取两点之间;多行首行起点列→行尾、末行行首→终点列、中间整行;
    /// 且与拖动方向无关(上→下 与 下→上 结果一致)。
    #[test]
    fn selection_rows_single_and_multi_line() {
        let area = Rect::new(0, 0, 10, 5);
        assert_eq!(
            selection_rows(&Selection { start: (2, 3), end: (6, 3) }, area),
            vec![(3, 2, 6)]
        );
        let top_down = selection_rows(&Selection { start: (3, 1), end: (4, 3) }, area);
        assert_eq!(top_down, vec![(1, 3, 9), (2, 0, 9), (3, 0, 4)]);
        let bottom_up = selection_rows(&Selection { start: (4, 3), end: (3, 1) }, area);
        assert_eq!(bottom_up, top_down, "选区应与拖动方向无关");
    }

    /// 取文本:CJK 宽字符(尾格被 ratatui reset 为空白)不得在汉字间多出空格;
    /// 跨行时首行取到行尾、末行取到终点,行间以换行连接、行尾空白去除。
    #[test]
    fn selection_text_extracts_cjk_without_extra_spaces() {
        let area = Rect::new(0, 0, 24, 4);
        let mut buf = Buffer::empty(area);
        // 7 个汉字,各占 2 格 → cols 0..=13(尾格空白)
        buf.set_stringn(0, 0, "奥斯陆天气晴朗", 24, Style::default());
        // ASCII 行
        buf.set_stringn(0, 1, "hello world", 24, Style::default());

        // 单行整段 CJK
        let sel = Selection { start: (0, 0), end: (13, 0) };
        assert_eq!(
            selection_text(&buf, &sel, area).as_deref(),
            Some("奥斯陆天气晴朗")
        );

        // 跨行:第 0 行 cols 4.. 到行尾 + 第 1 行 cols 0..=7
        let sel = Selection { start: (4, 0), end: (7, 1) };
        assert_eq!(
            selection_text(&buf, &sel, area).as_deref(),
            Some("陆天气晴朗\nhello wo")
        );
    }
}
