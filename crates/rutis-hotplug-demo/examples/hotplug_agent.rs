//! 热插拔 + agent:加载 .so,把 C ABI 工具翻译成 ToolDef,
//! 注册进运行中的 ToolRegistry → 运行中的 agent 立即可用新工具。
//!
//! 运行:`cargo run -p rutis-hotplug-demo --example hotplug_agent`

use libloading::Library;
use rutis_agent::{llm_key, Agent, AgentDriverPlugin, LlmResponse, ScriptedLlm, ToolDef, ToolsPlugin};
use serde_json::Value;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Arc;

type FnCount = unsafe extern "C" fn() -> u32;
type FnSchema = unsafe extern "C" fn(u32) -> *const c_char;
type FnInvoke = unsafe extern "C" fn(u32, *const c_char) -> *const c_char;

fn main() {
    let lib_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/debug/librutis_hotplug_demo.so".to_string());

    // 1. 运行中加载 .so
    unsafe {
        // 真实宿主:动态库随进程存活,用 Box::leak 提升为 'static。
        // 工具闭包需要 'static(跨 turn 存活),库必须活得一样久。
        let lib: &'static Library = Box::leak(Box::new(Library::new(&lib_path).expect("打开 .so")));
        let count: libloading::Symbol<FnCount> = lib.get(b"rutis_plugin_tool_count").unwrap();
        let schema: libloading::Symbol<FnSchema> = lib.get(b"rutis_plugin_tool_schema").unwrap();
        let invoke: libloading::Symbol<FnInvoke> = lib.get(b"rutis_plugin_tool_invoke").unwrap();
        let n = count();

        // 2. 把每个 C ABI 工具翻译成 ToolDef(宿主侧)
        //    注意:invoke 是借用 Symbol,转成 'static 裸函数指针(库已 leak,
        //    指针在进程生命周期内有效)。Arc 共享给多个工具闭包。
        let invoke_fn: FnInvoke = *invoke;
        let invoke = Arc::new(invoke_fn);
        let mut defs = Vec::new();
        for i in 0..n {
            let schema_json = CStr::from_ptr(schema(i)).to_str().unwrap().to_string();
            let parsed: Value = serde_json::from_str(&schema_json).expect("schema JSON");
            let name = parsed["name"].as_str().unwrap().to_string();
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
                        // 调 C ABI:把 args 转 CString,调 invoke
                        let args_c = CString::new(args_str).unwrap();
                        let out_ptr = invoke(idx, args_c.as_ptr());
                        let out = CStr::from_ptr(out_ptr).to_str().unwrap().to_string();
                        // 解析 JSON 结果(库返回 {ok, notes})
                        let v: Value = serde_json::from_str(&out).unwrap_or(Value::String(out));
                        if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
                            Ok(v.get("notes").cloned().unwrap_or(Value::String("ok".into())))
                        } else {
                            Err(v.to_string())
                        }
                    }
                },
            );
            println!("热加载工具: {name}");
            defs.push(def);
        }

        // 3. 启动 agent,工具集 = 热加载的工具
        let rt = tokio::runtime::Runtime::new().unwrap();
        let root = rutis::Ctx::root_with(rt.handle().clone());
        let scripted = ScriptedLlm::new(vec![
            LlmResponse::content("no tool needed"),
            LlmResponse::content("second turn"),
        ]);
        root.provide_as(llm_key(), rutis_agent::into_service(scripted)).unwrap();
        let tools_view = root.plugin(ToolsPlugin::new(defs));
        let driver_view = root.plugin(AgentDriverPlugin::new(10));
        rt.block_on(async {
            (&tools_view).await.unwrap();
            (&driver_view).await.unwrap();
            let agent = root.get_as::<dyn Agent>(rutis_agent::agent_key()).unwrap();
            let _ = agent.followup("hi").await;
            drop(agent);
            let _ = driver_view.dispose().await;
            let _ = tools_view.dispose().await;
        });

        println!("\n✅ 热插拔 + agent:运行中加载 .so 工具 → 注册进 ToolRegistry → agent 可用");
    }
}
