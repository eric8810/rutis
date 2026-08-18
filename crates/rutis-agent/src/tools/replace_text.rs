//! replace_text 工具(minimal mode,设计 §二.2):文件查看 / 创建 /
//! 局部替换编辑,对齐 dsh `tool-str-replace-editor` 的 view / create /
//! str_replace 三命令语义(insert / undo / 策略门禁裁掉)。
//!
//! 直接 `std::fs`,不抽象 `ctx.fs` seam(设计 §三:最小只要 runner)。
//! 错误消息抄 dsh 原文——多处匹配给出行号、`<response clipped>` 教
//! 模型用 `grep -n` 自救,这些是模型纠错的关键信号。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::ToolDef;

/// view / create 输出截断上限(保头,对齐 dsh maybeTruncate)。
const MAX_OUTPUT_CHARS: usize = 10_000;

const TRUNCATED_MESSAGE: &str = "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with `grep -n` in order to find the line numbers of what you are looking for.</NOTE>";

pub(crate) const EDITOR_DESCRIPTION: &str = "Custom editing tool for viewing, creating and editing files\n\
* State is persistent across command calls and discussions with the user\n\
* If `path` is a file, `view` displays the result of applying `cat -n`. If `path` is a directory, `view` lists non-hidden files and directories up to 2 levels deep\n\
* The `create` command cannot be used if the specified `path` already exists as a file\n\
* If a `command` generates a long output, it will be truncated and marked with `<response clipped>`\n\
\n\
Notes for using the `str_replace` command:\n\
* The `old_str` parameter should match EXACTLY one or more consecutive lines from the original file. Be mindful of whitespaces!\n\
* If the `old_str` parameter is not unique in the file, the replacement will not be performed. Make sure to include enough context in `old_str` to make it unique\n\
* The `new_str` parameter should contain the edited lines that should replace the `old_str`";

/// replace_text 工具:`ToolDef` 数据,装进 `ToolsPlugin`(设计 §三)。
pub fn replace_text_tool() -> ToolDef {
    ToolDef::new(
        "replace_text",
        EDITOR_DESCRIPTION,
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "enum": ["view", "create", "str_replace"] },
                "path": { "type": "string", "description": "Absolute path to the file or directory." },
                "file_text": { "type": "string", "description": "Required for `create`: the content of the new file." },
                "old_str": { "type": "string", "description": "Required for `str_replace`: the exact text to replace (must be unique in the file)." },
                "new_str": { "type": "string", "description": "Required for `str_replace`: the replacement text. Omit to delete `old_str`." },
                "view_range": { "type": "array", "items": { "type": "number" }, "description": "Optional for `view`: [start, end] line numbers (1-based); -1 as end means to the last line." }
            },
            "required": ["command", "path"]
        }),
        run_editor,
    )
}

async fn run_editor(args: Value) -> Result<Value, String> {
    let command = args["command"]
        .as_str()
        .ok_or_else(|| "Parameter `command` is required".to_string())?;
    let path = args["path"]
        .as_str()
        .ok_or_else(|| "Parameter `path` is required".to_string())?;
    let out = match command {
        "view" => view_path(path, args["view_range"].as_array())?,
        "create" => create_file(
            path,
            required_for_command(args["file_text"].as_str(), "file_text", "create", true)?,
        )?,
        "str_replace" => replace_in_file(
            path,
            required_for_command(args["old_str"].as_str(), "old_str", "str_replace", false)?,
            // new_str 省略 = 空串:空删除(设计 §二.2)
            args["new_str"].as_str().unwrap_or(""),
        )?,
        other => {
            return Err(format!(
                "unknown command `{other}`; expected `view`, `create` or `str_replace`"
            ))
        }
    };
    Ok(Value::String(out))
}

// ── 参数校验(消息抄 dsh requiredForCommand)─────────────────────────

fn required_for_command(
    value: Option<&str>,
    parameter: &str,
    command: &str,
    allow_empty: bool,
) -> Result<String, String> {
    let Some(value) = value else {
        return Err(format!(
            "Parameter `{parameter}` is required for command: {command}"
        ));
    };
    if !allow_empty && value.is_empty() {
        return Err(format!(
            "Parameter `{parameter}` is empty for command: {command}"
        ));
    }
    Ok(value.to_string())
}

