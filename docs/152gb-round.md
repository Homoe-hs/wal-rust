# 152GB 大波形内测轮 — P0 修复记录

> 📦 **复盘文档** —— 记录 152GB 大波形内测轮当时的问题与修法, 其中的路径/数字为当时状态, 不随代码更新。
> 当前行为以 `query-engine-design.md` §1 与 `4-state-semantics.md` 为准。

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

## 内网 #1 续 · FST 向量 panic —— 已定位为**上游 vcd2fst 写出坏 FST**(v0.12.25)

**复现(本机已完整复现)**

```bash
cat > tiny.vcd <<'V'
$timescale 1ns $end
$scope module t $end
$var wire 8 ! d $end
$upscope $end
$enddefinitions $end
#0
0!          # ← 向量用单字符值(VCD 允许的简写)
#10
1!
V
vcd2fst -v tiny.vcd -f tiny.fst
fst2vcd tiny.fst | grep b     # → b0!   defi !   (值里混进 VCD 文本/垃圾)
```

`fst2vcd` 自己也还原出垃圾,说明**文件已损坏**(vcd2fst 对"向量单字符值"
的长度处理错误),不是 wal-rust 读错。wellen 在解码时 `panic!("Unexpected
signal value: 0!   defi")`。

**0.12.25 的产品行为**(此前:原始 panic 文本 + 误导性 "signal not found" +
`count` 静默返回 0):

```
error: FST 信号 t.d 的数据: Unexpected signal value: 0!defi。该 FST 值无法解码
(文件可能已损坏:常见于 vcd2fst 处理含'向量单字符值'(如 0!/1!)的 VCD 时写出坏文件;
请把向量值写成 b<bits> 形式后重新生成)
```

- wellen 调用统一走 `quiet_guard`:捕获 panic 并**抑制默认 panic 打印**;
- 失败记入 `FstTrace::fatal`,顶层 `Evaluator::eval` 统一上报
  (查询快路径里的 `.ok()` 吞错不再能造成"看似正常的 0");
- 健康信号不受影响(`(get "t.c")` 正常)。

**给内网的绕过方法**:生成 VCD 时把向量写成 `b<bits>` 形式
(`b00000001 !` 而不是 `1!`),或改用 VCD 直接喂给 wal-rust(我们只读波形)。

## 内网 "NE/changes 漂移" —— 已用随机差分复现并修复三处(v0.12.26)

内网报的 NE/changes 计数漂移本地不可复现,于是新增
`tests/fuzz_vcd_fst_diff.rs`:**确定性伪随机波形**(含 x/z、delta 周期、毛刺、
45/128 位向量)× 同一波形写 VCD 与 FST × 一批表达式逐值对拍。立刻复现出三类缺陷:

1. **FST 向量信号没有边沿**(`src/trace/fst.rs`)
   `find_indices` 用 `sv_as_bit`(多位返回 None)比较 prev/cur → `(count (rising s))`
   对向量信号恒为 0;VCD 侧正常。已改为与 VCD 同口径:prev 全 0 且 cur 为
   "确定的非零" 即 rising。
2. **短名/叶子名解析只在部分路径生效**(`src/trace/{vcd,fst}.rs`)
   `op_get` 会解析短名,`rising/falling/changes/is-x/is-z` 直接按字面名读 →
   短名场景下这些谓词静默 false(`count/step`、`find`/`whenever` 回退路径全中招)。
   解析已下沉到 trace 层并带缓存,所有路径共用。
3. **FST writer 时间段压缩约定**(`src/fst/writer.rs`)
   fstapi 约定:压缩无收益时存原始字节(clen==uclen)。旧实现总是写 zlib 数据,
   当压缩后长度恰好等于原始长度(例如 11 个时间点、每步 10 → 11 字节)时,
   读端按"未压缩"解析 → 整块报废(wellen `I/O operation failed`、fst2vcd 输出乱序时间)。
   已按约定加回退。

**复现/回归**:`cargo test --test fuzz_vcd_fst_diff`(默认 40 波形,可用
`WAL_FUZZ_N=300 WAL_FUZZ_SEED=...` 加码;失败时自动把波形写到 `.tools/fuzz_fail.*`)。

## 现场报告处置(0.12.37)

