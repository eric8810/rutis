# rutis-agent

基于 [rutis](https://crates.io/crates/rutis) 五支柱的最小 agent 框架:一个 aimux
[`LanguageModel`](https://crates.io/crates/aimux-core) 服务 + 一个 `ToolRegistry`
插件 + 一个实现 `Agent` 接口的 driver 插件 + 一个内存 session。

- 流式 `followup` 只触发 turn + 回传终态;过程增量经 `agent/*` 事件广播
- 循环关键节点 waterfall 中间件:`agent/pre-step`(改写/拒绝消息)+
  工具三段管线(pre-execute 门控 / execute / post-execute 结果决策)
- minimal mode 内置 `bash` + `replace_text` 工具(能改文件、能跑命令的
  coding agent,语义对齐 deepseek-harness)
- ratatui TUI 插件订阅事件渲染

## 使用

```toml
[dependencies]
rutis-agent = "0.1.0"
```

命令行形态见 [rutis-cli](https://crates.io/crates/rutis-cli)。设计与验收文档见
[仓库 docs](https://github.com/eric8810/rutis/tree/main/docs)。

## License

MIT(继承自 [Cordis](https://github.com/shigma/cordis) © Shigma)。
