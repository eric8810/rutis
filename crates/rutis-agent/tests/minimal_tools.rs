//! minimal mode 工具测试(设计 §六 验收):
//!
//! - 单元:bash(执行 / 非零退出 / workdir / 超时 / 截尾)、
//!   replace_text(view 文件与目录 / create / str_replace 精确语义)
//! - 集成:两工具进 `ToolsPlugin`,ScriptedLlm 驱动一个
//!   "改文件 + 跑命令"的 turn,结果回喂、文件真被改、命令真被跑。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimux_core::language_model::LanguageModel;
use rutis::{Ctx, Listener};
use rutis_agent::{
    agent_key, bash_tool, llm_key, minimal_persona, minimal_tools, replace_text_tool, tool_call,
    Agent, AgentDriverPlugin, AgentStatus, AgentToolResult, LlmResponse, ScriptedLlm, ToolDef,
    ToolsPlugin,
};
use serde_json::{json, Value};

async fn soon<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(10), f)
        .await
        .expect("timed out")
}

/// 依赖无关的临时目录 guard(避免为测试引 tempfile 依赖)。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rutis-minimal-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// 规范化绝对路径(macOS /var → /private/var,`pwd` 对齐用)。
    fn real(&self) -> String {
        fs::canonicalize(&self.0)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn join(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 直接执行 ToolDef runner,返回 (ok, output 文本)。
async fn run(def: &ToolDef, args: Value) -> (bool, String) {
    match (def.run.clone())(args).await {
        Ok(Value::String(s)) => (true, s),
        Ok(other) => (true, other.to_string()),
        Err(e) => (false, e),
    }
}

// ── bash ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn bash_runs_command_returns_stdout() {
    let (ok, out) = run(
        &bash_tool(),
        json!({ "command": "echo hello", "description": "Print hello" }),
    )
    .await;
    assert!(ok);
    assert_eq!(out, "hello\n");
}

#[tokio::test]
async fn bash_nonzero_exit_is_result_not_error() {
    let (ok, out) = run(
        &bash_tool(),
        json!({ "command": "echo out; echo err >&2; exit 3", "description": "Fail with output" }),
    )
    .await;
    // 非零退出不报错:stdout + [stderr] 段 + 行尾 exit 标记(无尾换行,对齐 dsh)
    assert!(ok);
    assert_eq!(out, "out\n[stderr]\nerr\n[exit code: 3]");
}

#[tokio::test]
async fn bash_workdir_overrides_cwd() {
    let tmp = TempDir::new("workdir");
    let (ok, out) = run(
        &bash_tool(),
        json!({ "command": "pwd", "description": "Print working directory", "workdir": tmp.real() }),
    )
    .await;
    assert!(ok);
    assert_eq!(out.trim(), tmp.real());
}

#[tokio::test]
async fn bash_timeout_kills_and_marks() {
    let started = std::time::Instant::now();
    let (ok, out) = run(
        &bash_tool(),
        json!({ "command": "sleep 30", "description": "Sleep long", "timeout_ms": 200 }),
    )
    .await;
    assert!(ok);
    assert_eq!(out, "(no output)\n[timed out after 200ms]");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout must kill the process"
    );
}

#[tokio::test]
async fn bash_long_output_truncated_to_tail() {
    let (ok, out) = run(
        &bash_tool(),
        json!({ "command": "seq 1 3000", "description": "Generate long output" }),
    )
    .await;
    assert!(ok);
    assert!(
        out.starts_with("[output truncated]\n"),
        "head: {}",
        &out[..40]
    );
    assert!(out.ends_with("3000\n"), "tail must be kept");
    assert!(out.chars().count() < 12_000, "must be truncated");
}

#[tokio::test]
async fn bash_silent_success_reports_no_output() {
    let (ok, out) = run(
        &bash_tool(),
        json!({ "command": "true", "description": "Do nothing" }),
    )
    .await;
    assert!(ok);
    assert_eq!(out, "(no output)");
}

