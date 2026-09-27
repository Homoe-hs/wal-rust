# wal-rust 大文件性能基准(2026-09-06)

**机器**: Arch x86_64 16U (num_cpus), NVMe SSD, glibc 2.44;wal-rust 0.11.11 release(LTO)
**目标**: 150GB VCD 单信号查询 ≤ 1 分钟

## 测试资产(合成,与真实 152GB 同构:每时间戳 ~1100 变化、稀疏)

| 文件 | 大小 | 信号数 | 时间戳 | 变化(约) |
|---|---|---|---|---|
| bench/data/small.vcd | 76MB | 20k | 2000 | 2.2M |
| bench/data/bench_2g.vcd | 11.5GB | 700k | 300k | 330M |
| bench/data/bench_10g.vcd | 58.7GB | 3.5M | 1.5M | 1.65G |

## 结果

| 阶段 | 11.5GB | 58.7GB | 外推 150GB |
|---|---|---|---|
| 纯加载(pass-1b 索引) | 58.5s | **6:00(360s)** | ~13.5min |
| 冷查询(加载+首个 count) | 3:09.7(190s) | **11:14(674s)** | ~27min |
| 单查询扫描(扣除加载) | **131s** | **314s(5:14)** | ~13min |
| 同信号第二次查询(同进程) | 33s | 208s(缓存部分命中) | ~5-10min |
| 峰值 RSS | 9.6GB | 11.4GB | ~20-30GB |

**缩放率**: 查询时间 ≈ 0.47-0.58GB/s(有效吞吐,逐行解析);RSS 缓增(信号数主导稀疏索引)。

**结论**: 当前实现有效吞吐 ~0.58GB/s(逐行解析 + 每信号 BTreeMap);
150GB 单信号查询 ≈ 28 分钟(不含加载),**与 152GB 实测"数分钟-数十分钟"吻合**。

## 门禁与工具

- `scripts/gen_big_vcd.py` — 同构合成生成器(参数: 信号/时间戳/每拍变化/种子)
- `scripts/run_bench.sh` — 四项测量(加载/冷/热/RSS, `/usr/bin/time -v`)
- `scripts/diff_find.sh <old> <new> [fixtures]` — **差分门禁**: 同一 fixture 新旧实现
  9 条件(等值/非零/边沿/变化/IsX/IsZ/getwave)逐值必须一致(已通过 mt1/xv/zv2)

## 目标路线(150GB 单信号 ≤60s)

| 手段 | 预期 | 备注 |
|---|---|---|
| find_indices 内层 SIMD 化(memmem 定位 id) | 扫描 150GB ≈ 10-40s(并行,带宽受限) | 差分门禁守护;命中行才解析 |
| pass-1b 加载 SIMD 化(行定位+关键行解析) | 加载 150GB ≈ 20-60s | 需要 16 核+NVMe |
| 每块 madvise(DONTNEED) | RSS → 1-3GB | 页用完即弃 |
| 同信号重复查询 | 走 signal_cache(已有,待验证全条件命中) | 二次查询亚秒 |

**结论**: ≤60s 目标在 16 核 NVMe 上**可行**,条件是加载与查询都 SIMD 化 + 分块释放页。

## 0.12.2 (2026-09-07) — 加载采样改造 + anchored_changes 提取

| 阶段 | 0.11.11 | 0.12.0 | 0.12.2 |
|---|---|---|---|
| 加载(58.7GB 冷) | 360s | ~62s wall (user 1076s) | **42s wall (user 42s)** |
| 加载(页缓存) | — | ~12s | ~12s |
| 冷 count | 674s | 282s | **~150-175s** (disk 重读主导) |
| 同信号二次 | 208s | ms | ms(保留) |

改造: PASS-1b 改为**全局行采样**(每 100 行值行取一个锚点,解析/哈希只在采样行发生;
去掉 per-signal 计数 HashMap — 60s→42s wall 且 user 1076s→42s,CPU 26x);
锚点按信号期望密度不变;事件信号文件保留全解析路径。
find_indices/change_points 共用 `anchored_changes()`(缓存列或冷扫描),语义单一源。

## 操作级冷启动矩阵(v0.12.3,58.7GB 每进程独立)

| 操作 | wall(s) | RSS(MB) | 说明 |
|:--|--:|--:|:--|
| 加载(SIGNALS) | ~42 | ~9k* | 冷盘 ~1.4GB/s |
| count(字面量) | 150-175 | ~10k* | 锚定扫描,冷盘重读主导 |
| count(变量 RHS) | ~138 | ~4k | **0.12.3 修复**:原逐拍回退 >90s(11.5GB) |
| count-rise | 227 (冷) | 2.9k | 同一扫描路径,磁盘缓存状态波动大 |
| count-is-x | 227 (冷) | 2.8k | 同上 |
| find-rise(length) | 82 (暖) | 2.6k | 缓存热后 3x |
| at(一次性) | 223 (冷) | 2.8k | 首次全变更列表;后续(缓存列)ms |
| getwave(全历史) | 226 (冷) | 2.8k | 固有全历史 |

