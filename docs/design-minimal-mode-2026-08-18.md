# Minimal Mode 补充设计(replace_text + bash)

> 2026-08-18。目标:把 deepseek-harness 的 minimal mode(只有 `replace_text` + `bash` 两个工具)做成我们最小 agent MVP 的补充——一个能真干活的 coding agent。
> 依据:[design-min-agent-2026-08-18.md](design-min-agent-2026-08-18.md)(最小 agent 框架)、dsh [tool-catalog](../deepseek-harness/docs/tool-catalog.md) 与 headless bundle。
> 定位:MVP 增量——在"能对话"之上加"能改文件、能跑命令"。

## 一、minimal mode 是什么

dsh 的 minimal mode = 只挂两个工具的 coding agent:

| 工具 | dsh 对应物 | 干什么 |
|---|---|---|
| **bash** | `dsh-tool-bash` | 执行 bash 命令(`bash -c`),返回 stdout/stderr。每次调用新进程,状态不持久(cwd/变量不跨调用),用 `workdir` 而非 `cd` |
| **replace_text** | `dsh-tool-str-replace-editor` | 文件查看/创建/局部替换编辑。核心命令 `str_replace`:用 `old_str`(文件中唯一匹配的连续行)精确替换为 `new_str` |

这两个工具构成 coding agent 的最小能力:**读/改文件(replace_text)+ 跑命令(bash)**。

## 二、要增加的功能(按依赖序)

### 1. `bash` 工具插件(独立插件,提供 ctx.shell 能力)

```rust
pub struct BashTool;  // ToolDef 的实现
// schema:
//   command: string(必填)
//   description: string(必填;"这命令干什么"的一句话说明,UI 显示用,抄 dsh)
//   workdir: string(可选;状态不跨调用,用 workdir 而非 cd)
//   timeout_ms: number(可选,默认有上限)
// runner: tokio::process::Command 起 bash -c,捕获 stdout/stderr
```

要点(对齐 dsh):
- **每次调用新进程**,无持久 shell 状态——文档里写明"用 workdir,别用 cd"。
- **非零退出不报错**,返回 `[exit code: N]` + stderr——让模型看到真实结果而非崩。
- **长输出截断尾部**(dsh 行为);**有意裁剪**:dsh 会把完整输出存文件再报告路径,最小版只截尾,不引入文件存储。
- **超时**:默认上限,超时杀进程。
- **暂不做**:`run_in_background` + jobs(dsh 有,最小裁掉);sandbox 升级段(sandbox_permissions/justification)也裁掉。

### 2. `replace_text` 工具插件(独立插件,文件编辑)

dsh 的 `str_replace_editor` 有 view/create/str_replace/insert 四命令。**最小保留三个**(view/str_replace/create——create 精确创建新文件,比 bash 重定向可控):

```rust
// schema:
//   command: "view" | "create" | "str_replace"
//   path: string
//   file_text: string(create 必填,新文件内容)
//   old_str: string(str_replace 必填,文件中唯一匹配的连续行)
//   new_str: string(str_replace 必填,替换内容)
//   view_range: [start, end](view 可选,看局部行)
```

要点(对齐 dsh `replaceInFile` 实际逻辑):
- **view**:文件 → `cat -n`(带行号,6 位右对齐);目录 → 列 2 层非隐藏文件。**目录只能 view**,其他命令报 "only the `view` command can be used on directories"。`view_range` 校验起止行,`-1` 表到文件尾。长输出截断带 `<response clipped>` + 提示"用 grep -n 找行号重试"(教模型自救)。
- **str_replace**(精确语义,dsh replaceInFile):
  - `old_str` 找不到 → 报 `old_str ... did not appear verbatim in <path>`;
  - `old_str` 多处匹配 → 报 `Multiple occurrences of old_str ... in lines [行号列表]`,**给出行号**——模型靠这个补上下文,必须保留;
  - `new_str` 省略时默认空串——即 str_replace 可做空删除;
  - 成功返回 "The file <path> has been edited successfully."
- **create**:文件已存在则报 "Cannot overwrite files using command `create`";成功返回 "New file created successfully at: <path>"。
- **暂不做**:`insert`、`undo_edit`、read-before-write 策略门禁(dsh 的 fs-observation-policy,最小裁掉)。

### 3. system prompt + 工具描述(抄 dsh 原文)

dsh 的提示词主要**不在 system prompt,而在工具的 schema description**——描述写在工具自己身上,模型读工具时看到。所以最小 prompt 分两层:

**persona(静态,`{{cwd}}` 插值)**——对齐 dsh headless bundle 的 persona:

