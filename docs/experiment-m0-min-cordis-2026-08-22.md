# M0 基座实验结果：tool-bash on min-cordis（2026-08-22）

> 判据出处：[design-dsh-bridge-2026-08-21.md](design-dsh-bridge-2026-08-21.md) §六
> （两问全通 → min-cordis；任一不通 → vendor cordis），问题定义另见
> [review-dsh-bridge-2026-08-21.md](review-dsh-bridge-2026-08-21.md) §B。
> 实验代码与可复现脚本：`../deepseek-harness/experiments/m0-min-cordis/`
> （dsh 仓未跟踪目录，含自身 README）。本文件是结果记录。

## 结论

**两问全通 → 基座裁决 = min-cordis。**

补强证据（同日）：dsh 官方 `tool-pwsh-persistent/tests/loader-composition.spec.ts`
**原样纳入**、只换基座重跑，真 PowerShell 进程执行通过——执行层不再是
stub，且测试是官方自己写的，非本实验的等价复刻。

**评审 §B 头号风险实测不成立**："loader / include / group 是 min-cordis 的
真实分叉点"——事实上 loader 在 dsh 里是独立插件包（`vendor/loader`），
不是 cordis 核心的一部分；min-cordis 只是自己不内置 loader 生态，作为
基座**原封不动承载**了官方 `cordis-plugin-loader` 与 `cordis-plugin-include`。

## 结果

| 问 | 内容 | vendor cordis（基线） | min-cordis（实验组） |
|---|---|---|---|
| Q1 | tool-bash 直接装载 + `tools.schemas()` 打印 bash schema + 调用管道（stub shell） | ✅ 3/3 | ✅ 3/3 |
| Q2 | 真 `cordis.yml` Loader 组合（Loader + include + `internal.import` 模块表）装载 + schema + 调用 | ✅ 2/2 | ✅ 2/2 |
| Q3 | 官方 pwsh loader-composition spec 原样：真进程、持久会话、cwd/env 跨调用保持、大输出截断、exit 重置 | ✅ 12.7s | ✅ 13.6s |
| 纯净性 | 模块图审计（文件级断言 + run 级 `globalTeardown` 终检） | ✅ | ✅ 实验组 vendor cordis **零加载** |

Q2 与官方 16 个 loader-composition spec 同构（同一条
`new Context() → plugin(Loader) → builtins.include → internal.import →
loader.create('cordis:include') → loader.await()` 链）。tool-bash 本无官方
loader-composition spec，Q2 即为它补的等价物；Q3 则是官方文件一字未改。

## 实验设计（变量控制）

- **唯一变量是基座**：vite alias 只换 `@deepseek-ai/cordis` 一个说明符；
  dsh 包、schemastery、loader/include 插件保持仓内源码模式解析
  （`tsconfig.base.json` paths），复用官方 `standardDecoratorPlugin`。
- **win32 剥离（Q1/Q2）**：官方在 Windows 排除 bash 系测试（无 POSIX
  shell），实验把 shell 执行层换成 `StubShellExecutor`——进程 spawn 被替身
  收编，`tool-bash → tools.execute → shell seam → render` 全链路照真跑。
  Q3（pwsh）无此问题，真进程。
- **防混合基座假阳性**：`m0-module-audit` vite 插件把每个被转换模块 id
  落盘；`assertBaseIdentity()`（Context 同一性）与 `assertModuleGraphPurity()`
  （文件级）双断言，外加 `globalTeardown` 全图终检——min 模式下 vendor
  cordis 出现即失败退出。防"spec 层换了基座、包层仍走 vendor"的无效实验。
- **agent 替身**：照抄官方 persistent spec 的最小 `Agent` 对象（Inbox /
  Session / registry 注册），非伪造接口。

## 过程发现

1. **min-cordis 与 vendor cordis（4.0.1）是近亲**：`service.ts` 逐字同构
   （唯一差异：cosmokit 的 `defineProperty` 被内联进 utils），`registry.ts`
   / `reflect.ts` 行数一致；min-cordis 零外部依赖（单体自足），alias 替换
   无传递解析问题。评审记录的 rc.7 ↔ 4.0.1 漂移在本样本未显形。
2. **本机 node_modules 链接残缺**：koffi（native 依赖）与
   `@deepseek-ai/dsh-pwsh-local/src/*` 深路径的包级链接缺失（pnpm store
   里都在）。实验 config 用两条**路径等价** alias 补齐，不改任何被测代码
   语义；安装完整的环境不需要它们。
3. min-cordis 以 2026-08-22 快照运行（HEAD `80afa89`，派生自 cordis
   4.0.0-rc.7）。

## 边界（未覆盖项）

1. 真 bash-local / bash-sandbox 执行未测（win32 不可用；Q3 的 pwsh 真进程
   已覆盖"真执行 × min-cordis 基座"这一组合，bash 侧可在 linux lane 复核）。
2. 官方 16 个 loader-composition spec 实测 1 个原样（pwsh-persistent）。
3. min-cordis 经 vite 转译运行；桥宿主用 node 原生加载其 TS 直出 exports
   是桥 v1 的工作项。

## 复现

```bash
# deepseek-harness 仓根；node ≥22.19（本机 fnm v24.18.1）
node node_modules/vitest/vitest.mjs run --config experiments/m0-min-cordis/vitest.config.ts                 # 基线
M0_BASE=min-cordis node node_modules/vitest/vitest.mjs run --config experiments/m0-min-cordis/vitest.config.ts  # 实验组
# min-cordis 克隆位置可用 M0_MIN_CORDIS_ROOT 覆盖（默认 D:/code/min-cordis）
```