*旧式早期测量含 mmap 驻留混杂;新进程模型 RSS 稳定 2.6-2.9GB(madvise 生效)。

结论: 冷盘下每个新信号的首次扫描 ≈ 225s @58.7GB(外推 150GB ≈ 9min);
同进程后续同信号查询毫秒级。改善路径: 加载期构建被引用信号列边加载边可查
(IO-3,内存换磁盘),或转换 FST 路径(wellen 横格零扫描)。

## 0.12.3 — count-var 快路径
`(define v (get s)) (count (= (get s) v))` 原走逐拍回退(每拍一次完整值读)
→ 0.12.3 在查询期把绑定变量替换为字面量,走锚定扫描;11.5GB >90s→26s。

## 0.13.0 (2026-09-10) — 信号头 arena + 开放寻址(A1)

动机: 4M 信号波形光**解析 header** 就要 2.3-2.9s / 1.2GB,而 4M 个 `$comment`
只要 0.15s —— 代价是**每信号**的(独立 `Arc<str>` + `FxHashMap` 名字表 +
每信号 `HashMap` 初值条目),不是每字节的。

改造三件:
1. 名字 arena: `names_blob: Vec<u8>` + `name_meta: Vec<u64>`(低 32 位偏移 /
   高 32 位长度),`Arc<str>` 与逐信号分配全部消失;
2. `OpenIndex`: 开放寻址哈希表(线性探测,`mix64(h)` 打散 —— 直接用 FNV 低位
   是周期性的,4M 信号会退化到 19.4s),名字表与 ID 表共用;
3. `$dumpvars` 初值改紧凑 blob(`init_off: Vec<u32>` + `init_blob: Vec<u8>`),
   取代 `HashMap<u32, VcdValue>`。

| 指标(交错 A/B 两轮) | 0.12.45 | 0.13.0 |
|:--|--:|--:|
| 4M 信号头解析 wall | 2.35–2.90s | **1.59–1.82s** |
| 4M 信号峰值 RSS | 1.21–1.24GB | **968MB** |
| 1M 信号(83MB) wall | 0.63s | **0.45s** |
| 1M 信号 + dumpvars 初值 RSS | 347MB | **282MB** |

语义零变化: `tests/regression_matrix.rs` 38/38、`tests/fuzz_vcd_fst_diff.rs`
N=300 三闸全绿、`scripts/diff_find.sh` ALL MATCH;bench_2g `(count (= (get "s0") 1))`
= 77629、bench_10g(58.7GB) = 92871 与旧版一致。
跨进程缓存向后兼容: v0.12.x 写的 `.wcol` + 旁挂 `.col` 被 0.13.0 直接命中
(58.7GB 同查询 **2.47s**)。

## 0.13.2 (2026-09-10) — 懒索引 + 变更列融合(A3)

`(load)` 以前无条件整扫一遍 dump 区(建时间戳表/采样锚点/事件点), 而一次查询还要
再扫一遍 —— 冷盘上同一次查询读两遍文件。改造:

1. **load = 只读文件头**(scope/`$var`/`$dumpvars` 初值) + mmap;
2. dump 区索引 `DumpIndex` 由 `dump()` 懒构建(OnceCell);
3. 查询前能声明信号时(`Trace::prepare` ← `interval_scan`/`find_indices`), 索引与这些
   信号的**完整变更列**在**同一次遍历**里算出 → 冷启动只读一遍文件;
4. 冷文件按 64MB 窗口 `madvise(MADV_WILLNEED)` 预读, 扫完 `madvise(DONTNEED)`。

| 指标(58.7GB, 同机) | 0.11.11 | 0.13.0 | 0.13.1 | 0.13.2 |
|:--|--:|--:|--:|--:|
| `(SIGNALS)`(= 只加载) | 360s | 82s(冷)/42s(暖) | 62.8s | **1.08s** |
| 冷查询 `(count (= (get "s0") 1))` | 674s | 216s | 109s | **72.6s** |
| 11.5GB 冷查询(同表达式) | 190s | 41.6s | 12.5s | **10.5s** |
| 983MB 真实 VCS `(count (rising r000))` | — | — | 0.39-0.52s | **0.29-0.42s**(CPU 减半) |

**物理下限**: 本机 NVMe 顺序读 1.2-1.6GB/s(`dd` 单路与 4 路并行都是这个量级),
58.7GB 读一遍 37-49s, 150GB ≥94s。冷查询 ≤60s 在这台机器上做不到(只能不整读文件:
旁挂列缓存命中 2.5s, 或把常用信号列做成常驻索引)。

**踩坑(记一次)**: 融合路径的"廉价预筛"(先比行尾字节、再比 `<id>` 结尾)漏了用解析出的
ID 复核 —— 行 `1 11` 以 `1` 结尾、前一字符还是值字符 `1`, 但它的 ID 是 `11`。结果是
bench_2g 上 `(count (= (get "s0") 1))` 由 77629 变 30741。矩阵/diff 闸当时是绿的
(改动后没重跑矩阵 + 构建缓存), 靠"数字对不上"才发现。修正后新增
`matrix_digit_id_scalar_suffix`, 并确认该用例在带 bug 的构建上会失败。