```
You are a coding agent powered by the {{model}} model. Your working directory is {{cwd}}.
```

**bash 工具描述(抄 dsh `bashDescription`,裁掉 sandbox/background 段)**:

```
Execute a bash command (`bash -c`) and return its stdout/stderr. Each call runs in a fresh shell: no state (cwd, variables, functions) persists between calls — pass `workdir` instead of using `cd`. Non-zero exits are reported as `[exit code: N]`. Long output is truncated to its tail.
```

外加 `description` 参数(bash 调用必带,UI 显示用,抄 dsh):"Clear, concise description of what this command does in active voice, 5-10 words."

**replace_text 工具描述(抄 dsh `DEFAULT_DESCRIPTION`,裁掉 insert/undo 相关)**:

```
Custom editing tool for viewing, creating and editing files
* State is persistent across command calls and discussions with the user
* If `path` is a file, `view` displays the result of applying `cat -n`. If `path` is a directory, `view` lists non-hidden files and directories up to 2 levels deep
* The `create` command cannot be used if the specified `path` already exists as a file
* If a `command` generates a long output, it will be truncated and marked with `<response clipped>`

Notes for using the `str_replace` command:
* The `old_str` parameter should match EXACTLY one or more consecutive lines from the original file. Be mindful of whitespaces!
* If the `old_str` parameter is not unique in the file, the replacement will not be performed. Make sure to include enough context in `old_str` to make it unique
* The `new_str` parameter should contain the edited lines that should replace the `old_str`
```

**原则**:工具描述直接抄 dsh 原文(调过的、经过生产验证),只裁掉我们明确不做的功能段(sandbox/background/insert/undo);persona 用 dsh headless 的最小形式。不做 dsh 的 system-prompt section 装配注册表(超出最小)。

### 4. 注册进工具集

两个新工具进 `ToolsPlugin` 的 `ToolRegistry`(设计文档 §三.2 的工具注册表插件)。MVP 的工具集 = `bash` + `replace_text`(+ 可选保留 demo 的 `get_weather` 作示例)。**不需要新插件种类**——就是两个新 `ToolDef`,装进现有 `ToolsPlugin`。

## 三、这两个工具属于哪个插件、怎么注册、怎么消费

### dsh 的做法(原型)

dsh 里**每个工具是一个独立 cordis 插件包**,经 `ctx.tools.register(defineTool({...}))` 注册进中央工具服务:

```ts
// packages/shell/tool-bash/src/index.ts
export const name = 'tool-bash'
export const inject = ['tools', 'shell', 'systemPrompt', 'shellEnv']  // 依赖门控
// apply 里:
ctx.tools.register(defineTool({ name: 'bash', description, parameters, execute }))
// → 返回 disposer,fiber 卸载时自动注销

// packages/fs/tool-str-replace-editor/src/index.ts
export const name = 'tool-str-replace-editor'
export const inject = ['tools', 'fs']
```

- **注册**:`ctx.tools.register(def)` 返回 disposer,归本插件 fiber 所有(D28),卸载即注销;schema 汇入 prompt 装配(`ctx.systemPrompt`)。
- **消费**:driver 循环里模型产出 tool_call → `ctx.tools` 按名查找并 `execute` → 结果回喂。
- **关键**:dsh 的 `ctx.tools` 是个**中央工具服务**(插件包 `core/tools` 提供),工具插件往里注册;工具插件本身还 inject 自己的能力 seam(bash 注入 `ctx.shell`、编辑器注入 `ctx.fs`)。

### 我们的最小形态(裁剪)

我们**不做 dsh 的中央工具服务 + 能力 seam 两层**。最小版:

- **两个工具不是独立插件**,是 `ToolDef` 数据,装进**一个 `ToolsPlugin`**(设计文档 §三.2 的工具注册表插件)。`ToolsPlugin::new(vec![bash_tool(), replace_text_tool()])`。
- **注册**:`ToolsPlugin.apply` 把 `ToolRegistry`(含两工具)provide 成 `ctx.tools` 服务。工具随该 fiber 卸载而摘除。
- **消费**:`AgentDriver` 循环从 `ctx.tools` 取 `ToolRegistry`,模型 tool_call → `registry.execute(&call, &token)` → 结果回喂 session。
- **能力 seam 裁掉**:bash 直接 `tokio::process::Command`,不抽象 `ctx.shell`;replace_text 直接 `std::fs`,不抽象 `ctx.fs`。最小只要 runner,不要"可替换后端"抽象层。

### 对照表

