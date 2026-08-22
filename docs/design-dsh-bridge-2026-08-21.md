# dsh 桥设计(v3.2)——两缝底座桥(两级:基座桥 × dsh 面)

> 2026-08-21 v1(agent 层)→ v2(内核层,min-cordis 基座)→ v3(方向修订:TS 跑
> 完整 dsh 栈,Rust 只做底座;rutis-agent 独立基线)→ **v3.1:吸收独立评审
> ([review-dsh-bridge-2026-08-21.md](review-dsh-bridge-2026-08-21.md))中
> v3 方向下仍成立的发现**(传输层、版本事实、协议字段预留、活对象替身、
> waterfall 类型学、M0 重排、保真分级、M7 前移)。
> 依据:方向讨论三连、D31、[design-dual-core-2026-08-20.md](design-dual-core-2026-08-20.md)、
> 生态实测数据([plugin-reference](../deepseek-harness/plugin-reference/))与评审复算。
> **2026-08-22 M0 已执行:两问全通(含官方 pwsh loader-composition spec 原样
> 真进程复验),基座裁决 = min-cordis;评审 §B 的"loader 分叉点"担忧不成立。
> 实验记录:[experiment-m0-min-cordis-2026-08-22.md](experiment-m0-min-cordis-2026-08-22.md)。**
> **2026-08-22 v3.2:桥分两级——`rutis-cordis`(基座桥,通用 cordis 面:装载
> 仲裁/服务注册/事件总线四分发/isolate 作用域)与 `rutis-dsh`(dsh 面:llm
> 缝/事件类型映射/替身表/dshSemver/会话字段语义)。裁决依据:M1.5/M7 语料
> 本就是 cordis 插件生态(8889 候选仓、423 仓用 `ctx.loader`),桥必须能装载
> 任意 cordis 插件,而非只讲 dsh 的话;词汇归属见 §三之二。M1 独立评审
> ([review-dsh-m1-2026-08-22.md](review-dsh-m1-2026-08-22.md))的 Z1/F1/F2/F5
> 在两级结构中按层归位。**
> **2026-08-22 M1 已完成签字(两级结构,测试 17+4 全绿);M1.5 最小语料
> 矩阵已执行:L0 = 4/10,官方闭包 236 包零缺口(证实 §十.11),缺口全在
> 社区 workspace 内部包与第三方;语料需 shape 过滤(fork/VSCode 形态)。
> 实验记录:[experiment-m1p5-corpus-2026-08-22.md](experiment-m1p5-corpus-2026-08-22.md)。**

## 一、目标、层次纪律与口径

**目标(两张脸,v3 口径)**:

1. **底座方向(v1 全部内容)**:TS 侧是一个**完整的 dsh 部署**——真 agent loop、
   真服务、真插件;Rust 侧提供**模型调用(aimux)、事件可观察性、逐块锈化的
   服务**。桥是底座,不是宿主。
2. **编排方向(推迟,有再评估触发条件)**:TS 程序把 rutis 当引擎调用——推迟到
   dsh loop 语义收敛、rutis-agent 从基线长成引擎之后。**评审分歧记录**:独立
   评审 §六主张编排方向提前(未知有界、破坏面为零、前置依赖与第 1 波重合),
   本设计的保守推迟是方向讨论的裁决,不是否定评审论据;再评估触发条件 =
   底座方向 M2 通过后第一次路线评审。

**层次纪律(写死;v3.2 起三条腿)**:

```
rutis-agent  ──→ rutis                    独立基线,零 dsh 知识,独立测试发布
rutis-cordis ──→ rutis                    基座桥:cordis 词汇(装载仲裁/服务/
                                          事件总线四分发/scopeId),零 dsh 知识
rutis-dsh    ──→ rutis-cordis + aimux     dsh 关系的唯一所在(llm 缝/事件映射/
                                          替身表/dshSemver/sessionId/turnId 语义)
rutis-agent 与 rutis-cordis 互不依赖,是内核上的兄弟;rutis-dsh 骑在基座桥上。
```

