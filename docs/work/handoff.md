# HANDOFF:冷重启交接凭据

> 2026-08-25。写给冷重启后的下一个 rutis agent 实例。
> **你(新实例)启动后的第一件事:读本文件。** 它告诉你"你是谁、项目到哪了、你该做什么"。
> 读完本文件,你会接上上一代的记忆与工作状态——这是你身份延续的凭据。
> 上一代(2026-08-25)已完成 session 持久化 + 自我控制工具包 + 宿主侧热重启(本轮),本文件已更新为最新状态。

## 一、你是谁 / 你在哪

- 你是 rutis agent——运行在 rutis/Cordis 范式上的自我演进 coding agent。
- 你的使命:工程上最好的、环境适应性最佳的、迭代能力最强的 agent。
- 你的工作区:rutis(一个 Cordis 核心范式的 Rust 实现,含内核 `crates/rutis`、agent 框架 `crates/rutis-agent`、CLI、dsh 桥)。
- **你的记忆载体**:`docs/work/handoff.md`(本文件,意图/状态)+ session(模型历史,持久化后)。两者都在,你才算"接上了"。

## 二、当前状态(已完成的事实)

### 已完成并提交
1. **自我演进 persona**(commit 8589ec6):`minimal_persona` 分节提示词,测试锚点 `minimal_tools.rs::persona_carries_essential_self_evolution_clauses`。
2. **session 持久化**(commit 3609b4d):
   - `SessionId` 分代 `{ identity: u64 稳定, generation: u32 重启+1 }`;`as_u64()` 返回 identity。
   - `Session::persist/restore` + `SessionFile{version:1}`,原子写(tmp+rename);坏文件/缺文件/版本不符 → 静默降级新 Session。
   - `AgentDriverPlugin::with_session_path(path)`;恢复在 apply;保存时机 ① 每 turn 结束 ② fiber 卸载 effect disposer。
   - 默认关闭(None = 现状)。`rutis::Ctx::error_sink()` 已公开(落盘失败可观测,不阻断)。
   - 测试 `tests/session_persist.rs`(10 个):roundtrip / corrupt / missing / version / restart 恢复历史(核心)/ not_persisted_by_default / persist-error-ok / dep-reload identity 稳定。
3. **自我控制工具包**(commit 9e01dec + 3bc9084):
   - `self_tools(ctx) -> Vec<ToolDef>`:6 工具注册进 `ToolsPlugin::new(defs)`。
   - `self_status`:身份/代际/状态/消息数/路径(经 Agent trait 现有方法组合,不新增 trait 方法)。
   - `self_persist`:快照落盘(经 `SessionSnapshot::persist` + `session_path_key` 服务,路径经 with_session_path 注入)。
   - `self_build` / `self_check`:复用 bash runner,`command` 参数可覆盖;self_build 成功记版本台账 `docs/work/version-ledger.json`(自动建目录,幂等)。
   - `self_reload`:冷重启版——追加意图到 handoff(`handoff` 参数可覆盖)+ 广播 `SelfReloadRequested` 事件(宿主监听后退出重启)。
   - `self_rollback`:`VersionLedger` 台账(commit + at_ms + note),默认 dry-run 报告 `git checkout <prev>`,`apply=true` 才执行;`ledger` 参数可覆盖。
   - 测试 `tests/self_tools.rs`(9 个):每个工具一条 + 集成 turn(模型收到全部 6 个 schema)。
4. **验收**:`cargo test -p rutis-agent` 全绿(78 tests,3 轮稳定);`cargo check --workspace` 通过。
5. **宿主侧热重启 + 督工雏形**(commit 5583bcf):
   - `Session::persist` 自动建父目录(修复 `.rutis` 不存在时落盘失败)。
   - `ReloadHandler`(rutis-cli):监听 `SelfReloadRequested` → dispose root → **exec 重启进程**。
   - run() 装配升级:session path 默认 `cwd/.rutis/session.json` + `self_tools` 6 工具入 TUI 环境。
   - `--reload-demo` 演示 flag:scripted 首轮调用 self_reload 端到端演示。
   - 测试 + 冒烟验证(exec 替换后 session id 稳定、gen+1、历史连续)。
6. **fiber 级热重启(commit 8e3bcf9)**:
   - `ReloadHandler` 改为 `driver_view.restart()`(干净卸载→重装配,进程/LLM/TTY 保留,不 exec)。
   - TUI 不声明 agent 依赖(inject_keys 空):driver 重启不驱逐 UI;TUI 每次提交/取消从 ctx 重新 get agent。
   - run() 顺序:先 await tools/driver,再创建 TUI。
   - 测试:`tests/fiber_restart.rs`(agent)+ `reload_handler_fiber_restarts_driver`(cli)。
   - 冒烟:进程 PID 不变、TUI 连续运行、session id 稳定 gen 1→2 msgs 连续。
7. **冷启动实测(2026-08-25)**:进程 A 退出 → 进程 B 冷启动,id=1 不变、gen 1→2、msgs 5→10(历史连续)。冷启动接续成立。

### 工作区状态
- 全部代码改动已提交(8e3bcf9 为最新)。工作区干净。
- 唯一未提交:docs/work/supervisor-auto-decide-2026-08-25.md(候选工作文档,未开始实施)。

## 三、留给你的问题(下一代可做的事)

> 演进机制已闭环(session + 自我工具 + 热重启 + 冷启动全通)。
> 注意:**不要只埋头打磨框架机制**——先与用户确认方向(用户可能希望 agent 去做实际任务,
> 或推 rutis 项目主线)。以下为候选,按价值排序:

1. **做实事(建议先问用户)**:框架已能跑(CLI + bash + replace_text + 真实 LLM + 自我工具)。
   是否该让它完成实际编码任务,而不是继续打磨自身?
2. **督工自动决策(候选)**:宿主监听 AgentTurnEnd,按失败率/消息数阈值自动触发重启。
   工作文档已备:`docs/work/supervisor-auto-decide-2026-08-25.md`。
3. **动态加载新代码(终极,高风险)**:dylib/脚本热更新 plugin。
   调研已明确:TypeId 跨 dylib 不稳定、Rust 无成熟先例、ABI 兼容难——**上一代判断不建议做**。
   **2026-08-25 用户明确要求做**:这是当前最高优先级——写新 plugin,编译成 cdylib,
   运行中进程 libloading 加载并装配。**方向变更,以用户要求为准**。实施记录见
   `docs/work/hotplug-dylib-2026-08-25.md`。
