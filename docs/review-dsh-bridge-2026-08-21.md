# 评审:dsh 桥的路线问题

> 2026-08-21。对象:[design-dsh-bridge-2026-08-21.md](design-dsh-bridge-2026-08-21.md)(v2)
> 与 [design-dual-core-2026-08-20.md](design-dual-core-2026-08-20.md)(含 08-21 方向修订)。
> 依据:rutis 全部源码;dsh 实测源码(`../deepseek-harness`,tag `dsh-v0.1.0-rc.7`)。

08-21 的方向修订(TS 跑完整 dsh 栈,Rust 只做底座,桥缩到 llm 缝 + 事件缝)是对的。
但修订之后,v2 桥设计的大部分内容失效了,而**新方向自己的几个前提还没被检验**。
下面六条,前三条是"新方向下实际要做的事比文档写的小得多",后三条是"锈化路线
本身有没有收益"。

---

## 一、新方向下,Rust 在一次 turn 里只被调用一次

先把执行路径写出来。TS 侧跑完整 dsh 栈,一次 step 的顺序是:

```
agent/pre-step(TS 内)→ agent/request(TS 内)→ llm/stream waterfall(TS 内)
   → 最内层:调 Rust 的 aimux ← 唯一一次跨进程
→ 解析 chunk,得到 tool calls(TS 内)
→ tools/pre-execute → 工具本体 → tools/post-execute(全在 TS 内)
```

工具本体里如果用到将来锈化的 fs/sandbox,才会再有跨进程调用。也就是说:

**Rust 在新方向里的角色是 TS 的 RPC 服务端,被调用的次数是每 step 一次(llm)
加上每次工具执行零到几次。除此之外 Rust 不主动做任何事。**

这不是贬低,这是把工作量说清楚。v2 那套协议(hello / plugin/load / svc/define /
svc/resolve / evt/on / wf/register / wf/invoke / cancel / host/restart,13 个方法)
是为"两个插件框架互相装载对方的插件"设计的。新方向不需要装载对方的插件。

## 二、llm 缝的规格 dsh 已经写好了,不需要重新设计

dsh 的 `llm` 服务有 `registerAdapter(providers: string[], adapter: LlmAdapter)`。
`LlmAdapter` 是一个抽象类,只有一个抽象方法:

```ts
abstract class LlmAdapter {
  providerInfo(provider: string): LlmProviderInfo
  providerRetryPolicy(provider: string): ResolvedRetryPolicy | undefined
  listModels(provider: string): Promise<readonly LlmModelInfo[]>
  resolveModel(provider: string, model: string, signal?): Promise<LlmResolvedModelInfo>
  abstract stream(options: GenerateOptions): AsyncIterable<StreamChunk>   // ← 只有这个是抽象的
}
```

两个类型都是纯数据:

- `GenerateOptions` = `{provider, model, messages, system?, tools?, temperature?, maxTokens?, stop?, signal?, sessionId?, purpose?}`,除 `signal` 外全部可 JSON 化;
- `StreamChunk` = 7 个变体的联合(`block-start` / `text-delta` / `reasoning-delta` / `tool-call-delta` / `block-end` / `usage` / `finish`),全部可 JSON 化。

**所以整条 llm 缝的协议是:一个请求(GenerateOptions 去掉 signal)、一条 chunk 流、
一个取消帧,加四个元数据方法。** 没有活对象、没有作用域、没有 waterfall、没有
双向装载。这是整个设计里唯一一条天然干净的缝,而且它的接口定义在 dsh 里现成。

两个必须写死的位置选择:

1. **注册成 adapter,不要替换 `ctx.llm` 服务。** dsh 的 `llm/stream` 是 waterfall,
   retry / 缓存 / 计量 / 日志都包在外面(官方 `llm-retry` 就是这样),社区
   `registerAdapter` 用了 5421 次。注册成 adapter,这些照常工作,Rust 只拿最内层
   的"发请求、收 chunk"。替换服务的话这些全废。