dsh 的一切(dsh 协议事件名、dshSemver、会话字段语义)封在 `rutis-dsh`;
cordis 的一切(装载、服务名、事件分发语义、isolate)封在 `rutis-cordis`——
任意 cordis TS 插件(语料库、社区插件)只经基座桥即可装载观察,不要求是 dsh
部署。基线的 169 个测试就是它自洽的证明——桥重构、桥出 bug、方向推翻,
基线不动。

**覆盖口径:保真分级(v3.1 替换二元口径)**:

v2 的"94% 可装载"**作废**——评审复算证实该数字不可复现(同源数据下"服务集
⊆ 桥接集"为 1242 仓 / 18.6%),且静态调用集本就不是门槛(值级导入未覆盖,
见 §十.11)。v3.1 起以**保真分级**度量,每级配语料数字与已知破口清单:

| 级 | 含义 | 断言 |
|---|---|---|
| L0 | 装得上 | 宿主装载成功,无异常 |
| L1 | 注册生效 | 工具 schema / prompt 段进入 TS 侧真 loop 的装配结果 |
| L2 | 事件到达 | 订阅事件按发射序到达 Rust 观察者 |
| L3 | 模型面保真 | llm 缝下完整 turn 行为与原生 adapter 无差异(chunk 粒度 / finish / usage) |
| L4 | 事件载荷保真 | 活对象载荷(agent/signal/actor)的替身合成正确(§四.3) |
| L5 | 锈化服务保真 | sandbox/fs 替换后行为等价(逐块,届期定义) |

**非目标**(v3.1 更新):~~TS 工具装进 Rust driver~~(适配层删除);嵌 JS
引擎;同步 API 过桥;Electron 插件与 84 个发行版;waterfall 过桥(v1 不启用,
锈化波启用,§五)。

## 二、总体结构

```
┌─ rutis 进程 ──────────────┐        ┌─ Node 宿主进程(完整 dsh 部署)────────┐
│ rutis 内核                 │        │ min-cordis 基座(首选)↳ 回退 vendor/cordis│
│ aimux(模型接入)           │◄─ fd3 ─►│ dsh agent-loop + 服务包 + TS 插件      │
│ rutis-cordis(基座桥)      │ 或sock  │ 桥端 TS 侧同样两级:基座桥端(装载/    │
│ rutis-dsh(dsh 面)         │        │ evt 通用转发)+ dsh 桥端(llm adapter) │
│ (观察者:前端/遥测/语料库)  │        └───────────────────────────────────────┘
└───────────────────────────┘       纯 cordis 插件(M1.5/M7 语料)只接基座桥
   rutis-agent(独立基线,不连桥,不在本图数据流中)
```

- **传输(评审 §四.1 采纳)**:**专用通道传帧——fd 3 或 unix socket**;
  stdout / stderr 全部留给插件日志。换行 JSON 走 stdout 的方案作废:任何
  TS 插件一句 `console.log` 都是帧损坏,按规则 2 触发 restart 且会循环。
  (8889 个候选仓大量是批量生成物,不能赌它们不打印。)
- **基座裁决(M0,评审 §五重排后两问)**:① `tool-bash` 在 min-cordis 上装载
  并打印 schema;② 其 `loader-composition.spec.ts` 通过。min-cordis 的真实
  分叉点是 **loader / include / group**(明确删了 ~1400 行;dsh 官方有 16 个
  loader 组合 spec 是包级契约,社区 423 仓用 `ctx.loader`),不是 Proxy(dsh
  包内 Context 引用 1359 处,min-cordis 的 reflect 面相当),配置校验已销号
  (schemastery 实现了 `~standard`,两侧校验路径逐字相同)。**回退代价比
  v2 预估更轻**:dsh 仓内 vendor 了 cordis(workspace 依赖,版本天然锁死),
  回退不存在"追版本"。