| | dsh | 我们(最小) |
|---|---|---|
| 工具载体 | 每工具一个独立插件包 | `ToolDef` 数据,装进一个 `ToolsPlugin` |
| 中央工具服务 | `core/tools` 提供 `ctx.tools`,`register` 返回 disposer | `ToolRegistry` 即 `ctx.tools`,`ToolsPlugin` provide |
| 能力 seam | bash→`ctx.shell`、editor→`ctx.fs`(可替换后端) | 直接 `tokio::process` / `std::fs`,无抽象层 |
| 依赖门控 | 工具插件 inject 自己的 seam | `AgentDriverPlugin` injects=[llm, tools] |
| 卸载 | register 的 disposer 随 fiber 卸载 | ToolRegistry 服务随 ToolsPlugin fiber 摘除 |

**理由**:dsh 的"中央工具服务 + 能力 seam"是为了多部署形态(本地/沙箱/远程换后端)。最小 MVP 单进程本地跑,不需要换后端,所以砍成"一个 ToolsPlugin 装所有工具、runner 直写"。未来要多后端时,再把 bash/editor 抽成各自的 seam 插件。

## 四、不需要新增的(明确边界)

- **不新增插件种类**:bash/replace_text 是 `ToolDef`,进现有 `ToolsPlugin`;不引入 `ctx.shell`/`ctx.fs` 这种 dsh 的 capability seam(最小只要 runner,不要抽象层)。
- **不做中央工具服务**(dsh 的 `core/tools` 包):最小版 `ToolRegistry` 就是工具服务,`ToolsPlugin` 直接 provide。
- **不做 system-prompt 装配注册表**:静态 persona + cwd 插值即可。
- **不做 sandbox/approval/permission**(dsh 的生产级安全):最小 MVP 本地跑,信任用户;后续要再补。
- **不做 jobs/background**、**不做 read-before-write 门禁**、**不做 Code Mode**(dsh 的 worker 执行):超出最小。
- **不做持久化/session 日志**:沿用内存 session(设计文档已定)。

## 五、MVP 完整形态(补充后)

```
rutis 框架(fiber/事件/注册表)
  └─ rutis-agent
       ├─ LLM 服务(aimux,直接 provide)
       ├─ ToolsPlugin:bash + replace_text [+ get_weather 示例]
       ├─ AgentDriverPlugin(injects=[llm, tools])
       └─ TuiPlugin(消费 Agent,流式交互)
```

用户输入"帮我把 config.rs 里的 timeout 改成 30 并跑测试" → driver 循环:llm 调 replace_text 改文件 → 调 bash 跑 `cargo test` → 回结果 → 终答。**这就是一个能干活的最小 coding agent。**

## 六、验收

终点是**在 TUI 里真人跑通"改文件 + 看文件 + 跑脚本"**,不只是测试绿。

### TUI 端到端(真实验收,`cargo run -p rutis-agent --example tui`)

用真实 key,在 TUI 里给 agent 一个真实 coding 任务,肉眼验证:

1. **改文件**:"在当前目录新建 hello.rs 写个 main 打印 hello" → 看 agent 调 `replace_text` create → 本地 `cat hello.rs` 确认文件真被建。
2. **看文件**:"看看 hello.rs 里有什么" → 看 agent 调 `replace_text` view → TUI 显示带行号的内容。
3. **改已有文件**:"把 hello 改成 world" → 看 agent 调 `str_replace` → 本地确认改动精确生效。
4. **跑脚本**:"用 bash 跑 `rustc hello.rs && ./hello`" → 看 agent 调 bash → TUI 显示 `world` 输出。
5. **多轮连续**:上面几步在同一 session 连续进行,验证 history 连续(第二步 agent 知道 hello.rs 是第一步建的)。
6. **工具可见**:TUI 对话区能看到 `⚙ replace_text(...)` / `⚙ bash(...)` 及结果(流式)。
7. **驱逐**:中途另开操作 dispose llm 服务,看 driver 自动驱逐回 Pending。

### 自动化测试(CI)

- **单元**:bash(命令执行/非零退出 `[exit code: N]`/workdir/超时/截尾)、replace_text(view 文件带行号/列目录、create 不覆盖、str_replace 精确匹配/多处报行号/空删除/替换正确)。
- **集成**:两工具进 `ToolsPlugin`,脚本后端驱动一个"建文件+改文件+跑命令"的多步 turn;卸载 llm 驱逐 driver。
- **真实后端**(`#[ignore]`):MockReplayModel 录制一个 coding turn 回放。

**TUI 端到端 7 条全过,才算 minimal mode 完成。**
