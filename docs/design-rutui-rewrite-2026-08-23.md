# 用 rutui 重写 agent TUI baseline 的设计

> 2026-08-23。配套 [design-agent-verification-tui-2026-08-18.md](design-agent-verification-tui-2026-08-18.md)(当前 TUI 设计)与 [design-min-agent-2026-08-18.md](design-min-agent-2026-08-18.md)(agent 框架)。本文定义:**用 [rutui](https://github.com/eric8810/rutui) 重写 `rutis-agent` 的 `TuiPlugin`**——替换全手写的对话/输入/折行逻辑,换成 rutui 的 block 式 scrollback + 多行 prompt editor,agent 框架核心零改动。

## 一、动机:当前 baseline 的局限

当前 [`crates/rutis-agent/src/tui.rs`](../crates/rutis-agent/src/tui.rs) 是全手写 TUI(约 460 行,ratatui 0.30 + crossterm 0.29):

| 部分 | 现状 | 局限 |
|---|---|---|
| 对话区 | `App.transcript: Vec<Line<'static>>` + 手写 [`wrap_line`](../crates/rutis-agent/src/tui.rs#L294)(~50 行 + 4 单测处理 CJK 全角列宽) | 无滚动条、无折叠、无选择/复制、无搜索、无 sticky header |
| 输入框 | `App.input: String` + 手写单行编辑(仅 `Char`/`Backspace`) | 无光标移动、无多行、无 readline 快捷键、无 undo、无粘贴、无历史 |
| assistant 流式 | `AgentTextDelta` → 追加 span 到当前行 | 纯文本,**无 markdown 渲染、无代码高亮** |
| 工具展示 | 手写 `* name(args)` / `-> [name] output` 行 | 无折叠、无 diff、无状态色 |
| 状态栏 | 手写 `status_line()` | 无快捷键提示 |
| 终端 | 手写 `setup/restore_terminal` | 无终端能力探测(truecolor/sixel/kitty/OSC8/tmux) |

**核心痛点**:markdown/代码高亮缺失 + 输入框能力贫弱 + 折行/滚动全是手写轮子。rutui 恰好是从一个生产级 coding agent 抽取的、零业务耦合的 TUI 工具箱,直接对症。

## 二、rutui 是什么

[rutui](https://github.com/eric8810/rutui)(Apache-2.0)抽取自生产级 agentic coding CLI(`xai-grok-pager`),基于 ratatui + crossterm,Elm 式数据流(input → intent → 同步状态更新 → async effect)。**零应用业务逻辑**:状态/协议/产品功能由调用方经泛型与 `install_*` 接缝注入。

### crate 一览(与本次重写相关)

| crate | 作用 | 本次用途 |
|---|---|---|
| `rutui-core` | block 式 scrollback 渲染管线:内容块、滚动、折叠、选择、搜索、sticky header、timeline rail | **对话区** |
| `rutui-prompt` | 多行 prompt 编辑器:textarea + 粘贴/图片元素 + ghost text + completion + 模糊历史搜索 | **输入框** |
| `rutui-input` | 键抽象 + 修饰键归一化 + `ActionRegistry<A,C>` 键绑定注册表(3 级冒泡) | **键绑定**(P2) |
| `rutui-theme` | 5 内置主题 + 终端能力探测 + 色彩量化 + 外观配置 | **主题/终端探测**(P2) |
| `rutui-widgets` | modal/overlay/progress bar/shortcuts bar | **快捷键栏**(P2) |
| `rutui-markdown` | 流式 markdown 渲染器(pulldown-cmark + syntect 高亮) | agent 消息块内置使用 |
| `rutui-foundations` | 伞 crate:textarea + inline viewport + TTY 安全子进程 | (间接,经 core/prompt) |

### 可用性验证

对 5 个核心 crate 跑 `cargo check -p rutui-core -p rutui-prompt -p rutui-input -p rutui-theme -p rutui-widgets`,**全部通过**(含 `rutui-prompt` 的 nucleo git 依赖)。编译耗时 ~15s。

## 三、核心 API 映射

### 3.1 对话区:`ScrollbackState` + `ScrollbackPane`

[`ScrollbackState`](https://github.com/eric8810/rutui/blob/main/rutui-core/src/scrollback/state/mod.rs) 是对话状态机,持有 `IndexMap<EntryId, ScrollbackEntry>`;[`ScrollbackPane`](https://github.com/eric8810/rutui/blob/main/rutui-core/src/scrollback/scrollback_pane.rs) 是 `StatefulWidget`,`State = ScrollbackState`。

流式 API(与我们 agent 事件语义直接对应):

```rust
// ScrollbackState
pub fn push_block(&mut self, block: RenderBlock) -> EntryId;        // 追加一个块,返回 id
pub fn push_chunk_to_agent(&mut self, id: EntryId, chunk: &str) -> bool;  // 流式追加到 agent 消息
pub fn set_entry_running(&mut self, id: EntryId, running: bool);    // 标记块运行中(动画 bullet)
pub fn finish_running(&mut self, id: EntryId);                      // 标记完成(停止动画)
pub fn tick(&mut self) -> bool;                                     // 动画推进,返回是否需重绘
pub fn set_appearance(&mut self, appearance: AppearanceConfig);     // 外观(折叠/raw 等)

// ScrollbackPane(渲染)
impl StatefulWidget for ScrollbackPane {
    type State = ScrollbackState;
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State);
}
// 或 render_with_scratch(area, buf, &state, &mut scratch) 复用 scratch buffer
```

### 3.2 `RenderBlock` 变体 ↔ agent 事件

[`RenderBlock`](https://github.com/eric8810/rutui/blob/main/rutui-core/src/scrollback/block.rs#L371) 枚举(只列我们用到的变体,其余忽略):

| `RenderBlock` 变体 | 对应 agent 事件 | 块类型 |
|---|---|---|
| `UserPrompt(UserPromptBlock)` | 用户 Enter 提交 | 用户消息 |
| `AgentMessage(AgentMessageBlock)` | `AgentTextDelta`(流式) | **markdown + 代码高亮** |
| `ToolCall(ToolCallBlock::Other(OtherToolCallBlock))` | `AgentToolCall` + `AgentToolResult` | 通用工具块(bash/replace_text) |
| `SessionEvent(SessionEventBlock)` | `AgentTurnEnd{ok:false}` | turn 失败事件 |
| `System(SystemMessageBlock)` | intro 行 | 系统说明 |

`AgentMessageBlock` 流式用法:

```rust
let id = state.push_block(RenderBlock::AgentMessage(AgentMessageBlock::streaming()));
state.set_entry_running(id, true);
for delta in stream { state.push_chunk_to_agent(id, &delta); }
state.finish_running(id);
```

`AgentMessageBlock` 底层是 [`MarkdownContent`](https://github.com/eric8810/rutui/blob/main/rutui-core/src/scrollback/blocks/markdown_content.rs),直接用 `rutui_markdown::StreamingMarkdownRenderer`——**不走 `install_renderer` 接缝**,markdown 渲染与代码高亮开箱即用(代码高亮经 `get_syntect()`,从 bundled `.tmTheme` 资产惰性构建,无需宿主注入)。

`OtherToolCallBlock`(通用工具,适合 bash/replace_text):

```rust
let id = state.push_block(RenderBlock::ToolCall(ToolCallBlock::Other(
    OtherToolCallBlock::new(name, args)
)));
state.set_entry_running(id, true);
// 结果回来:
//   finish_running(id) + (替换块为带 output/error 的版本,或用 replace_tool_block)
```

### 3.3 输入框:`PromptWidget`

[`PromptWidget`](https://github.com/eric8810/rutui/blob/main/rutui-prompt/src/prompt_widget/mod.rs) 是多行编辑器:

```rust
pub struct PromptWidget { /* textarea + 元素 + ghost text + completion */ }
impl PromptWidget {
    pub fn new() -> Self;
    pub fn handle_key(&mut self, key: &KeyEvent) -> PromptEvent;  // Edited | Ignored
    pub fn route_enter(&mut self, key: &KeyEvent) -> EnterOutcome; // NewlineInserted | Submit | PassThrough
    pub fn text(&self) -> &str;
    pub fn set_text(&mut self, text: &str);
    pub fn desired_height(&self, ...) -> u16;  // 自适应高度
}
```

内置 readline(Ctrl-A/E/W/U/K/Y)、undo/redo、粘贴元素、ghost text、completion dropdown、模糊历史搜索。`handle_key` 返回 `PromptEvent::Ignored` 时交由调用方处理(Esc/Tab/Ctrl-D 等),契合我们的键分发需求。

### 3.4 接缝:零强制

rutui-theme 的 `install_*` 接缝全是 `OnceLock` 可选注入:

| 接缝 | 不装的后果 |
|---|---|
| `md_style::install_renderer` | 返回 `None`——但 `AgentMessageBlock` 不走这条路(用 `rutui_markdown` 直连),**无影响** |
| `clipboard::install_host` | copy 返回 Err,**不 panic**,优雅降级 |
| `appearance::install_pager_config_path` | 用默认路径 |

`Theme::current()` 自带默认主题(读 `~/.grok/config.toml` 失败回退 GrokNight),`AppearanceConfig::default()` 可用。**bootstrap 零强制接缝**——基本渲染开箱即用。

## 四、事件 → block 映射

```
用户 Enter(submit)    → push_block(UserPromptBlock::new(text)) + agent.followup(text) + running=true
AgentTextDelta(首个)  → id=push_block(AgentMessageBlock::streaming()); set_entry_running(id,true); 记 cur_agent=id
AgentTextDelta(后续)  → push_chunk_to_agent(cur_agent, &delta)
AgentToolCall         → id=push_block(OtherToolCallBlock::new(name, args)); set_entry_running(id,true); 记 cur_tool=id
AgentToolResult{ok}   → 补全 tool 块 output + finish_running(cur_tool)
AgentToolResult{err}  → 补全 tool 块 error + finish_running(cur_tool)
AgentTurnEnd{ok}      → finish_running(cur_agent)(如有) + running=false
AgentTurnEnd{err}     → push_block(SessionEvent::failed(error)) + running=false
intro                 → push_block(SystemMessageBlock::new(line))(apply 启动时)
```

与当前 `UiEvent` 枚举 + `App::on_ui_event` 的语义一致,只是状态载体从 `Vec<Line>` 换成 `ScrollbackState`,折行/滚动/动画交给 rutui。

## 五、重写范围与不动边界

### 改

- [`crates/rutis-agent/src/tui.rs`](../crates/rutis-agent/src/tui.rs)——重写。
- [`crates/rutis-agent/Cargo.toml`](../crates/rutis-agent/Cargo.toml)——依赖调整(见 §六)。

### 不动

- agent 框架核心:[agent.rs](../crates/rutis-agent/src/agent.rs) / [driver.rs](../crates/rutis-agent/src/driver.rs) / [events.rs](../crates/rutis-agent/src/events.rs) / [session.rs](../crates/rutis-agent/src/session.rs) / [minimal.rs](../crates/rutis-agent/src/minimal.rs) / [scripted.rs](../crates/rutis-agent/src/scripted.rs) / tools。
- `TuiPlugin` 的**插件骨架**:`Plugin` trait、`injects = [agent]`、`apply` 即主循环、`with_intro`、fiber 卸载取消(`ctx.cancelled()`)、listener 随 fiber 卸载(D28)——全保留。
- [examples/](../crates/rutis-agent/examples)(tui.rs / tui_scripted.rs / demo.rs)与 [rutis-cli](../crates/rutis-cli/src/main.rs)——API 兼容,自动受益。

### 新 `TuiPlugin` 结构(示意)

```rust
pub struct TuiPlugin {
    inject_keys: Vec<TypeKey>,   // vec![agent_key()]  不变
    intro: Vec<String>,          // 不变
}

struct App {
    scrollback: ScrollbackState,        // 替换 Vec<Line>
    prompt: PromptWidget,               // 替换 input: String
    cur_agent: Option<EntryId>,         // 当前流式 agent 块
    cur_tool: Option<EntryId>,          // 当前运行中工具块
    running: bool,
    scratch: ScratchBuffer,             // 复用渲染 scratch
}
```

4 个 `Listener`(Delta/ToolCall/ToolResult/TurnEnd)仍把 `agent/*` 事件转发进 `mpsc::channel`,主循环 `select!` 消费后调 `App` 方法操作 `ScrollbackState`——**事件桥架构不变**,只是 `UiEvent` → `App` 方法 → `ScrollbackState` 操作。

## 六、依赖与版本对齐(最大风险)

### 冲突

| crate | rutis-agent 现用 | rutui 锁定 |
|---|---|---|
| ratatui | 0.30.2 | **0.29** |
| crossterm | 0.29.0 | **0.28** |
| unicode-width | 0.2.2 | 0.2(兼容) |

rutui 在**公共 API 中暴露 ratatui 类型**(`StatefulWidget`、`handle_key(&KeyEvent)`、`ScrollbackPane::render`),消费者**必须对齐版本**,否则类型不匹配编译失败。

### 解法:rutis-agent 降级

将 rutis-agent 的 ratatui 降到 0.29、crossterm 降到 0.28。tui.rs 用的都是基础类型(`Frame`/`Terminal`/`Block`/`Paragraph`/`Layout`/`Color`/`Style`/`Span`/`Line`),0.29→0.30 间这些 API 无破坏性变更,降级改动极小(主要在重写时自然完成)。

> 备选:等 rutui 升级到 ratatui 0.30。但 rutui 刚抽取、仅 1 commit、自述"Expect breaking changes",短期内不可预期。降级是当下可行解。

### rutui 依赖方式

rutui **不在 crates.io**(1 commit),只能 git 依赖:

```toml
# crates/rutis-agent/Cargo.toml
[dependencies]
rutui-core   = { git = "https://github.com/eric8810/rutui.git", rev = "<pin>" }
rutui-prompt = { git = "https://github.com/eric8810/rutui.git", rev = "<pin>" }
# P2 再加: rutui-input / rutui-widgets / rutui-theme
```

- **必须锁 `rev`**:rutui API 不稳定,锁 commit 防意外破坏。
- `rutui-prompt` 会拉 [nucleo](https://github.com/helix-editor/nucleo)(git 依赖,用于模糊历史搜索)——编译已验证通过。
- 长期若 rutui 发 crates.io,改回版本依赖即可。

### rust-version

rutui 各 crate `edition = "2024"`,需 Rust 1.85+(rutis 工作区 `rust-version = "1.85"`,满足)。

## 七、分阶段实施

### P1:核心替换(跑通对话/流式/工具/输入)

1. Cargo.toml:降级 ratatui/crossterm + 加 rutui-core/rutui-prompt git 依赖。
2. 重写 tui.rs:
   - `App`:`ScrollbackState` + `PromptWidget` + `cur_agent`/`cur_tool`/`running` + `ScratchBuffer`。
   - 4 个 listener:翻译 agent 事件 → channel 命令(同当前架构)。
   - 主循环:`select!`(cancelled / tick / input / ui_rx),渲染 = `ScrollbackPane::render_with_scratch` + `PromptWidget` 渲染 + 手写状态行。
   - 键分发:`PromptWidget::handle_key` → `Edited`/`Ignored`;`Ignored` 时手写 match 处理 Enter(submit)/Esc/Ctrl+C(cancel)/Ctrl+Q(quit)。
   - 保留 `setup/restore_terminal` + fiber 卸载兜底 + `with_intro`。
3. 验证:`cargo run -p rutis-agent --example tui_scripted`(离线)+ `--example tui`(真实,需 key)。

### P2:增强(键绑定 + 主题 + 终端探测)

1. `ActionRegistry` 定义 send/cancel/quit 动作,替换手写 key match。
2. `ShortcutsBar` 渲染快捷键提示,替换手写状态行。
3. `rutui-theme` 终端能力探测(truecolor/sixel/kitty/OSC8/tmux/keyboard 协议)+ 主题选择,增强 setup。
4. (可选)折叠/raw 模式切换、文本选择复制、搜索——rutui 已内置,接键即可。

## 八、验证

| 层 | 方式 |
|---|---|
| 编译 | `cargo check -p rutis-agent`(依赖对齐 + git 依赖拉取) |
| 离线 TUI | `cargo run -p rutis-agent --example tui_scripted`(脚本后端,无需 key) |
| 真实 TUI | `cargo run -p rutis-agent --example tui`(需 `DEEPSEEK_API_KEY`) |
| CLI | `rutis-cli --scripted` / `rutis-cli`(真实) |
| 现有测试 | `cargo test -p rutis-agent`——tui.rs 的 4 个 `wrap_line` 单测随重写移除(rutui 内部处理折行),其余测试不受影响 |

## 九、风险与缓解

| 风险 | 缓解 |
|---|---|
| rutui API 不稳定(1 commit,自述 breaking changes) | 锁 `rev`;重写时只依赖稳定核心 API(`ScrollbackState`/`ScrollbackPane`/`PromptWidget`/`RenderBlock`),避开小众块 |
| 版本降级(0.30→0.29)波及其他 | 只 rutis-agent 用 ratatui;rutis-cli 经 rutis-agent 间接消费,降级一致;内核 crate 无 TUI 依赖 |
| git 依赖(nucleo)增加构建复杂度 | 编译已验证通过;锁 rev;长期转 crates.io |
| rutui 抽取自 Grok agent,隐含领域假设 | 用到的变体(UserPrompt/AgentMessage/OtherToolCall/SessionEvent/System)都是通用语义;Grok 专属块(ContextInfo/CreditLimit/Btw 等)忽略不用 |
| `OtherToolCallBlock` 的 running→finished 流程 | rutui state 层有 `set_entry_running`/`finish_running`/`replace_tool_block`,语义匹配;P1 先验证 |

## 十、实施记录(2026-08-23,P1 已完成)

### 依赖方式:path 而非 git

实施时改为 **path 依赖**(rutui clone 到 rutis 同级目录 `../rutui`),而非设计的 git 依赖。理由:

- rutui 刚抽取(1 commit,API 不稳定),现在发 crates.io 为时过早。
- P1 重写正是最高频调 rutui 的阶段,path 依赖改 rutui 即时生效;git 依赖每次要 push+改 rev。
- 等 rutis-agent 重写完、rutui API 在实战中稳定后,再按 `path → git → crates.io` 顺序切换。

实际 Cargo.toml(`crates/rutis-agent/Cargo.toml`):

```toml
ratatui = "0.29"     # 从 0.30.2 降级(rutui 锁 0.29)
crossterm = { version = "0.28", features = ["event-stream"] }  # 从 0.29.0 降级
rutui-core   = { path = "../../../rutui/rutui-core" }
rutui-prompt = { path = "../../../rutui/rutui-prompt" }
rutui-theme  = { path = "../../../rutui/rutui-theme" }
```

### 关键发现一:ScrollbackState 非 Send → 专用 OS 线程

`ScrollbackState` 内部的 `MarkdownContent` → `StreamingMarkdownRenderer` 含 onig_sys 的 `*mut`(非 Send)。`App` 持有 `ScrollbackState` 故非 Send,而 rutis 的 `BoxFuture` 要求 `Send`——不能跨 `tokio::select!` 的 await 持有。

**解法**:TUI 天然单线程,把 `App` + `Terminal` 放进专用 OS 线程(`std::thread::spawn`)跑同步渲染循环,async `apply` 只持有 Send 的通道句柄做 IO 多路复用:

```
async apply (Send)                     OS 线程 "rutis-tui" (非 Send OK)
┌─────────────────────┐                ┌──────────────────────────────┐
│ EventStream → key   │──ThreadMsg──▶  │ loop { draw; recv_timeout }  │
│ agent 事件 → UiCmd  │──ThreadMsg──▶  │   App(ScrollbackState)       │
│ ctx.cancelled()     │──Cancel────▶   │   PromptWidget               │
│                     │◀─outcome─────  │   Terminal                   │
│ handle.spawn(       │                │ handle_key → HandleOutcome   │
│   agent.followup)   │                │   Quit/Cancel/Submit         │
└─────────────────────┘                └──────────────────────────────┘
```

- 渲染线程持有 `Arc<dyn Agent>`(`Agent: Send + Sync`),`Submit`/`Cancel` 经 `Handle::spawn` 回 tokio runtime 执行 `agent.followup`/`agent.cancel`。
- 渲染线程用 `std::sync::mpsc::recv_timeout(50ms)` 兼顾渲染帧率与消息响应。
- fiber 卸载(`ctx.cancelled()`)经 `ThreadMsg::Cancel` 通知渲染线程退出。

### 关键发现二:render 前必须 prepare_layout

`ScrollbackPane::render_with_scratch` 要求调用方**先**调 `ScrollbackState::prepare_layout(width, height)`(计算各 entry 高度/布局缓存),否则 panic("layout cache must be valid - was prepare_layout() called?")。rutui 的 `StatefulWidget::render` 默认实现内部不含 prepare_layout(注释明确:"All layout preparation is now done by state.prepare_layout() BEFORE render is called")。

```rust
app.scrollback.prepare_layout(conv.width, conv.height);  // 必须先调
ScrollbackPane::new().render_with_scratch(conv, buf, &app.scrollback, &mut scratch);
```

### 验证结果

| 验证项 | 结果 |
|---|---|
| `cargo check --workspace --examples` | ✅ 全工作区编译通过 |
| `cargo test -p rutis-agent` | ✅ unit_loop 15 项 + integration + doc-test 全通过 |
| `tui_scripted` 离线 PTY 运行 | ✅ get_weather/Oslo/scripted/status/idle/running 均渲染,无 panic,exit 0,终端恢复 |
| `rutis-cli --scripted` 离线 PTY 运行 | ✅ bash/replace_text/status 渲染,无 panic,exit 0,终端恢复 |

### 实际 event → block 映射

| 事件 | App 操作 |
|---|---|
| intro | `push_block(System)` |
| Enter(submit) | `push_block(UserPrompt)` + `agent.followup` |
| `AgentReasoning`(首个) | **先 `finish_running(cur_thinking)`** 已处理;`push_block(Thinking::streaming())` + `set_entry_running(id,true)` + `cur_thinking=id` |
| `AgentReasoning`(后续) | `push_chunk_to_thinking(id, &delta)` |
| `AgentTextDelta`(首个) | **先 `finish_running(cur_thinking)`**;`push_block(AgentMessage::streaming())` + `set_entry_running(id,true)` |
| `AgentTextDelta`(后续) | `push_chunk_to_agent(id, &delta)` |
| `AgentToolCall` | **先 `finish_running(cur_thinking)` + `finish_running(cur_agent)`** 再 `push_block(ToolCall::Other(new(name,args)))` + `set_entry_running`;记 `cur_tool=(id,args)` |
| `AgentToolResult` | `replace_tool_block(id, Other with_output/with_error, started_at)` + `finish_running`(不强制展开) |
| `AgentTurnEnd{ok}` | `finish_running(cur_thinking)` + `finish_running(cur_agent)` + idle |
| `AgentTurnEnd{err}` | `finish_running(cur_thinking)` + `finish_running(cur_agent)` + `push_block(SessionEvent::TurnFailed)` + idle |

### 关键发现三:turn 内 text 与 toolcall 必须交替

**Bug**(真实操作时发现):初版 `cur_agent` 整个 turn 指向同一个 text block,`ToolCall` 到来时没有关闭它,导致工具调用**之后**的文本被追加到**第一个** text block 里——显示在工具之前。

driver 的事件顺序(每个 step):`AgentTextDelta`×N → `AgentStepEvent` → `AgentToolCall` → `AgentToolResult`。正确显示应是 `[text1][tool1][text2][tool2][text3]` 按时间交替。

**修复**:`UiCmd::ToolCall` 处理时先 `finish_running(cur_agent)`(关闭并定格当前 text block,`cur_agent=None`),再 push 工具块;后续 `TextDelta` 因 `cur_agent=None` 自然新开 block。同时把工具调用参数(args)带进 `cur_tool: Option<(EntryId, String)>`,结果回来时保留 summary、把 output/error 分开填,避免把结果误填成 summary。

回归测试 `tui::tests::text_and_tool_calls_interleave_in_order` 钉死该顺序。

### 关键发现四:toolcall 结果被折叠(数据已合并,默认折叠显示)

**现象**(真实操作时发现):工具调用之后,结果没有显示在 toolcall 块上。

**根因**:`replace_tool_block` 会把 entry 的 `display_mode` 重置为 `block.default_display_mode()`,而 `OtherToolCallBlock::default_display_mode()` = `Collapsed`;`Collapsed` 模式只渲染 header(名字+summary)、**不渲染 output**。`OtherToolCallBlock::finished_display_mode()` 用默认(返回 `None`),`finish_running` 因此不会展开。结果数据其实已写进 block(单测 `tool(get_weather,city=Oslo,out=18°)` 证明),但被折叠隐藏。

**ground truth 验证**:曾用 `tui::tests::tool_result_output_visibility`——把 ToolCall+ToolResult 序列经真实 `ScrollbackPane::render_with_scratch` 渲染进 `Buffer`,确认修复前只渲染出 `◆ get_weather  city=Oslo`、无 output(即 Collapsed)。现已改为数据层校验 `tui::tests::tool_result_merged_into_block`。

**结论/处理**:这是 rutui 的设计——工具块默认折叠(`Collapsed`),结果数据已合并进同一 block,只是默认不展开、需手动展开(Ctrl+E / 点击)才显示 output。按用户确认"不需要默认展开",因此**不强制 Expanded**,保留 rutui 默认折叠行为;仅额外保留 `started_at` 计时(手动展开时能显示正确耗时)。若后续希望默认直接看到结果,可在 `ToolResult` 里对 entry 设 `display_mode = DisplayMode.Expanded/Truncated`(或改 rutui 让 `OtherToolCallBlock` 默认 `Truncated`)。

### 关键发现五:reasoning(思考过程)完全不显示

**现象**(真实操作时发现):模型有 reasoning/chain-of-thought 输出,但 TUI 里完全不显示。

**根因(两层)**:
1. **driver 丢弃**:`aimux_core::StreamPart` 有 `ReasoningStart/ReasoningDelta/ReasoningEnd` 变体,但 `driver.rs` 的流式循环只处理 `TextDelta/ToolCall/Error`,其余(含 `ReasoningDelta`)全被 catch-all `Chunk::Part(Ok(_)) => {}` 静默丢弃,且没有 reasoning 事件。
2. **TUI 不监听**:原 `TuiPlugin::apply` 只订阅 `AgentTextDelta/AgentToolCall/AgentToolResult/AgentTurnEnd`,没有 reasoning 事件可监听。

**修复(3 文件联动)**:
- `events.rs`:新增 `AgentReasoning { session, step, delta }` 事件(与 `AgentTextDelta` 同构,`#[allow(dead_code)]` 因监听器只读 `delta`)。
- `driver.rs`:在流式循环新增 `Chunk::Part(Ok(StreamPart::ReasoningDelta { delta, .. }))` 分支,调用新增的 `emit_reasoning(...)` 广播 `AgentReasoning`(reasoning 不进 assistant 文本、不回写 session,仅用于展示)。
- `tui.rs`:新增 `UiCmd::ReasoningDelta(String)` + `ReasoningL` 监听器并在 `apply()` 订阅;`App` 新增 `cur_thinking: Option<EntryId>`;`on_ui_cmd` 里把 reasoning 渲染为 rutui 的 `ThinkingBlock`(`streaming()` + `push_chunk_to_thinking` + `finish_running`),并让 `TextDelta/ToolCall/TurnEnd` 先 `finish_running(cur_thinking)` 以保证 `[reasoning][text][tool]` 的时间顺序。

**效果**:reasoning 显示为独立折叠的 "Thought" 块(默认折叠,用户可 Ctrl+E/点击展开),排在正文/工具调用之前。

回归测试 `tui::tests::reasoning_rendered_before_text` 钉死该顺序与合并。