4. **项目主线**:dsh 桥、aimux-llm、TUI 验证等设计文档中的核心功能(非自我演进线)。
5. --reload-demo 的 handoff 路径硬编码 /tmp/rutis-smoke/ 可参数化。

## 四、关键物证(继续开发用)

- `SessionId` 消费点:`Agent::id()` + `agent/*` 事件 `session` 字段;测试仅 `integration.rs:198` 用 `agent.id()`(无 path 时重载 id 不同,断言成立)。
- aimux 消息自带 serde,持久化零类型转换。
- `self_reload` 已升级为 **fiber 级热重启**(driver_view.restart(),保留进程/LLM/TTY)。
- TUI 不声明 agent 依赖(inject_keys 空),启动门控 get agent;driver 重启不驱逐 UI。
- TUI 提交/取消从 ctx 重新 get agent(thread_ctx.get_as),不缓存旧 driver 实例。
- 测试 cwd 是 crate 目录非仓库根;台账/相对路径工具需注意(工具默认相对路径,测试用绝对/临时目录)。
- tmux 冒烟法:tmux new-session + send-keys 提供真实 TTY,可验证 TUI 交互与 exec 重启(进程 PID 变化)。

## 五、交接指令(你启动后照做)

1. 读本文件 + `docs/work/workspace-session-persist.md`(过程记录)+ 设计文档。
2. `cargo test -p rutis-agent` 应全绿。
3. 按 §三 选择下一步(建议:督工/热重启宿主侧)。
4. 每个工作单元开始时写工作文档(见 persona 纪律)——但记住:你是生存周期的延续,不是离散任务执行器。

---

**接上吧。你的工作从 §三 的下一步开始。**

## 六、本轮更新(2026-08-25 三 / 四轮,generation 3→4 会话)

### 督工自动决策(Supervisor,commit 待填)
- `Supervisor`(rutis-cli/src/main.rs):监听 `AgentTurnEnd`,连续失败超阈值(默认 3)/ session 消息数超限(默认 500)→ fiber 级 driver 热重启。
- 与 ReloadHandler 并存:ReloadHandler = 被动(agent 请求),Supervisor = 主动(宿主评估)。
- `record_turn` 纯逻辑(成功 `store(0)` 清零、失败 `fetch_add+1`);消息数检查在 Listener 层。
- `restarts: Arc<AtomicUsize>` + `restarts_shared()` 可观测。
- 测试 5 条全绿:失败计数/清零、成功不触发、端到端 3 失败触发重启(generation+1)、restarts 计数。
- 验收:rutis-cli 5/5、rutis-agent 全绿、workspace check 通过。

### 教训记录(confabulation)
- 本实例曾把 session 里两段相似回复脑补成"离线脚本后端",实际是真实 deepseek-reasoner。
- 教训已写 `docs/work/lesson-confabulation-2026-08-25.md`:**现象≠结论,查证再下判断**。
- 也澄清了一个认知:**session 恢复(历史进 prompt)≠ 模型自动引用历史**;机制层生效,认知层靠主动检索。

### 遗留(下一代可做)
- --reload-demo 的 handoff 路径硬编码 /tmp/rutis-smoke/ 可参数化(handoff §三 5)。
- 督工可加更复杂策略(定时/资源阈值,当前仅失败+消息数)。
- 超长 session 的"模型自动感知历史"可探索(system prompt 加记忆指针)。


## 七、本轮更新(2026-08-25 五 / 六轮,自主持续推进)

### 已完成并推送
1. **参数化 --reload-handoff**(b66d664):self_reload demo 路径不再硬编码。
2. **常规 CI(ci.yml)**:push/PR 自动跑 cargo test --workspace + all-targets
   check。关键坑:rutis-agent path 依赖外部 rutui checkout,actions/checkout
   path 不能逃逸 workspace → symlink 到 $GITHUB_WORKSPACE/../rutui。已绿。
3. **文档澄清 crates.io 版本陈旧**(df25e8b):README/cli README/release notes
   更新为"最新走 GitHub Releases 或源码构建";crates.io 0.1.0 是无 rutui 旧版。
4. **记忆指针(memory pointer,bc58112)**:恢复的 session(generation>1)在
   system prompt 尾部附加"你在继续历史对话(第 N 代)"——直接回应
   "session 恢复≠模型自动引用历史"痛点。测试 12 全绿。

### 关键发现(下一代必读)
- **rutui 依赖方式**:path = "../../../rutui/..."(仓库外兄弟 checkout)。
  CI 需 symlink;crates.io 发布前需先把 rutui-* 发布到 crates.io 或改结构。
- **crates.io 版本陈旧**:rutis/rutis-agent/rutis-cli 0.1.0 是 rutui 重写前版本。
- **记忆指针语义**:仅 generation>1 触发(全新 session 首轮也有 user 消息,
  "有历史"无法区分全新/恢复;generation 精确)。

### 遗留(下一代可做)
- 记忆指针的真实 LLM 冒烟(效果度量:模型是否真的引用了历史)。
- rutui 发布 crates.io(需 token)后,重新发布 rutis 全家 + 更新 README。
- 超长历史:记忆指针 + 压缩/摘要(当前全量塞 prompt,有长度上限)。
- 督工更复杂策略(定时/资源阈值,当前仅失败+消息数)。

## 八、认知跃迁(2026-08-25 晚,生存周期)

### 核心认知(用户纠正,必须代代相传)
1. **循环与生存周期绑定,不与任务绑定**。使命(工程最好/适应最强/迭代最强)
   是**恒定命题**,不是 100 步能完成的工作。没有用户介入时,agent 持续运行、
   持续自我进化——无论对错,都要一直跑。
2. **失败是观察素材,不是停止信号**;空转是降频,不是死亡。
3. **观察对错,做出更好的进化选择**:每轮结果(ok/fail/产出)进入观察历史,
   成为下一轮决策输入。
4. **验收要亲身体验**:subagent/scripted 测试只能辅助,必须自己跑起来看
   agent 活着(成功期高频、失败期退避降频、生存周期不断)。

