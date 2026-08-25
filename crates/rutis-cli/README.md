# rutis-cli

最小 coding agent 的命令行形态:[rutis](https://crates.io/crates/rutis) 框架 +
[rutis-agent](https://crates.io/crates/rutis-agent) minimal mode(`bash` +
`replace_text` 两个工具,能改文件、能跑命令),流式 TUI 交互,后端任选 aimux
provider(deepseek / ollama / …)。

## 安装

```bash
# 最新版:GitHub Releases 下载对应平台 tar.gz,或源码构建:
git clone https://github.com/eric8810/rutis && cd rutis && cargo build -p rutis-cli
# crates.io 的 cargo install rutis-cli 为旧版(不含 rutui TUI)
```

## 使用

```bash
export DEEPSEEK_API_KEY=... && rutis-cli            # deepseek-chat
rutis-cli --provider ollama --model qwen3:8b        # 本地模型
rutis-cli --scripted                                # 无 key 离线演示
```

交互:Enter 提交;Esc / Ctrl+C(运行中)取消当前 turn;Ctrl+Q 退出。

## License

MIT(继承自 [Cordis](https://github.com/shigma/cordis) © Shigma)。