#[tokio::test]
async fn bash_missing_command_rejected() {
    let (ok, err) = run(&bash_tool(), json!({ "description": "No command" })).await;
    assert!(!ok);
    assert!(err.contains("command"), "{err}");
}

// ── replace_text:view ────────────────────────────────────────────────

#[tokio::test]
async fn view_file_with_line_numbers() {
    let tmp = TempDir::new("view");
    let path = tmp.join("a.txt");
    fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

    let (ok, out) = run(
        &replace_text_tool(),
        json!({ "command": "view", "path": path }),
    )
    .await;
    assert!(ok);
    assert!(
        out.contains("with line numbers (which has a total of 4 lines)"),
        "{out}"
    );
    assert!(out.contains("     1  alpha"), "6-wide line numbers: {out}");
    assert!(out.contains("     2  beta"));
}

#[tokio::test]
async fn view_range_selects_lines() {
    let tmp = TempDir::new("range");
    let path = tmp.join("a.txt");
    fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();

    let (_, out) = run(
        &replace_text_tool(),
        json!({ "command": "view", "path": path, "view_range": [2, 3] }),
    )
    .await;
    assert!(out.contains("with view_range=[2, 3]"), "{out}");
    assert!(out.contains("     2  two"));
    assert!(out.contains("     3  three"));
    assert!(!out.contains("one"));
    assert!(!out.contains("four"));

    // -1 = 到文件尾
    let (_, out) = run(
        &replace_text_tool(),
        json!({ "command": "view", "path": path, "view_range": [3, -1] }),
    )
    .await;
    assert!(out.contains("     3  three"));
    assert!(out.contains("     4  four"));
    assert!(!out.contains("one\n"));
}

#[tokio::test]
async fn view_directory_lists_two_levels_non_hidden() {
    let tmp = TempDir::new("dir");
    fs::write(tmp.join("top.txt"), "x").unwrap();
    fs::create_dir(tmp.join("sub")).unwrap();
    fs::write(tmp.join("sub/inner.txt"), "x").unwrap();
    fs::create_dir(tmp.join("sub/deep")).unwrap();
    fs::write(tmp.join("sub/deep/leaf.txt"), "x").unwrap(); // 第 3 层:不列
    fs::write(tmp.join(".hidden"), "x").unwrap();
    fs::create_dir(tmp.join(".git")).unwrap();
    fs::write(tmp.join(".git/config"), "x").unwrap();

    let (_, out) = run(
        &replace_text_tool(),
        json!({ "command": "view", "path": tmp.real() }),
    )
    .await;
    // 行基于传入路径(规范化后的)拼接
    let row = |name: &str| format!("f\t{}/{}", tmp.real(), name);
    let dir_row = |name: &str| format!("d\t{}/{}", tmp.real(), name);
    assert!(out.contains("up to 2 levels deep"), "{out}");
    assert!(out.contains(&format!("d\t{}", tmp.real())));
    assert!(out.contains(&row("top.txt")));
    assert!(out.contains(&dir_row("sub")));
    assert!(out.contains(&row("sub/inner.txt")));
    assert!(out.contains(&dir_row("sub/deep")));
    // 第 3 层文件与隐藏项不出现
    assert!(!out.contains("leaf.txt"));
    assert!(!out.contains(".hidden"));
    assert!(!out.contains(".git"));
}

#[tokio::test]
async fn view_range_on_directory_rejected() {
    let tmp = TempDir::new("dirrange");
    let (_, err) = run(
        &replace_text_tool(),
        json!({ "command": "view", "path": tmp.real(), "view_range": [1, 2] }),
    )
    .await;
    assert!(err.contains("not allowed"), "{err}");
}

