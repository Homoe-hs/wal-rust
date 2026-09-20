# wal-rust 文档总览

**先看这里**: 本文件是 docs/ 的唯一入口。新增文档必须登记到下面的表格, 否则
`python3 scripts/check_docs.py`(CI 的 `docs` 阶段)会报"索引不全"。

## 按任务找文档

| 我想…… | 看这篇 |
|---|---|
| 学会 WAL 语言、查算子用法 | [`wal-编程手册.md`](wal-编程手册.md) |
| 搞清 x/z 在各算子上的行为 | [`4-state-semantics.md`](4-state-semantics.md) — **权威定义** |
| 搞清查 `(get)`/边沿/`count`/`find` 的语义 | [`query-engine-design.md`](query-engine-design.md) §1 — **权威定义** |
| 加一个新波形后端(VCD/FST/FSDB 之外的) | [`query-engine-design.md`](query-engine-design.md) + `src/trace/trace.rs` 的 `Trace` trait |
| 动 FSDB 后端 / 排查 NPI 问题 | [`fsdb-npi.md`](fsdb-npi.md) |
| 改性能路径前先看历史教训 | [`waveform-io-plan.md`](waveform-io-plan.md)、[`152gb-round.md`](152gb-round.md) |
| 知道每个版本改了什么 | [`../CHANGELOG.md`](../CHANGELOG.md) |
| 参与开发/发版 | [`../CONTRIBUTING.md`](../CONTRIBUTING.md) |

## 文档清单

| 文档 | 行数 | 性质 | 状态 |
|---|---|---|---|
| [`wal-编程手册.md`](wal-编程手册.md) | 2663 | 使用手册 | ✅ 现行(面向使用者, 随功能更新) |
| [`4-state-semantics.md`](4-state-semantics.md) | 104 | 规范 | ✅ 现行(四值语义**唯一权威**) |
| [`query-engine-design.md`](query-engine-design.md) | 219 | 设计 | ✅ 现行(§1 查询语义权威; 后半是当初的方案, 实现进度见文内标注) |
| [`fsdb-npi.md`](fsdb-npi.md) | 319 | 设计 + 实测 | ✅ 现行(FSDB 后端; §9 多文件/缓存规则) |
| [`waveform-io-plan.md`](waveform-io-plan.md) | 154 | 调研 + 计划 | 🟡 部分落地(IO-1..6 的进度见文内) |
| [`agent-cli.md`](agent-cli.md) | 163 | 草案 | 🟡 草案 v0, **未实现**, 只作为接口约定讨论稿 |
| [`migration-0.8-0.11.md`](migration-0.8-0.11.md) | 37 | 迁移指南 | 🟡 历史区间(0.8→0.11), 供老用户对照 |
| [`152gb-round.md`](152gb-round.md) | 202 | 复盘 | 📦 复盘(当时的结构与数字) |
| [`internal-feedback-review.md`](internal-feedback-review.md) | 38 | 复盘 | 📦 复盘(0.10.10 批次) |
| [`history/construction-0.5.0.md`](history/construction-0.5.0.md) | 216 | 归档 | 🗄 历史(0.5.0 构建文档, 仅存档) |

## 约定(为什么这么定)

1. **每篇文档开头一行状态横幅**: `✅ 现行` / `🟡 计划或草案` / `📦 复盘` / `🗄 历史`。
   读者一眼知道"这篇说的是现在的行为, 还是当时的情况"。
2. **历史文档不追改**: 复盘/归档文档里的路径、版本号、数字保持当时的样子, 只在文件头部
   加一行 `> ⚠️ 历史文档` 横幅。`scripts/check_docs.py` 对带横幅的文档把"路径已不存在"
   降级为提示 —— 篡改历史比留下过时路径更糟。归档文档统一放 `docs/history/`。
3. **权威只有一个**: 语义类问题以「四值语义」和「查询引擎 §1」为准; 版本变更以 `CHANGELOG.md`
   为准; 其他文档与它们冲突时, 以后者为准, 并提 issue 修。
4. **数字要能被核对**: 文档里写"约 N 个测试""N 行"这类可核对数字时, 写成可解析的形式
   (如 `cargo test  # all Rust tests (~290)`), CI 会与仓库实际值比对(偏差 >30% 时提示;
   注意 `cargo test` 会把 lib 单元测试在 bin target 里再跑一遍, 实跑数大于静态计数)。
5. **新增文档**: 放 `docs/` 下、登记到本文件、写清状态与适用范围; 面向使用者的内容优先并入
   `wal-编程手册.md` 而不是新开一篇。

## 相关但不属于 docs/ 的文件

| 位置 | 内容 |
|---|---|
| [`../README.md`](../README.md) | 项目门面: 安装、30 秒上手、特性、架构速览 |
| [`../CONTRIBUTING.md`](../CONTRIBUTING.md) | 开发流程: 本地 CI、提交规范、发版步骤 |
| [`../AGENTS.md`](../AGENTS.md) | 给 AI 编码代理的项目说明(结构与约定, 与 CONTRIBUTING 互补) |
| [`../CHANGELOG.md`](../CHANGELOG.md) | 每个版本的用户可感知变化 |
| [`../bench/RESULTS.md`](../bench/RESULTS.md) | 性能数字与复现方法(样本不入库) |
| [`../examples/wal/`](../examples/wal/) | 可直接运行 WAL 示例脚本 |