- **生命周期配对**:宿主进程归桥插件 fiber;TS 侧插件生死由 TS 自治,Rust 只
  观察 evt 流。**硬约束(评审 §三.7 精神,Rust 侧版)**:桥提供的任何服务对
  rutis-agent 是可选依赖(软查询),永不进 driver `injects`——宿主重启不得
  触发 driver 换代(v3 下 driver 根本不接桥,此条作为未来组合时的红线保留)。

## 三、协议(桥协议 v1.1,内核原语 + 预留字段)

三类消息,双向并发,每条请求带关联 id;**全部帧预留 `scopeId` 字段**(v1 不
过滤——rutis 内核 D29 明确事件不按 isolate 过滤,过滤是 dsh 语义,将来由桥端
实现,不动内核)。

```json
{"type":"req","id":1,"method":"...","params":{...}}
{"type":"res","id":1,"ok":true,"result":{...}}
{"type":"res","id":1,"ok":false,"error":{"code":"...","message":"..."}}
{"type":"ntf","method":"...","params":{...}}
```

**连接规则(六条,写死)**:

1. **`hello` 握手 + 能力集协商(两级,v3.2)**:宿主首发 `{protocol: 1, base,
   baseSemver, stack, caps: {services, wfKinds, scopes}}`——**基座级**,由
   `rutis-cordis` 校验(`base` ∈ min-cordis|cordis,装载/事件能力);dsh 部署
   **附加 `dsh: {dshSemver, services}` 节**——由 `rutis-dsh` 校验,纯 cordis
   宿主不带此节。Rust 逐级回对称的能力集。**版本事实(评审 §四.6 修正)**:
   dsh 实测 tag `dsh-v0.1.0-rc.7`,219 包全线同版;`dshSemver` 声明即对齐它。
   能力集求差在**装载期**生效:`plugin/load` 返回 `injects` 与宿主能力集求差,
   差集非空即显式拒绝或降级提示(§十.8 的载体)。版本错配在握手期报错
   (两级任一失败都算握手失败)。
2. **宿主级控制 `host/restart`**:杀进程 + 重拉,覆盖 cancel 到不了的场景。
3. **申报与同名仲裁**:按插件整体覆盖(幂等重载);同名冲突拒绝后者、指名
   已有者。
4. **取消语义完整(评审 §四.5)**:`target` 带类型前缀(`call:` / `inv:` /
   `disp:`)避免命名空间相撞;**取消后迟到的 `res` 按孤儿应答丢弃并计数**;
   每类调用的超时值由**调用方在请求里声明**(默认值桥定,全局上限配置)。
5. **事件分发模式(评审 §三.2 预留)**:`evt/on` 申报带 `mode ∈ emit |
   parallel | serial`。emit 走 ntf;parallel / serial 走请求形状
   (`evt/invoke {dispatchId, event, mode, params}` → `res {bail?}`),Rust 侧
   带超时,超时按失败计数不等待。v1 底座方向只消费 emit;parallel/serial 为
   锈化波(storage 的 flush 类语义)预留——`session/flush`(parallel,语料
   1190 引用)与 `agent/turn-stopping`(serial,273)**不得静默降级为 ntf**。
6. **会话归属(v3.2 归属拆层)**:`sessionId` / `turnId` / `scopeId` 字段 v1
   全部预留、全局广播不过滤,v2 启用——显式声明。线格式三字段集中定义在
   `rutis-cordis` 的帧信封(一处定义,两层共用);**语义归属分层**:`scopeId`
   是 cordis isolate 语义(基座桥),`sessionId`/`turnId` 是 dsh 会话语义
   (dsh 面,基座桥只透传不解释)。

### 三之二、词汇归属表(v3.2 新增)