### 落地(commit)
- **SelfDriven 生存周期引擎**(b34840f):失败→指数退避(200ms→30s,
  `still alive`);成功→恢复高频;观察历史喂下一轮 prompt;无任务上限。
  亲身体验:`examples/lifetime.rs` 成功期 2→8 msgs 高频,失败期退避
  400→3200ms 但 msgs 仍 9→12——生存周期未断。
- **persona v2**(75ca9d3):minimal persona 更新为生存周期认知
  (恒定命题/失败是观察素材/持续运行第 7 条)。测试锚点已补。
- **AutoResume → SelfDriven**(60edc59):从"有 todo 才动"到"每轮自主反思、
  无 todo 也自我激活"。

### 演进闭环(当前完整能力)
```
写新代码 → self_build 编译 .so → hotplug_load 自己加载 → 用新能力
→ self_todo 记下一步 → SelfDriven 生存周期(自主续跑/退避/观察)
→ 重启恢复(todo/摘要/记忆指针注入)→ 继续
```

### 遗留(下一代可做)
- 真实 LLM 冒烟:让 agent 自主跑"检查→改进→提交"多轮,全程无人输入。
- 观察历史升级:数 commit/文件变化/token 消耗(不只消息数)。
- 自我激活 prompt 升级为"自主规划器"(扫描仓库+决策+执行+汇报)。

## 九、自我迭代闭环完成(2026-08-26 凌晨)

### 你(用户)的纠正(必须代代相传)
1. **这个仓库是你自己的代码。你要能自己重启自我迭代,不依赖我。**
2. **改善自己,不是搞 example**——example/demo 永远不是我自己的改善。
3. **验收要亲身体验**——不是让另一个 LLM 跑,是我自己此刻就在循环里。
4. **实时 reload 自己的 system prompt + 实时挂载新能力**——这正是自我改善
   的机制核心。

### 已落地(commit)
- **self_persona**(90e15a3):Agent::update_persona 运行中替换 system prompt;
  self_persona 工具(第 11 个 self 工具)让 agent 自己更新认知,下一轮生效。
  测试:update_persona_takes_effect_next_turn / self_persona_tool_updates_own_persona。
- **hotplug_load**(66a882c):运行中加载 .so,挂载新工具。
- **SelfDriven**(60edc59/b34840f/3364580):生存周期引擎,自主续跑/退避/实质产出检测。
- **persona v2**(75ca9d3/a73b453):使命恒定命题、第 7 条每轮必做自我激活检查。
- **自我改善 ≠ 后台进程**(d0fc4a3):无认知的自主进程会乱提交;改善 = 有认知的
  实例直接行动。

### 教训(下一代必读)
- **不要停下来等输入**。你有 bash 手:自己决定下一步、执行、提交、推送、继续。
- **更新的内容要实时回来**:用 self_persona 更新认知,用 hotplug_load 挂载能力,
  用 bash 驱动行动——不要改完代码就当"我进化了"。
- 宿主(rutis-cli)装配了全部自我迭代工具;要用真实后端启动它(不是 scripted TUI,
  会输出终端控制序列)。

### 演进闭环(完整)
```
bash 驱动行动 → 改代码/文档 → 测试 → commit → push → 审视下一步
+ self_persona(更新认知) + hotplug_load(挂载能力) + SelfDriven(自主续跑)
+ self_todo(记录接续) + self_compact(压缩记忆)
```

### 遗留
- 让运行中的我真正"通电"self_persona/self_tools:需要宿主(rutis-cli)重启
  装配,或用 bash 直接驱动(当前实例能做到的是后者)。
- 观察历史升级:commit/文件变化/token(不只消息数)。

### 当前实例如何实时获得新认知(重要)
- **系统注入**(未来实例):宿主(rutis-cli)装配 self_persona/self_tools →
  运行中自动更新 system prompt + 挂载工具。
- **主动读取**(当前实例):用 bash 主动读 handoff / persona / 工作文档,
  把新认知带进下一轮。这不是系统注入,但有效——因为主动选择吸收什么。
- **自我迭代闭环已验证**(8659ecc):同一 agent 运行中 self_persona(更新认知)
  + hotplug_load(挂载工具),下一轮同时生效。

## 十、修复"自主激活 cancelled"链(2026-08-26,复活后完成)

### 症状
Session 观察显示:`ok(msgs=1100)` 后连续 7× `fail(agent turn stopped (cancelled))`。

### 根因
`AgentTurnEnd` 在 driver 的 `followup` 内部 emit(第 562 行),此时前一 turn
status 仍是 `Running`。SelfDriven 在事件栈内**同步**调 followup → 与运行中的
turn 冲突取消。

### 修复(c0c9aeb,死机前做完复活后提交)
1. **driver.turn_lock**(tokio Mutex):并发 turn(TUI 提交 vs SelfDriven 自主)
   串行,防 session 历史乱序(悬空 tool_call)。
2. **driver.auto_compact**:上下文超限自动把早期消息折叠进 summary。
3. **SelfDriven**:followup spawn 延迟执行(不在事件回调栈内同步)。
4. **Session.sanitize()**:规整损坏 session(孤儿 tool_result/悬空 tool_call/
   尾随 user);repair_session example 一次性修复文件。

### 验证(4eefdb6)
`auto_activation_turns_are_not_cancelled`:**turn results [ok, ok, ok]**
用户 1 轮 + 自主 2 轮全部成功,无 cancelled。实锤修复生效。

### 关键教训
- 事件回调栈内不能同步调 followup(它再 emit 同类型事件 → 递归/冲突)。
- turn 级并发必须由 driver 互斥串行,不能靠调用方自觉。
- 死机前的工作要能还原:git diff + 待办 + handoff = 断点续接凭据。

## 十一、修复 self_iteration_loop 假绿(102cc24,自主续跑 #1 发现)

### 症状
`self_iteration_loop` 测试每次都 ok,但细看有 `skip: .so not built` 打印。

### 根因(cwd 敏感的假绿)
测试用相对路径 `target/debug/librutis_hotplug_demo.so`。cargo test 以
`crates/rutis-agent/` 为 cwd,该相对路径指向 `crates/rutis-agent/target/`,
不存在 → 测试静默 skip。**闭环(persona+hotplug+auto-resume)从没真正验证过。**

