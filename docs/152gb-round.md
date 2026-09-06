# 152GB 大波形内测轮 — P0 修复记录

> 触发: 内测 152GB VCD + 对应 FST(585MB VCD 转换)。
> 版本: P0-a 随 v0.11.14 发布;P0-b 分两次落地 — v0.11.15(读值语义统一)与 v0.11.16(扫描起点统一)。

## P0-a · `(count (&& ... (is-x ...)))` 丢谓词 — 已修复 (v0.11.14)

`(count (&& (= en 1) (is-x data)))` 只对 `en` 计数,`is-x` 被静默丢弃。
decompose 路径对不可解析子条件直接跳过 → 修复为返回 `Ok(None)` 落入逐拍回退。
回归: `tests/wal_integration_test.rs` compound-is-x。

## P0-b-1 · 同一时间戳多次跳变(毛刺/delta 周期)读值不一致 — 已修复 (v0.11.15)

本地复现: 45-bit 波形(vcd2fst 转 FST,每 7 拍一次同拍双跳变)上,VCD 的
`get`/`at`/`change_points`/解码缓存取**第一个**写入值,而 `find_indices`/
batch 扫描取**最后**一个;FST 的 wellen 同索引组(`DataOffset.elements`)
按组内**第一**个读。→ get 与 count 同一索引语义相反。

修复(统一为 **per-index 最后写入值胜出**,符合 VCD 规范):
- `src/trace/vcd.rs::find_signal_in_block` — 返回块内最后一次出现;
- vcd.rs find_indices 缓存合并 — 同索引保留最后一项;
- `src/trace/fst.rs::signal_value` — `get_value_at(&d_off, elements - 1)`;
- fst.rs `find_indices`/`change_points` — 先按索引折叠到最后一个值再逐索引求条件。

回归: `trace::vcd::tests::test_glitch_timestamp_last_value_wins`、
`tests/fst_wellen_backend_test.rs::test_wellen_backend_glitch_same_index`。

## P0-b-2 · count/find 回退路径从游标处起扫,而非全时间线 INDEX 0 — 已修复 (v0.11.16)

内测 0.11.15 复测确认: FST ≥3 次 `(step 57)+(get D)` 后,`(count (= (get D) v))`
(v 为变量)= 184(字面量恒 301)。根因(内测 §7.6 更正):

**变量-RHS 的 count 落逐拍回退后,从全局 INDEX 当前值开始扫描;字面量-RHS
路径则总是从 0 起扫(fast path find_indices)。** `step` 是相对推进
(`(step 0)` no-op;每次 `(step 57)` 累加),三条 `(step 57)` 后
INDEX=57→115→174;命中区恰为 174..357 → 184 = 357-174+1;字面量从头扫 = 301。
VCD 同构造同样命中过 — 同一缺陷,只是 FST 侧"扫描后回写 INDEX"分支差异
使现象更明显。

修复: `count`/`find`/`find/g`/`step_scan(count/step、find/step)` 的逐拍回退
**一律先把游标重置到 INDEX 0 再扫描,结束后恢复原游标**(与 whenever 回退
既有行为一致;字面量 fast path 本就如此)。

验证(本地 al45c,step 174 后):
- 修复前: var=184、字面量=301;修复后: var=301=字面量=301,`count/step`=301,
  两次调用结果稳定,`INDEX` 保持在 174(VCD 与 FST 完全一致)。
- 回归: `tests/wal_integration_test.rs::test_count_fallback_scans_from_zero_after_step`。
- 差分门禁 `scripts/diff_find.sh` ALL MATCH;全套 220 测试通过。

> 注: 0.11.15 的 delta-cycle 修复与 0.11.16 的扫描起点修复互相独立;
> 内测"0/1/3 次 get 后 301/184"的根因是后者(INDEX 起点),不是游标解压。
