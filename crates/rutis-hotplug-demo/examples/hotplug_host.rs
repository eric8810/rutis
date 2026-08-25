//! 热插拔宿主(运行中加载 .so):
//! 1. libloading 打开 `target/debug/librutis_hotplug_demo.so`
//! 2. 读取 C ABI 导出:插件名、工具 schema、调用函数
//! 3. 在宿主侧构造 ToolDef,注册进运行中的 ToolRegistry
//! 4. 实际调用新工具 → 生效
//!
//! 运行:`cargo run -p rutis-hotplug-demo --example hotplug_host`

use libloading::Library;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

type FnName = unsafe extern "C" fn() -> *const c_char;
type FnCount = unsafe extern "C" fn() -> u32;
type FnSchema = unsafe extern "C" fn(u32) -> *const c_char;
type FnInvoke = unsafe extern "C" fn(u32, *const c_char) -> *const c_char;
type FnFree = unsafe extern "C" fn(*mut c_char);

fn main() {
    let lib_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/debug/librutis_hotplug_demo.so".to_string());
    println!("加载动态库: {lib_path}");

    // 1. 运行中加载 .so
    unsafe {
        let lib = Library::new(&lib_path).expect("打开 .so");
        let name: libloading::Symbol<FnName> = lib.get(b"rutis_plugin_name").unwrap();
        let count: libloading::Symbol<FnCount> = lib.get(b"rutis_plugin_tool_count").unwrap();
        let schema: libloading::Symbol<FnSchema> = lib.get(b"rutis_plugin_tool_schema").unwrap();
        let invoke: libloading::Symbol<FnInvoke> = lib.get(b"rutis_plugin_tool_invoke").unwrap();
        let free: libloading::Symbol<FnFree> = lib.get(b"rutis_plugin_free").unwrap();

        // 2. 读取插件名
        let name_c = CStr::from_ptr(name()).to_str().unwrap().to_string();
        println!("插件名: {name_c}");
        let n = count();
        println!("工具数: {n}");

        // 3. 读 schema(宿主侧构造 ToolDef 用)
        let mut schemas = Vec::new();
        for i in 0..n {
            let s = CStr::from_ptr(schema(i)).to_str().unwrap().to_string();
            println!("  tool[{i}] schema: {s}");
            schemas.push(s);
        }

        // 4. 实际调用工具(模拟宿主在运行中注册后模型调用)
        let args = CString::new(r#"{"version":"v0.2.0"}"#).unwrap();
        let out_ptr = invoke(0, args.as_ptr());
        let out = CStr::from_ptr(out_ptr).to_str().unwrap().to_string();
        println!("调用 tool[0]: {out}");
        // 释放 .so 分配的字符串(由库的 free 导出负责)
        free(out_ptr as *mut c_char);

        println!("\n✅ 热插拔闭环:运行中加载 .so → 读取 schema → 调用工具,无需重编译主程序");
    }
}