2. **v2 §四"引擎 provider 面由 aimux 独占,dsh 的模型适配生态不继承"要作废。**
   在新方向下 aimux 是众多 adapter 之一,不是独占者。

## 三、rutis 内核在这条路线上用不到

新方向下 Rust 侧要做三件事:接 llm 请求、观察 TS 事件、以后放锈化的服务。逐个看
需不需要 rutis 内核(fiber 六态 / 依赖门控 / 级联卸载 / 依赖驱动重载 / 服务注册表):

| Rust 侧的东西 | 需要内核吗 | 理由 |
|---|---|---|
| aimux 的 RPC 服务端 | 否 | tokio + serde 即可 |
| sandbox 的 `confine` | 否 | 一个纯函数 |
| fs 操作 | 否 | 函数调用 |
| 事件观察 | 看消费者 | 若 Rust 侧只有一个消费者,`tokio::sync::broadcast` 就够 |

内核的五支柱解决的是"进程内有很多互相依赖、会热插拔的插件"。新方向明确把插件
生态留在 TS,Rust 侧没有插件。**所以 rutis 这个 crate 在新路线的生产路径上不被使用。**

这不是说它没价值——它是 cordis 语义的 Rust 实现,有 115 项契约与 4677 行对拍测试,
可以独立开源,也可能在 loop 真锈化时用上。但它意味着:**v2 桥设计之所以那么复杂,
是因为它假设桥要连接两个内核;新方向下这个假设不成立,桥的设计不该再围绕内核展开。**

如果确实想让 Rust 侧也有装配能力(比如以后锈化了五六个服务需要按配置组合),
现在的做法应该是一个几十行的 `struct App { llm, sandbox, fs }`,不是 fiber 状态机。

## 四、sandbox 不是好的第一块锈化,它的收益接近零

双核文档说"sandbox 这类信任边界是论据最硬的候选",第一块锈化定的是 sandbox/fs。
查 dsh 的实际实现:

`sandbox` 服务**只有一个方法**:

```ts
abstract confine(argv: readonly string[], policy: SandboxPolicy): ConfinedArgv
```

它做的事是把一条命令行改写成受限的命令行。真正的隔离由操作系统做——
`sandbox-local` 走的是 Linux 的 `bwrap` 和 landlock(经 `@deepseek-ai/node-addon-landlock-run`
这个 native addon)、macOS 的 seatbelt。**信任边界的执行方已经不是 TS 了。**

把 `confine` 从 TS 搬到 Rust,搬的是"拼 argv 的纯函数"。隔离强度不变,性能不变。

真正有决策权的是 `sandboxPolicy`:

```ts
readonly defaultMode: SandboxMode
resolve(request: SandboxPolicyRequest): SandboxExecutionPolicy
overrideOf(session: Session): SandboxMode | undefined     // ← 需要 Session
```

而 `overrideOf` 要一个 `Session` 对象。session 的事实源在 TS(见 §六)。**所以
sandbox 里有收益的那部分搬不动,搬得动的那部分没收益。**

按同样的方法查一遍,`fs` 更不该是第一块:318 个仓库使用,带两个 waterfall
(`fs/write-intent` / `fs/edit-intent`),它的 `actor` 参数是活对象——官方
`fs-observation-policy` 的实现是 `(actor as FsObservationActor)?.agent?.session`,
一路导航到 session 对象并拿它当读前写账本的键。

**结论:锈化清单需要重排,而重排的依据不能是"离 agent 核心回路多近"(那是耦合度,
不是收益),要按"CPU 占用"和"是否需要脱离 Node 运行"来排。** 按这个标准,真正的
候选是 token 计数、diff、代码搜索这类 CPU 密集的东西,不是 sandbox 和 fs。

## 五、逐块锈化在做完最后一块之前,不产生分发收益

这是路线层面最要紧的一条。

Rust 化的理由,双核文档列了五条判据:语义收敛、机制而非措辞、性能或分发敏感、
不绑 npm 生态、信任边界。实际算一下每条在当前候选上成不成立:

