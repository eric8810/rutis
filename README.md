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
| [`rutis-agent`](crates/rutis-agent) | 最小 agent 框架:aimux `LanguageModel` 服务 + `ToolsPlugin` + `AgentDriverPlugin`(流式 `followup`)+ 内存 session + ratatui TUI;minimal mode 内置 `bash` + `replace_text` 工具(能改文件、能跑命令的 coding agent,设计见 [docs/design-minimal-mode-2026-08-18.md](docs/design-minimal-mode-2026-08-18.md)) |

agent crate 一句话:**一个 aimux [`LanguageModel`](../aimux) 服务 + 一个 `ToolRegistry` 插件 + 一个实现 `Agent` 接口的 driver 插件 + 一个内存 session(连续 loop 的事实源)**。设计见 [docs/design-min-agent-2026-08-18.md](docs/design-min-agent-2026-08-18.md) 与 [docs/design-agent-verification-tui-2026-08-18.md](docs/design-agent-verification-tui-2026-08-18.md)。

## 依赖布局

agent crate 经 path 依赖消费 [aimux](https://github.com/eric8810/aimux)(LLM 统一访问层,`LanguageModel` / `CallOptions` / 329 provider)。请与本项目并列检出:

```
Code/
├── rutis/
└── aimux/
```

## 快速开始

```bash
cargo test                                    # 全量:内核对拍 + agent 三层中的前两层
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
- [cordis-spec-parity-2026-08-18.md](docs/cordis-spec-parity-2026-08-18.md) — 与 cordis 原版 spec 对拍
- [design-min-agent-2026-08-18.md](docs/design-min-agent-2026-08-18.md) / [design-agent-verification-tui-2026-08-18.md](docs/design-agent-verification-tui-2026-08-18.md) — agent 框架与 TUI 设计
- [design-minimal-mode-2026-08-18.md](docs/design-minimal-mode-2026-08-18.md) — minimal mode(bash + replace_text)
- [review-rust-impl-2026-08-17.md](docs/review-rust-impl-2026-08-17.md) — 实现评审

## License

MIT(继承自 [Cordis](https://github.com/shigma/cordis) © Shigma)。