| 概念 | 层 | 落点 |
|---|---|---|
| 帧信封(req/res/ntf + 关联 id + 三预留字段) | 线格式 | `rutis-cordis`(一处定义) |
| 事件分发 mode(emit/parallel/serial) | cordis | `rutis-cordis`(事件总线四分发语义) |
| waterfall kind(decide/around/stream) | cordis | `rutis-cordis`(同上,§五 类型学) |
| plugin 装载/同名仲裁/幂等重载 | cordis | `rutis-cordis`(loader 语义) |
| `base`(min-cordis/cordis)、loader 能力集 | cordis | `rutis-cordis` hello |
| `scopeId`(isolate) | cordis | `rutis-cordis` |
| `dshSemver`、dsh 服务集(injects 求差) | dsh | `rutis-dsh`(hello 的 `dsh` 节) |
| `sessionId`/`turnId` 语义 | dsh | `rutis-dsh` |
| llm 缝(`svc/define` 的 llm 面 → aimux) | dsh | `rutis-dsh` |
| `agent/*` 等事件类型映射、活对象替身 | dsh | `rutis-dsh` |
| `host/restart` 的宿主重启编排 | dsh | `rutis-dsh`(基座桥提供进程句柄) |

### 方法表(v1.1)

**Rust → TS(宿主)**

| method | params | 结果 | 用途 |
|---|---|---|---|
| `plugin/load` · `plugin/unload` | `{pluginId, entry, config}` | `{ok, injects}` | 受控装载(基座实验期与语料库用) |
| `svc/call` | `{service, method, params, timeoutMs}` | 调用结果 | 调 TS 侧服务 |
| `wf/enter` | `{invocationId, event, value}` | 见 §五 | waterfall 两段式(锈化波启用) |
| `cancel` | `{target: "call:12"\|"inv:3"}` | —(通知) | 取消传播(规则 4) |
| `host/restart` | `{reason}` | —(杀进程 + 重拉) | 规则 2 |

**TS(宿主)→ Rust**

| method | params | 结果 | 用途 |
|---|---|---|---|
| `hello` | 规则 1 | 能力集 | 握手 |
| `svc/define` | `{service, methods}` | `{ok}` | **llm 缝**(TS 声明 llm 面,Rust 实现指向 aimux);锈化波同通道反向 |
| `evt/on` | `{pluginId, events:[{name, mode}]}` | `{ok}` | 事件订阅申报(**事件缝**) |
| `evt/emit` | `{event, params}` | —(通知) | TS 侧事件过线 |
| `evt/invoke` | `{dispatchId, event, mode, params}` | `{bail?}` | parallel/serial 分发(预留) |
| `wf/next` | `{invocationId, value}` | — | 两段式续延点(§五,锈化波启用) |
| `wf/register` | `{pluginId, events:[{name, kind}]}` | `{ok}` | kind ∈ decide\|around\|stream(§五) |

**事件映射的二选一(评审 §四.3 裁决:选类型化)**:每个过桥事件在 `rutis-dsh`
里有一个 Rust 类型(编译期封闭集合,与版本声明制、集中适配一致;细粒度顺序,
不塌成单链)。新 dsh 事件 = 发一版 Rust,这是版本声明制本来的含义。**放弃**
单一擦除信封(会把全部过桥事件塌进同一条 D31 尾链,一个慢观察者堵住全部,
且 rutis 原生监听方拿不到类型)。

## 三点五、语义差清单(桥切断的东西,逐条配纪律)

| # | 被切断的语义 | 差异 | 实现纪律 |
|---|---|---|---|
| 1 | **emit 同步性** | TS 侧 emit 同步内联;过线后单向异步 | Rust 观察方不假设"emit 返回即已处理";TS 插件无感知(其世界内仍同步) |
| 2 | **事件顺序** | 同类型:发射序 = 到达序(D31 尾链 + 单连接 FIFO);跨类型不保序 | 沿用 D31 边界;类型化映射使"同类型"按 Rust 事件类型计 |
| 3 | **取消的强度** | `cancel` → AbortSignal 协作式 | 超时即按失败处理(超时值规则 4);持续不响应由 host/restart 兜底 |