- **性能**:agent 的时间几乎全花在等模型上(token 间隔 20–100ms)。把 fs 或
  sandbox 搬到 Rust 省下的是微秒级。**不成立**,除非搬的是 CPU 密集的东西。
- **信任边界**:sandbox 的执行已经在 OS/native 层。**不成立**(§四)。
- **分发**:这是最主要的理由,但它有一个性质——**只要还有任何一块在 TS,就还需要
  装 Node。** 所以逐块锈化的中间态,每一步都增加了一个跨进程调用的成本,却一步
  分发收益也拿不到,直到最后一块搬完为止。而"双核永久"这个决策明确说最后一块
  永远不会搬完。
- **语义收敛**:决策 4 自己说了 loop 还没收敛,不复刻。

所以现在的路线是:**为了一个按定义永远不会到达的终点,持续付出中间态的成本。**

这不代表路线错,而是说**它需要一个别的理由**。可能的理由有两个,要选一个:

1. **目标是"更好的 dsh"**:那 Rust 只做 TS 明显做不好的事(CPU 密集、需要系统级
   能力),不追求覆盖面,rutis 内核和 rutis-agent 都不在路线上,项目的主要产出是
   一个 dsh 插件(TypeScript)加一个很薄的 Rust 二进制。
2. **目标是"一个不装 Node 就能用的 agent"**:那不能逐块搬,要一次性确定最小可用集
   (loop + 几个工具 + llm,不要插件生态)并做完——**这正是 `rutis-agent` 已经在做的
   事**,它不该被降级成"对照组"。

这两个目标要求的路线相反,不能写在同一张路线图上。目前双核文档同时保留了它们
(决策 1 和决策 3),所以每个决定都在两个方向之间摇摆。

## 六、状态归属要先于缝的划分定下来

不管选哪个目标,有一件事必须先定:**session 归谁。**

dsh 的 session 是持久事实源,有日志、投影、fork、持久化(`sessionPersistence`
240 仓、`sessionProjections` 196 仓、`sessionQuery` 190 仓)。rutis 的
`Session` 是一个内存 `Vec<ModelMessage>`,而且换代就丢——这有测试钉死:
`crates/rutis-agent/tests/integration.rs:214` `assert_ne!(agent2.id(), session1); // 重载即新 session`。

新方向下 session 显然归 TS。这条一旦确定,推论是硬的:

- **Rust 侧的任何服务都不能持有跨 turn 的状态**,除非它能按 session 身份索引;
- 要能按 session 索引,Rust 就必须知道 session 的创建和销毁——**这才是"事件缝"
  的真正用途**。
- 但反过来,今天还没有任何 Rust 侧的东西需要按 session 索引。所以**事件缝现在
  没有消费者**。为一个还不存在的消费者先建基础设施,是 v1 不该做的事。

**建议:v1 只做 llm 缝。事件缝等到第一个需要 session 身份的 Rust 服务出现时再做,
那时它的需求也就清楚了(要哪些事件、要不要保序、丢了要不要紧)。**

---

## 七、可以直接删掉的

- **M0(tool-bash 装到 min-cordis)**。新方向说 TS 侧跑完整 dsh 栈,那就用 dsh
  仓内 vendor 的 `@deepseek-ai/cordis` 4.0.1,**没有基座选择问题**,这个实验的
  结论不影响任何决定。(顺带:v2 §十.10 把它列为最大风险之一,也随之消失。)
- **v2 的覆盖率承诺(94% 可装载)**。它量的是"TS 插件能否装进 rutis",新方向下
  插件根本不装进 rutis。相关的口径问题见附录 A.1,只在方向回摆时才需要。
- **v2 的协议方法表**。13 个方法里,新方向只需要其中的流式调用和取消。

## 八、建议的下一步

1. 在双核文档里**选定 §五 的两个目标之一**,另一个明确标为"暂不追求"。这一条
   不做,后面所有决定都会继续摇摆。