// ── 路径解析与存在性(消息抄 dsh resolveTarget / statExisting)─────────

fn resolve_target(path: &str) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("path must be a non-empty string".to_string());
    }
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!(
            "The path {path} is not an absolute path, it should start with `/`. Maybe you meant /{path}?"
        ));
    }
    Ok(p.to_path_buf())
}

fn stat_existing(target: &Path, command: &str) -> Result<fs::Metadata, String> {
    let display = target.display();
    match fs::metadata(target) {
        Err(_) => Err(format!(
            "The path {display} does not exist. Please provide a valid path."
        )),
        Ok(meta) if meta.is_dir() && command != "view" => Err(format!(
            "The path {display} is a directory and only the `view` command can be used on directories"
        )),
        Ok(meta) => Ok(meta),
    }
}

// ── view(文件 cat -n / 目录 2 层列举,对齐 dsh viewPath)──────────────

fn view_path(path: &str, view_range: Option<&Vec<Value>>) -> Result<String, String> {
    let target = resolve_target(path)?;
    let meta = stat_existing(&target, "view")?;
    if meta.is_dir() {
        if let Some(range) = view_range {
            if !range.is_empty() {
                return Err(
                    "The `view_range` parameter is not allowed when `path` points to a directory."
                        .to_string(),
                );
            }
        }
        return list_directory(&target);
    }
    if !meta.is_file() {
        return Err(format!(
            "cannot view \"{}\": not a regular file or directory",
            target.display()
        ));
    }
    let content = fs::read_to_string(&target).map_err(|e| format!("read {path} failed: {e}"))?;
    format_file_view(&target.display().to_string(), &content, view_range)
}

/// `cat -n` 视图(行号 6 位右对齐 + 两空格,view_range 支持 `-1` 到尾)。
fn format_file_view(
    path: &str,
    content: &str,
    view_range: Option<&Vec<Value>>,
) -> Result<String, String> {
    let all_lines: Vec<&str> = content.split('\n').collect();
    let mut lines: &[&str] = &all_lines;
    let mut initial_line: usize = 1;
    let mut prompt = format!(
        "Here's the content of {path} with line numbers (which has a total of {} lines)",
        all_lines.len()
    );
    if let Some(range) = view_range {
        let parsed = parse_view_range(range, all_lines.len())?;
        initial_line = parsed.0;
        lines = if parsed.1 == -1 {
            &all_lines[parsed.0 - 1..]
        } else {
            &all_lines[parsed.0 - 1..parsed.1 as usize]
        };
        prompt.push_str(&format!(" with view_range=[{}, {}]", parsed.0, parsed.1));
    }
    let numbered = lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}  {}", initial_line + i, line))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(maybe_truncate(&format!("{prompt}:\n{numbered}\n")))
}

/// (start, end),end 可为 -1;校验消息抄 dsh formatFileView。
fn parse_view_range(range: &[Value], total: usize) -> Result<(usize, i64), String> {
    if range.len() != 2 || !range.iter().all(|v| v.is_i64() || v.is_u64()) {
        return Err("Invalid `view_range`. It should be a list of two integers.".to_string());
    }
    let as_int = |v: &Value| v.as_i64().unwrap_or(i64::MAX);
    let (start, end) = (as_int(&range[0]), as_int(&range[1]));
    if !(1..=total as i64).contains(&start) {
        return Err(format!(
            "Invalid `view_range`: [{start}, {end}]. Its first element `{start}` should be within the range of lines of the file: [1, {total}]"
        ));
    }
    if end != -1 && end > total as i64 {
        return Err(format!(
            "Invalid `view_range`: [{start}, {end}]. Its second element `{end}` should be smaller than the number of lines in the file: `{total}`"
        ));
    }
    if end != -1 && end < start {
        return Err(format!(
            "Invalid `view_range`: [{start}, {end}]. Its second element `{end}` should be larger or equal than its first `{start}`"
        ));
    }
    Ok((start as usize, end))
}

