//! 热插拔工具 `hotplug_load`:运行中的 agent 加载 cdylib 插件。
//!
//! 自迭代闭环:agent 写新代码 → 编译成 .so → 调用 `hotplug_load` →
//! 新工具立即注册进运行中的 ToolRegistry → 后续 turn 可用。
//! 全程不重启进程、不重编译主程序。

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Arc;

use libloading::Library;
use rutis::Ctx;
use serde_json::{json, Value};

use crate::tools::{ToolDef, ToolRegistry};

type FnCount = unsafe extern "C" fn() -> u32;
type FnSchema = unsafe extern "C" fn(u32) -> *const c_char;
type FnInvoke = unsafe extern "C" fn(u32, *const c_char) -> *const c_char;

/// `hotplug_load`:加载 cdylib 插件,把其 C ABI 工具注册进运行中的
/// ToolRegistry。参数:`path`(动态库路径,如 target/debug/librutis_x.so)。
/// 返回:注册的工具名列表。
pub fn hotplug_load(ctx: Ctx) -> ToolDef {
    ToolDef::new(
        "hotplug_load",
        "Hot-load a cdylib plugin at runtime: load the .so, read its C ABI tools, and register them into the running ToolRegistry. Pass the path to the shared library. New tools are immediately available to the model — no rebuild of the main program, no restart.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the shared library (.so/.dylib/.dll), e.g. target/debug/librutis_myplugin.so"
                }
            },
            "required": ["path"]
        }),
        move |args: Value| {
            let ctx = ctx.clone();
            async move {
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| "error: path is required".to_string())?
                    .to_string();

                let registry = ctx
                    .get_as::<ToolRegistry>(crate::tools_key())
                    .ok_or_else(|| "error: tool registry not loaded".to_string())?;

                let loaded = load_plugin_tools(&path, &registry)?;
                Ok(Value::String(format!(
                    "hot-loaded {} tool(s) from {path}: {}",
                    loaded.len(),
                    loaded.join(", ")
                )))
            }
        },
    )
}

/// 加载 .so,读 C ABI 工具,注册进 registry。返回注册的工具名。
fn load_plugin_tools(path: &str, registry: &ToolRegistry) -> Result<Vec<String>, String> {
    // 安全:加载动态库是 unsafe;库随进程存活(Box::leak)。
    unsafe {
        // 库必须 'static(工具闭包跨 turn 存活)
        let lib: &'static Library =
            Box::leak(Box::new(Library::new(path).map_err(|e| format!("open {path}: {e}"))?));
        let count: libloading::Symbol<FnCount> =
            lib.get(b"rutis_plugin_tool_count").map_err(|e| format!("symbol count: {e}"))?;
        let schema: libloading::Symbol<FnSchema> =
            lib.get(b"rutis_plugin_tool_schema").map_err(|e| format!("symbol schema: {e}"))?;
        let invoke: libloading::Symbol<FnInvoke> =
            lib.get(b"rutis_plugin_tool_invoke").map_err(|e| format!("symbol invoke: {e}"))?;
        let n = count();

        let invoke_fn: FnInvoke = *invoke;
        let invoke = Arc::new(invoke_fn);
        let mut names = Vec::new();
        for i in 0..n {
            let schema_json = CStr::from_ptr(schema(i)).to_str().unwrap_or("{}").to_string();
            let parsed: Value = serde_json::from_str(&schema_json)
                .map_err(|e| format!("bad schema[{i}]: {e}"))?;
            let name = parsed["name"]
                .as_str()
                .ok_or_else(|| format!("tool[{i}] missing name"))?
                .to_string();
            let description = parsed["description"].as_str().unwrap_or("").to_string();
            let params = parsed["parameters"].clone();
            let invoke = invoke.clone();
            let idx = i;
            let def = ToolDef::new(
                &name,
                &description,
                params,
                move |args: Value| {
                    let invoke = invoke.clone();
                    async move {
                        let args_str = args.to_string();
                        let args_c = CString::new(args_str).unwrap();
                        let out_ptr = invoke(idx, args_c.as_ptr());
                        let out = CStr::from_ptr(out_ptr).to_str().unwrap_or("").to_string();
                        let v: Value = serde_json::from_str(&out).unwrap_or(Value::String(out));
                        if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
                            Ok(v.get("notes").cloned().unwrap_or(Value::String("ok".into())))
                        } else {
                            Err(v.to_string())
                        }
                    }
                },
            );
            registry.register(def);
            names.push(name);
        }
        Ok(names)
    }
}