2. 把桥设计 v3 改写成 **`aimux-llm-adapter` 的设计**:一个 dsh 插件(TS)注册
   `LlmAdapter`,加一个 Rust 二进制。协议就是 §二 说的三件事。篇幅应该比 v2 短
   一个数量级。
3. 重排锈化清单:按 CPU 占用和"是否需要脱离 Node"排,而不是按离核心回路的远近。
   先量一次真实 profile,再决定第一块。
4. 传输不要用 stdout。TS 侧任何一个 `console.log` 都会破坏帧。用 fd 3 或
   unix socket,stdout/stderr 留给日志。这条与方向无关,现在写进去就行。

---

## 附录:v2(插件方向)的细节问题

以下全部只在"TS 插件装进 rutis 宿主"这个方向下成立。新方向不需要它们,
保留是为了方向回摆时不用重查。

### A. 八个结构性问题(v2 插件方向)

#### A.1 覆盖口径:定义与实际门槛错位,数字不可复现

文档 §一定义:"**可装载**:静态调用集 ⊆ **桥接服务集** —— 当前实测 94%"。

两处问题:

**(a) 门槛写错了。** 决定一个 TS 插件能否装载的是**宿主组合出的服务集**(TS 侧),不是桥接服务集。一个 `ctx.slots.register` 的插件不需要 `slots` 过桥,只需要宿主里有 `slots`。所以这个 94% 要么在量"宿主装了全部 dsh 服务包"(那它恒等于 100%,不含信息),要么在量别的东西。两种读法下这个数字都不能支撑"v1 交付承诺"。

**(b) 数字复现不出来。** 用文档引用的同一份原始数据 `raw/ctx-repo-operations.tsv`(50304 行)复算:

| 口径 | 复算值 |
|---|---|
| 有 ctx 操作的仓库总数 | 6673 |
| 成员集 ⊆ {`tools.register`, `systemPrompt.section`} | 872 |
| 服务集 ⊆ {tools, systemPrompt} | 1016 |
| 服务集 ⊆ {tools, systemPrompt, events, sessions, subagents, llm} | 1242(**18.6%**) |

文档的 1374 / 2707 / 29% / 94% 都落不到这几个数上。可能来自另一轮扫描,但**当前 repo 里没有能推出 94% 的数据**。

**(c) 更要紧的是:静态调用集根本不是全部门槛。** `tool-bash` 这个 M0 选定的样本,`peerDependencies` 列了 12 个 `@deepseek-ai/dsh-*` 包,源码里 `import { defineTool, TOOL_ABORTED } from '@deepseek-ai/dsh-tools'`、`HarnessError` from dsh-llm、`approveEscalation/canonicalPath/validateEscalationArgs` from dsh-sandbox、`DSH_ENV_PREFIX` from dsh-shell——全是**值级静态导入**。ctx 调用扫描不覆盖这一层。"不改一行装进宿主"的真实前提是:**宿主的 npm 闭包 ≈ 一套完整 dsh**。这不影响架构可行性,但它把"宿主是薄基座 + ~10 个服务"的图景推翻了,而覆盖口径应该反映它。

**处方**:废掉二元的"可装载",换成 §六.2 的保真分级;每一级给出可复算的语料数与已知破口清单。

---

#### A.2 事件面:五种分发只桥了两种,两个高频事件会静默丢语义

dsh 的 56 个事件(`api-catalog.ts` 生成物)分布:**41 emit / 13 waterfall / 1 serial / 1 parallel**。

| 事件 | 模式 | 语料引用数 | 现协议下的结果 |
|---|---|---|---|
| `session/flush` | **parallel**(等所有监听器) | 1190 | `evt/dispatch` 是 `ntf`,不等待 → **flush 未完成就返回,可丢数据** |
| `agent/turn-stopping` | **serial**(顺序等待、可短路) | 273 | 同上 → 停机钩子不被等待 |

