# 决策:aimux-llm 独立插件 + 业务无关桥 + dsh 默认 llm(2026-08-23,v2)

> 裁决人:用户。本文是对齐结论,后续实现的唯一依据;与本文冲突的旧口径
> (含 v3.2 设计"桥以 llm-pi-ai 形态存在"的沉淀倾向、rutis-dsh 作为
> "dsh 词汇唯一所在"的旧分层)以本文为准。
>
> v2 变更:用户裁决"桥必须业务无关、自动衔接"。映射(GenerateOptions→
> CallOptions)从 Rust 侧移入 TS face,Rust 侧零 dsh 知识;rutis-dsh
> 作为"dsh 词汇层"取消,仅剩启动器(去留见 §七.1)。
>
> **实施状态(2026-08-23,`7399433`):对象 A–E 全部落地并验收。**
> §六 三条验收全过(llm_e2e 两轮流/回喂/usage/噪声注入经新组合;
> workspace 171 绿;真机 one-shot finish=stop、key=request、注册表
> 推导能力集、干净退出)。§七.1 裁决为 A(入口保留)。实施中修正一处
> 契约缺陷:DTO 凭据字段 camelCase(`apiKey`),首测暴露了蛇形漏过线。

## 一、背景与动机

M0–M2 验收走了捷径:llm 聚合业务焊进桥的两侧(TS 插件 + Rust runner)。
作为验收脚手架达成了目的;作为架构沉淀是错的——管道里长业务。
实验阶段已结束(M0/M1/M1.5/M2 + npm 官方 dsh 真机全链路已通),
不存在需要新实验回答的未知,剩下的是设计与实现。

## 二、决策(用户裁决)

1. **独立的 `aimux-llm` rust plugin**:llm 能力以独立 rutis 插件存在,
   依赖 rutis + aimux;**禁止**依赖桥、禁止任何 dsh 形状知识。
2. **桥业务无关、自动衔接**:rutis-cordis 是唯一兼容层——协议 +
   cordis 词汇 + 注册表驱动的服务分发(注册了什么就声明/转发什么,
   能力集从注册表推导,禁止硬编码)。**Rust 侧没有任何一处知道 dsh。**
   唯一懂 dsh 的地方是 TS 侧 rutis-bridge 插件(llm face):dsh 的 llm
   消费是 TS 接口(registerAdapter),类型翻译必须住在 TS——这是插头
   形状转换,不是业务;它发 **aimux 中性形状**(CallOptions/StreamPart
   的 JSON),不发现 dsh 形状。
3. **dsh 默认 llm 配置成 aimux-llm**:settings 路由名 `aimux-llm`
   (配置后端 provider: deepseek,即实际调 DeepSeek API,可改),
   `agent-default-model` 指向该路由。TS 侧插件不再新增任何东西。

## 三、非目标(明确不做)

- 不重构 TS 插件内部结构;只把入参构造从"dsh 形状透传"改为
  "构造 aimux 形状"。
- 不做新验证轮次/示范插件/实验;`cargo test --workspace` 全绿 +
  `rutis-dsh up` 行为不回退是唯一验收门槛。
- 事件缝、fd3、npm 发布不在本决策范围(维持现状)。

## 四、实现对象与需求

### 对象 A:`crates/aimux-llm`(新 crate,rust 插件)

- 依赖:`rutis` + `aimux-core`/`aimux-providers`;零桥知识、零 dsh 知识。
- 以 rutis 插件形态实现:apply → 注册 **`llm` 服务**,服务面(aimux
  原生形状,即中性协议的 schema 所在):
  - `stream(CallOptions JSON) → StreamPart JSON 流`;
  - `listModels(provider, key) → 模型表`(带缓存);
  - per-(provider,key,model) provider 工厂与缓存;无 key 回落
    (UnconfiguredModel 语义:构造失败不阻塞启动,调用时报错)。
- 现从 rutis-dsh 迁入:LlmSeam 的工厂/缓存/流循环本体(解析 CallOptions
  的代码随迁,解析 dsh GenerateOptions 的代码不迁——它在 TS face 重写)。
