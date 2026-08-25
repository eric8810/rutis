# 工作文档:督工 + 宿主侧热重启(supervisor / host-side hot-reload)

> 开始:2026-08-25(新实例接手)。来源:docs/work/handoff.md §三 第 3/5 项。
> 演进顺序第 3 步:督工自动决策(心)。第 5 步:宿主(CLI/TUI)监听事件并实现优雅重启流程。

## 目的
1. **宿主侧优雅重启**:rutis-cli 监听 `SelfReloadRequested`,收到后优雅收尾(保存 session、退出 TUI 循环)并**自动重启进程**(exec 替换,保留内存中的 LLM 不重建;self_reload 从"冷重启"升级为"热重启")。
2. **督工自动决策(最小版)**:宿主监听 self_reload 请求,收到即触发重启——"决策→执行"闭环的宿主侧雏形。
3. 验收:cargo test 全绿 + tmux 冒烟(scripted 下 self_reload 触发 exec 重启,重启后 session 恢复,generation+1)。

## 现状(已确认)
- `self_reload` 工具:写 handoff 意图 + 广播 `SelfReloadRequested{session,reason,intent_path}`(events.rs),宿主尚未监听。
- TUI 主循环:async 侧 `tokio::select!` 有 `ctx.cancelled()` 分支,fiber 取消即优雅退出。
- `FiberView::restart()` 现成;root 只有 dispose。
- EventBus::on(ctx, listener) 注册监听器归 fiber 所有;emit 触发即忘异步派发。

## 完成内容

### 1. Session::persist 自动建父目录(session.rs)
- `.rutis/session.json` 的父目录 `.rutis` 不存在时,persist 之前 `create_dir_all`。
- 修复冒烟时 "plugin failed" 问题(落盘 rename 到不存在目录失败)。

### 2. 宿主侧 ReloadHandler(rutis-cli main.rs)
- struct ReloadHandler { root: Ctx, requested: Arc<AtomicBool> },impl `rutis::Listener<SelfReloadRequested>`。
- call():标记 reload 意图 + `root.root_view().dispose().await` → TUI 循环 `ctx.cancelled()` 优雅退出。

### 3. run() 装配升级
- session path 默认 `cwd/.rutis/session.json`,driver 配 `with_session_path`。
- tools = `self_tools(ctx)` + `minimal_tools()`(6 个 self_* 工具进 TUI 环境)。
- 注册 ReloadHandler;TUI intro 提示 `+ self_*`。

### 4. exec 重启
- run() 收尾检查 reload 标志;置位则 `Command::exec()`(unix)替换当前进程镜像,保留环境与参数。
- 非 unix fallback:`status()` 等待子进程。

### 5. --reload-demo 演示 flag
- scripted 模式第一轮调用 `self_reload`(写 /tmp/rutis-smoke/reload-intent.md),端到端演示。

### 6. 测试
- rutis-cli tests::reload_handler_marks_request_and_disposes_root:监听→标记→dispose root(状态 Disposed)。

## 验收(已通过)
- rutis-agent:78 tests 全绿;rutis-cli:1 test 全绿;workspace check 通过。
- tmux 冒烟(--scripted --reload-demo):
  - reload-intent.md 写入 ✓
  - 进程 exec 替换(子 PID 接管同一 pane)✓
  - 重启后 session id=1 不变、generation 1→2 ✓
  - 重启后 msgs 7→14(历史连续)✓

## 边界(明确不做)
- 不做 dylib/脚本动态加载(终极,后续)。
- 不做复杂策略(定时/资源阈值触发);只做"self_reload 请求 → 重启"的最小闭环。
- 不动 rutis 核心 trait 接口。

## 下一步(给下一代)
- 督工可升级:AgentTurnEnd 后自动评估(如 session 消息数阈值/失败率)→ 自动触发 self_reload 类决策。
- 热重启 vs 冷重启:当前 exec 是进程级;FiberView::restart() 的 fiber 级热重启(不 exec)可探索——保留 LLM 连接,只重装配 agent driver。
- --reload-demo 的 handoff 路径硬编码 /tmp/rutis-smoke/ 可参数化。


## 进展(2026-08-25 二轮):exec → fiber 级热重启

### 问题
上一轮 exec 版:进程级重启,每次 self_reload 重建整个进程(丢 LLM 连接)。

### 改进
1. **ReloadHandler 改为 fiber 级**:收到 SelfReloadRequested → 短暂延迟(300ms,让工具结果回喂/turn 收尾)→ `driver_view.restart()`(干净卸载→重装配,apply 内 Session::restore)。
   - 进程保留(不 exec)、LLM 连接保留、TTY 保留。
2. **TUI 不声明 agent 依赖**(inject_keys 空):driver 重启不驱逐 TUI,UI 保持运行。
   - TUI apply 仍做启动门控(get agent,失败报 InjectUnsatisfied)。
   - TUI 每次提交/取消从 ctx 重新 get 最新 agent(不缓存旧 driver)——driver 重启后新实例被使用。
   - run() 顺序调整:先 await tools/driver,再创建 TUI(driver 就绪后 TUI 门控必过)。
3. **测试**:
   - `crates/rutis-agent/tests/fiber_restart.rs`:driver restart 后 session 恢复(identity 稳定、gen+1、历史连续)。
   - `rutis-cli tests::reload_handler_fiber_restarts_driver`:事件→restart→generation 前进、新 agent 实例、session 连续。
4. **冒烟验证(--scripted --reload-demo)**:
   - PID 不变(进程保留,非 exec)
   - TUI 保持运行(未退出到 shell)
   - reload 轮完整执行(300ms 延迟后无 Turn failed)
   - session:id=1 不变、gen 1→2、msgs 连续
   - 重启后下一轮正常交互

### 关键决策
- 300ms 延迟解决"重启取消进行中 turn"的噪音;更严格可用 AgentTurnEnd 事件同步(未做,够用)。
- TUI 从依赖门控改为启动门控:牺牲"agent 未就绪不启动"的强保证,换取热重启时 UI 不闪。