文档 §三点五 #1 的纪律("需要确认语义的场景改走请求而非事件")把责任推给了插件作者,但插件作者写的是 `ctx.on('session/flush', ...)`——**是宿主选错了传输形状,不是插件用错了 API**。

**处方**:`evt/on` 申报里带 `mode`;emit 走 `ntf`,parallel/serial 走 `req`(`evt/invoke {dispatchId, event, mode, params}` → `res {bail?}`),Rust 侧带超时,超时按 §三点五 #3 的既有纪律处理(失败计数,不等待)。协议增量:一个字段 + 一个方法。

---

#### A.3 waterfall 面:只支持"决策型",表达不了"环绕型"与"流型"

文档 §五的模型是:一次 `wf/invoke` 把值送过去,TS 侧整条链跑完,回 `pass/veto/error`,Rust 再决定调不调自己的 next。

这对**决策型**中间件成立(`tools/pre-execute`、`tools/post-execute`、`agent/pre-step`、`fs/write-intent`、`fs/edit-intent`)。对另外两类不成立:

**(a) 环绕型**——中间件在 `await next()` **之后**还要处理下游结果:

```
'tools/execute'(exec, next: () => Promise<ToolExecutionResult>): Promise<ToolExecutionResult>
'system-prompt/assemble'(assembly, context, next: () => Promise<PromptAssembly>): Promise<PromptAssembly>
'agent/request-error'(payload, next: () => Promise<RequestErrorAction>): Promise<RequestErrorAction>
'approval/request'(req, next: () => Promise<ApprovalOutcome>): Promise<ApprovalOutcome>
```

在现协议下,TS 侧的 `next()` 只能返回一个**桩值**——因为真正的下游(Rust 链尾 + Terminal)要等 `wf/invoke` 返回之后才跑。于是中间件 `await next()` 之后的那半段代码**在假数据上运行,且不报错**。这正是 §十.7"协议对得上不等于行为对得上"的最硬实例,而它不是长尾,是四个核心扩展点。

**(b) 流型**——`llm/stream(options, next: () => AsyncIterable<StreamChunk>): AsyncIterable<StreamChunk>`。waterfall 的值是一个异步迭代器,`{action, value}` 这个 JSON 形状表达不了。官方 `llm-retry` 就是这个形状,语料里 `llm/stream` 被引用 462 次。

**(c) 文档为这个简化给出的唯一依据不成立。** 原话:"159 个 waterfall 仓无一需要交错排列。出处:community-code-analysis.md §二(ctx 耦合扫描)"。查该文件 §二:它只有一句"159 个仓库使用 `ctx.waterfall`"加一张调用次数表,**没有任何关于用法形状的分析**。而且同一节自己注明榜首的 `mstar-*`(263–327 次调用)"疑似 AI 生成样板"、`deepseek-harness-desktop/cli/gui` 系列是"嵌入官方源码的发行版"。这条引用支撑不了它被用来支撑的结论。

**处方**:`wf/register` 申报 `kind ∈ decide | around | stream`。
- `decide`:保持现协议不变。
- `around`:**两段式续延**。Rust 发 `wf/enter {invocationId, event, value}`;TS 链跑到最内层 `next()` 时,反向发 `wf/next {invocationId, value}` 给 Rust;Rust 跑完自己剩下的链和 Terminal,回真结果;TS 链带真结果解栈,最后应答 `wf/enter`。
  代价:每条侧链多一个来回(不是每个中间件一个来回),且要求双向并发重入——**M1 已经要测重入**(§十.6),增量成本接近零。
  这需要把根限制的措辞改掉:不是"next 不过线",是"**函数不过线,续延点过线**"。
- `stream`:v1 明确拒绝——`wf/register` 收到 `llm/stream` 时**在装载期报错**,而不是装上去之后静默失效。

---

#### A.4 isolate / 作用域缺席,而它正是 subagent 的承重墙

文档 §三 规则 4:"所有 `evt/emit` 与请求携带可选 `sessionId`/`turnId`;**v1 语义 = 全局广播,不做过滤**"。

