//! TUI 前端插件(验证文档 §二)——消费 `Agent` 服务,不是 loop 的一部分。
//!
//! - 输入行:Enter 提交 → `agent.followup(text)` 流式消费
//! - 对话区:`TextDelta` 逐字追加,`ToolCall`/`ToolResult` 可见
//! - 状态栏:idle | running
//! - Esc / Ctrl+C(运行中)取消当前 turn;Ctrl+Q 退出(卸载 TUI fiber)
//!
//! apply 即 UI 主循环:运行期间 fiber 停在 Loading,settle(`(&view).await`)
//! 在退出后完成;fiber 卸载(`ctx.cancelled()`)与 draw 失败都走终端恢复。

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, TypeKey};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use crate::agent::{Agent, TurnEvent};

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

fn setup_terminal() -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    Ok(())
}

fn restore_terminal() -> std::io::Result<()> {
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

// ── UI 状态 ────────────────────────────────────────────────────────

#[derive(Default)]
struct App {
    transcript: Vec<Line<'static>>,
    input: String,
    running: bool,
    tool_count: usize,
    /// 当前流式 assistant 行(transcript 下标);工具/终答后关闭。
    cur_assistant: Option<usize>,
}

impl App {
    fn push_line(&mut self, line: Line<'static>) {
        self.cur_assistant = None;
        self.transcript.push(line);
    }

    fn submit(&mut self, text: &str) {
        self.push_line(Line::from(Span::styled(
            format!("you> {text}"),
            Style::default().fg(Color::Blue),
        )));
    }

    fn on_turn_event(&mut self, ev: TurnEvent) {
        match ev {
            TurnEvent::TextDelta(delta) => match self.cur_assistant {
                Some(idx) => self.transcript[idx].spans.push(Span::raw(delta)),
                None => {
                    self.transcript.push(Line::from(vec![
                        Span::styled("agent> ", Style::default().fg(Color::Green)),
                        Span::raw(delta),
                    ]));
                    self.cur_assistant = Some(self.transcript.len() - 1);
                }
            },
            TurnEvent::ToolCall { name, args } => {
                self.tool_count += 1;
                self.push_line(Line::from(Span::styled(
                    format!("* {name}({args})"),
                    Style::default().fg(Color::Cyan),
                )));
            }
            TurnEvent::ToolResult { name, ok, output } => {
                let style = if ok {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Red)
                };
                self.push_line(Line::from(Span::styled(
                    format!("  -> [{name}] {output}"),
                    style,
                )));
            }
            TurnEvent::Done(Ok(_)) => {
                self.running = false;
                self.tool_count = 0;
            }
            TurnEvent::Done(Err(e)) => {
                self.push_line(Line::from(Span::styled(
                    format!("! {e}"),
                    Style::default().fg(Color::Red),
                )));
                self.running = false;
                self.tool_count = 0;
            }
        }
    }

    fn status_line(&self) -> String {
        if self.running {
            format!(
                " status: running (tool {}) | Esc cancel | Ctrl+Q quit",
                self.tool_count
            )
        } else {
            " status: idle | Ctrl+Q quit".to_string()
        }
    }

    /// 按键处理。返回 true 表示退出。
    fn handle_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        agent: &Arc<dyn Agent>,
        handle: &Handle,
        turn_tx: &mpsc::Sender<TurnEvent>,
    ) -> bool {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) if !self.running => return true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => agent.cancel(),
            (KeyCode::Enter, _) => {
                if !self.running && !self.input.trim().is_empty() {
                    let input = std::mem::take(&mut self.input);
                    self.submit(&input);
                    self.running = true;
                    let agent = agent.clone();
                    let turn_tx = turn_tx.clone();
                    handle.spawn(async move {
                        let mut stream = agent.followup(&input);
                        while let Some(ev) = stream.next().await {
                            if turn_tx.send(ev).await.is_err() {
                                break;
                            }
                        }
                    });
                }
            }
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => self.input.push(c),
            (KeyCode::Backspace, _) => {
                self.input.pop();
            }
            _ => {}
        }
        false
    }
}

/// 按显示宽度把一条逻辑行软折成多条显示行(保留 span 样式)。
/// CJK 全角占 2 列,按 unicode-width 计列,保证右边框不因宽字符错位、
/// 长回复不被截断。
fn wrap_line(line: &Line<'_>, max: usize) -> Vec<Line<'static>> {
    if max == 0 {
        // 宽度不可用(极端窄窗):按静态行重建,避免借用泄漏
        return vec![Line::from(
            line.spans
                .iter()
                .map(|s| Span {
                    content: std::borrow::Cow::Owned(s.content.to_string()),
                    style: s.style,
                })
                .collect::<Vec<_>>(),
        )];
    }
    let mut lines = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut cur_w = 0usize;
    for span in &line.spans {
        let mut buf = String::new();
        let mut buf_w = 0usize;
        for ch in span.content.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if cur_w + buf_w + cw > max {
                if !buf.is_empty() {
                    cur.push(Span {
                        content: std::mem::take(&mut buf).into(),
                        style: span.style,
                    });
                    buf_w = 0;
                }
                lines.push(Line::from(std::mem::take(&mut cur)));
                cur_w = 0;
            }
            buf.push(ch);
            buf_w += cw;
        }
        if !buf.is_empty() {
            cur.push(Span {
                content: buf.into(),
                style: span.style,
            });
            cur_w += buf_w;
        }
    }
    lines.push(Line::from(cur));
    lines
}

