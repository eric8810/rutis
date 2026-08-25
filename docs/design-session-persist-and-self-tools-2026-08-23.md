# 设计:session 持久化 + 自我控制工具包

> 2026-08-23。目标:让 agent 具备"重启不丢记忆 + 能控制自身热加载"的最小能力。
> 定位:自我演进的基础设施,最小演进——只加两个东西:记忆恢复 + 控制工具。
> 依据:[design-self-evolving-agent-2026-08-23.md](design-self-evolving-agent-2026-08-23.md)(persona 设计)。

## 一、session 持久化

### 目标
进程重启/依赖重载后,模型可见历史(session)恢复,`agent.id()` 不变。

### 现状(物证)
- `Session` = `{ id: u64(全局自增), messages: Vec<ModelMessage> }`,纯内存,无持久化(`session.rs`)。
- 重启(`fiber restart`)→ 新 driver → 新 `Session::new()` → 失忆。
- `SessionId` 消费点:`Agent` trait 的 `id()`,及 `agent/*` 事件载荷的 `session` 字段。测试仅 `integration.rs:198` 用 `agent.id()`。

### 设计
1. **格式**:单 JSON 文件(默认 `.rutis/session.json`),`SessionFile { version: 1, id: u64, messages: Vec<ModelMessage>, saved_at_ms: u64 }`。aimux 消息自带 serde,零转换。
2. **`SessionId` 分代**:`{ identity: u64(稳定,首次分配), generation: u32(重启 +1) }`。identity 落盘跨重启不变 = 对齐;generation 标识"第几代"。`as_u64()` 保留(返回 identity),兼容现有消费点。
3. **保存时机**:① 每 turn 结束(`followup` 返回前)② fiber 卸载时(挂 effect disposer,LIFO 中后注册→先清理)。原子写(临时文件 + rename)。
4. **恢复**:`AgentDriverPlugin::with_session_path(path)` → `apply` 时 `Session::restore(path)`;失败静默降级为新 Session,不阻断。
5. **默认关闭**:`None` = 不持久化,现状不变。

### 验证
- `session_persist_roundtrip`(纯单测)
- `corrupt_file_starts_fresh`(坏文件 → 新 Session,不卡死)
- `session_restored_after_driver_restart`(核心:ScriptedLlm 两轮,第二轮 prompt 含第一轮 history)
- `not_persisted_by_default`(不传 path = 现状)

## 二、自我控制工具包

### 目标
给 agent 操舵自身热加载的"手"。控制回路 = 决策(督工,后续)+ 执行(工具,本设计)。

### 最小工具集(6 个)
| 工具 | 作用 | 复用/新增 |
|---|---|---|
| `self_status` | 读 session id/代际/状态/版本 | 新增(读 AgentDriver 状态快照) |
| `self_persist` | 手动落盘 session | 新增(调 Session::persist) |
| `self_build` | `cargo build -p rutis-agent` | 复用 bash |
| `self_check` | `cargo test` | 复用 bash |
| `self_reload` | 触发重启(冷:写意图+优雅退出;热:请求督工) | 新增 |
| `self_rollback` | 回滚到上一代 | 新增(版本台账) |

### 挂载点
- 工具作为 `ToolDef` 注册进 `ToolRegistry`(`ToolsPlugin::new(defs)` 的 defs 列表)。
- `self_status`/`self_persist`/`self_reload` 需要访问 `AgentDriver` 状态——**通过 `Agent` trait 的现有方法组合**(`id()`/`status()`/`session()`),不新增 trait 方法(避免污染接口);持久化路径经 `with_session_path` 注入。
- `self_build`/`self_check` 复用 bash,不造轮子。
- `self_reload` **先做冷重启版**(写交接意图 + 退出,宿主重启进程),督工(热重启)为后续演进。
- 工具本身在 ToolRegistry 里,registry 可热替换——**agent 加/换工具 = 热替换 registry**。

### 验证
每个工具一条 scripted 测试;`self_reload` 验证"写意图文档 + 请求退出"。

## 三、明确不做(边界)
- 不做 dsh 两层事件流持久化/回放。
- 不做多 session 管理/切换。
- 不做动态加载新代码(dylib/脚本)——那是后续课题,本设计只做"重启不丢记忆 + 能触发重启"。
- 不做督工自动决策(后续)。

## 四、演进顺序
1. session 持久化(地基,本设计)
2. 自我控制工具包(手,本设计)
3. 督工自动决策(心,后续)
4. 动态加载新代码(终极,后续)