实测:dsh 有一个专门的 `@deepseek-ai/dsh-scope` 包做作用域路由,不是可选装饰:

- 14 个事件以 `agent` 为路由主体、若干以 `session` 为主体(`scoped-events.generated.ts`);
- 作用域**可嵌套**:`scopeParents` 维护父链,"祖先作用域的监听器收得到后代作用域派发的事件",注册视图沿链向下继承;
- 大量事件签名带 `this: Scoped<Agent>`——`this` 绑定本身就是作用域载体。

而编排方向的头号场景恰好是子代理:`subagents.startContinuable` 12256 次、`subagent/end` 1676 次引用。**没有作用域过滤,一个挂在子代理上的插件会收到全树所有代理的事件**——它不是"少一个过滤器",它是行为错误。

同时注意 §一.3:rutis 内核已经决定事件不按 isolate 过滤(D29)。所以过滤只能建在**适配层**。

**处方**:
- `scopeId` 提升为一等字段,出现在 `plugin/load` / `evt/on` / `evt/dispatch` / `wf/invoke` / `svc/call` 上;
- 新增 `scope/create {scopeId, parentScopeId}` / `scope/dispose {scopeId}`,让 Rust 侧的 agent/session 生死驱动 TS 侧的作用域树;
- 过滤实现在 `rutis-dsh-agent`,**不动内核 D29**;文档写死"作用域是 dsh 语义,由适配层承担"。

---

#### A.5 "载荷纯数据"与 dsh 现实的冲突要逐个点名,不能一句"已满足"

文档 §五说"现有 4 个跨界事件载荷已满足[纯数据]"。实测这四个(及其同族)的载荷:

| 载荷成员 | 是什么 |
|---|---|
| `agent: Agent` | 活对象:`session`/`inbox`/`ctx: Context`/`cancel()`/`whenIdle()`/`runMaintenance()`/`send()`/`followup()`/`steer()`/`inject()` |
| `signal: AbortSignal` | 活对象 |
| `exec: ToolExecution` | 含 `signal` + `agent?` + `token: ToolExecutionToken` |
| `session: Session` | 活对象,日志是持久事实源 |
| `actor: object \| undefined` | fs intent 的"不透明执行上下文",消费方**向下导航**取活对象 |

最后一条尤其要命。官方 `fs-observation-policy` 的实现是:

```ts
private owner(actor: object | undefined): object | undefined {
  return (actor as FsObservationActor | undefined)?.agent?.session   // src/index.ts:36-40
}
```

——它把"不透明 actor"强转后一路导航到 `.agent.session`,并**拿 session 对象本身当读前写账本的键**。同时 `fs/write-intent` 的语义是"第一个返回 intent 的监听器**独占**决定,而不是与同侪组合"。过桥后 `actor` 只能是编号:导航链断掉,独占决定权落到谁手上取决于两侧编号规则是否一致,而且**不报错**。这不是"纯数据纪律"能覆盖的,这是替身合成问题。

**处方**:不要写"已满足",写一张**替身表**(每个活对象 → 过线表示 → 替身在哪一侧合成 → 哪些成员不可用)。这张表就是 §十.7"行为保真长尾"的可执行版本,也是 M7 的断言来源。

---

#### A.6 双工具管线:没有裁决谁是权威

文档 §四.1:适配层"执行时 `svc/call tools.execute` 往返";§六:"`tools/invoke`:走 Rust 侧完整三段管线(门控不因外部调用而绕过)"。

但 TS 侧的 `tools.execute(exec)` **不是一个裸执行入口**,它是 dsh 的完整管线:`tools/pre-execute` → guard/restrict → `tools/execute` 环绕 → `tools/post-execute` → invariants。于是一次模型工具调用会:

```
rutis: tools/pre-execute(Rust 链) → svc/call tools.execute
                                        → dsh: tools/pre-execute(TS 链) → 真执行 → tools/post-execute(TS 链)
       ← 结果 → tools/post-execute(Rust 链)
```

