//! 热插拔 plugin demo(cdylib):一个"release notes"工具,经 C ABI 导出。
//!
//! 宿主(rutis-cli 或任意 rutis 程序)在运行中用 libloading 加载本 .so,
//! 读取工具 schema + 执行函数指针,在宿主侧构造 ToolDef 并注册,
//! 运行中的 agent 立即可用——不重编译主程序、不重启进程。

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

/// 插件名(C ABI)。
#[no_mangle]
pub extern "C" fn rutis_plugin_name() -> *const c_char {
    cstr("hotplug-release-notes")
}

/// 工具数量(C ABI)。
#[no_mangle]
pub extern "C" fn rutis_plugin_tool_count() -> u32 {
    1
}

/// 第 i 个工具的 schema(JSON 字符串,C ABI)。越界返回空串。
#[no_mangle]
pub extern "C" fn rutis_plugin_tool_schema(i: u32) -> *const c_char {
    if i != 0 {
        return cstr("");
    }
    cstr(
        r#"{
            "name": "release_notes",
            "description": "Return the rutis release notes summary",
            "parameters": {
                "type": "object",
                "properties": {
                    "version": { "type": "string", "description": "optional version filter" }
                },
                "required": []
            }
        }"#,
    )
}

/// 第 i 个工具的 schema 长度(宿主分配 buffer 用)。
#[no_mangle]
pub extern "C" fn rutis_plugin_tool_schema_len(i: u32) -> u32 {
    unsafe {
        let s = CStr::from_ptr(rutis_plugin_tool_schema(i));
        s.to_bytes().len() as u32
    }
}

/// 执行第 i 个工具:args_json 是 JSON 参数字符串,返回 JSON 结果字符串。
#[no_mangle]
pub extern "C" fn rutis_plugin_tool_invoke(i: u32, args_json: *const c_char) -> *const c_char {
    if i != 0 {
        return cstr(r#"{"error":"unknown tool index"}"#);
    }
    // 解析参数(简单处理:忽略,固定返回)
    let _args = unsafe {
        if args_json.is_null() {
            ""
        } else {
            CStr::from_ptr(args_json).to_str().unwrap_or("")
        }
    };
    cstr(
        r#"{
            "ok": true,
            "notes": "v0.2.0: hot-loading (self_hotload), memory compact (self_compact), memory pointer, supervisor auto-restart, CI"
        }"#,
    )
}

/// 释放宿主从本库拿到的 C 字符串(由宿主在读取后调用)。
#[no_mangle]
pub extern "C" fn rutis_plugin_free(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)) }
    }
}

/// 占位:让 `c_void` 不未用(未来可放上下文指针)。
#[allow(dead_code)]
fn _touch(_: *mut c_void) {}

/// 构造一个 'static C 字符串(泄漏内存,由 rutis_plugin_free 释放)。
fn cstr(s: &str) -> *const c_char {
    CString::new(s).unwrap().into_raw()
}
