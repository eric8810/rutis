# 工作文档:补常规 CI(测试自动化)

> 开始:2026-08-25。来源:主线扫描——仓库只有 release/bridge-release 两个
> 发布流程,没有常规测试 CI;回归不会被自动抓住(本次会话中 Supervisor 的
> 测试就差点引入 bug,靠本地跑才抓住)。补 `ci.yml` 是主线高价值项。

## 目的
1. push / PR 时自动跑 `cargo test --workspace`(跳过 node e2e)+ `cargo check`。
2. 任何回归在 CI 上显式变红,而不是靠开发者本地发现。
3. 快速反馈(<5 min),不阻塞发布流程。

## 现状
- `.github/workflows/` 只有 `bridge-release.yml`(npm 发布)+ `release.yml`(二进制发布),均 tag 触发。
- 无 push/PR 触发的测试 workflow。
- `RUTIS_SKIP_NODE_E2E=1` 是 node e2e 的逃生门(缺 DSH_ROOT/MIN_CORDIS_ROOT 时跳过)。
- `cargo test --workspace` 本地全绿(除 node e2e,环境限制)。

## 设计
- 新 `ci.yml`:push + pull_request 触发。
- 单 job(ubuntu-latest):
  - checkout + rust toolchain(stable)+ rust-cache。
  - `cargo test --workspace`(env RUTIS_SKIP_NODE_E2E=1)。
  - `cargo check --workspace --all-targets`(含 examples/tests 编译)。
- 不跑 node e2e(需外部 checkout,见 handoff §四)。

## 验证
- 本地模拟 CI 命令全绿。
- push 后看 GitHub Actions 是否触发(远程验证)。

## 边界
- 不做 matrix(单平台足够防回归;release 已有平台矩阵)。
- 不缓存 node_modules(无 node 步骤)。

## 追加发现与处理(同一轮)
- **path 依赖可移植性**:rutis-agent 以 `path = "../../../rutui/..."` 引用
  仓库外兄弟 checkout rutui。CI 无此目录必然失败 → ci.yml 增加 checkout rutui
  到 `../rutui` 步骤(模拟验证:cargo check --workspace 通过)。
- **crates.io 版本陈旧**:rutis/rutis-agent/rutis-cli 在 crates.io 是 0.1.0 旧版
  (rutis-agent 0.1.0 无 rutui 依赖,是重写前版本)。README 声称的
  `cargo install rutis-cli` 装的是旧版 → 已更新三处文档(README /
  cli README / release.yml notes)澄清:最新走 GitHub Releases 或源码构建。
- **rutui 发布前置**:要把最新 TUI 版发布到 crates.io,需先发布 rutui-*
  (git workspace 子 crate,Cargo git 依赖不支持;需 rutui 拆独立 crate 或
  发布到 crates.io)。留给下一代(需 crates.io token)。