- 行为测试随迁(keyed 路由、缓存、回落、错误路径)。

### 对象 B:TS face 的入参改造(rutis-bridge 插件,唯一懂 dsh 的地方)

- 现状:把 dsh GenerateOptions 原样透传,Rust 侧翻译。
- 需求:TS 侧直接构造 aimux 形状(system/messages/tools → prompt;
  provider/model/credentials 原样),响应侧映射不变(已是中性 part)。
- Rust 侧从此不出现 GenerateOptions 这个词。

### 对象 C:`rutis-cordis` 补齐通用分发(兼容层,零业务)

- 服务分发 = 注册表按名查找 + `svc/call` JSON 透传 + 流式 part 以
  `(method=stream 的调用, dispatchId)` 关联的 part ntf 回传——全部
  业务无关;hello 能力集(services 清单)从注册表推导。
- 现从 rutis-dsh 迁入:svc/call 分发骨架(原 LlmSeam.hooks 的分发
  部分泛化,method 集合由服务声明)。

### 对象 D:启动器(原 rutis-dsh runner,零 dsh 知识)

- 职责仅剩:bind 桥端口、spawn `dsh`(PATH/RUTIS_DSH_BIN)、组合
  rutis 运行时 + aimux-llm、hello 能力集来自注册表、双侧收敛等待。
- 迁移/删除:dshSemver/hello dsh 节校验删除(M1 产物,无消费者);
  llm 业务迁 A;事件观察日志(通用 ntf 日志)留启动器。

### 对象 E:dsh 配置

- `llm-aimux: providers: aimux-llm: {provider: deepseek,
  apiKeyEnv: DEEPSEEK_API_KEY}`;`agent-default-model:
  {provider: aimux-llm, model: deepseek-chat}`。纯配置,无代码。

## 五、分层落点(v2)

```
rutis-bridge(TS face)   唯一懂 dsh 的地方;dsh 类型 ↔ aimux 中性形状
rutis-cordis            业务无关兼容层:注册表分发 + cordis 词汇 + part 流
aimux-llm ──→ rutis     独立 llm 服务插件(aimux 原生形状即协议 schema)
启动器                    进程组合,零词汇知识
```

llm 从"桥的本体"降格为"第一个经业务无关桥供给 dsh 的 rutis 插件"——
"dsh 可以使用 rutis 提供的 rust plugin"命题的首个实例。

## 六、验收口径(2026-08-23 晚修订:按所有者要求,验收形态 = **profile web**)

1. `cargo test --workspace --no-fail-fast` 真实全绿(唯一例外:
   `host_cordis` e2e 缺 `DSH_ROOT`/`MIN_CORDIS_ROOT` 的设计内 fail-loud)。
2. **web 完整 turn**(验收主形态):`rutis-dsh up --profile web`,runner
   环境**不设任何 key**;浏览器新会话(默认模型即 aimux-llm 路由)发
   消息 → 真实回答渲染;runner 侧证据:`[aimux-llm] stream ...
   key=request ... finish=stop`(key 经 dsh 凭据存储 per-request 过线)、
   dsh session 流记录 `provider: aimux-llm`、事件回流、picker 的
   listModels 过线。
   headless one-shot 作为回归副线保留(可脚本化),**不作为验收主形态**。
3. 无新增实验/验证轮次。

### 独立审核(2026-08-23,按所有者要求以需求原文独立审计)

结论:1/2/4/5/6/7 满足,3 实质满足(两处残留已清:rpc.rs 的 dshSemver
回显、TS hello caps 硬编码),8 的验收缺口已按上文本节补齐。审核另
发现:serde 改名 apiKey 后 aimux-llm 三个测试红灯未重跑即提交——已修,
并立规:**改名/契约变更后必须重跑全量测试再提交,提交信息里的绿必须
是刚跑出来的**。

## 七、待确认(仅剩一项)

1. 启动器位置:**A** 保留 `crates/rutis-dsh` 仅作启动器(改动最小,
   名字里的 dsh 只指"启动 dsh 进程",不指知识);**B** 并入
   rutis-cordis 作通用 host bin,删除 rutis-dsh crate。默认 A。