## 夹具重建(生成器与耗时,2026-09-10 记录)

`bench/data/*.vcd` 是**合成夹具**,不进 git,可按需重建(生成器 `scripts/gen_big_vcd.py` 在库内):

```bash
python3 scripts/gen_big_vcd.py bench/data/bench_2g.vcd  700000 300000 1100   # 11.5GB, 实测 4m51s
python3 scripts/gen_big_vcd.py bench/data/bench_10g.vcd 3500000 1500000 1100 # 58.7GB, 实测 24m49s
```

`scripts/run_bench.sh` / `scripts/perf_history.sh` 依赖这些文件;`scripts/diff_find.sh`
在缺少 `bench/data/small.vcd` 时会自动跳过。性能数字本身在 `bench/perf-history.csv`
与本文里,不依赖文件常驻。

## FSDB 名字路径 @ 400 万信号(2026-09-26, 离线基准, 不需要波形文件)

现场形状: **300MB / 400 万信号**。名字表是这一档波形的头号内存开销, 所以先把它单独量出来:

```bash
cargo test --release --lib -- --ignored --nocapture fsdb_name_path_4m
```

| 阶段 | 时间 | 说明 |
|---|---|---|
| 名字进 arena(4M) | 220ms | 复用 buffer 直接 push, 不造 4M 个 String |
| 写 `.fnames`(encode) | 56ms | → **207MB** |
| 读 `.fnames`(内存 decode) | 58ms | 需要额外背一份 207MB 缓存 |
| 读 `.fnames`(**文件流式 decode**, 生产路径) | 127ms | 峰值省掉那 207MB |
| 建名字索引(哈希→下标) | 558ms | OpenIndex, 负载 ≤0.7 |
| 叶子名排序(短名解析用) | **360ms** | 曾经 3528ms(比较器里 rsplitn → 先预计算叶子 span) |

内存(4M 信号, 名字表本体):

| 项 | 大小 |
|---|---|
| arena(名字字节) | 199MB |
| spans(每信号 8B) | 30MB |
| 名字索引 | 96MB |
| 叶子序(每信号 4B) | 15MB |
| **合计** | **~340MB**(改动前"四份 String + HashMap"≈ **1.1GB**) |

> 改动前口径见 `trace::name_store::per_signal_bytes_stay_small` 的输出: 280B/信号 → 73B/信号(**省 74%**)。

## FSDB 时间线冷建 @ 400 万信号(2026-09-27, 合成夹具, 宿主原生读)

现场形状是 **300MB / 400 万信号**(VCS X-2025.06-SP3 写)。真文件没到手前, 先用同一形状的
合成夹具把"信号数这一维"量出来(生成: `vcd2fsdb` 转 4M 信号 VCD, 见 `.tools/bench/`)。
结论先给: **时间线冷建的瓶颈是"每块固定开销", 不是"过一遍数据"**。

```bash
# 夹具: 4,000,000 信号 / 200 个时间戳 / 3.6MB(名字为 4M 个 `big.sN`)
scripts/fsdb_landing_check.sh .tools/bench/v2f/many4m.fsdb big.s0
```

### 每块固定开销: `npiFsdbTimeBasedVcIter::start()` 每次 ~100~140ms

单进程、同一个查询(电平计数, 逼出全局时间线), 只改 `WAL_FSDB_CHUNK`:

| 块大小 | 块数 | eval 时间 |
|---|---|---|
| 4096(旧默认) | 977 | **112.4s** |
| 16384 | 245 | 40.2s |
| 65536 | 62 | 20.5s |
| 262144 | 16 | **14.2s** |

→ 每块固定 ≈102ms(与块内信号数无关: 4M 夹具 977 块 vs 16 块差 98s);
另一台夹具(19 信号 / 200 万时间戳)上 18 块比 1 块多 2.45s → 136ms/块, 与文件数据量也无关。
每信号边际成本 ≈ **3.5µs**(4M × 3.5µs ≈ 14s, 就是大块下的地板)。
`WAL_FSDB_KEEP_VC=1`(不每块 unload_vc)对时间没有影响 —— 开销在 `start()`, 不在卸载。

修法: 默认块大小随信号数放大 `chunk_size = clamp(signals/64, 4096, 65536)`(总块数 ≈64 封顶),
worker 分片按**文件总信号数**定块。

### 修前 → 修后(4M 信号夹具, 已入 `bench/perf-history.csv`)

| 操作 | 修前 | 修后 | 倍数 |
|---|---|---|---|
| 冷建时间线(单进程) | 112s | **14.2s** | 7.9× |
| 冷建时间线(8 路并行) | 69.6s | **12.1s** | 5.8× |
| `(count (= (get s) 1))` 冷(cache off) | 75.6s | **21.2s** | 3.6× |
| 同上 build(建缓存) | 73.8s | **17.5s** | 4.2× |
| 同上 hit(暖) | 75.6s | **4.9s** | **15.6×** |
| 只加载(名字树) | 5.65s | 6.6s | — |
| 只加载(`.fnames` 命中) | 0.85s | 0.78s | — |