| 编号 | 现象 | 处置 |
|---|---|---|
| B1 | `(== a b)` 恒假 | 词法把 `==` 拆成两个 `=` → 语法树 `(= = a b)`。解析后归一化 `(= = …)`/`(= = = …)` → `(= …)`,`!==` 同理。**修复** |
| B2 | 1bit 标量 x 的三路口径不一致(`x == 0` 有的真有的假) | `VcdValue::to_i64`/FST `sv_to_i64` 对 x/z 返回 None;`(get)` 的位切片对 x/z 返回位串 `"x"`。**修复**(与向量口径一致) |
| B14 | dumpvars 初值 → 首条变化 的跨界边丢失 | 新增 `Trace::defined_initial_value`:索引 0 的前驱 = **确定的** $dumpvars 初值(初值含 x/z 或后端无初值概念 → 仍无前驱)。变更列/逐拍/区间引擎三条路径统一。**修复** |
| B6 | 未知信号静默退 0 | count/find/whenever 快路径与 `rising/falling/changes/is-x/is-z` 全部改为报错(`signal '…' not found`);顺带让这些谓词支持多 trace。**修复** |
| B11 | .gz/.bz2 静默退化 | 加载前按 magic(1f 8b / BZh)明确报错"请先解压"。**修复** |
| B8 | FSDB 直接 panic / 错误信息误导 | 按扩展名与 magic 识别 FSDB,给出"FSDB 暂不支持,请转 VCD/FST"的明确错误。**当时已修复** ——⚠️ **本条与下面"FSDB 被拒绝"只适用于 ≤0.12.38;0.14.x 起 FSDB 已支持**(借 Verdi NPI 直读, 见 `docs/fsdb-npi.md`) |
| 截断 VCD | 静默接受 | 缺少 `$enddefinitions` 时打印截断告警。**修复** |
| B3 | `dump-trace` 写 `.fst` 名不副实 / 字符串值当位串写 / 时标用索引号 | `.fst` 路径明确拒绝;非波形值(字符串/闭包)跳过并告警;输出改用源波形真实时间戳。**修复** |
| B4 | `(doc <符号>)` 报错、核心算子无文档 | `(doc sym)` 接受符号;补齐 get/at/rising/falling/changes/is-x/is-z/count/find/whenever/=/==/!=/&&/\|\|/not/div/help 等条目。**修复** |
| B5 | `/` 返回浮点,硬件语境要整除 | 新增 `(div a b)` 整数除法(向零取整);`/` 保持浮点。**新增** |
| B7 | RSS 27–32GB 与"<2GB"宣传不符 | 计量口径问题:波形经 mmap 读取,RSS 含**文件页驻留**;堆内存为 O(信号数+变更列)。文档已写明口径(见 waveform-io-plan.md)。跨进程旁挂列缓存(0.12.36)已消除重复全文件扫描 |
| B9 | 脚本模式 + 深层宽信号挂起 >14min | 本地未复现;需要最小复现脚本 |

**语义确认(非 bug)**:`count <wave> <sig>` 默认 `=1`(值计数,非变化计数);`changes` 只计跳变、`topsig` 计"值事件"(含初值),两者相差 1 是定义差异。

## 真实 VCS 波形验证(0.12.38)

用 VM 里的 VCS O-2018.09-SP2 生成了两类真实波形(自建目录, 未触碰他人工作目录):

**A. 握手/初值/dumpoff 场景**(3 层层次、512b/1024b 向量、`$dumpvars`、`$dumpoff`/`$dumpon`):

| 查询 | 结果 |
|---|---|
| `(count (rising "valid"))` | **12**(与 TB 里 12 次握手完全一致) |
| `(count (rising "clk"))` / falling / changes | 29 / 30 / 61 |
| `(count (is-x "clk"))` | 1(dumpoff→dumpon 之间只有 1 个索引, 与文件结构一致) |
| `(getwave "tri_sig")` | `((0 "z") (245000 "x") (345000 "z") (385000 0))` |

**VCD ≡ FST**(`vcd2fst` 转换后)在 14 条查询上**全部一致**,含 512b/1024b 向量、
x/z、dumpoff 窗口、`is-x`/`is-z`。

**B. 深层宽信号**(30 层层次、每层 1024b 寄存器):`-c` 内联与脚本模式均为 0.01s
(B9"脚本模式挂起"未复现;需要现场最小复现)。

**FSDB**(⚠️ **≤0.12.38 的现场**):VCS 直接产出的 `.fsdb` 被明确拒绝并提示转换(不再 panic)。**0.14.x 起 FSDB 已支持**。

**注意**:VCS 的 `$dumpvars` 块写在 `#0` 之后且内容是 t=0 的**最终**值,因此
"dumpvars 初值 ≠ #0 值"这种跨界边在 VCS 输出里不出现;B14 的修复仍按语义生效,
由合成夹具 `matrix_dumpvars_first_edge_at_index_zero` 覆盖。

**其他**:歧义信号名(`wide` 在 5 个层次都有)现在报"ambiguous + 候选列表",
不再误报"not found"。