## 四、两缝(v1 全部)与锈化缝(逐块增)

1. **llm 缝**:桥在 TS 侧以 **dsh llm adapter** 形式注册(生态自有机制,不劫持
   ctx.llm):`registerAdapter("aimux", …)`,stream 过线 → aimux,chunk 以 evt
   流回传,有界队列背压。dsh 自有 adapter 生态保留共存,aimux 以 adapter 身份
   进入,用户可选。**保真焦点(评审 §三.3b 关联)**:`llm/stream` 本身是 dsh
   的**流型 waterfall**(值是 AsyncIterable)——本缝实现于 adapter 层而非
   wf 层,绕开流型协议问题,但 chunk 粒度 / finish / usage 的行为差异是 L3
   级保真的断言来源(§九 M2)。
2. **事件缝**:`evt/on` 申报 → `evt/emit` 流出 → Rust 观察者(前端 / 遥测 /
   语料库)。**丢弃策略按事件类别分(评审 §四.2)**:离散事件(工具边界、
   状态变化)drop + 计数;**有序增量流(text-delta 类)禁止 drop**——coalesce
   合并相邻增量,或断连重启;对有序流 drop = 静默损坏重建文本。
3. **载荷替身表(评审 §三.5 采纳,L4 级保真的定义)**:过桥事件的载荷含活对象
   (`agent: Agent`、`signal: AbortSignal`、`exec / token`、`session: Session`、
   fs intent 的 `actor`),逐个给出:**过线表示(编号 / 快照)、替身在哪侧合成、
   哪些成员不可用**。示例:`actor` 过线为 `{agentId, sessionId}` 快照——官方
   fs-observation-policy 会对 actor **向下导航**(`actor?.agent?.session`)拿
   session 对象当账本键,替身必须合成同键语义,否则读前写门禁静默失效。此表
   随事件类型逐条维护,是"行为保真长尾"的可执行版本与 M7 断言来源。
4. **锈化缝(第 2 波起)**:sandbox / fs / jobs 实现为 Rust 服务,`svc/define`
   反向暴露替换 TS 同名服务;带门控者启用 waterfall(§五)。

**归属裁决(v3.1)**:agentLoop 用户(169 仓)在真 loop 内运行,不扣除;
llm.registerAdapter 生态保留、aimux 共存;jobs / subprocess / webServer 留 TS。
**双管线问题(评审 §三.6)在 v3 下不存在**——Rust 侧不调 `tools.execute`,
TS loop 自跑完整管线;编排方向若启用,必须先裁决管线权威(评审推荐 Rust
权威 + TS 暴露 dispatch-only 入口,记入 §六 届期前置)。

## 五、waterfall 跨界(协议 v1.1 定形,锈化波启用)

**根限制(v3.1 措辞修正,评审 §三.3)**:不是"next 不过线",是**"函数不过线,
续延点过线"**。

**kind 三型(`wf/register` 申报,v1.1 帧字段即刻预留)**:

- **decide**(决策型):一次 `wf/enter` → TS 整链跑完 → `{pass|veto|error}`。
  适用 `tools/pre-execute` 等门控点。
- **around**(环绕型,评审 §三.3a):中间件在 `await next()` **之后**还要处理
  下游真结果(`tools/execute`、`system-prompt/assemble`、`agent/request-error`、
  `approval/request` 四个核心扩展点是此形)。机制:**两段式续延**——Rust 发
  `wf/enter`;TS 链跑到最内层 next 时反向发 `wf/next`;Rust 跑完剩余链与
  Terminal,回真结果;TS 解栈应答 `wf/enter`。每条侧链多一个来回(非每中间件
  一个);要求双向并发重入——M1 已列重入测试,增量近零。**不这么做,环绕型
  中间件的下半段将在桩值上运行且不报错**(行为保真最硬的坑)。