pre/post 各跑两遍。后果不是"多花几毫秒":`approval/request` 会**请求两次审批**,`ctx.invariants` 会因为阶段顺序不符而报错(dsh 自己在 `packages/core/tools/src/invariant.ts` 里就断言 `tools/execute must follow tools/pre-execute`)。

**处方**:写死一条归属裁决。推荐 **Rust 是管线权威**(它拥有循环与门控),TS 暴露 **dispatch-only** 入口——dsh 已经有这个缝:`ToolRuntimeScheduler.prepare/dispatch/finalize`(标 `@internal`)。桥调 `dispatch`,不调 `execute`。若走不通,退而求其次:反过来让 TS 当权威,Rust 侧不跑三段——但**必须选一个,并写进 §四**。

---

#### A.7 生命周期:桥不得进 driver 的 `injects`(缺失的红线)

§一.4 已给出证据。展开成规则:

- 若 `rutis-dsh` 的桥插件 `provide` 了任何 driver `injects` 的服务键,则宿主进程崩溃 → 桥 fiber 卸载 → 服务摘除 → driver 被驱逐 → 重载 → `AgentDriver::new` → `Session::new()` → **历史清零**。
- 正确形态:桥提供的服务对 driver 是**可选依赖**(经 `ctx.get` 软查询,或用 `provide_as_with_check` + 适配层内的内部可变注册表),永不进 `injects`。
- 反过来,§二"每个 TS 插件 = TS 侧一个 fiber = rutis 侧一个 fiber"这条配对是好的,但要补一句:**配对的是插件,不是能力**;宿主生死只应影响"TS 贡献的那部分能力",不应影响循环本身。

**处方**:在 §二"生命周期配对"后加一条硬约束,并在 M1 加一个断言:杀宿主 → `agent.id()` 不变。

---

#### A.8 适配层不"薄":rutis-agent 缺两个缝

文档估 `rutis-dsh-agent` "~数百行"。按 §一.5 的实现事实,它要么写 hack,要么先改 `rutis-agent`:

| 缺的缝 | 现状 | 需要 |
|---|---|---|
| 动态工具注册 | `ToolRegistry` 不可变,`ToolsPlugin::apply` 一次成型 | 内部可变的注册面,或一个 `tools/resolve` 汇聚点 |
| 执行环绕点 | 只有 `tools/pre-execute`(只能否决,`Option<String>`)与 `tools/post-execute`(改结果) | 一个 `tools/execute` 环绕 waterfall(值 = `ToolOutput`,Terminal = 现在的 `registry.execute`) |

顺带:加了 `tools/execute` 环绕点之后,§三.3 的 `around` 类型在 Rust 侧也有了对称物,而且**rutis 自己吃狗粮**——这与 `rutis-agent` lib.rs 里"框架自己吃狗粮"的既有说法一致。

**处方**:把这两项从"适配层"移到"前置依赖",与 prompt 装配服务、session 日志并列进 §八 的依赖顺序。

---

### B. min-cordis vs cordis:M0 风险清单重排(新方向下仍适用)

| 风险项 | 文档说法 | 实测 |
|---|---|---|
| Standard Schema / 配置校验 | 未提 | ✅ **销号**:两边都走 `Config['~standard'].validate`,schemastery 已实现 `~standard` |
| `Context` Proxy / `Service` 基类 | "依赖深度未知" | 中等:dsh 包内 `Context` 引用 1359 处、`Service` 134 处、`Fiber` 34 处;min-cordis 全都有(`reflect.ts` 419 行 vs cordis 418 行) |
| **loader / include / group** | **未提** | ⚠️ **新增风险**:min-cordis 明确删掉了这三个(~1400 行);dsh 官方有 16 个 `loader-composition.spec.ts`(即"可经 loader 组合"是包级契约),社区 423 仓用 `ctx.loader`(`entries`×3612 / `create`×2591) |
| **rc.7 ↔ 4.0.1 漂移** | 未提 | ⚠️ **新增风险**:min-cordis 派生自 cordis `4.0.0-rc.7`,dsh vendor 的是 `4.0.1`;min-cordis 的 `fiber.ts` 888 行 vs cordis 754 行,已经是两条分支 |
| 回退代价 | "回到 v1 的处境:继续追 @deepseek-ai/cordis 版本" | ✅ **实际更轻**:cordis 是 dsh **仓内 vendor 包**(`vendor/cordis`,workspace 依赖),组合 dsh 服务包时版本天然锁死,不存在"追版本" |