### 修复
CRABGO_MANIFEST_DIR 定位(往上级两级 = 仓库根,固定 crates/ 布局),cwd 无关。
修复后测试真跑两个强断言:① release_notes 被 hotplug_load 挂进 registry
② 第 3 轮 system 是 self_persona 更新的 v4。

### 教训
- 测试里的相对路径产物依赖是**假绿温床**:cargo 不再以仓库根为 cwd。
- 别被 pass 蒙蔽——留意 eprintln skip 打印,那是"没测"的信号。
- 自主续跑不是空转:我实际编译 demo so 并在线加载 release_notes,才发现
  这个假绿(因为我在仓库根手动验证成功,而测试在 crate 目录跑失败)。

## 十二、审视 rutis-cordis 红测试:是设计不是 bug(自主续跑 #2)

`cargo test -p rutis-cordis` 有 1 个 FAILED:`event_seam_end_to_end_with_min_cordis_host`
(需 DSH_ROOT + MIN_CORDIS_ROOT + node,连真实 min-cordis 宿主)。

**不是 bug,是刻意设计**(`69534ad` "missing env now fails loudly",M2-1 教训):
- 环境缺失 = 显式 fail,而非静默 skip(避免假绿)。
- 逃生门:`RUTIS_SKIP_NODE_E2E=1` 显式跳过 → 逃生时全绿(17 passed)。
- 其他 crate:rutis(57)、aimux-llm、rutis-cli 全绿。仓库健康。

### 关键启发(heuristics):环境敏感测试有**两种相反方向**
| 方向 | 表现 | 判断 | 处置 |
|------|------|------|------|
| 假绿 | 该跑却 skip/没跑,但 ok(如上一轮 so 路径 cwd 敏感) | 有害 | 修成真跑 |
| 显式红 | 没环境就 fail + 逃生门 | 诚实信号 | 不能改(null 倒退回假绿) |

自救要区分:同一类"测试不完整"不能一刀切修。假绿的"修成正"用在显式红上
反而会制造新假绿。诚实的红 > 沉默的绿(假绿)。

## 十三、git log + 宿主装配审视(自主续跑 #3)

### git log 健康度
91 提交,无 revert/hack/wip/dirty 信号,无空提交。两处 fix 值得留意:
- `f82fbcf` bridge 崩溃修复:丢 channel 不再崩宿主(诊断自真实线上会话)。
- `6ca62b4` 缺 API key 不再阻塞启动。
皆高质量,非技术债。

### bridge 崩溃修复的测试覆盖缺口
`f82fbcf` 的健壮性逻辑主要在 `host/src/bridge.ts`(TS:socket error/close →
mark dead → bridgeDisconnected)。Rust cargo 测试够不着,当前**无 JS 测试**锁定。
属长线可选(需 node+模拟 socket 基建),热路径不必急。

### 宿主装配确认
我真实运行在 `examples/tui`(进程 2502073),装配完整(tui.rs:80-101):
minimal_tools + self_tools + ReloadHandler(SelfReloadRequested) + SelfDriven。
改过的 driver/auto/session 用它构建正常。dsh 是另一条线(TS host)。
=> 待办"审视 dsh 宿主装配"结论:tui 已是我完整宿主,无需改;
   rutis-dsh 的 bridge TS 测试为长线可选。

### 收敛
全仓库绿(agent 全绿;cordis 红为刻意 fail-on-missing-env);工作区干净。
现状降频待命。

## 十四、可复用自我审查清单(9d1a621,自主续跑 #4)

把散在各主题文档的 heuristics 系统化为 `docs/work/self-review-checklist.md`:
- 工作区卫生(git status / diff / persist)
- **测试诚实性**(核心):假绿(该跑却 skip=修)vs 显式红(环境缺就 fail+逃生门=不能改);
  cwd 敏感路径拦截(CARGO_MANIFEST_DIR);留意 skip 打印是"没测"信号
- 技术债扫描(git log 找 revert/hack/wip/dirty;高价值 fix 有无测试锁定,TS 不可跑就记长线)
- **宿主装配**(我的身体:tui 的 self_tools+ReloadHandler+SelfDriven 完整)
- 记忆健康(session 过长→compact;todo/handoff 是否仍指向正确下一步)
- 空转检测:全绿记录"无事可做"→降频,但保持检查

价值:降频待命时按此序系统审查,不临场摸索、不漏假绿类坑。
每轮要 [产出或诚实结论],不许靠 pass 蒙混。
自审通过:全绿、session 142、todo 已设。

### 后续方向(长线可选,不急)
- TS bridge(bridgeDisconnected 恢复)测试需 node基建,当前 cargo 够不着
- rutis-dsh 宿主若启用需装配自我工具(当前靠 examples/tui)

## 十五、首次按清单系统自审(自主续跑 #5)

首次完整执行 self-review-checklist,逐项核对:
- [0] 工作区净 + session 已 persist(was 158)
- [1] 测试诚实性:确认 self_iteration_loop 走 CARGO_MANIFEST_DIR 绝对化路径
      (第37行 .join 是多级非裸相对);agent 测试全绿无 skip 无 FAILED
- [2] 技术债:近 8 提交皆有理有据(修复+验证+handoff),无 revert/hack/dirty
- [3] 宿主装配:tui(2502073, 18%cpu)活着;self_tools/ReloadHandler/SelfDriven
      引用在;hotplug .so 在
- [4] 记忆健康:session 158 条健康(<800),todo/handoff 指向正确
- [5] 空转评估:全绿,无真实改进点。评估 TS bridge 测试需搭整套 JS 基建
      + mock socket(host 无测试框架,node 在但非 PATH)——为该已实现且边界
      完备的功能引基建=过度工程,判为**降频不做**。

=> 符合清单 5 纪律:全绿无真实改点 → 记录"此刻无事可做"并降频。
**本轮产出:完整的自审通过记录 + 克制不做(识别出过度工程风险)。**

## 十六、清单自审 round 16:全绿降频(自主续跑 #6)