峰值 RSS 全程 1.5~2.0GB(名字树 340MB + NPI/库 + arena 临时), 块大小放大**没有**抬内存。

### 暖路径为什么曾经完全不暖

时间线 `.ftl` 的落盘判据原来只有"点数 ≥256"。FSDB 的冷建代价 ≈ **信号数 × 每信号开销**,
与点数无关 —— "400 万信号 / 200 个时间点"点数不够, 于是**每个新进程都重建一次**(75.6s/次,
`hit` 与 `cold` 一模一样)。现在判据是 `timeline_worth_caching(points, build_time)`:
点数 ≥256 **或**本次建了 ≥150ms **或** `WAL_CACHE=build`(并行/单进程共用)。

### 还剩什么

* 每信号 ~3.5µs 的地板(4M 信号 = 14s): 下一步试 `npi_fsdb_load_vc_by_range` 之类的批量装载,
  或减少每信号的 FFI 次数。
* 并行加速比只有 ~1.2×(18.4s vs 22.6s): 每个 worker 各付一次名字树装载(0.8~5s)+ 各自扫
  自己那 50 万信号, 合并前没有共享。放大小块之后瓶颈已经转到"每信号成本 × 每 worker 的信号数"。
* **线性扫描类查询**(电平计数、区间引擎)按语义必须物化全局时间线; 边沿计数
  (`(count (rising s))`)走时间域, 不付这笔钱(edge 6.3s vs level 21.2s)。

## 真实现场波形: core 级 smoke test(2026-09-27 收到, 321MB / 1796 万信号)

```
aicore_smoke_test_000.fsdb   336,004,134 B   VCS Release X-2025.06-SP2_Full64
信号 17,956,098 / scope 1,620,912 / 时间跨度 0..43,776,606(1ps 时基, 43.78µs)
```
**本机 reader(X-2025.06-SP1)能直接打开**, 没有 NSIS —— 不用搬 SP3 的 NPI 也能读。

### 加载: 时间与内存(宿主原生读, 单进程)

| 操作 | 修前 | 修后 |
|---|---|---|
| 冷加载(`WAL_CACHE=off`, 走 NPI 树) | 66.9s / **17.8GB** | 54.2s / **13.0GB** |
| 建缓存加载(写 `.fnames`) | 72.6s / 17.3GB | 57.9s / **13.3GB** |
| 暖加载(`.fnames` 命中) | 13.5s / **11.0GB** | 11.1s / **6.1GB** |
| `(length (SIGNALS))`(只要个数) | 12.1s / 11.3GB | **11.0s / 6.1GB** |
| `sigs <pattern> 8`(按名字搜) | ~11GB | **6.1GB**(按下标懒遍历) |

改动三处(见 `AGENTS.md`):`.fnames` 升 v2 定长 header + 精确 reserve;冷路径边遍历边**流式**
写缓存;`(length (SIGNALS))` 与 CLI `sigs` 不再物化整张名字表(~4.8GB)。

内存账(暖加载, `WAL_DEBUG_FSDB=1` 的三段 RSS):NPI open 后 **1.43GB** → 名字就位
**6.13GB**(名字 arena 2.85GB + spans 140MB + `sigs` 18M×24B + 162 万 scope 字符串)
→ 索引建完 **6.20GB**。**冷加载的 13.6GB 峰值大头在 NPI 自己**(遍历 18M 信号期间涨到 ~10GB,
`mmap` 之外还持续增长), 不是我们的 arena —— 这一条是下一轮的目标。

### 还没量完的: 时间线冷建

`(MAX-INDEX)` / `(count (= (get s) 1))` 这类**索引空间**查询要物化全局时间线 = 整文件过一遍。
在 1796 万信号 / 4377 万时间戳上**单进程 20 分钟没跑完**(`scripts/bench_fsdb.sh` 里 1800s 超时被打掉),
所以本轮没有 level/at 的基线。已知约束:
* `-j auto`(8 worker)**不安全**: 每个 worker 要 6GB 级内存, 8 路 = 十 GB 级;
* 下一轮方向: 让 worker **只取句柄不建名字表**(分片只需要 handle), 把单 worker 压到 1~2GB,
  再谈并行;或试 NPI 的 `npi_fsdb_load_vc_by_range` / `sigdb` 批量装载。

### 这份 smoke test 到底做了什么(用新算子量出来的)

```bash
# 一次加载问完(单进程复用加载):
target/release/wal-rust --stdin -l aicore_smoke_test_000.fsdb < probes.wal
```

