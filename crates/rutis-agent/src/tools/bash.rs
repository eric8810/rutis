//! bash 工具(minimal mode,设计 §二.1):`bash -c` 新进程执行,
//! 状态不跨调用(workdir 参数而非 `cd`);非零退出以 `[exit code: N]`
//! 标记回喂而非报错;长输出截尾;超时杀进程。
//!
//! 描述抄 dsh `bashDescription`,裁掉 sandbox / background /
//! 环境变量段(minimal mode 明确不做);输出拼装对齐 dsh render:
//! stdout 主体 + `[stderr]` 段 + 行尾标记(exit 标记在最后)。

use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};

use super::ToolDef;

/// 单流(stdout / stderr 各自)最大字符数,超出截尾。
const MAX_OUTPUT_CHARS: usize = 10_000;
/// 默认超时;`timeout_ms` 参数可覆盖,封顶 [`MAX_TIMEOUT_MS`]。
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

pub(crate) const BASH_DESCRIPTION: &str =
    "Execute a bash command (`bash -c`) and return its stdout/stderr. \
Each call runs in a fresh shell: no state (cwd, variables, functions) persists between calls — \
pass `workdir` instead of using `cd`. Non-zero exits are reported as `[exit code: N]`. \
Long output is truncated to its tail.";

/// bash 工具:`ToolDef` 数据,装进 `ToolsPlugin`(设计 §三,非独立插件)。
pub fn bash_tool() -> ToolDef {
    ToolDef::new(
        "bash",
        BASH_DESCRIPTION,
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The bash command to execute." },
                "description": {
                    "type": "string",
                    "description": "Clear, concise description of what this command does in active voice, 5-10 words (shown in the UI)."
                },
                "workdir": { "type": "string", "description": "Working directory for this command. Defaults to the process working directory; state does not persist between calls, so pass `workdir` instead of using `cd`." },
                "timeout_ms": { "type": "number", "description": "Timeout in milliseconds. The executor applies its configured default and cap, and kills the command on expiry." }
            },
            "required": ["command", "description"]
        }),
        run_bash,
    )
}

async fn run_bash(args: Value) -> Result<Value, String> {
    let command = args["command"]
        .as_str()
        .ok_or_else(|| "missing required parameter `command`".to_string())?
        .to_string();
    let timeout_ms = args["timeout_ms"]
        .as_u64()
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .min(MAX_TIMEOUT_MS);

    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c")
        .arg(&command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = args["workdir"].as_str() {
        cmd.current_dir(dir);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn bash: {e}"))?;

    let output =
        tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await;
    match output {
        // 超时:wait_with_output 被 drop,kill_on_drop 杀进程
        Err(_) => Ok(Value::String(render(
            "",
            "",
            &[format!("[timed out after {timeout_ms}ms]")],
        ))),
        Ok(Err(e)) => Err(format!("failed to run command: {e}")),
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            Ok(Value::String(render(
                &stdout,
                &stderr,
                &status_markers(&out.status),
            )))
        }
    }
}

/// 非零退出 / 信号标记(exit 标记保持最后,对齐 dsh parse 契约)。
fn status_markers(status: &std::process::ExitStatus) -> Vec<String> {
    if let Some(code) = status.code() {
        if code != 0 {
            return vec![format!("[exit code: {code}]")];
        }
        return Vec::new();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let signal = status
            .signal()
            .map_or_else(|| "?".to_string(), |s| s.to_string());
        vec![format!("[killed by signal: {signal}]")]
    }
    #[cfg(not(unix))]
    vec!["[killed]".to_string()]
}

/// dsh render 同构:stdout 主体 + `[stderr]` 段 + 行尾标记;空输出 `(no output)`。
fn render(stdout: &str, stderr: &str, markers: &[String]) -> String {
    let mut body = truncate_tail(stdout);
    let err = truncate_tail(stderr);
    if !err.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&err);
    }
    if body.is_empty() {
        body = "(no output)".to_string();
    }
    if markers.is_empty() {
        return body;
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&markers.join("\n"));
    body
}

/// 截尾保留末尾 [`MAX_OUTPUT_CHARS`] 个字符(UTF-8 边界安全)。
fn truncate_tail(s: &str) -> String {
    let count = s.chars().count();
    if count <= MAX_OUTPUT_CHARS {
        return s.to_string();
    }
    let tail: String = s.chars().skip(count - MAX_OUTPUT_CHARS).collect();
    format!("[output truncated]\n{tail}")
}