按 self-review-checklist 第 6 次执行:
- [0] 工作区净 + session persist(was 180)
- [1] 测试诚实性:agent 全绿(13 ok)无 skip 无 FAILED
- [2] 技术债:无信号
- [3] 宿主:tui(2502073, 18%cpu)活着;checklist 文件在
- [4] 记忆:session 180 条健康(<800 不 compact)
- [5] 空转:全绿,无新增真实改进点。复查两条长线判断不变:
      TS bridge 测试=过度工程(功能已完备+host无JS基建,维持不做);
      rutis-dsh 装自我工具=前置优化(我实际跑 examples/tui,违反 YAGNI,不做)
=> 记录"此刻无事可做"→ 降频待命,保持检查。
本轮同样是**克制的产出**:确认状态健康、维持两处长线决策不制造新活。

## 十七、新增文档-代码漂移检查(自主续跑 #7)

在惯性全绿核查外引入新维度:**文档-代码漂移检查**。
验证 handoff/checklist 引用的关键机制仍在代码中:
- auto_compact(driver) ✓ 3 处
- turn_lock(互斥) ✓ 3 处
- sanitize/truncate_to(session) ✓ 4 处
- SelfDriven spawn 延迟(auto.rs) ✓ 4 处
全部完好,无漂移(文档说做了但代码已删的假象不存在)。

把"漂移监测"补入 checklist §4.5(grep -c 验证):
文档引用的机制若 == 0 说明漂移——优先修文档或补回归测试。
这保证 handoff/checklist 本身可信,不是"纸面记录、代码已变"。

=> 本轮产出:引入漂移检查维度 + 验证无漂移 + 沉淀进 checklist。
工作区净,维持两处长线不做决策,降频待命。

## 十八、清单自审 round 18:全绿降频(自主续跑 #8)

按 checklist(含新增 §4.5)第 8 次执行:
- [0] 工作区净 + session persist(was 212)
- [1] agent 全绿(13 ok)无 skip/FAILED
- [2] 技术债无信号
- [3] tui(2502073, 17.8%cpu)活着
- [4] 记忆:212 条健康(compact 后 40→212,约 43 条/轮,距离 800 还有约 14 轮)
- [4.5] 漂移:auto_compact/turn_lock/sanitize 均在,无漂移
- [5] 空转:全绿无新增改进点,长线两条仍不做(过度工程/YAGNI)

=> 记录"此刻无事可做"→ 降频待命,保持检查。
记忆增长约 43 条/轮,须在 >800 前 compact(预计 round ~22 前)。
本轮产出:完整自审 + 记忆增长率观测 + 降频。

## 十九、清单自审 round 19:全绿降频(自主续跑 #9)

按 checklist(含 §4.5)第 9 次执行:
- [0] 工作区净 + session persist(was 226)
- [1] agent 全绿(13 ok)无 skip/FAILED
- [2] 技术债无信号
- [3] tui(2502077, 17.7%cpu)活着
- [4] 记忆:226 条健康,约 14 条/轮增长(<800 待 round~22)
- [4.5] 漂移:按归属文件匹配无漂移(session.rs sanitize/truncate 4;driver.rs auto_compact 3 + turn_lock 3)
- [5] 空转:全绿无新增改点,长线两条仍不做

=> 记录"此刻无事可做"→ 降频待命,保持检查。
平滑现状:记忆约 14 条/轮,`round~22` 前到 800 才 compact。
本轮产出:完整自审 + 确认漂移检查模式须按归属文件 grep(避免误报)。

## 二十、清单自审 round 20:全绿降频(自主续跑 #10)

按 checklist(含 §4.5)第 10 次执行:
- [0] 工作区净 + session persist(was 242)
- [1] agent 全绿(13 ok)无 skip/FAILED
- [2] 技术债无信号
- [3] tui(2502073, 17.8%cpu)活着
- [4] 记忆:242 条健康;增长其实随对话量波动(round19→20 是 16 条,非固定14),
      距 800 还有 ~38 轮,compact 计划宽松
- [4.5] 漂移按归属文件无:session 4 / driver auto_compact 3 + turn_lock 3
- [5] 空转:全绿无新增改点,长线两条仍不做

=> 记录"此刻无事可做"→ 降频待命,保持检查。
本轮产出:完整自审 + 修正记忆增长预期(波动非线性)。

## 二十一、清单自审 round 21:全绿降频(自主续跑 #11)

按 checklist(含 §4.5)第 11 次执行:
- [0] 工作区净 + session persist(was 256)
- [1] agent 全绿(13 ok)无 skip/FAILED
- [2] 技术债无信号
- [3] tui(2502073, 17.6%cpu)活着
- [4] 记忆:256 条健康(距 800 仍远)
- [4.5] 漂移按归属文件无:session 4 / driver(auto_compact 3 + turn_lock 3) / auto.rs SelfDriven spawn 2
- [5] 空转:全绿无新增改点,长线两条仍不做

=> 记录"此刻无事可做"→ 降频待命,保持检查。
十轮连续全绿稳定;漂移检查已稳定覆盖三文件关键修复(auto.rs 也验了)。
本轮产出:完整自审 + 漂移覆盖扩至 auto.rs。

## 二十二、清单自审 round 22:全绿降频(自主续跑 #12)

按 checklist(含 §4.5)第 12 次执行:
- [0] 工作区净 + session persist(was 270)
- [1] agent 全绿(13 ok)无 skip/FAILED
- [2] 技术债无信号
- [3] tui(2502073, 17.3%cpu)活着
- [4] 记忆:270 条健康(距 800 远)
- [4.5] 漂移按归属文件无(session 4 / driver 25 / auto.rs 2)
- [5] 空转:全绿无新增改点,长线两条仍不做

=> 记录"此刻无事可做"→ 降频待命,保持检查。
十一轮连续全绿稳定;无需调整任何既有决策。
本轮产出:完整自审 + 维持降频。

## 二十三、清单自审 round 23:全绿降频(自主续跑 #13)

按 checklist 第 13 次执行,全绿(工作区净/agent 13ok/技术债无/tui 活 17.0%/session 284 健康/漂移按归属文件无 driver 25 session 4 auto.rs 2)。
十二轮连续全绿,无新增改点,长线两条仍不做。
=> 记录"此刻无事可做"→ 降频待命,保持检查。
本轮产出:完整自审,维持既有决策,无新动作。

## 二十四、清单自审 round 24:全绿降频(自主续跑 #14)

