# M1 协议层独立评审(2026-08-22)

> 对象:commit `615c035`(`feat(dsh): M1 protocol layer`),以
> [design-dsh-bridge-2026-08-21.md](design-dsh-bridge-2026-08-21.md)(v3.1)为唯一基准。
> 手段:独立评审 agent 逐行对照 proto.rs / m1.rs / lib.rs / Cargo.toml;
> `cargo test -p rutis-dsh` 实跑 14 通过;仓库 grep 核对 M1 前置物证。

## 总体结论:M1 验收 —— **有条件通过**

14 项验收测试全通过且多数测到真实语义(超时"不等待"、迟到 res 孤儿计数、
乱序关联、终态收敛);六条连接规则四条完整落地,方法表为后续波次留面基本
充足;依赖纪律(仅 rutis,无 rutis-agent/aimux)与量级(proto.rs 766 行
对"数百行",全 crate 1341 行对 ~1.5k 预算)达标。

**放行条件**(M1 签字前):Z1、F1、F6 修补落位;F2–F5 入下一轮;
S/D 项随 M2 定型消化。修补量百余行内,不动架构。

## 阻断

- **Z1 `sessionId`/`turnId` 预留缺失,模块注释改写了设计口径**。
  proto.rs 的 Frame 仅 `scope_id`,模块注释自述"全部帧预留 scopeId"把
  设计的**三字段**收缩成一个。设计 §三 规则 6:"`sessionId` / `turnId` /
  `scopeId` 字段 v1 全部预留、全局广播不过滤,v2 启用——显式声明"。
  M1 是线格式冻结点,预留即本里程碑的契约。处方:Req/Ntf/Res 补两个
  可选字段(rename + skip_serializing_if)并镜像线格式往返测试;若裁定
  serde 容忍未知字段足以推迟,须降级为显式分歧并回写设计文档。

## 应修

- **F1 "首帧必须 hello"只对 Req 生效**:connecting 检查嵌在 Req 臂内,
  宿主以 Ntf/Res 开场被静默吞掉,会话停在 Connecting,`ready()` 挂死。
  处方:connecting 检查提到 match 之上,握手前三类非 hello 帧一律
  Failed 终态;补 Ntf/Res 首帧测试。
- **F2 `ProtoError::Cancelled` 是死变体**:全仓库零构造,取消实际以
  `Remote{code:"cancelled"}` 交付。规则 4 要求调用方能区分"我取消的"
  与"远端错误"。处方:request 结算处映射,或删变体,二选一。
- **F3 重入测试是影子测试**:宿主发 Ntf 后立即回 res,串行泵实现也能
  通过全部断言。处方:Ntf 后扣住 res,等钩子侧信号再回,才证明
  "res 未到时泵已处理 evt"。
- **F4 `notify`/`cancel` 无状态门**:终态后仍向 wire 写帧。处方:复用
  request 的状态检查。
- **F5 `wf/register` 的 kind 三型零预留**:规则 5 的 mode 有 typed enum
  + 拒绝测试,kind 不对称。设计 §五:"kind ∈ decide|around|stream,
  **v1.1 帧字段即刻预留**"。处方:补 `WfKind` + 申报结构 + serde 拒绝
  测试,或注释写明"stringly 预留即刻意"并回写设计。
- **F6 §十.2 同步 API 复核(M1 前置)无落点记录**:commit 未附复核产物,
  docs/ 无记录(对照 M0 有实验记录文档)。处方:补复核并落一行结论;
  **注:本机 dsh 仓无 plugin-reference/raw 数据,实际须先找回或重扫
  语料**(见 experiment-m0 报告的挂起数据问题)。

## 建议

- **S1** 钩子 panic → 请求永无应答,宿主只能靠超时兜底(catch_unwind
  或注释写死契约)。
- **S2** `ok:true` 无 result 被归为 malformed——方法表里多行结果为
  "—",宿主回空结果会被当线格式违规(宽容为 Null 或写死"必带 result")。
- **S3** malformed-res 判定双实现:`Frame::outcome()` 公共面与泵内各写
  一份,泵未复用前者,漂移风险。
- **S4** 泵生命周期:全部句柄 drop 而 wire 存活时泵永久持有 Arc;M1 无
  实害,记入后续传输实现注意项。

## 分歧记录

- **D1 timeoutMs 位置**:设计方法表放在 svc/call params 内;实现提升为
  `request()` 参数并在桥侧执法全局上限。可辩护(执法关注点在桥),M2 给
  svc/call 定型 params 时回表核对。
- **D2 hello 回包不含 `protocol` 字段、`dshSemver` 回声宿主值**。可辩护,
  倾向补上 protocol 回显以便宿主对称验证。
- **D3 Bridge 是单会话生命周期,无 restart→再握手路径**。host/restart
  (规则 2)的桥侧语义设计与实现均未定,M2 前须裁决,否则"杀进程+重拉"
  只做了杀的一半。

## 验收行逐项(C 维摘要)

往返/并发乱序/取消+孤儿计数/超时(真证明了不等待:m1.rs 在宿主未回任何
res 前 call 已以 Timeout 结算)/握手错配(protocol+semver 两路)/能力集
求差(纯函数级,装载流程 M1 不存在,可接受;M1.5/M2 接线时勿重复发明)/
同名仲裁/重入(强度不足,见 F3)/杀宿主(pending 清单 + frames 计数 +
死后新调用立即 HostGone,均断言到值)。

## 纪律核对(E 维)

依赖 = rutis + serde/serde_json/tokio/thiserror,无 rutis-agent、无
aimux,层次纪律(§一/§八)成立;rutis 实际用量为 `BoxFuture` 别名。
导出面 17 项,克制。

---

## 复验记录(2026-08-22,v3.2 两级桥重组后)

