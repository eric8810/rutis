# M1.5 最小语料矩阵:10 仓装载实验(2026-08-22)

> 判据出处:[design-dsh-bridge-2026-08-21.md](design-dsh-bridge-2026-08-21.md) §九 M1.5 行——
> "10 仓最小语料(含静态值级导入重的样本)跑 L0/L1。暴露宿主 npm 闭包
> 问题——会改变宿主形态的发现,放在最便宜的位置。"
> 实验代码:`../deepseek-harness/experiments/m1p5-corpus/`(未跟踪目录);
> 语料源:生态服务器 `100.121.215.57:dsh-ecosystem/`(9398 浅克隆仓)。

## 结果矩阵

| # | 样本(选取依据) | L0 装载 | L1 注册生效 | 发现 |
|---|---|---|---|---|
| 1 | btspoony/mstar-harness(耦合王 1122.5 分,值级导入最重) | ✗ | — | **npm 闭包**:`@mstar-harness/engine`(社区 workspace 内部包) |
| 2 | btspoony/mstar-workflow(914.5 分,同族) | ✗ | — | **npm 闭包**:`@mstar-harness/engine` |
| 3 | jianxx/dsh-cc-plugins·cc-memory(16 子包 workspace 成员) | ✗ | — | **npm 闭包**:`@jianxx/dsh-cc-tools`(社区内部包) |
| 4 | andy8647/dsh-auto-approval(工具类) | ✗ | — | **npm 闭包**:`zod`(第三方) |
| 5 | leeminjing/dsh-messages-sanitizer(零依赖标准形态) | ✓ | none | 纯事件型,无工具/prompt 面 |
| 6 | leemiracle/deepseek-rust-harness | ✓ | **✓** | `tools:[r_fmt,r_lint,r_build,r_miri]` 四个 schema 真注册 |
| 7 | oowjzzoo/dsh-plugin-api(251 分中等耦合) | ✓ | none | — |
| 8 | pwnky/dsh-session-link(轻) | ✓ | none | 依赖 `@deepseek-ai/dsh-session-reference`——官方真有此包,被闭包接住 |
| 9 | ayuanwong/deepseek-harness-ux | ✗ | — | **形态**:dsh 仓整 fork,非插件仓 |
| 10 | 112gt/deepseek-harness-vscode | ✗ | — | **形态**:VSCode 扩展,无 package.json |

**L0 = 4/10(插件形态内 4/8);L1 工具面 1、prompt 面 0(三个 none 为
事件/客户端型插件)。**

## 核心发现(会改变宿主形态的那些)

1. **官方闭包全部可满足**:样本对 `@deepseek-ai/*` 的每一个 import——
   包括 `dsh-settings`、`dsh-session-reference` 这类非核心包——都被官方
   236 包的源码面接住,零缺口。**证实 §十.11"宿主 npm 闭包 ≈ 完整 dsh"**:
   宿主不是"薄基座 + 少量服务",是真·全栈组合。
2. **闭包缺口全部在非官方面**,两类:社区 workspace 内部包
   (`@mstar-harness/engine`、`@jianxx/dsh-cc-tools`——依赖仓内兄弟包,
   npm 上不存在或不稳定)与第三方库(`zod`)。**推论:M2 宿主的闭包策略
   = 官方 236 包整体 + 逐插件的第三方闭包装载**;社区内部包依赖是装载
   硬失败,只能拒绝并指名(plugin/load 的 injects 求差机制正好承载)。
3. **语料需要形态过滤器**:9398 候选仓里混着 dsh 整 fork 与 VSCode 扩展
   ——它们不是插件形态,L0 必然失败。M7 语料管线(周更矩阵)前置一道
   shape 过滤(有 package.json + 有插件入口 + 非 fork)。
4. **耦合深度与装载成功率负相关但原因可解**:耦合 top 的失败全是闭包,
   不是接口不兼容——接口面(ctx 操作)在官方闭包就位后没有任何失败。

## 实验设计

- **宿主组合**:真 dsh 服务三件套(SystemPrompt + ToolRuntime +
  AgentRegistry)on vendor cordis(源码模式)。M1.5 测闭包不测基座,
  基座可换性 M0 已证。
- **闭包模拟**:官方 236 包按 `gen-pkg-map.cjs` 生成名→src 映射,经
  vitest alias 接进样本(等价于"宿主闭包含全部官方包");社区内部包与
  第三方不预装——缺失即发现。样本走 `import.meta.glob` 强制进 vite
  转换管线(node 原生 ESM 解析看不见 pnpm workspace 链接,这是两轮
  基建调试的根因)。
- **L0 判定**:import + 插件面提取(default ?? module)+ `ctx.plugin()`
  20s 超时保护(防 inject 门控永久等待);失败分类 npm-closure /
  inject-wait / apply-error / shape。
- **L1 判定**:装载后 `tools.schemas()` 非空(工具面)∨
  `systemPrompt.assemble()` 段数较装载前增加(prompt 面)。
- 样本构成:3 重耦合(mstar 族,值级导入最重)+ 1 workspace 成员 +
  1 工具类 + 3 轻/中 + 2 特殊形态(fork / VSCode,预期失败样本)。

## 边界

1. 别名机制模拟的是"官方闭包就位";M2 真宿主用真 node_modules 装配,
   闭包结论(官方可满足/缺口在非官方)按构造迁移,但**安装体积与时长**
   要到 M2 才有实测。
2. L1 事件面未测(总线监听不可从 ctx 公开面枚举);prompt 面测的是段数
   delta,未验内容保真。
3. 10 仓是横截面抽样,分层配额(服务分布)是 M7 的做法;本矩阵的失败
   分类清单才是交付物。
4. inject-wait 分类 0 例:样本选取未覆盖"缺宿主服务"的形态,M2 全栈
   宿主(服务面更全)下预期同样罕见。