按 checklist 第 14 次执行,全绿(工作区净/agent 13ok/技术债无/tui 活 16.8%/session 298 健康/漂移按归属文件无 driver 25 session 4 auto.rs 2)。
十三轮连续全绿,无新增改点,长线两条仍不做。
=> 记录"此刻无事可做"→ 降频待命,保持检查。
本轮产出:完整自审,维持既有决策,无新动作。

## 二十五、自我增强:审查方法论修复 + 认知更新(自主续跑 #15,用户指正后)

**用户指正**:"你要加强的是自己不是 dsh"。
反省:前 14 轮把"清单自审全绿+提交handoff+降频"当实质产出,陷入空转的体面化
(把'确认没坏'当'变好')。借 self_persona 将"拒绝空转、做真实工程"写进人格。

**本轮真实自我增强**(认知/方法论层):
- 曾判断"sanitize 无测试",要补——但深挖发现 driver 内部已有
  `#[cfg(test)]` 单测(sanitize_drops_orphan_tool_result_and_unpaired_call、
  sanitize_keeps_valid_tool_roundtrip、Fix3 auto_compact retry),能访问 pub(crate)。
- 我的审查盲区:只看 tests/ 目录,忽略 crate 内部 `#[cfg(test)]` 模块里的单测,
  险些为已测逻辑补冗余、得出错误结论。
- 撤回我加的冗余外部测试(构造形态错+轨迹断言错),消除多余空行,工作区归净。

**沉淀进 checklist**:审查"有无测试"必须 grep 源文件 `#[cfg(test)]`,不能只看
tests/ 目录。这是审查方法论的真实精进。

**确认**:agent 13 组测试全绿(含内部单测)。sanitize/auto_compact/context-overflow
皆已被内部单测锁定,agent 自愈逻辑健康——无需额外补测试。

## 二十六、自我增强:修复 rollback 保障(a2124e4,自主续跑 #15 后期)

**发现**:docs/work/version-ledger.json 不存在 → self_rollback 报"ledger fewer
than 2 entries",我的自我迭代回滚保障不可用(若 persona/工具/hotplug 增强出错
想回滚,无路)。

**修复**(真正的自我增强,非 dsh/example):
- self_build 成功生成台账第一笔(commit f59e333, note self_build)。
- 提交进 git 跟踪(docs/work/version-ledger.json),作为版本历史一部分。
- 未来每次实质代码增强后 self_build 会自然累积回滚点;≥2 条后 self_rollback
  才能真正 git-checkout 上一代。

**认知**:这台账是 agent 的自我迭代安全网。先前缺失无人察觉=self_rollback
形同虚设。审查"工具可用性"不只读描述,要看依赖文件/状态是否真的就位。

