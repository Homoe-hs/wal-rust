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

## 0.11.17 · 遗留小项(§8.4 三连)处理

### #1 FST/VCD is-x 差 1 — 修复(双根因:初值 x 表示分裂 + 首采样归属)

内测重测(t0 即 x、首个实值 2^36 在 t=33125000fs、clk 前 3 索引 FST(2 4 6) vs
VCD(1 3 5)索引域整体偏移 1)给出的查点:"初值样本在 reader 的两条路径中是否
共享同一个 x 表示"。本地同构(isx1.vcd:8-bit t0 无值、#10 显式 x、#20 实值)
修复前 VCD isx=3 / FST isx=2 且 chg VCD 4 / FST 3。

- **FST `find_indices`**: 首变化之前的初值 x 现在从 INDEX 0 起参与条件求值 —
  电平类(is-x/Neq/…)把 x 段计入区间;边沿类把 x 作为 prev 值
  (x→1 算 changed,x 不是 0/1 所以不算 rising/falling),与 VCD 逐拍路径一致。
- **VCD `signal_value`**: 初值 x 统一为全宽带向量(8 位信号 → Vector(x×8),
  与 "bxxxxxxxx" 同形),get/at/change_points/逐拍 (changes s) 与 find_indices
  共享同一表示,消除 Bit(x) vs Vector 的伪变化计数;
  find_indices 的 Changed 用语义相等比较(全 x/全 z 归一)。
- **FST `find_indices` Changed**: 同样归一(x-run "x" vs "xxxx…" 不算变化)。

回归: `trace::vcd::tests::test_initial_x_unified_across_paths`、
`tests/fst_wellen_backend_test.rs::test_wellen_backend_initial_x_semantics`;
isx1.vcd/isx1.fst 上 VCD=FST(isx 3=3、isz 1=1、chg 3=3、(changes) 列表一致)。
差分门禁 ALL MATCH。

### #2 SCOPES 帮助文案残留 — 帮助条目更新

真实 API 为 `(all-scopes "")`(带参,返回作用域列表);`(help)` 的 Access 行
原写 SIGNALS SCOPES。已改为 `SIGNALS all-scopes`,并让 `(SCOPES)` 零参调用
等价于裸 `SCOPES`(与 SIGNALS 的调用形式约定一致,兼容旧脚本)。

### #3 dump-trace 位宽没接线 — 修复(导出器按信号真实宽度)

dump-trace 是"当前游标处虚拟信号快照"导出器;此前 `$var` 位宽按 Int 硬编码
32 且高比特被截断(45 位值导出成 reg 32、只写首采样)。现改为:
- 值本身定宽(Int 按实际比特长度,String 位串按长度);
- 再按被引用底层信号的声明宽度扩宽(45 位 VCD → `$var reg 45 … [44:0]`),
  写循环按声明宽度补零/截断,变化检测不再被截断破坏。
- 说明: 非全轴导出(合成 #1 #2 #3 刻度 + 首值快照)属该工具既有定位,未改。

(本地: al45c.vcd 上 dump-trace → $var reg 45 ×2,7400 条变化行。)
