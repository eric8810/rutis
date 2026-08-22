# rutis

Cordis 核心范式的 Rust 惯用实现(自 [min-cordis](https://github.com/eric8810/min-cordis) 的 Rust 工作区独立成库)。

## 五支柱

1. **插件 = 装配单元**:一次 `apply`,提供服务 / 监听 / 清理
2. **fiber = 生命周期容器**:六态状态机 + 依赖门控 + 级联卸载 + 恰好一次清理
3. **服务 = 类型键注册表 + isolate 作用域**
4. **事件总线 = 四分发语义**(emit / parallel / serial / waterfall)
5. **依赖驱动重载**:provider 卸载 → 消费者驱逐并自动重载

## Crates

| crate | 内容 |
|---|---|
| [`rutis`](crates/rutis) | 内核:Ctx / fiber / registry / event bus / effect(115 项契约与对拍测试) |
| [`rutis-cordis`](crates/rutis-cordis) | cordis 基座桥:协议机制(`Wire` 传输接缝 / 在飞表 / 取消 / 超时 / 孤儿计数)+ cordis 词汇(hello 能力集 / evt mode / wf kind / 装载仲裁),零 dsh 知识(M1;设计 [docs/design-dsh-bridge-2026-08-21.md](docs/design-dsh-bridge-2026-08-21.md) v3.2) |
| [`rutis-dsh`](crates/rutis-dsh) | dsh 桥的 dsh 面:hello 的 `dsh` 节校验 / dshSemver pin / dsh 服务集求差;llm 缝与事件映射按 M2 进入 |
| [`rutis-agent`](crates/rutis-agent) | 最小 agent 框架:aimux `LanguageModel` 服务 + `ToolsPlugin` + `AgentDriverPlugin`(流式 `followup` + waterfall 中间件 + `agent/*` 事件广播)+ 内存 session + ratatui TUI;minimal mode 内置 `bash` + `replace_text` 工具(设计见 [docs/design-minimal-mode-2026-08-18.md](docs/design-minimal-mode-2026-08-18.md)) |
| [`rutis-cli`](crates/rutis-cli) | 命令行形态:最小 coding agent TUI(`cargo install rutis-cli` 或 [GitHub Releases](https://github.com/eric8810/rutis/releases) 下载) |

agent crate 一句话:**一个 aimux [`LanguageModel`](../aimux) 服务 + 一个 `ToolRegistry` 插件 + 一个实现 `Agent` 接口的 driver 插件 + 一个内存 session(连续 loop 的事实源)**。设计见 [docs/design-min-agent-2026-08-18.md](docs/design-min-agent-2026-08-18.md) 与 [docs/design-agent-verification-tui-2026-08-18.md](docs/design-agent-verification-tui-2026-08-18.md)。

## 依赖布局

agent / cli crate 经 crates.io 版本消费 [aimux](https://crates.io/crates/aimux-core)(LLM 统一访问层,`LanguageModel` / `CallOptions`,329 provider),**无需并列检出**。要 hack 本地 aimux,在工作区根加未提交的 `[patch]` 指向本地路径即可。

## 快速开始

```bash
cargo test                                    # 全量:内核对拍 + agent 三层中的前两层
cargo install -p rutis-cli --path crates/rutis-cli   # 装 CLI(或 cargo install rutis-cli)
rutis-cli --scripted                          # 无 key 离线演示
cargo run -p rutis-agent --example demo       # 真实后端两轮对话 + 依赖驱动驱逐(需 DEEPSEEK_API_KEY)
cargo run -p rutis-agent --example tui        # 交互式 TUI,流式逐字 / 工具可见 / Esc 取消
cargo run -p rutis-agent --example tui_scripted   # 离线脚本后端,无需 key
cargo test -p rutis-agent --test real_backend -- --ignored   # 真实端到端(不进 CI)
```

provider / model 可用 `AIMUX_PROVIDER` / `AIMUX_MODEL` 覆盖(如本地 `AIMUX_PROVIDER=ollama AIMUX_MODEL=qwen3:8b`)。

## 验证三层(agent)

- **单元**:`ScriptedLlm` 实现真 `LanguageModel`(`do_stream`),循环逻辑 / 多轮 history / max_steps / 取消 / 失败回喂
- **集成**:aimux `MockReplayModel` 录制回放;双门控、卸载 llm 自动驱逐重载、fiber 卸载取消、事件观察
- **真实端到端**:真实 provider 多轮 + 工具调用(`#[ignore]`,需 key 手动触发)

## 文档

- [design-rust-port.md](docs/design-rust-port.md) — 内核设计
- [design-dual-core-2026-08-20.md](docs/design-dual-core-2026-08-20.md) — 双核架构与经验锈化路线(rutis Rust 脊柱 × dsh TS 功能面)
- [design-dsh-bridge-2026-08-21.md](docs/design-dsh-bridge-2026-08-21.md) — dsh 桥 v1 设计(TS 插件接入 + 编排 API)
- [cordis-spec-parity-2026-08-18.md](docs/cordis-spec-parity-2026-08-18.md) — 与 cordis 原版 spec 对拍
- [design-min-agent-2026-08-18.md](docs/design-min-agent-2026-08-18.md) / [design-agent-verification-tui-2026-08-18.md](docs/design-agent-verification-tui-2026-08-18.md) — agent 框架与 TUI 设计
- [design-minimal-mode-2026-08-18.md](docs/design-minimal-mode-2026-08-18.md) — minimal mode(bash + replace_text)
- [review-rust-impl-2026-08-17.md](docs/review-rust-impl-2026-08-17.md) — 实现评审

## License

MIT(继承自 [Cordis](https://github.com/shigma/cordis) © Shigma)。