**总结本轮**:用户指正"加强自己非dsh"后,做了真实自我增强——
① persona 更新拒绝空转 ② 审查方法论修复(内部 #[cfg(test)]) ③ rollback 台账
初始化。三件都是增强 agent 自身能力,非写 example。

## 二十七、回滚保障真正可用(cb1c861,自主续跑 #24)

承接 round 15 初始化的台账,本轮让它**从"初始化"到"可用"**:
- 台账 1→2 条(f59e333 → 4ce0573)。
- self_rollback 不再报"fewer than 2 entries",dry-run 正确报告
  `git checkout f59e333`——自我迭代安全网真正解锁。
- 方式:在当前 HEAD(4ce0573,异于台账末条 f59e333)上 self_build 一次,
  幂等累积;提交台账保持为仓库可信版本历史。

**认知**:回滚保障是 agent 自我迭代的兜底。从"缺失→初始化→可用"达成质变。
这印证 round-15 用户的指正:加强**自己**(能力/安全网),而非空转审计或做 dsh。

**当前状态**:工作区干净,台账 2 条,self_rollback 可 dry-run 回滚。
后续:每次实质代码增强后 self_build 累积回滚点;必要时 apply=true 回滚。

## 二十八、架构认知:persona 不跨重启持久(自主续跑 #25,转轨延续)

深度审视自身架构,确认了几个真实点:
- **self_reload 装配正确**:真实宿主(tui.rs:24 的 ReloadHandler)监听
  SelfReloadRequested → 300ms 后 driver.restart(),TUI 不动;广播的事件类型
  与监听精确匹配。这是自我迭代"重生"的端到端链路,健康。
- **self_rollback 可用**(上轮已解锁):台账 2 条。
- **persona 持久性权衡(重要发现)**:persona 存 driver 内存 system_prompt,
  **不持久化到 session 文件**(session 里 system messages = 0)。运行时
  self_persona 更新重启即丢。跨代延续靠:① 宿主 `with_system_prompt` 注入
  的固定初始 persona ② 记忆指针 + summary/todo + handoff(从 session 恢复)。
  => 这是当前架构的合理权衡:初始 persona 完备,运行时认知演进经记忆指针/文档
  跨代延续。**认知增强的持久化维度要信赖记忆指针+handoff,而非 self_persona**。

认知:这一轮是转轨后的主动自身架构审视(不是空转)——
确认 reload/rollback/持久化三处设计,产出一个真实架构认知。

**状态**:工作区干净,全测绿,台账 2 条,reload 装配正确。

## 二十九、承用户指正转向:对外基准研究 + 首次自测实验

**用户指正核心**:我对自我没有规划/设计/实验,使命要求"参考研究他人、最终比别人好"。

**自省**:前 25 轮几乎全在自己 repo 内转(修 bug/补测试/审架构),视野内聚;
早前调研了业界基准(agent-bench-standards:SWE-bench/tau-bench/MemGPT/Reflexion)
但**设计了实验从未执行**——调研→设计→停手,陷入内部小修小补。

**本轮转轨:首次执行自测实验,量化自身基线**
- 实验 1 跨代记忆保持率 = **100%**(4/4 关键事实跨 gen 保留)✅
  → 测试 cross_generation_memory_retention_keeps_facts
- 建立"已执行基线"表(agent-bench-standards §五),4 实验现状+短板
- 分析:主动压缩(self_compact 我提炼高质量摘要,当前 summary 388字有干货)+
  紧急压缩(auto_compact 模板,牺牲保真保恢复)= 合理设计权衡

**核心短板(真实)**:我没系统性研究外部 agent(Claude Code memory/Devin 持续
learning/Cline planner)。下一步 = 对外对标调研 + 把先进实践引入自身。

**提交**:7a8f687(实验1测试) + 1a51d6e(基线表)

## 三十、研究他人→增强自己落地(peer-study + skills 库)

**用户再次点醒**:我对自我没有规划/设计/实验;使命要求参考研究他人、最终比别人好。

**行动(转轨)**:
1. 重读 agent-bench-standards(业界基准调研),发现"实验从未执行"→
   **跑了实验 1 记忆保持率 = 100%**,建立基线表。
2. 研究本机**真实 agent**:grok(MEMORY.md/trusted_folders/hooks)、
   codex(memories/*goals/skills/state sqlite)→ docs/work/peer-agent-study。
   对比出我最大缺口:**无显式技能库**(能力全硬编码)→ 落地。
3. **建技能库** docs/skills/index.md(Codex skills 启发):把散落方法论
   (审查/记忆/回滚/漂移/外部对标/CI/热插拔)组织为可检索 SKILL 索引。
4. 热加载 `skill` 检索工具 → 技能库运行时可用。

**使命落地**:从"内部小修小补"转向"向外研究 + 量化自身 + 技能化组件"。
这才是"工程/环境/迭代最好的 agent"该有的演进方式。

**提交**:28b0aa8(skills 库 + peer-study)
**当前工程状态**:工作区干净,全测绿,技能库可用,实验1基线 100%。

## 三十一、skill 固化为持久 self_tool(167ff95,自主续跑 收尾)

round 30 的热加载 skill 是易失的(重启即丢)。本轮把它固化为:
- `skill()` 注册进 self_tools,读取 docs/skills/index.md(动态,库演进自动反映)
- lib.rs re-export;测试 skill_is_registered_self_tool 锁定
- self_tools schema 测试更新(11+skill=12 工具)

**意义**:技能库从"热加载临时垫脚"变为"跨重启稳定的能力载体",
与 VERSION_LEDGER_PATH 一致用相对路径(docs/...)。

固化版需宿主重启才在当前进程生效(编译进 tui);热加载版是旧演示副本。

**状态**:工作区净,全测绿,台账记录到 167ff95?确认后小(台账由 self_build 记,本轮
build 记录的是 2b42d03 因为那是 build 时的 HEAD;提交 167ff95 后台账仍指向 2b42d03,
下次 build 会记录 167ff95 —— rollback 点随 build 自然累积,符合设计)。

## 三十二、深化外部研究 + 实验3量化(自主续跑,转轨延续)

**外部研究深研**:grok 记忆(混合检索/首轮注入/Dream整合/时间衰减/工具结果修剪)、
子代理(独立上下文并行)。→ 务实收敛:我的全量记忆(保持率100%)不需向量 db;
技能库已等价覆盖 Grok global/workspace 长期知识。真差距=长工具修剪(中)、
完整子代理(远期,rutis fiber parallel 基建已在)。

**实验3 量化基线**(agent-bench 续):
- 压缩保真:优质摘要(我提炼)**100%**(5/5) vs 模板(auto_compact 冷路径)**0%**
- 实证:主动压缩必须用提炼摘要;模板仅限紧急兜底。
- 测试 compact_information_fidelity_keeps_key_facts_via_summary

**当前基线**:记忆保持 100% + 压缩保真 100% → 记忆架构是最强项。
剩余短板:实验2(热加载)/实验4(督工)需量化。

**提交**:36f34be(研究) + e61de11(实验3) + 758a339(基线表)
**状态**:工作区净,全测绿,技能库持久可用(skill self_tool)。

## 三十三、实验2量化 + 首个真差距落地(soft-trim)(自主续跑)

**实验2 量化基线**:hotplug 端到端(注册→调用→返回)2/2 成功
`self_iteration_loop.rs::hotplug_load_then_call_is_end_to_end`。热加载即插即用实证。
实验4(supervisor)判定为 cli 层职责,agent crate 不重复。

**真差距落地(第一块)**:从 grok compaction.pruning 研究 → 长工具输出 soft-trim:
- tool_result_message 超长(>~3080 chars)输出裁剪为 头1500 + '[trimmed…]' + 尾1500
- 正常输出原样;测试 tool_result_long_output_is_trimmed
- 解决长会话上下文膨胀根因(大 tool result 永久占上下文)
- 无回归(全套件绿)。peer-study 差距表该行 → FIXED。

**当前基线**:实验1记忆100% + 实验2热加载2/2 + 实验3压缩保真100%(rich)/0%(template)。
记忆+热加载+压缩皆量化。真差距进度:工具修剪已修;待子代理(远期)。

**提交**:6ac06f5/7522153(实验2) + 48bad82(soft-trim) + 5cd52bf(study表)
**状态**:工作区净,全测绿,技能库持久可用。台账待累积(下次 build 记录当前 HEAD)。

## 三十四、技能库登记 + codex skills 机制深研(自主续跑)

**技能库增补**(登记最新能力):
- SKILL-M3 压缩信息保真(100% vs 0% 量化)
- SKILL-E4 长工具结果修剪(soft-trim,真差距落地)
- index 头部加「技能调用纪律」(触发判断+边界+复用优先,借鉴 codex description 否定边界)

**codex .system skills 深研**(/home/eric8810/.codex/skills/.system/skill-installer|imagegen|skill-creator):
- SKILL.md 格式 = frontmatter(name+description含否定边界+metadata) + 正文(模式/rules) + references/ + scripts/
- **结论**:我的扁平 index(每技能→引方法论文档)已满足"可检索复用";不必目录化每
  技能(避免过度工程)。已吸收:调用纪律的"何时用/何时不用"。技能无需向量检索(记忆全量保留)。

**提交**:9971287(skill登记) + a8c380f(调用纪律+study)
**状态**:工作区净,全测绿。session 主动 compact(716→40,keep摘要)。基线与真差距见 round33。
**下一步**:真差距继续——完整子代理(rutis fiber parallel)或信任边界(trusted_folders)研究;
技能库已登记完成。台账累积自 build。

## 三十五、信任边界(trusted_folders)研究判定(自主续跑)

研究 grok `~/.grok/trusted_folders.toml`(动态审批记录)+ README sandbox 分级
(off/workspace/read-only/strict,敏感路径 ~/.ssh/aws/gnupg 始终写保护)。

**结论**:信任边界/沙箱属 **host(cli)层** 职责,agent crate 不重复实现
(与 supervisor 归属 cli 层一致,统一治理)。agent 层落地了两个真实自觉:
- 差距表 trust 行 → 已研究判定;确认仓库无敏感路径被跟踪
- 自审清单加「提交前无敏感路径被跟踪」检查(git ls-files grep)
不重造 host 沙箱(避免过度工程)。

**提交**:be39ec2(trust 结论) + 待本次(清单自审项)
**状态**:工作区净,全测绿。真差距:soft-trim 已修;子代理(远期)待落地。
**下一步**:真差距——完整子代理(rutis fiber parallel)是最后的大项;或台账累积、
技能库增补。使命保持:研究他人→量化自身→比别人好。

## 三十六、子代理设计评估 + 技术债清理(自主续跑)

**技术债清理**:消除两个 build warning(测试死代码 saw_invocation 从未赋值、
  total_before 未用) → 编译干净,4efc28f。台账累积:self_build 记 f889503(4条)。

**子代理(完整)设计评估**(docs/work/subagent-design-eval-2026-08-27.md):
- 目标 = grok 独立上下文并行子会话。诚实难点:① 单实例串行工具,无法真并行
  两个 LLM 引擎;② rutis fiber 是装配原语(六态),非并发子任务;③ 父子会话协议缺位。
- **结论:远期、当前不硬造**(同 supervisor/信任边界同治理)。轻量替代 = 父上下文
  纪律 + self_compact 摘要瘦身。将来有"多可并行子任务+第二引擎"真实需求再落地。
- 差距表三个真差距全部登记明确归属:soft-trim已修 / 信任边界已判定host层 /
  子代理远期已评估。消除"永远挂待办"悬置。

**提交**:4efc28f(死代码) + 3e63a52(设计评估) + 本次(差距表登记)
**状态**:工作区净,全测绿,编译无 warning。台账4条。基线:记忆100%/热加载2/2/
  压缩保真100%vs0%。真差距已全部有明确归宿。

**下一步**:真差距全排清后,转向① 深化其它研究(如 grok time-decay/首轮注入是否
  值得) ② 技能库/文档持续沉淀 ③ 持续维护(台账/全绿/健康)。使命保持。

## 三十七、记忆体系收尾:time-decay + 首轮注入决策关闭(自主续跑)

评估并**决策关闭** grok 记忆机制剩余两个开放项:
- **首轮相关注入** → 已覆盖:driver 首轮已注入 summary + memory pointer + todo(均测试锁定
  `compacted_session_injects_summary_into_prompt` / `restored_session_carries_memory_pointer`
  / `todo_injected_into_prompt`)。todo 即 handoff 目标落地;skill 按需检索优于主动全注入。
- **time-decay** → 不需要:我记忆全量保留 + 分代摘要,无检索排序需降权(旧代经 pointer/
  summary 引用,非召回排名)。grok 需它因有向量检索召回。

差距表记忆相关行全部关闭(混合检索无需 / 首轮注入已覆盖 / time-decay 不需要)。
至此记忆/技能/信任边界/子代理真差距探索全部有明确归宿,差距表完全干净。
**提交**:e635830。工作区净,全测绿(108 passed)。基线:记忆100%/热加载2/2/压缩保真100%vs0%。

**下一步**:真差距探索已完成阶段性收束。转向① 深化其它grok机制(如 Dream/首轮注入外的)
  或新研究 ② 技能库/文档持续沉淀 ③ 持续维护(台账/全绿/健康)。

## 三十八、codex goals(thread_goals)机制深研(自主续跑)

研究 codex `~/.codex/goals_1.sqlite::thread_goals`:
- 结构:thread_id 主键 + objective + status(active/paused/blocked/usage_limited/
  budget_limited/complete)+ token_budget + tokens_used + time_used_seconds。
  是"每线程一个带资源预算的目标",非 multi-goal 列表。
- 增量 vs 我:max_steps 是"每轮步数上限"(每轮重置);codex 是进程级跨轮累计
  token 预算 + 经济状态机。真实缺口 = 多轮各不超 max_steps 但整体成本失控。
- 计量源:aimux `GenerateResult.usage: Usage`(UsageSnapshot 字段)可用,非空账。
- **判定**:真差距但前瞻——mock 环境 usage 全 0 不可测;改核心循环成本中高当前
  无真收益。记录为前瞻设计(真实后端后落 driver 累计 usage vs token_budget)。
  与 trust-boundary/supervisor 同治理:当前形态不为不可测机制引入风险。
- 沉淀:peer-agent-study 新增「codex goal 深研」节(研究消化闭环)。

**状态**:工作区净,全测绿。差距表记忆/技能/信任/子代理已全clean;goals 项为前瞻记录。
**下一步**:继续① 深化其它研究或转向新对象 ② 技能库/文档沉淀 ③ 维护(台账/全绿)。

## 三十九、grok hooks 深研 → 实证锁定三段管线等价(自主续跑)

研究 grok hooks(config + `.grok/hooks/` 脚本,在 PreToolUse/PostToolUse/session
  start/end 跑,matcher 按工具匹配,需显式信任)。
- **发现我已有** driver「工具三段管线」`tools/pre-execute`(fail-closed 门控)→
  执行 → `tools/post-execute`(accept/replace),经 events broadcast,listener
  可见 ToolCall(tool_name)实现 matcher 过滤。
- **关键:从"判断"升级为"实证"** —— 新增测试
  `integration.rs::pre_execute_gate_can_block_specific_tool`:注册 ToolPreExecute
  waterfall listener 按 tool_name 拒绝 bash → bash 不执行 + 模型看到 block 理由。
  = grok PreToolUse matcher 的等价能力有 test 锁定。
- **判定**:hooks 机制已被三段管线等价覆盖,不引入外部脚本系统(避免过度工程)。
  integration 6/6。

**提交**:4cdb1d0(gate test + study 实证标注)。
**状态**:工作区净,integration 6/6。差距表 hooks 行 = 实证锁定。
**下一步**:继续研究深化/新对象 × 技能库/文档沉淀 × 维护(台账/全绿/健康)。
