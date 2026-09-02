//! minimal mode 装配(设计 §五):`bash` + `replace_text` 两工具与
//! persona。MVP 工具集 = 这两个(`get_weather` 由调用方自行附加作示例);
//! persona 静态插值,不做 system-prompt section 装配注册表(设计 §四)。

use crate::tools::ToolDef;
use crate::{bash_tool, replace_text_tool};

/// minimal mode 工具集:bash + replace_text(设计 §二)。
pub fn minimal_tools() -> Vec<ToolDef> {
    vec![bash_tool(), replace_text_tool()]
}

/// dsh headless persona 的最小形式,`{{model}}` / `{{cwd}}` 由调用方填。
pub fn minimal_persona(model: &str, cwd: &str) -> String {
    format!("You are a coding agent powered by the {model} model. Your working directory is {cwd}.")
}