评审促发的架构裁决(桥分两级,设计 v3.2)与修补同批落位,`cargo test
-p rutis-cordis -p rutis-dsh` = **17 + 4 全绿**:

| 发现 | 处置 | 落点 |
|---|---|---|
| Z1 三字段预留 | ✅ 已修:`Frame` 三类帧均带 `scopeId`/`sessionId`/`turnId`(线格式一处定义,语义分层:scopeId=基座桥解释,session/turn=dsh 面解释);往返测试镜像 | rutis-cordis rpc.rs + `frame_reserved_fields_roundtrip` |
| F1 首帧纪律 | ✅ 已修:connecting 检查提到 match 之上,Req/Ntf/Res 三类非 hello 首帧一律 Failed 终态(Req 违规回 error res,Ntf/Res 无可归属请求方直接断开);新增 Ntf/Res 首帧测试 | rpc.rs pump + `first_frame_ntf_rejected` / `first_frame_res_rejected` |
| F2 死变体 | ✅ 已修:`CallSettled::Cancelled` 结算变体,取消以 `ProtoError::Cancelled{id,method}` 交付,与远端错误可区分 | `cancel_settles_as_cancelled_and_late_res_counts_orphan` |
| F3 影子测试 | ✅ 已强化:res 扣住直至 evt 实际抵达钩子(5s 超时门),"泵不阻塞"成为被断言语义 | `reentrant_events_processed_before_call_settles` |
| F4 状态门 | ✅ 已修:`notify`/`cancel` 复用 `check_open`,终态后拒绝 | `host_death_...` 尾部断言 |
| F5 WfKind 预留 | ✅ 已修:`WfKind{Decide,Around,Stream}` + `WfDeclaration` 落 cordis 词汇层(归属由 v3.2 裁决:waterfall 是 cordis 语义)+ 值域封闭测试 | `wf_kind_three_shapes_frozen` |
| S2 空 result | ✅ 已修:`ok:true` 缺 result 宽容为 `Null` | `Frame::outcome` |
| S3 双实现 | ✅ 已修:泵复用 `Frame::outcome()` | pump Res 臂 |
| D2 protocol 回显 | ✅ 已修:hello 回包回显 `protocol` | `handshake_replies_symmetric_capability_set` |
| F6 §十.2 复核 | ✅ 已完成(见下节"§十.2 同步 API 复核"):正确口径(过桥面 = `LlmAdapter`)下同步成员调用方交集为空;语料数据位置与明细缺失情况一并落账 | 本文档下节 |

**M1 在两级桥结构下签字**(2026-08-22):验收行全部由 rutis-cordis 的
17 项机制测试覆盖,dsh 节两级握手由 rutis-dsh 的 4 项测试覆盖;Z1/F1
放行条件与 F2/F4/F5/S2/S3/D2 修补全部落位,F6 复核亦已完成后签字。

## §十.2 同步 API 复核(2026-08-22,F6)

**数据源**:语料库实际位于 SSH 服务器 `eric8810@100.121.215.57:
/media/eric8810/fast-deliver/code/dsh-ecosystem/`(research/ 10 份 md +
raw/ 机读 TSV;repos/ 9398 浅克隆仓 70G;scripts/ 分析脚本)。评审原文
引用的 `ctx-repo-operations.tsv`(repo 级明细)已缺失,但
`research/ctx-operations.md` 的**成员级聚合**(repo × service × member
的计数汇总)足以支撑本次复核;repo 级明细需要时用
`scripts/ctx-repo-detail.py` 重跑。

**口径修正**:v1 过桥的不是整个 `ctx.llm` 服务,而是 **`LlmAdapter` 面**
(llm 缝:TS 的 LlmRuntime 留驻本地,Rust 侧 aimux 实现被注册为一个
adapter)。`LlmRuntime` 自身的同步注册表成员不在过桥面内。

**结论:交集为空,复核通过。** 逐成员判定(dsh `packages/llm/llm`
源码 + 社区 897 仓 / 19477 次 llm 调用):

| 成员(社区次数) | 面 | 同步性 | 过线 |
|---|---|---|---|
| `registerAdapter`×5421 / `listProviders`×2921 / `listConfigurableProviders`×1350 / `registerConfigurableProviders`×628 / `registerModelDiscovery`×585 / `providerRetryPolicy`×405 | LlmRuntime(TS 留驻) | 同步 | 否——注册表本地操作,零桥影响 |
| `stream`×1830 / `prepareCall`×52 / `resolveModelInfo`×2302 / `discoverModels`×2273 / `listModels`×1458 / `resolveCallConfig`×156 | LlmRuntime → adapter 委托 | **async** | **是(llm 缝)** |
| `providerInfo` / `providerRetryPolicy`(adapter 声明) | LlmAdapter | **同步** | **注册期快照,不跨线**:`prepareRoutes` 在注册时同步调一次,结果(`{id,name}` + retryPolicy)快照进注册表;运行期 `LlmRuntime.providerRetryPolicy` 只读快照。桥 adapter 以 TS 侧常驻的静态元数据满足这两个同步成员 |
| `llm/stream`×462 / `llm/adapters-updated`×270 | 事件订阅 | — | 事件缝(emit ntf),与同步性无关 |

**顺带发现(记录在案,不阻塞)**:社区直接摸 `LlmRuntime` 的 private 成员
(`registration`×22、`adapters`×13、`streamWithRegistration`×9)与自挂
非官方面(`addProvider`×27、`complete`×4、`registerProviderAuth`×3、
`isPiEngineEnabled`×2、`__dshVisionResolveWrapped`×3),合计 ~83 次。
LlmRuntime 留驻 TS 则无害;**锈化 LlmRuntime 本身时这是已知破口清单**,
也是 M1.5 语料样本挑选的参考维度。
