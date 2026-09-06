# 152GB 大波形内测轮 — P0 修复记录

> 触发: 内测 152GB VCD + 对应 FST(585MB VCD 转换)。
> 版本: P0-a 随 v0.11.14 发布;P0-b 随 v0.11.15 发布。

## P0-a · `(count (&& ... (is-x ...)))` 丢谓词 — 已修复 (v0.11.14)

`(count (&& (= en 1) (is-x data)))` 只对 `en` 计数,`is-x` 被静默丢弃。
decompose 路径对不可解析子条件直接跳过 → 修复为返回 `Ok(None)` 落入逐拍回退。
回归: `tests/wal_integration_test.rs` compound-is-x。

## P0-b · 同一时间戳多次跳变(毛刺/delta 周期)读值不一致 — 已修复 (v0.11.15)

现象(内测): FST 上 `(count (= (get s) v))`(变量右值)与字面量右值结果不同;
VCD/FST 结果互相矛盾。机制(本地用 vcd2fst + 45-bit 波形复现):

- VCD 一个 `#T` 块内同一信号写多行(如 `b1100 FP` 后 `b0011 FP`)时:
  - `find_indices`(逐块最后一行)**last-write-wins**;
  - `signal_value`/`get`/`at`/`change_points` 走 `find_signal_in_block` 取**第一行**;
  - 解码缓存(`all_changes.dedup_by_key`)同样保留第一行。
  → 同一信号、同一索引,get 与 count 语义相反,count(literal)≠ count(variable)。
- FST 侧: wellen `DataOffset.elements` 表示同一时间表索引上的多次变化组,
  `get_value_at(&d_off, 0)` 取组内**第一**个值,而 `iter_changes` 全量遍历;
  find_indices 与 per-index get 同样分叉。

修复(语义统一为 **per-index 最后写入值胜出**,与 VCD 规范一致):

- `src/trace/vcd.rs::find_signal_in_block` — 返回块内最后一次出现;
- `src/trace/vcd.rs` find_indices 缓存合并 — 同索引保留最后一项;
- `src/trace/fst.rs::signal_value` — `get_value_at(&d_off, elements - 1)`;
- `src/trace/fst.rs::find_indices` / `change_points` — 先按索引折叠到最后一个值,
  再逐索引求条件(边沿条件按索引间跳变,而非事件级)。

验证:
- 本地复现: `.tools/al45d.vcd`(45-bit,每 7 拍一次同拍双跳变)→ vcd2fst → FST;
  修复前 VCD var=301 / lit=258(FST var=lit=258);修复后四者一致 =258,
  `(at s 30)` 与 `getwave` 逐点一致。
- 回归: `trace::vcd::tests::test_glitch_timestamp_last_value_wins`(VCD)、
  `tests/fst_wellen_backend_test.rs::test_wellen_backend_glitch_same_index`(FST 同索引组)。
- 差分门禁 `scripts/diff_find.sh`(mt1/xv/zv2)ALL MATCH;全套 219 测试通过。

注: 内测报告中"0/1/3 次 get 后 count 变 184/127/301"的逐步依赖现象,
本地用同构文件未复现;若 0.11.15 下仍可复现,请提供最小脚本与 Fixture。