- **stream**(流型):值是 AsyncIterable,`{action, value}` 表达不了。**装载期
  显式拒绝**(申报即报错),不装上去之后静默失效。

**启用时机**:v1 底座方向不启用(TS 中间件挂真 loop,在家跑);首个带门控的
锈化服务(sandbox 波)启用 decide;around 随编排面 / approval(v2)。JSON 形态
纪律(纯数据 + aimux serde/ts_rs 同源生成)沿用。

**交错边界的证据修正(评审 §三.3c)**:v2 所引"159 仓无一需要交错"的出处
**不成立**(该文件只有调用次数统计,无用法形状分析,且榜首含 AI 样板与嵌入
官方核心的发行版)。边界本身仍然成立——两段式续延下侧链位置仍以注册序整块
插入——但它的依据是 dsh 事件签名与协议结构,不是仓数统计。此条作为教训
记录:引用必须支撑被引用的结论。

## 六、编排方向(推迟,条件、锚点与届期前置)

推迟条件:dsh loop 语义收敛 + rutis-agent 长成引擎。届期实现
`sessions`(create/get/list/fork)、`subagents`(**startContinuable 第一公民**,
12256 次主流调用)、`llm/stream`、`tools/invoke`,全部为 svc/evt 组合。
**届期前置(评审 §三.4/§三.6)**:作用域过滤(`scopeId` 已预留,过滤在桥端
实现,不动内核 D29——没有它,挂在子代理上的插件会收到全树事件,是行为错误
不是少个过滤器);工具管线权威裁决。**再评估触发**:底座 M2 通过后的第一次
路线评审(评审 §六主张提前,理由:未知有界、破坏面零、与第 1 波前置重合)。

## 七、性能预算(实测口径)

| 项 | 量级 | 对照 |
|---|---|---|
| 单次往返(小 JSON) | 0.1–1ms | token 间隔 20–100ms |
| llm 缝 chunk 流 | 每边 10–100µs | 低 token 间隔 2 个数量级 |
| Node 常驻 | 30–80MB + 启动 50–150ms | 一次性 |
| 大载荷(1MB) | 1–10ms | — |

**背压**:每订阅者有界队列;**丢弃策略分类**(§四.2:离散 drop+计数 / 有序
流 coalesce 或断连 / 阻塞不采用)。

## 八、实现结构与技术量级

| 件 | 位置 | 依赖 | 量级 |
|---|---|---|---|
| 协议层(双向并发 RPC + 流 + fd3/socket) | `crates/rutis-cordis/src/rpc.rs` | 仅 rutis | 数百行 |
| 基座桥词汇(装载仲裁/事件 mode/wf kind/hello 基座校验/scopeId) | `crates/rutis-cordis` | **仅 rutis,零 dsh 知识** | ~0.5k 行含测试 |
| dsh 面(llm 缝/事件类型映射/替身表/dshSemver/engine 预留) | `crates/rutis-dsh` | **rutis-cordis + aimux** | ~1k 行含测试 |
| TS 宿主(基座 + 全栈组合 + aimux adapter + evt 转发) | `host/`(npm,本仓库内) | min-cordis(首选)/ vendor cordis(回退) | ~600–1000 行 |
| ~~薄适配层 `rutis-dsh-agent`~~ | — | — | **v3 删除** |

依赖顺序:**M0 基座实验 → 桥 v1(两缝)→ 最小语料矩阵(§九)→ sandbox/fs
锈化(首个 svc/wf 缝)→ … → loop 锈化(rutis-agent 成长)→ 编排面**。

## 九、验收(逐层,每层独立可验证)