| 事件 | 时刻(ps) | 说明 |
|---|---|---|
| 4 个 core 的 `por_rstn` 释放 | **27,000** | core0..3 同时(复位只动这一次) |
| gnode load 引擎收到命令 `elane_rx_tvalid_i` | 39,957,869(宽 7.4ns) | e0s0 lane 一条命令 |
| tnode_top_0 **hart0** `op_inst_valid` ×11 | 39,857,558 → 40,260,469 | 全部走 `strong_barrier_exe`(`weak` 路径 0 次) |
| tnode_top_0 **hart1/2/3** 各 1 次 | ~40,00x,xxx | 三个 hart 几乎同时各发一拍 |
| tnode_top_1/2/3 的 16 个 hart 信号 | — | **全 0**(只有 tnode_top_0 在动) |
| gnode `store_0.awvalid` → 4 拍 `wvalid` → `bvalid` | 40,089,184 → 40,117,978 | 一笔 4-beat 写 |

延迟/性能(时钟 `period = 670ps` ≈ 1.49GHz):

| 指标 | 值 | 换算 |
|---|---|---|
| `(latency (rising awvalid) (rising bvalid))` | 28,794 ps | **≈43 cycle**(写响应往返) |
| AW → 第一拍 W | 5,309 ps | ≈7.9 cycle |
| `op_inst_valid → op_inst_ready`(10 对) | p50=0 / mean=1,143 / **max=11,432 ps** | 9 次当拍接受, 1 次停 17 cycle |
| `sync_holder → strong_barrier_exe`(11 对) | 全 0 ps | 组合直通(两者波形逐点相同) |
| 11 条 op 的窗口 | 402,911 ps | ≈601 cycle → **~55 cycle/op** |
| IPC(整段 43.78µs) | **1.7e-4** | 11 op / 65,296 clk 上升沿 |
| IPC(活跃窗口 ~601 cycle) | ≈0.018 | 相对"能跑的额定吞吐"极低 |

**结论**: 这是一个**上电连通性 smoke test** —— 4 个 core 同时出复位后, 空闲约 **39.83µs
(≈59.4k cycle, 占整段 99.9%)**, 然后只有 tnode_top_0 动了 ~0.4µs: 4 个 hart 各跑几条
barrier/sync 类操作 + 一笔 4-beat 写 + 一条 load lane 命令;APB/CSR 访问 0 次, HVM→SM 通路
0 次(见上)。它证明"复位、时钟、hart 唤醒、store 通路能通", **但它不是性能测试** ——
拿它算 IPC/延迟只能当"通路健康"的基线, 不能当吞吐基线。

### 时间线冷建的代价模型(2026-09-27, 分片剖析)

在 1796 万信号 / 4377 万时间戳的真波形上, 把"整文件过一遍"这件事拆开量:

```bash
# 分片(轮转取 1/N 信号), 暖 .fnames 命中, 打印各阶段耗时
WAL_DEBUG_FSDB=1 target/release/wal-rust fsdb-timeline-map aicore_smoke_test_000.fsdb 0 16384 out.part
# [fsdb] 时间线扫描剖析: 1096 信号, handle_of 0.6s / add 0.0s / start 0.4s / next×1902667 2.4s → 130595 个时间点
```

| 阶段 | 实测 | 单价 |
|---|---|---|
| `handle_of`(名字→句柄, 暖路径) | 0.6s / 1096 信号 | **~550µs/信号** |
| `iter.add()` | 0.0s | 可忽略 |
| `iter.start()` | 0.4s / 1.9M 条 | ~0.2µs/条(**装载**这一块要过一遍的记录) |
| `iter.next()` | 2.4s / 1.9M 条 | **~1.25µs/条** |
| 合计 | — | **≈1.45µs / 变更记录** |

要点:

* **代价跟着"变更记录条数"走, 不跟信号数走**。样本里 ~1700 条/信号 —— 因为时钟类信号
  独占大头(单个 `hvm2sm_adapter.clk` 就 130,590 次变化), 而普通信号只有个位数。
  所以"时间线冷建 = 把全文件的变更记录过一遍"这句话在真波形上意味着
  **每条 1.45µs**; 单进程十几分钟起, 与信号数无关的那部分(每信号几 µs)可以忽略。
* 1/64 分片(28 万信号)实测 71~96s → 外推整文件在**十几分钟量级**;
  `scripts/bench_fsdb.sh` 的 level 项(1800s 超时)因此跑不完。
* **块大小有甜点**: 4096→96s / **65536→71s** / 262144→93s(块太大反而慢: 一次要装载的
  记录更多, 峰值内存也高);`WAL_FSDB_KEEP_VC=1`(不每块 unload)无影响。
* `iter_next(time&)` 的单参数重载(不吐句柄)**没有提速**(2.4s vs 2.4s) ——
  开销在 NPI 内部逐条处理, 不在"归到哪个信号"。
* **暖路径的句柄解析是致命项**: `.fnames` 命中时句柄按名字懒解析, 18M 信号 × 550µs
  ≈ **2.75 小时**。新增 `WAL_FSDB_HANDLES_ONLY=1`(时间线 worker 默认开):
  跳过名字 arena/索引/scope, 走树只留句柄 → `handle_of` 0.0s, 代价变成一次树遍历
  (41s, NPI 自身在遍历期涨到 9.8GB RSS —— 这部分**不是**我们的 arena: 名字全跳过也照样涨)。