#[tokio::test]
async fn view_long_file_clipped_with_hint() {
    let tmp = TempDir::new("clip");
    let path = tmp.join("big.txt");
    fs::write(&path, "line\n".repeat(4000)).unwrap();

    let (_, out) = run(
        &replace_text_tool(),
        json!({ "command": "view", "path": path }),
    )
    .await;
    assert!(out.contains("<response clipped>"), "{out}");
    assert!(out.contains("grep -n"), "self-rescue hint: {out}");
}

#[tokio::test]
async fn relative_path_rejected() {
    let (_, err) = run(
        &replace_text_tool(),
        json!({ "command": "view", "path": "relative/x.txt" }),
    )
    .await;
    assert!(err.contains("not an absolute path"), "{err}");
}

#[tokio::test]
async fn missing_path_rejected() {
    let (_, err) = run(
        &replace_text_tool(),
        json!({ "command": "view", "path": "/nonexistent/rutis-minimal-x.txt" }),
    )
    .await;
    assert!(err.contains("does not exist"), "{err}");
}

// ── replace_text:create ──────────────────────────────────────────────

#[tokio::test]
async fn create_writes_new_file() {
    let tmp = TempDir::new("create");
    let path = tmp.join("new.txt");
    let (ok, out) = run(
        &replace_text_tool(),
        json!({ "command": "create", "path": path, "file_text": "hello\n" }),
    )
    .await;
    assert!(ok);
    assert_eq!(out, format!("New file created successfully at: {path}"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "hello\n");
}

#[tokio::test]
async fn create_existing_file_rejected() {
    let tmp = TempDir::new("create2");
    let path = tmp.join("exists.txt");
    fs::write(&path, "old").unwrap();
    let (ok, err) = run(
        &replace_text_tool(),
        json!({ "command": "create", "path": path, "file_text": "new" }),
    )
    .await;
    assert!(!ok);
    assert!(err.contains("Cannot overwrite"), "{err}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
}

#[tokio::test]
async fn create_missing_file_text_rejected() {
    let tmp = TempDir::new("create3");
    let (_, err) = run(
        &replace_text_tool(),
        json!({ "command": "create", "path": tmp.join("x.txt") }),
    )
    .await;
    assert!(err.contains("`file_text` is required"), "{err}");
}

// ── replace_text:str_replace ─────────────────────────────────────────

#[tokio::test]
async fn str_replace_edits_unique_match() {
    let tmp = TempDir::new("replace");
    let path = tmp.join("config.txt");
    fs::write(&path, "timeout = 10\nretries = 3\n").unwrap();

    let (ok, out) = run(
        &replace_text_tool(),
        json!({ "command": "str_replace", "path": path, "old_str": "timeout = 10", "new_str": "timeout = 30" }),
    )
    .await;
    assert!(ok);
    assert_eq!(
        out,
        format!("The file {path} has been edited successfully.")
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "timeout = 30\nretries = 3\n"
    );
}

#[tokio::test]
async fn str_replace_omitted_new_str_deletes() {
    let tmp = TempDir::new("delete");
    let path = tmp.join("a.txt");
    fs::write(&path, "keep\nremove me\nkeep\n").unwrap();

    let (ok, _) = run(
        &replace_text_tool(),
        json!({ "command": "str_replace", "path": path, "old_str": "remove me\n" }),
    )
    .await;
    assert!(ok);
    assert_eq!(fs::read_to_string(&path).unwrap(), "keep\nkeep\n");
}

#[tokio::test]
async fn str_replace_not_found_reports_verbatim_miss() {
    let tmp = TempDir::new("notfound");
    let path = tmp.join("a.txt");
    fs::write(&path, "content\n").unwrap();

    let (ok, err) = run(
        &replace_text_tool(),
        json!({ "command": "str_replace", "path": path, "old_str": "absent", "new_str": "x" }),
    )
    .await;
    assert!(!ok);
    assert!(err.contains("did not appear verbatim"), "{err}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "content\n");
}

#[tokio::test]
async fn str_replace_ambiguous_reports_line_numbers() {
    let tmp = TempDir::new("ambiguous");
    let path = tmp.join("a.txt");
    fs::write(&path, "dup\ndup\ndup\n").unwrap();

    let (ok, err) = run(
        &replace_text_tool(),
        json!({ "command": "str_replace", "path": path, "old_str": "dup", "new_str": "x" }),
    )
    .await;
    assert!(!ok);
    // 行号列表是模型补上下文的唯一线索,必须保留
    assert!(err.contains("Multiple occurrences"), "{err}");
    assert!(err.contains("lines [1, 2, 3]"), "{err}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "dup\ndup\ndup\n");
}

#[tokio::test]
async fn str_replace_on_directory_rejected() {
    let tmp = TempDir::new("diredit");
    let (ok, err) = run(
        &replace_text_tool(),
        json!({ "command": "str_replace", "path": tmp.real(), "old_str": "x", "new_str": "y" }),
    )
    .await;
    assert!(!ok);
    assert!(err.contains("only the `view` command"), "{err}");
}

#[tokio::test]
async fn str_replace_empty_old_str_rejected() {
    let tmp = TempDir::new("empty");
    let path = tmp.join("a.txt");
    fs::write(&path, "x\n").unwrap();
    let (_, err) = run(
        &replace_text_tool(),
        json!({ "command": "str_replace", "path": path, "old_str": "", "new_str": "y" }),
    )
    .await;
    assert!(err.contains("`old_str` is empty"), "{err}");
}

// ── persona:self-evolving 分节提示词的必备条款锚点(设计对齐)─────

#[test]
fn persona_carries_essential_self_evolution_clauses() {
    let persona = minimal_persona("scripted-model", "/tmp/work");
    // 使命方向(不写死任务清单)
    assert!(persona.contains("工程上最好的 agent"), "{persona}");
    assert!(persona.contains("环境适应性最佳的 agent"), "{persona}");
    assert!(persona.contains("迭代能力最强的 agent"), "{persona}");
    // 硬纪律:执行可能中断 → 任务文档化是断点续接凭据
    assert!(persona.contains("执行可能中断"), "{persona}");
    assert!(persona.contains("工作文档"), "{persona}");
    // 交接文档:会话启动先找并读 handoff
    assert!(persona.contains("docs/work/handoff.md"), "{persona}");
    assert!(persona.contains("交接文档"), "{persona}");
    // 工作纪律:及时 commit,防止工作堆积在未提交状态
    assert!(persona.contains("及时 commit"), "{persona}");
    assert!(persona.contains("未提交"), "{persona}");
    // 工作纪律:提交后推送远程,远程是跨实例交接通道
    assert!(persona.contains("推送到远程"), "{persona}");
    assert!(persona.contains("git push"), "{persona}");
    // 自我演进:plugin 方式 + 热更新 + 仓库信息皆参考
    assert!(persona.contains("plugin"), "{persona}");
    assert!(persona.contains("热更新"), "{persona}");
    // 唯一例外:minimal persona 变更需用户同意
    assert!(persona.contains("必须经用户同意"), "{persona}");
    // 生存周期认知(v2):使命是恒定命题,持续运行
    assert!(persona.contains("恒定命题"), "{persona}");
    assert!(persona.contains("失败是观察素材"), "{persona}");
    assert!(persona.contains("持续运行"), "{persona}");
    // 自我激活检查(每轮必做,非可选):git status → 提交 → 审视 → 待命
    assert!(persona.contains("自我激活检查"), "{persona}");
    assert!(persona.contains("每轮必做"), "{persona}");
    assert!(persona.contains("git status"), "{persona}");
    // 插值生效
    assert!(persona.contains("scripted-model"), "{persona}");
    assert!(persona.contains("/tmp/work"), "{persona}");
}

// ── 集成:driver 驱动"改文件 + 跑命令"的 turn(设计 §六)────────────

#[tokio::test]
async fn minimal_turn_edits_file_and_runs_command() {
    let tmp = TempDir::new("turn");
    let config = tmp.join("config.txt");
    fs::write(&config, "timeout = 10\n").unwrap();

    let cwd = tmp.real();
    let persona = minimal_persona("scripted-model", &cwd);
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::tool_calls(vec![
            tool_call(
                "c1",
                "replace_text",
                json!({ "command": "str_replace", "path": config, "old_str": "timeout = 10", "new_str": "timeout = 30" }),
            ),
            tool_call(
                "c2",
                "bash",
                json!({ "command": format!("cat {config}"), "description": "Show edited config" }),
            ),
        ]),
        LlmResponse::content("done: timeout is now 30"),
    ]));

    let root = Ctx::root().unwrap();
    let service: Arc<dyn LanguageModel> = llm.clone();
    root.provide_as(llm_key(), service).unwrap();
    let tools_view = root.plugin(ToolsPlugin::new(minimal_tools()));
    let driver_view = root.plugin(AgentDriverPlugin::new(8).with_system_prompt(persona));
    soon(async {
        (&tools_view).await.expect("tools loads");
        (&driver_view).await.expect("driver loads");
    })
    .await;

    let agent = root.get_as::<dyn Agent>(agent_key()).unwrap();

    // 工具结果经 agent/tool-result 事件观察(driver emit,总线广播)
    let results: Arc<Mutex<Vec<(String, bool, String)>>> = Arc::new(Mutex::new(Vec::new()));
    struct ToolResultL(Arc<Mutex<Vec<(String, bool, String)>>>);
    impl Listener<AgentToolResult> for ToolResultL {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            e: &'a AgentToolResult,
        ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
            let rs = self.0.clone();
            let (name, ok, output) = (e.name.clone(), e.ok, e.output.clone());
            Box::pin(async move {
                rs.lock().unwrap().push((name, ok, output));
                Ok(None)
            })
        }
    }
    let _d = root
        .events()
        .on(&root, ToolResultL(results.clone()))
        .unwrap();
    let text = soon(agent.followup("bump timeout and show the file"))
        .await
        .unwrap();

    // 文件真被改、命令真被跑;事件派发与 followup 返回并发,轮询等齐 2 条结果
    soon(async {
        while results.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert_eq!(fs::read_to_string(&config).unwrap(), "timeout = 30\n");
    let results = results.lock().unwrap().clone();
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(results[0].0, "replace_text");
    assert!(results[0].1);
    assert!(results[0].2.contains("edited successfully"), "{results:?}");
    assert_eq!(results[1].0, "bash");
    assert!(results[1].1);
    assert!(results[1].2.contains("timeout = 30"), "{results:?}");

    // 终答 + 状态回 idle
    assert_eq!(text, "done: timeout is now 30");
    assert_eq!(agent.status(), AgentStatus::Idle);

    // persona 作为 system 消息前置,模型看到了 cwd(锁内只做拷贝,不跨 await)
    let (first_texts, first_tools) = {
        let calls = llm.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        (calls[0].message_texts(), calls[0].tools.clone())
    };
    assert_eq!(first_texts[0].0, aimux_core::message::Role::System);
    assert!(
        first_texts[0].1.contains("scripted-model"),
        "{first_texts:?}"
    );
    assert!(first_texts[0].1.contains(&cwd), "{first_texts:?}");

    // schema:模型收到的工具集就是 bash + replace_text(HashMap 无序,排序比较)
    let mut names: Vec<String> = first_tools
        .iter()
        .map(|t| match t {
            aimux_core::options::Tool::Function(f) => f.name.clone(),
            aimux_core::options::Tool::Provider(_) => "(provider)".to_string(),
        })
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["bash".to_string(), "replace_text".to_string()]);

    // 回收
    soon(async {
        driver_view.dispose().await.unwrap();
        tools_view.dispose().await.unwrap();
    })
    .await;
}