/// 目录列举:2 层非隐藏(dsh 还排 node_modules / __pycache__),行排序。
fn list_directory(root: &Path) -> Result<String, String> {
    fn visit(dir: &Path, depth: usize, rows: &mut Vec<String>) -> std::io::Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && name != "node_modules" && name != "__pycache__"
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let row = format!(
                "{}\t{}",
                if is_dir { "d" } else { "f" },
                entry.path().display()
            );
            rows.push(row);
            if is_dir && depth < 2 {
                visit(&entry.path(), depth + 1, rows)?;
            }
        }
        Ok(())
    }

    let mut rows = vec![format!("d\t{}", root.display())];
    visit(root, 1, &mut rows)
        .map_err(|e| format!("list {dir} failed: {e}", dir = root.display()))?;
    rows.sort_by(|l, r| {
        let lp = l.split_once('\t').map(|x| x.1).unwrap_or(l);
        let rp = r.split_once('\t').map(|x| x.1).unwrap_or(r);
        lp.cmp(rp)
    });
    Ok(maybe_truncate(&format!(
        "Here're the files and directories up to 2 levels deep in {}, excluding hidden items, node_modules, and Python cache directories:\n{}\n",
        root.display(),
        rows.join("\n")
    )))
}

// ── create(已存在拒绝,消息抄 dsh createFile)────────────────────────

fn create_file(path: &str, file_text: String) -> Result<String, String> {
    let target = resolve_target(path)?;
    if target.exists() {
        return Err(format!(
            "File already exists at: {}. Cannot overwrite files using command `create`.",
            target.display()
        ));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create parent of {path} failed: {e}"))?;
    }
    fs::write(&target, &file_text).map_err(|e| format!("write {path} failed: {e}"))?;
    Ok(format!(
        "New file created successfully at: {}",
        target.display()
    ))
}

// ── str_replace(唯一匹配替换,消息抄 dsh replaceInFile)───────────────

fn replace_in_file(path: &str, old_str: String, new_str: &str) -> Result<String, String> {
    let target = resolve_target(path)?;
    stat_existing(&target, "str_replace")?;
    let before = fs::read_to_string(&target).map_err(|e| format!("read {path} failed: {e}"))?;
    let offsets = match_offsets(&before, &old_str);
    let Some(&offset) = offsets.first() else {
        return Err(format!(
            "No replacement was performed, old_str `{old_str}` did not appear verbatim in {}.",
            target.display()
        ));
    };
    if offsets.len() > 1 {
        let lines = line_numbers_at(&before, &offsets);
        return Err(format!(
            "No replacement was performed. Multiple occurrences of old_str `{old_str}` in lines [{}]. Please ensure it is unique",
            lines.join(", ")
        ));
    }
    let after = format!(
        "{}{}{}",
        &before[..offset],
        new_str,
        &before[offset + old_str.len()..]
    );
    fs::write(&target, after).map_err(|e| format!("write {path} failed: {e}"))?;
    Ok(format!(
        "The file {} has been edited successfully.",
        target.display()
    ))
}

fn match_offsets(content: &str, search: &str) -> Vec<usize> {
    if search.is_empty() {
        return Vec::new();
    }
    content.match_indices(search).map(|(i, _)| i).collect()
}

fn line_numbers_at(content: &str, offsets: &[usize]) -> Vec<String> {
    let (mut line, mut cursor) = (1usize, 0usize);
    offsets
        .iter()
        .map(|&offset| {
            while cursor < offset {
                if content.as_bytes()[cursor] == b'\n' {
                    line += 1;
                }
                cursor += 1;
            }
            line.to_string()
        })
        .collect()
}

/// 保头截断 + dsh `<response clipped>` 自救提示(教模型 grep -n 重试)。
fn maybe_truncate(content: &str) -> String {
    if content.chars().count() <= MAX_OUTPUT_CHARS {
        return content.to_string();
    }
    let head: String = content.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}{TRUNCATED_MESSAGE}")
}