* **并行仍受内存卡脖子**: worker RSS 7GB(带名字) / 10GB(只句柄+树遍历),
  8 路 = 50~80GB, 在 27GB 机器(还跑着 10GB 的许可 VM)上不可行。

下一轮方向(按收益排序):

1. **别逐条走**: FSDB 文件里有**全局时间表**, 直接按格式读它就能拿到变更时间并集 ——
   纯 Rust 解析(格式知识在同项目的 `fsdb-parser`), 预期从十几分钟降到秒级;
2. **边走边扫**: 按 scope 分批(平均 11 信号/scope)边走树边喂迭代器并释放句柄,
   把 worker 常驻内存压到 GB 以下, 再谈 8 路并行;
3. 试 `npi_fsdb_load_vc_by_range` / `npi_fsdb_sigdb_*` 的批量装载(未测)。

### "边走边扫"建时间线(2026-09-27, 真波形)

常规路径在大波形上有两个坏选择: 要么先按名字把句柄一个个解析出来(**550µs/个** → 18M ≈ 2.75h),
要么把 18M 个句柄全攒着(NPI 遍历期自己涨到 ~10GB)。"边走边扫"把两者都绕开:

```text
走树 → 攒到 65536 个信号 → 塞进 TimeBasedVcIter → start → 逐条取时间并集 → unload_vc
     → **丢掉这批句柄** → 继续走
```

* **正确性**: 与常规 worker 的 `.part` **逐字节相同**(`cal1m.fsdb` 上 `cmp` 验证);
  默认在信号数 ≥ `WAL_FSDB_WALK_SCAN_MIN`(2,000,000)时启用, `WAL_FSDB_WALK_SCAN=0` 关闭。
* **实测(1/8 分片, 约 224 万信号)**: **369s / 11.9GB**, 产出 318,809 个时间点。
  单 walker 全量按线性外推 **≈45 分钟** —— 换路免掉的是"按名字解析 18M 次句柄"的两小时,
  **没有**免掉逐条扫描的 ~1.45µs/记录。
* **按 scope 子树分片**(`WAL_FSDB_SCOPE_SPLIT=<depth>`): depth=1 时这份设计只有**一个顶层
  scope**, 切不动(实测分 8 片只有 shard0 有活); 要用更深的切分点才能让多 worker 的内存
  各自随子树大小下降 —— 这是下一步做并行的入口。
* **内存叠加**: 同进程里父进程已背着名字表(6GB), NPI 遍历期再加 ~10GB, 27GB 机器(还有
  7GB 级常驻的许可 VM)会压到 0 可用、构建被拖慢 3 倍。为此加了内存护栏
  (`inprocess_walk_scan_allowed`, 可用 < 18GB 就不在本进程叠)与**子进程建 `.ftl`**
  (`fsdb-timeline-map` + `fsdb-timeline-merge`)两条路。
* **运维建议**: 这类波形第一次用索引空间查询前, 先跑 `make fsdb-prewarm FSDB=x.fsdb LOCAL=1`
  (或 LSF), 让构建独占机器; 之后 `.ftl` 命中, 查询进程常驻只要 6GB。

### 按 scope 子树的并行时间线(2026-09-27, 真波形)

单 walker 全量 ≈45 分钟; 按 scope 子树切开后每个 worker 只走自己那几棵子树, 遍历期内存随之下降:

```bash
export WAL_FSDB_WALK_SCAN=1 WAL_FSDB_SCOPE_SPLIT=4      # 先标定: WAL_FSDB_SCOPE_STATS=6
for i in $(seq 0 7); do wal-rust fsdb-timeline-map aicore_smoke_test_000.fsdb $i 8 p$i.part & done; wait
wal-rust fsdb-timeline-merge aicore_smoke_test_000.fsdb p*.part   # 装 .ftl
```

| 分片口径 | 墙钟 | 每 worker RSS | 并集 |
|---|---|---|---|
| 单 walker(全量) | ~2700s(外推) | ~12GB | — |
| depth4 / 8 片 | 570s | 5.1~6.0GB | **320,204 点** |
| **depth5 / 8 片** | **494s** | 4.0~7.9GB | **320,204 点** |
| depth3 / 7 片 | (仅取并集) | — | **320,204 点** |

> **三种互不相同的分片口径并集逐点相同(320,204)** —— 这条对拍就是抓上面那个 bug 的手段,
> 也是"并行 map/reduce 产物正确"的现场证据。

* **选层靠一次标定**: `WAL_FSDB_SCOPE_STATS=6` 一次树遍历(44s)打出各层子树数与最大子树占比 ——
  这份设计 depth1/2 **只有 1 个子树**(切了等于没切), depth3 = 7 棵(最大 24.5%),
  depth4 = 334 棵(最大 13.7%), depth5 = 2121 棵。
* **并行度受"最慢的 worker"限制**: depth4 的 8 片里快的一半 ~190s、慢的一半 ~530s(子树大小
  按 `下标 % N` 分配, 不均匀) → 墙钟 570s。更细的 depth5 或按子树大小做 LPT 贪心分桶还能再压。
