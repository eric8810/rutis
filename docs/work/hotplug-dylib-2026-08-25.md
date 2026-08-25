# 工作文档:动态加载 plugin(热插拔)——用户明确要求

> 开始:2026-08-25。来源:用户多次明确要求"新建 plugin 并热加载它,
> 不是 build"。handoff §三 3 上一代判断"不建议做",**方向已由用户推翻**。

## 目标
写一个新的 rutis plugin,编译成 cdylib(.so/.dylib/.dll),
在**运行中的进程**里用 libloading 加载它,让它 apply 生效——
不重新 build 主程序,不重启进程。

## 技术约束(必须诚实面对)
1. **Rust `dyn Plugin` 无法直接跨 cdylib**:trait 方法签名含 `Ctx`/
   `BoxFuture` 等 Rust 类型,过不了 C ABI。TypeId 跨 .so 也不稳定。
2. **可行路径**:cdylib 导出 **C ABI 的纯数据/纯函数**(名字、工具描述、
   回调函数指针);宿主(libloading)加载后,在**宿主侧**把描述翻译成
   `ToolDef`/`Plugin`,`ctx.plugin()` 装配 → 立即生效。
3. 这是"热加载新 plugin"的现实形态:代码在 .so 里,宿主运行中读取并装配。

## 方案
### 1. plugin crate(cdylib)
- `crates/rutis-hotplug-demo`:crate-type = ["cdylib"]。
- 导出 C ABI:
  ```rust
  #[no_mangle]
  pub extern "C" fn rutis_plugin_name() -> *const c_char;      // 插件名
  #[no_mangle]
  pub extern "C" fn rutis_plugin_tool_count() -> u32;          // 工具数
  #[no_mangle]
  pub extern "C" fn rutis_plugin_tool_schema(i: u32) -> *const c_char; // 工具 schema(JSON)
  #[no_mangle]
  pub extern "C" fn rutis_plugin_tool_invoke(i: u32, args_json: *const c_char) -> *const c_char; // 工具执行
  ```
  C 字符串用 CString 管理;宿主负责释放(导出 free 函数)。

### 2. 宿主加载器(运行中)
- `libloading` 打开 .so,取符号,构造 `ToolDef`,`ctx.plugin(ToolsPlugin::new(...))`
  或直接 `registry.register(def)`。
- 验证:运行中的 agent 进程,加载 .so → 新工具出现在 schema → 模型可调用。

## 验证
- `cargo build -p rutis-hotplug-demo` → target/debug/librutis_hotplug_demo.so
- 运行 host demo:加载 .so,调用工具,确认生效。
- 全测试绿 + CI。

## 边界
- 不做完全动态的 `dyn Plugin`(Rust 类型跨 ABI 不可行,诚实记录)。
- 工具是"数据 + 纯函数回调",不含 Ctx 捕获(跨 .so 的 Ctx 引用不可行)。
- 若需复杂 plugin(服务/事件),未来可走进程外桥(rutis-cordis),那是另一条路。

## 进展
- [x] handoff 方向变更记录。
- [x] workspace 加 rutis-hotplug-demo member。
- [ ] plugin crate 实现(C ABI 导出)。
- [ ] 宿主加载器(运行中 libloading + 装配)。
- [ ] 验证 + 测试 + CI。

## 实现完成 + 验证通过
- **plugin crate `rutis-hotplug-demo`**:cdylib,导出 6 个 C ABI 符号:
  `rutis_plugin_name/count/schema/schema_len/invoke/free`。工具 = release_notes。
- **宿主加载器**:
  - `examples/hotplug_host.rs`:libloading 打开 .so → 读 schema → 调用工具。
  - `examples/hotplug_agent.rs`:加载 .so → C ABI schema 翻译成 ToolDef →
    注册进 ToolRegistry → agent 可用。
- **关键约束(实现中确认)**:
  - `dyn Plugin` 无法跨 cdylib(Rust 类型过不了 C ABI)→ 走 C ABI 纯数据/函数。
  - ToolDef 闭包要求 'static → 库用 `Box::leak` 提升为进程级存活;
    invoke 符号转 'static 裸函数指针。
  - `Ctx::root()` 需 runtime → 用 `Ctx::root_with(rt.handle())`。
- **验证**:两个 example 均跑通,workspace 全测试绿(除 pre-existing node e2e)。

## 结论
- **"新建 plugin + 热加载"已实现**:新 plugin 编译成 .so,运行中进程
  libloading 加载,工具立即注册进 ToolRegistry 并被 agent 使用。
- 形态是"数据 + C ABI 回调"而非完整 `dyn Plugin`——这是 Rust 动态加载的
  现实边界,诚实记录。未来若要完整 plugin 语义,走进程外桥(rutis-cordis)。
