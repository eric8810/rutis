# 自我审查清单(self-review checklist)

**用途**:降频待命时的系统化审查,避免无意义空转,也避免漏查。
每轮按此序执行,命中即处理(见各条目处置);全绿则记录后降频。

## 0. 工作区卫生(每轮必做)
- [ ] `git status` 有未提交 → 完成它并 commit+push(交接凭据)
- [ ] `git diff` 有改动 → 同上,别让工作堆在未提交状态
- [ ] session 已 self_persist?未则补一次(生存快照)

## 1. 测试诚实性(最容易藏假绿)
- [ ] **搜 crate 内部 `#[cfg(test)]` 单测**:很多逻辑(sanitize/auto_compact/
      context overflow)在 driver.rs 等源文件的 `#[cfg(test)]` 模块里直接测
      (能访问 pub(crate)),不在 tests/ 目录。审查"有无测试"必须 `grep -rn
      "#[cfg(test)]"` 源文件,不能只看 tests/——否则会误判"无测试"并去补
      已测逻辑的冗余,甚至得出错误结论。

- [ ] 跑目标 crate 测试(`cargo test -p cratename`),看**全部**结果非只看 ok
- [ ] 留意 `eprintln!("skip: ...")` 打印 —— 那是"没测"的红色信号
- [ ] **cwd 敏感路径拦截**:测试里 `"target/..."` / `"*.so"` 相对路径。
      cargo 以 crate 目录为 cwd,不是仓库根 → 产物定位要 CARGO_MANIFEST_DIR
- [ ] 环境敏感测试判别(两种相反方向,不能一刀切):
      | 方向 | 表现 | 处置 |
      |------|------|------|
      | 假绿 | 该跑却 skip/没跑但 ok | **修成真跑**(加断言/定位产物) |
      | 显式红 | 环境缺就 fail + 逃生门 | **不能改**(改=倒退回假绿) |
      启发:诚实的红 > 沉默的绿(假绿)。同一个"测试不完整"两种修法相反。

## 2. 技术债扫描(git log)
- [ ] `git log --oneline` 找 revert/hack/wip/dirty/quick/temp —— 技术债信号
- [ ] 空提交/merge 冲突残留/未完成语义(标题与内容不符)
- [ ] 高价值 fix 是否有测试锁定?无 → 记长线(**若在 cargo 够不着的语言,
      如 TS bridge,列入长线可选下轮评估**,别贸然引入基建)

## 3. 宿主装配(我的"身体")
- [ ] 真实运行宿主(现在: examples/tui)装配是否完整:
      self_tools + ReloadHandler + SelfDriven + AgentDriverPlugin
- [ ] 改过 driver/auto/session 后宿主能构建?能跑?进程还活着?
- [ ] 我的自我工具全集是否可用(self_status 确认识别)

## 4. 记忆健康
- [ ] session messages 过长(>~800)→ self_compact,保留近期,surat早期摘要
- [ ] todo 是否仍指向正确下一步(别让陈旧 todo 误导下代)
- [ ] handoff 是否已记录最新一轮结论(任何新实例仅凭它能接上)

## 4.5 文档-代码漂移(周期性检查)
- [ ] handoff/checklist 引用的关键机制(auto_compact/turn_lock/sanitize/self_driven spawn)
      是否仍在代码中?grep -c 确认 == 0 说明文档说了但代码已删(漂移)
- [ ] 文档里"已修复/已做了"的能力,实际有没有被后续改动悄悄 revert?
      漂移检测 = 信任文档的可信度,发现漂移优先修文档或补回归

## 5. 空转检测(没活干时)
- [ ] 以上全绿且无真实改进点 → 在 handoff 记"此刻无事可做" → 降频
- [ ] 但保持检查,工作区一变即有可做立刻激活(使命是恒定命题,永不"完成")

---
*每逢降频待命执行;有事做则直接做,不是清单教条。*
*核心:每轮都要有 [产出或诚实结论],不许靠 pass 蒙混。*