fn render(frame: &mut Frame, app: &App) {
    let [conv, status, input] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let conv_block = Block::bordered().title(" conversation ");
    let inner_w = conv.width.saturating_sub(2) as usize;
    let inner_h = conv.height.saturating_sub(2) as usize; // 边框上下各 1
    let mut display: Vec<Line> = Vec::new();
    for line in &app.transcript {
        display.extend(wrap_line(line, inner_w));
    }
    let skip = display.len().saturating_sub(inner_h);
    let lines: Vec<Line> = display.iter().skip(skip).cloned().collect();
    frame.render_widget(Paragraph::new(lines).block(conv_block), conv);

    frame.render_widget(
        Paragraph::new(app.status_line()).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        status,
    );

    let input_block = Block::bordered().title(" input ");
    let input_inner = input_block.inner(input);
    frame.render_widget(Paragraph::new(app.input.as_str()).block(input_block), input);
    // 光标跟随输入尾部:按显示宽度(CJK 全角占 2 列),并夹在输入框内
    let width = unicode_width::UnicodeWidthStr::width(app.input.as_str()) as u16;
    let col = input_inner.x + width.min(input_inner.width.saturating_sub(1));
    frame.set_cursor_position((col, input_inner.y));
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
            let backend = CrosstermBackend::new(std::io::stdout());
            let mut terminal = Terminal::new(backend)
                .map_err(|e| CordisError::PluginFailed(format!("tui terminal: {e}").into()))?;

            let (input_tx, mut input_rx) = mpsc::channel::<CrosstermEvent>(16);
            let (turn_tx, mut turn_rx) = mpsc::channel::<TurnEvent>(64);
            // 输入读取任务:退出时 abort
            let input_task = ctx.handle().spawn(async move {
                let mut events = EventStream::new();
                while let Some(Ok(ev)) = events.next().await {
                    if input_tx.send(ev).await.is_err() {
                        break;
                    }
                }
            });

            let mut app = App::default();
            for line in &self.intro {
                app.push_line(Line::from(Span::styled(
                    line.clone(),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            let outcome: Result<(), CordisError> = loop {
                if let Err(e) = terminal.draw(|f| render(f, &app)) {
                    break Err(CordisError::PluginFailed(format!("tui draw: {e}").into()));
                }
                tokio::select! {
                    // fiber 卸载 → 退出 UI(终端恢复走 effect 兜底 + 下面的显式调用)
                    _ = ctx.cancelled() => break Ok(()),
                    _ = tick.tick() => {}
                    ev = input_rx.recv() => {
                        let Some(CrosstermEvent::Key(key)) = ev else { continue };
                        if key.kind == KeyEventKind::Press
                            && app.handle_key(key, &agent, ctx.handle(), &turn_tx)
                        {
                            break Ok(());
                        }
                    }
                    ev = turn_rx.recv() => {
                        if let Some(ev) = ev {
                            app.on_turn_event(ev);
                        }
                    }
                }
            };
            input_task.abort();
            let _ = restore_terminal();
            outcome?;
            Ok(Effect::Done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Line<'static> {
        Line::from(Span::raw(text.to_string()))
    }

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone().into_owned())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn short_line_is_unchanged() {
        let out = wrap_line(&plain("hello"), 20);
        assert_eq!(texts(&out), vec!["hello".to_string()]);
    }

    #[test]
    fn ascii_wraps_at_column_boundary() {
        let out = wrap_line(&plain("abcdefghij"), 4);
        assert_eq!(
            texts(&out),
            vec!["abcd".to_string(), "efgh".to_string(), "ij".to_string()]
        );
    }

    #[test]
    fn cjk_counts_display_columns_not_chars() {
        // 5 个全角字符 = 10 列:max=4 → 每行 2 个全角
        let out = wrap_line(&plain("你好世界呀"), 4);
        assert_eq!(
            texts(&out),
            vec!["你好".to_string(), "世界".to_string(), "呀".to_string()]
        );
    }

    #[test]
    fn span_styles_survive_wrapping() {
        let line = Line::from(vec![
            Span::styled("you> ", Style::default().fg(Color::Blue)),
            Span::raw("奥斯陆今天十八度晴"),
        ]);
        let out = wrap_line(&line, 8); // "you> "(5 列)+ 奥(2 列)= 7 ≤ 8,再 +1 超限
        assert_eq!(
            texts(&out),
            vec![
                "you> 奥".to_string(),  // 前缀 span + 1 个全角
                "斯陆今天".to_string(), // 尾段 16 列折两行
                "十八度晴".to_string(),
            ]
        );
        assert_eq!(out[0].spans.len(), 2); // 样式边界保留:前缀 span + 内容 span
        assert_eq!(out[1].spans.len(), 1);
        assert_eq!(out[2].spans.len(), 1);
    }
}