* **暖查询回到秒级**: `.ftl` 装好后 `(count (= (get "concern_sig") 1))` = **11.4s / 6.35GB**
  (命中 320,204 个时间点), 这是之前 20 分钟都跑不完的那条查询。
* ⚠️ **抓到一个真 bug**: 用了 scope 分片后仍叠加"信号下标 % N"过滤, 每个 worker 丢掉自己子树里
  7/8 的信号 —— 并集只少 43 个时间点(接近饱和, 很隐蔽)。靠"**不同分片口径必须产出同一并集**"
  的三方对拍抓到, 已修; 修复后 depth3/depth4 两条独立分片口径逐点一致。

### 自动化这两步: worker 自己开"边走边扫 + 按 scope 分片"(2026-09-28, 真波形)

上一节的配方要人工 `export WAL_FSDB_WALK_SCAN=1 WAL_FSDB_SCOPE_SPLIT=<depth>`;忘一个,
8 个 worker 就各走整棵树。现在 `fsdb-timeline-map` 对 ≥128MB(`WAL_FSDB_WALK_SCAN_MB`)的文件
**默认**开边走边扫, 并在多分片时自动选切分层 —— 即用户敲的就是
`make fsdb-prewarm FSDB=… SHARDS=8 LOCAL=1`。

**但"自动选层"第一版把自己坑了**: 选层要先数一遍树, 而这一遍的 NPI 驻留约 10GB。8 个 worker
同时启动 = 80GB, 27GB 机器上实测 NPI 直接**段错误**:

```
[fhdb][fatal] Can not get user data        ← 刷了 22 万行
*WARN* [fhdb][error] Ei/Cg/Fn (16440/-1/564) with no driver and not primary.
catch signal 11 (Segmentation fault)
```

修复 = **flock 串行化 + 双检缓存**: 第一个 worker 拿
`<cache>/<file_identity>-v1.fscope.lock` 走一遍, 把每层子树数与**每棵子树大小**写进 `.fscope`
(魔术字 `WSCP2`, 228KB, 原子 rename), 其余 worker 在锁上等 ~44s 后命中;以后每次运行 0 代价。
缓存目录不可写时不猜也不走树, 直接按最深一层分片。

**"怎么选层"是第二个坑: 信号数均衡 ≠ 时间均衡。** 先按最自然的想法试了两次, 都栽了:

| 选层口径(端到端 `make fsdb-prewarm SHARDS=8 LOCAL=1`, 同机同天) | 每 worker 认领 | 墙钟 | 并集 | `.ftl` md5 |
|---|---|---|---|---|
| depth4 = 够切即止(42 棵/worker) | 42 | 668s | 320,204 点 | `ca1cb095…` |
| depth8 = **按信号数算最均衡**的一层(4595 棵) | 4595 | **760s** ✗ | 320,204 点 | `ca1cb095…` |
| **depth5 = 每 worker 认领数最接近 256** | 265 | **636s** ✓ | 320,204 点 | `ca1cb095…` |
| (上一节手工 depth5/8, 无统计遍历) | 265 | 494s | 320,204 点 | `ca1cb095…` |

* **按信号数选层是错的**: depth8 每片恰好拿到 ≈12.3% 的信号(数学上的"完美平衡"), 却最慢 ——
  决定耗时的是**变更密度**(时钟类信号每条 13 万次变化, 逻辑信号常常几次), 均匀切信号数
  并不等于均匀切工作量; 而且层越深, 每 worker 要认领更多棵树、也都要重复处理浅层信号。
  depth8 的分片时间戳还留了证据: 偶数片 05:17:1x 全完, 奇数片 05:20:1x-2x 才完 ——
  相邻子树按"变更密度"交替, `下标 % N` 把重活全分给了奇/偶一侧。
* **现在的规则**: 在"子树数 ≥ 分片数"的层里, 取**每 worker 认领子树数最接近 256** 的那层
  (甜点来自上表三点; `WAL_FSDB_SCOPE_TARGET_PER_WORKER` 可调, `WAL_FSDB_SCOPE_SPLIT=<depth>`
  可在做 A/B 时钉死)。本机各层子树数 `[1,1,7,334,2121,4761,13106,36760]`(8 片 → depth5)。
* **三条口径的 `.ftl` 逐字节相同**(`md5 ca1cb095c9a46800c3a014c06e60a959`, 460,014 B, 320,204 个
  时间点)—— 选层换口径不动产物, 这条对拍就是自动路径的正确性守门。
* 下一步想再压: 分片分配目前是 `子树号 % N`, 与"重活落在哪"无关; 若能拿到**每棵子树的变更数**
  (只有真扫一遍才知道)就能做 LPT 贪心分桶。depth5 快慢两半仍有 ~1.4× 差, 上限大概就是把
  这个差抹平的 ~15%。

### 回归库补齐: 0.14.43 vs 0.14.37(同一台机, 321MB / 1796 万信号)

`bench/perf-history.csv` 里这个 fixture 原来只有 0.14.37 的 7 行;现在 0.14.43 有 11 行
(cold/build/hit × load/edge/at + hit-level + par-build)。冷热都换了 `WAL_CACHE` 状态与 cwd,
口径见 `bench/README.md`。

