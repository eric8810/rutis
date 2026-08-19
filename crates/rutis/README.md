# rutis

Cordis 核心范式的 Rust 惯用实现(自 [min-cordis](https://github.com/eric8810/min-cordis) 独立成库)。

## 五支柱

1. **插件 = 装配单元**:一次 `apply`,提供服务 / 监听 / 清理
2. **fiber = 生命周期容器**:六态状态机 + 依赖门控 + 级联卸载 + 恰好一次清理
3. **服务 = 类型键注册表 + isolate 作用域**
4. **事件总线 = 四分发语义**(emit / parallel / serial / waterfall),同事件类型 emit 按发射序投递(D31 尾链)
5. **依赖驱动重载**:provider 卸载 → 消费者驱逐并自动重载

## 使用

```toml
[dependencies]
rutis = "0.1.0"
```

内核零 serde、零 unsafe,依赖仅 tokio / tokio-util / thiserror。设计与对拍文档见[仓库 docs](https://github.com/eric8810/rutis/tree/main/docs)。

## License

MIT(继承自 [Cordis](https://github.com/shigma/cordis) © Shigma)。