**重排后的 M0**:min-cordis 的真实分叉点是 **loader**,不是 Proxy。M0 的实验应该改成两问:①`tool-bash` 在 min-cordis 上能否装载并打印 schema;②它的 `loader-composition.spec.ts` 能否通过。第二问不通就直接用 vendor 的真 cordis——而这个回退**几乎没有代价**,因为它就在 dsh 仓里。

---


### C. 协议层面的具体缺陷(v2 遗留;第 1、2 条在新方向下仍然适用)

**1. 不要用 stdout 传帧。** 换行分隔 JSON 走 stdout,意味着任何一个 TS 插件的 `console.log` 都是**帧损坏**,按 §三 规则 2 触发 `host/restart`——一个日志语句变成一次进程重启,而且会循环。官方包自律(`console.log` 只有 5 处),但生态是 8889 个候选仓库,其中大量是批量生成物。
→ 用 fd 3 或 unix socket 传帧,stdout/stderr 全部留给插件日志。这一条删掉的是一整类故障,不是一个 bug。

**2. `evt/dispatch` 的丢弃策略要分类。** "drop + 计数"对 `agent/tool-call` 这类离散事件是对的;对 `AgentTextDelta` 这类**有序增量流**是错的——TS 侧按 delta 重建文本的插件会得到一段静默损坏的文本。
→ 按事件类别配策略:离散事件 drop+计数;有序流 coalesce(合并相邻 delta)或断连;禁止对有序流 drop。

**3. 事件映射的二选一要写明。**(见 §一.1)"每事件一个 Rust 类型"给类型安全与细粒度顺序,代价是新事件要发 Rust 版本;"单一擦除信封"给运行时可扩展,代价是所有过桥事件塌成一条尾链、且 rutis 原生监听方拿不到类型。**推荐前者**(与"版本声明制、集中适配"一致),但必须在文档里点名,而不是留给实现。

**4. `hello` 要协商能力集,不只是版本。** 现在只有 `{protocol, base, baseSemver, dshSemver}` → `{accepted}`。版本号表达不了"这个宿主没装 approval"。
→ `hello` 双向交换 `{modes, wfKinds, scopes, services}`;`plugin/load` 返回的 `injects` 与宿主能力集求差,差集非空即在**装载期**显式降级提示(这正是 §十.8 想要的机制,现在缺载体)。

**5. `cancel` 的语义要写完整。** 现在 `cancel {target}` 是通知;`target` 命名空间(callId 与 invocationId 会不会撞)、取消后是否还允许该 id 回 `res`、超时值由谁定,都没写。§三点五 #3 定了纪律("超时后按失败处理,不等待"),但超时值不在协议里。

**6. 版本目标与实测不符。** 文档三处写"对齐 dsh 0.3.x"(§一非目标、§三 hello、§六),实测仓库 tag `dsh-v0.1.0-rc.7`,219 个包版本全线 `0.1.0-rc.7`。要么 0.3.x 是未来目标(那要说明它还不存在),要么是笔误。**版本声明制的第一个动作就是把版本号写对。**

---

---

*本评审的数字可复算:ctx 操作统计来自 `../deepseek-harness/plugin-reference/raw/ctx-repo-operations.tsv`,
事件/服务签名来自 `../deepseek-harness/packages/extensions/tool-cordis/src/api-catalog.ts`(生成物),
dsh 版本来自 tag `dsh-v0.1.0-rc.7`。生态数据是 2026-08-20 快照,会漂移。*