| op | 0.14.37 | 0.14.43 | 备注 |
|---|---|---|---|
| cold-load(`(length (SIGNALS))`) | 54.16s / 13,295MB | **52.54s / 13,293MB** | 冷建名字树 |
| build-load | 57.94s / 13,601MB | **53.36s / 13,602MB** | 顺带落盘缓存 |
| hit-load | 11.08s / 6,197MB | **10.49s / 6,197MB** | `.fnames` 命中 |
| cold-edge | 56.63s / 13,267MB | **54.56s / 13,294MB** | `(count (rising sig))` |
| build-edge | 10.54s / 6,199MB | 11.78s / 6,199MB | edge 走时间域, 不建索引空间 |
| hit-edge | 10.11s / 6,200MB | 10.46s / 6,199MB | |
| cold-at | — | **56.71s / 13,116MB** | 新增行 |
| build-at | — | **11.19s / 6,202MB** | 新增行 |
| hit-at | — | **10.50s / 6,201MB** | 新增行 |
| **hit-level**(`.ftl` 命中) | 11.39s / 6,198MB | **11.10s / 6,199MB** | → 320,172(时间线 320,204 点) |
| **par-build**(8 片自动选层) | 570s / 5,800MB(depth4 手工) | **636s**(自动 depth5, 含 44s 统计遍历) | 单 walker 外推 ~2700s |

* **冷加载达标**: 52.5s / 13.0GB(cold)、10.5s / 6.2GB(warm);`fsdb-par-build` 636s 换来的
  `.ftl` 让之后每次索引空间查询回到 ~11s。相对 0.14.37 全线持平或更好, 无 >20% 回归。
* `fsdb-cold-level` / `fsdb-build-level` 这两行**故意不记**: 在查询里冷建整条时间线实测
  **>8min**(480s 超时被杀), 会撞脚本的 1800s 上限;规范路径是先 `make fsdb-prewarm`。

### 指标算子(Goal-2)在真实波形上的验收(0.14.43, 321MB)

`.tools/bench/goal2_full.wal` 一次进程跑完 8 个形式, **10.0s / 6.2GB**:

| 形式 | 结果 |
|---|---|
| `(stats (latency (rising …hart0_sync_holder.op_inst_valid) (rising …op_inst_ready)))` | `n=10 min=0 p50=0 mean=1143.200 p90=1143.200 p99=10403.120 max=11432 stddev=3615.116` |
| `(histogram (latency …) 8)` | 8 桶: `[0,1429)` 9 次, `[10003,11432]` 1 次(其余 0) |
| `(percentile (latency …) 50)` / `95` | `0` / `6287.600` |
| `(median (latency …))` | `0` |
| `(stddev (latency …))` | `3615.1158` |
| `(stats (latency (rising …u_store_0.awvalid) (rising …bvalid)))` | `n=1 min=max=28794`(ps, ≈43 个 670ps 周期) |
| `(ipc (rising …op_inst_valid) (rising …hvm2sm_adapter.clk))` | `1.6847e-4` |

指标只读**时间域变更列**(不建索引空间), 所以千万时间戳的大波形上不会触发全文件物化;
语义与手算闸见 `docs/waveform-metrics.md` 与 `tests/metrics_test.rs`。

### 4M 夹具上的一个真回归: "只看信号数"的换路判据(0.14.43 → 0.14.44 修)

`many4m.fsdb`(400 万信号 / 3.6MB / 200 个时间戳)的 cold-level 从 0.14.35 的 21.2s 涨到
0.14.43 的 31.4s(+48%, 越过仓库"回归 >20% 必须写明原因"的线)。三次测量确认不是噪声:

| 口径 | cold level(3 次) | 说明 |
|---|---|---|
| 0.14.43 默认 | 30.1 / 31.2 / 31.3s | 被"信号数 ≥ 200 万"判据拖去走**单 walker**(子进程被强制 `WALK_SCAN=1`) |
| `WAL_FSDB_WALK_SCAN=0` | 13.0 / 12.9 / 13.3s | 常规 8 路并行(可见 `jobs 决策` + `并行构建时间线: 8 workers, 11.1s`) |
| 0.14.44 默认(判据改成"信号数 **且** 文件 ≥128MB") | **15.0 / 15.3 / 15.5s** | 与 `WALK_SCAN=0` 同路; 真波形不受影响(它两个条件都满足) |

* 同轮还修掉一个语义反转: worker 用 `env_ok("WAL_FSDB_WALK_SCAN")` 判"要不要换路", 于是
  **`=0`(文档写的"关")反而打开了它**;现在三态判断。真波形上 `=0` 已实测走回常规 worker
  (`worker offset=0 stride=8: 2244513 个信号`, 名字表 9.8GB), `=1` / 未设走边走边扫。
* 教训: 阈值判据要反映**真正的成本驱动量**(变更记录数), 文件大小是它的代理; 只数信号数会
  在"多信号、少变更"的夹具上做错选择。
