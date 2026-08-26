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