| 层 | 内容 | 验收标准 |
|---|---|---|
| **M0 基座实验(两问)** | ① tool-bash 装到 min-cordis 打印 schema;② 其 loader-composition.spec 通过 | 两问全通 → min-cordis;任一不通 → vendor cordis(代价近零) |
| M1 协议 | 内存 wire 下内核原语全套:往返、并发、取消(含迟到 res 丢弃)、超时、握手错配与能力集求差、同名仲裁、**重入**、**杀宿主进程 → Rust 侧仅失 llm 缝,观察连续性记录在案** | `cargo test -p rutis-dsh`,零 Node;前置:§十.2 复核 |
| **M1.5 最小语料矩阵(评审 §七.2 前移)** | 10 仓最小语料(含静态值级导入重的样本)跑 L0/L1 | 暴露宿主 npm 闭包问题——会改变宿主形态的发现,放在最便宜的位置 |
| **M2 全栈 turn(核心)** | 完整 dsh 栈 + 真插件,llm 缝走 aimux,完整 turn,事件流回 Rust;**注入 `console.log` 的测试插件 → 帧流不损坏**(fd3 验证) | 集成测试(需 Node);插件社区测试套件(若有)桥下通过;L3 级保真断言(chunk/finish/usage) |
| M3 事件链路 | 到达序 = 发射序;**有序流 coalesce 正确性**(合并后重建文本无损) | order_probe 模式多线程回归 |
| M4 waterfall | 推迟至 sandbox 波:decide 三例 + **around 一例(TS `await next()` 必须拿到 Rust Terminal 真结果)** | 届时定义 |
| M5 编排面 | 推迟(§六) | 届时定义 |
| M6 组合 | 语料库抽样集全栈同跑 | 通过率 ≥ 阈值(首版定后写入) |
| M7 语料库(**持续设施,头号指标**) | 按服务分布分层抽 20–50 真实插件,完整栈 + 两缝下跑**保真分级矩阵**(L0–L4 逐级计数 + 已知破口清单),周更 | "可正确运行"的唯一收敛机制 |

## 十、限制与风险(诚实清单)

1. 函数不过线,续延点过线(§五)→ 交错粒度 = 侧链整块;载荷纯数据 + 替身表纪律。
2. 同步 API 不过桥。复核方法(M1 前置):raw/ctx-repo-operations.tsv 过滤
   过桥服务同步成员调用方,确认交集为空。
3. 版本漂移:对齐 `dsh-v0.1.0-rc.7`(实测 tag);集中、离散适配。
4. 桥不可用时 TS 栈退回自带 adapter(不崩);Rust 基线完全不受影响。
5. 信任模型:TS 插件 = bash 级信任;sandbox 锈化后收紧。
6. 重入(llm 缝执行中 evt 回流)由双向并发 + 关联 id 覆盖;M1 显式测。
7. **llm adapter 保真(L3)**:aimux 与 dsh 原生 adapter 的流式语义差异——M2
   社区套件直接针对它。
8. **活对象替身(L4)**:agent/signal/exec/session/actor 的替身合成错误 =
   静默行为损坏(fs-observation-policy 的 actor 导航是已点名实例);替身表
   逐事件维护,M7 断言。
9. **min-cordis 组合风险(loader 为真分叉)**:loader/include/group 已删
  (~1400 行),dsh 有包级 loader 组合契约;M0 第二问直击。回退代价近零
   (vendor cordis,仓内锁定)。
10. **rc.7 ↔ 4.0.1 漂移**:min-cordis 派生自 cordis 4.0.0-rc.7,dsh vendor
    4.0.1(fiber.ts 888 行 vs 754 行)——已是两条分支;M0 结果落此处记账。
11. **宿主 npm 闭包 ≈ 完整 dsh**(评审 §三.1c):静态调用集不是门槛,值级
    导入(peerDeps + import 值)才是——tool-bash 一个插件就引 12 个
    @deepseek-ai/dsh-* 包。"薄基座 + 少量服务"的图景不成立,宿主就是完整
    dsh 组合;v3 方向(TS 跑全栈)已按此设计,M1.5 语料验证它。
12. **dsh 核心内部用法未扫描**:锈化波替换 TS 服务前,该服务在 dsh 核心内部
    的调用方清单是前置数据。
