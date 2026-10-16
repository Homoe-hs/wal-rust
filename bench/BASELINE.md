# wal-rust 逐功能性能基线(FSDB 真波形)

> 这是 `scripts/bench_features.sh` 在**唯一一份 core 级真波形**上建立的**逐功能**基线。
> 权威数字在 [`bench/perf-history.csv`](perf-history.csv);本文件是它的**人类可读版本 + 读数口径**。
> 建立在二进制 **0.14.50** / HEAD **`e7c24b6`**(分支 `dev/profiling`)。

**一句话结论**:这份 336MB / 1796 万信号的波形上,单进程端到端时间几乎全是**加载**(地板 5.37~6.52s / 峰值 RSS 约 6.2GB);
一次 `--stdin` 会话把加载压到 6.40s 后,29 条查询探针合计 6.77s,其中两条 `find-sig` 占 92%。

---

## 0. 三条命令(基线怎么用)

```bash
# 1) 重跑装置(需要 Verdi 许可通道 + 这份 FSDB;装置自己串行化、自己重试 license_up)
scripts/bench_features.sh all          # = single(mode A) + session(mode B);只跑一类就写 single / session

# 2) 把中位数按现有 schema 追加进 bench/perf-history.csv(version 取 Cargo.toml)
scripts/bench_features.sh append-csv

# 3) 判回归(离线, 不需要 FSDB / 许可; 同一 op 相对基线 >20% 即判红并以非 0 退出)
python3 scripts/check_bench_regression.py --baseline <基线版本> --new <新版本>
python3 scripts/check_bench_regression.py --baseline 0.14.49 --new 0.14.51   # 例:比 mode A 那批 op
```

装置与闸的完整开关:`scripts/bench_features.sh --help`、`python3 scripts/check_bench_regression.py -h`。

---

## 1. 被测对象(身份)

| 项 | 值 | 出处 |
|---|---|---|
| 波形(唯一) | `/home/hesheng/Downloads/core 级波形/aicore_smoke_test_000.fsdb`(本机路径,**含空格**) | — |
| 文件大小 | 336,004,134 B(约 320 MiB) | `ls -l`;[`bench/RESULTS.md`](RESULTS.md) |
| 信号数 | 17,956,098 | `session.1.log` probe #1 / [`bench/RESULTS.md`](RESULTS.md) |
| scope 数 | 1,620,912 | `session.1.log` probe #2 |
| 时间范围 | 0 .. 43,776,606 ps @ 1ps(约 43.78 µs) | [`bench/RESULTS.md`](RESULTS.md) |
| 时间点数 | 320,203(`MAX-INDEX`) | `session.1.log` probe #4 输出 `(0 0 320203)` |
| 二进制 | `target/release/wal-rust`;`Cargo.toml` version **0.14.50** | `Cargo.toml` |
| 提交 | HEAD **`e7c24b6`**(分支 `dev/profiling`) | `git rev-parse --short HEAD` |
| 机器 | **32 核 / 27 GB 内存** | `nproc` / `free`(取测量当次机器状态) |
| 功能清单(权威) | [`bench/features.tsv`](features.tsv) = 32 条(29 表达式 + 3 CLI) | 入库文件 |
| mode B 输入 | [`bench/probes.wal`](probes.wal) = 29 条表达式 probe | 入库文件 |

---

## 2. 测量口径

### mode A —— 每条功能一个独立进程(含加载)

* 一条功能 = 一个 `target/release/wal-rust ... -l "$WAVE"` 进程;**端到端 wall 含加载**;
* 每条功能跑 **3 次**,取**中位数**(`single.csv` 90 行原始 → `single.median.csv` 30 行中位;`extra/single.csv` 3 行 edges-list → `extra/single.median.csv` 1 行中位);
* `RSS` = 该进程峰值常驻内存(MB,`/usr/bin/time` 口径);
* 覆盖 **31/32** 条功能;唯一跳过的是 `cli-topsig`(原因见 §6);
* 单条命令完整路径记录在 `perf-history.csv` 的 `note` 里。

### mode B —— 一次 `--stdin` 会话跑完全部 probe

* 一条命令:`WAL_PROF=1 WAL_PROF_TRACE=1 target/release/wal-rust --stdin -l "$WAVE" < bench/probes.wal`;
* **加载只付一次**;数字是**会话内边际** —— `WAL_PROF_TRACE=1` 让每次阶段调用打一行 `[prof] trace probe #k <ms>`,`probe #k` 与 `probes.wal` 的行序一一对应,相邻两次时戳之差就是该功能的边际耗时;
* 会话跑 **3 次**,取中位数(`session.csv` = 3 × 38 行:每轮 1 `wall-total` + 8 条阶段账 + 29 条 `session-probe`);
* `RSS` 只有**会话级峰值**(9381 MB),**没有逐探针 RSS**(缺测);
* 覆盖 **29/29** 条表达式 probe;3 条 CLI 功能没有表达式形式,不进会话(缺测,见 §6)。

---

## 3. 版本与提交口径(如实说明,存在差异但语义一致)

| 口径 | 实际测量所在树 | 该树 `Cargo.toml` 版本 |
|---|---|---|
| mode A(31 功能 × 3 = 93 次进程) | commit `5845bf7`..`c5034be` | 0.14.47 → **0.14.48** |
| mode B(3 次会话 × 29 probe = 87 条边际) | `c5034be`..`e7c24b6`(会话行挂到 0.14.49 那次追加上) | 0.14.48 → **0.14.50** |

两棵树之间(`git diff --stat 5845bf7 c5034be`)**只有**这些差异:

* `src/prof.rs` **+17 行**:新增 `WAL_PROF_TRACE=1` 开关,设置后每次阶段调用多打一行
  `[prof] trace <阶段> #<第几次> <ms>ms`。**默认关闭** —— 没设这个环境变量时 `trace_on()` 为 `false`,
  除一次 `OnceLock` 读外不进任何分支,mode A 根本没设它;
* `docs/profiling.md` +3 行(记录这个开关);
* `Cargo.toml` / `Cargo.lock` / `README.md` 各 1 行的**版本号字符串** bump。

**结论:`mode A` 与 `mode B` 走同一条代码路径**,后加的 `0.14.49 / 0.14.50` 行与先加的 `0.14.47 / 0.14.48` 行语义一致;
两批数字可直接互相参照。

两点必须写明的偏差:

1. `perf-history.csv` 里 mode A 的 31 行标的是 `0.14.49`,而 mode A 实际量在 `5845bf7`..`c5034be` 那棵树上 ——
   因为 `append-csv` 取的是**追加那一刻** `Cargo.toml` 的版本号,不是"测量那一刻代码差异的版本号"。这里不隐瞒。
2. mode B 的会话采集设了 `WAL_PROF_TRACE=1`,比 mode A 多 29 行 stderr 输出;这 29 行的开销**已算在**
   `session` 阶段墙钟里(即 mode B 的数字对被计的那次会话是保守口径),不是额外白送。

---

## 4. mode A —— 单功能单进程(按中位 wall 降序)

`$WAVE` = `/home/hesheng/Downloads/core 级波形/aicore_smoke_test_000.fsdb`(含空格,复制命令时加引号)。
★ = 中位 wall ≥ 7s,标注见本节末。来源表 = `single.median.csv`(main)/ `extra/single.median.csv`(extra)。

| 功能 | 中位 wall (s) | 中位 RSS (MB) | 表达式 / 子命令 | 来源表 |
|---|---:|---:|---|---|
| ★ `name-find-sig-wide` | 10.90 | 9408 | `(length (find-sig "clk")) -l "$WAVE"` | main |
| ★ `cli-count` | 8.19 | 9273 | `count "$WAVE" tb.aicore_top.core0_por_rstn 1` | main |
| ★ `name-find-sig-precise` | 8.19 | 9267 | `(length (find-sig "core0_por_rstn")) -l "$WAVE"` | main |
| ★ `meta-scope-count` | 7.25 | 6260 | `(length SCOPES) -l "$WAVE"` | main |
| `meta-signal-count` | 6.52 | 6196 | `(length (SIGNALS)) -l "$WAVE"` | main |
| `cli-sigs` | 6.38 | 6194 | `sigs "$WAVE" core_clk 10` | main |
| `meta-timescale` | 6.15 | 6194 | `(timescale) -l "$WAVE"` | main |
| `value-initial` | 5.95 | 6197 | `(initial "tb.aicore_top.core0_por_rstn") -l "$WAVE"` | main |
| `level-count` | 5.92 | 6200 | `(count (= (get "tb.aicore_top.u_tnode_top_0.u_rvsys.llp_awvalid") 1)) -l "$WAVE"` | main |
| `change-count` | 5.91 | 6194 | `(count (changes "tb.aicore_top.u_tnode_top_0.u_rvsys.core_clk")) -l "$WAVE"` | main |
| `metric-histogram` | 5.91 | 6196 | `(histogram (latency (rising "tb.aicore_top.u_gnode_top.u_store_0.awvalid") (rising "tb.aicore_top.u_gnode_top.u_store_0.bvalid")) 10) -l "$WAVE"` | main |
| `metric-ipc` | 5.91 | 6196 | `(ipc (rising "tb.aicore_top.u_gnode_top.u_store_0.bvalid") (rising "tb.aicore_top.u_tnode_top_0.u_rvsys.core_clk")) -l "$WAVE"` | main |
| `cond-and-count` | 5.90 | 6196 | `(count (&& (= (get "tb.aicore_top.u_tnode_top_0.u_rvsys.llp_awvalid") 1) (= (get "tb.aicore_top.u_tnode_top_0.u_rvsys.llp_wvalid") 1))) -l "$WAVE"` | main |
| `index-get` | 5.87 | 6196 | `(get "tb.aicore_top.core0_por_rstn") -l "$WAVE"` | main |
| `metric-median` | 5.87 | 6194 | `(median (latency (rising "tb.aicore_top.u_gnode_top.u_store_0.awvalid") (rising "tb.aicore_top.u_gnode_top.u_store_0.bvalid"))) -l "$WAVE"` | main |
| `index-specials` | 5.84 | 6196 | `(list INDEX TS MAX-INDEX) -l "$WAVE"` | main |
| `time-at` | 5.84 | 6196 | `(at "tb.aicore_top.u_tnode_top_0.u_rvsys.llp_awaddr" 1000000) -l "$WAVE"` | main |
| `metric-stats` | 5.81 | 6194 | `(stats (latency (rising "tb.aicore_top.u_gnode_top.u_store_0.awvalid") (rising "tb.aicore_top.u_gnode_top.u_store_0.bvalid"))) -l "$WAVE"` | main |
| `index-find` | 5.79 | 6198 | `(length (find (rising "tb.aicore_top.u_tnode_top_0.u_rvsys.core_clk") 10)) -l "$WAVE"` | main |
| `name-signal-width` | 5.79 | 6196 | `(signal-width "tb.aicore_top.u_tnode_top_0.u_rvsys.llp_awaddr") -l "$WAVE"` | main |
| `index-sample-at` | 5.78 | 6194 | `(sample-at "tb.aicore_top.cbus2gnode_pready_o" 100) -l "$WAVE"` | main |
| `assert-eq-t0` | 5.76 | 6198 | `(assert-eq "tb.aicore_top.core0_por_rstn" 0 0 (get "tb.aicore_top.core0_por_rstn")) -l "$WAVE"` | main |
| `change-list` | 5.73 | 6196 | `(length (getwave "tb.aicore_top.cbus2gnode_pready_o")) -l "$WAVE"` | main |
| `metric-percentile` | 5.70 | 6195 | `(percentile (latency (rising "tb.aicore_top.u_gnode_top.u_store_0.awvalid") (rising "tb.aicore_top.u_gnode_top.u_store_0.bvalid")) 99) -l "$WAVE"` | main |
| `metric-latency-len` | 5.68 | 6196 | `(length (latency (rising "tb.aicore_top.u_gnode_top.u_store_0.awvalid") (rising "tb.aicore_top.u_gnode_top.u_store_0.bvalid"))) -l "$WAVE"` | main |
| `nav-step` | 5.68 | 6195 | `(do (step 5) (step -5) INDEX) -l "$WAVE"` | main |
| `time-search` | 5.65 | 6195 | `(length (search "tb.aicore_top.u_tnode_top_0.u_rvsys.llp_awaddr" "1" 0 1000000)) -l "$WAVE"` | main |
| `edge-rising` | 5.62 | 6197 | `(count (rising "tb.aicore_top.u_tnode_top_0.u_rvsys.core_clk")) -l "$WAVE"` | main |
| `edge-window` | 5.60 | 6199 | `(count-edges "tb.aicore_top.u_tnode_top_0.u_rvsys.core_clk" 0 1000000) -l "$WAVE"` | main |
| `edge-falling` | 5.59 | 6194 | `(count (falling "tb.aicore_top.u_tnode_top_0.u_rvsys.core_clk")) -l "$WAVE"` | main |
| `edges-list` | 5.37 | 6198 | `(length (edges "tb.aicore_top.u_tnode_top_0.u_rvsys.core_clk" 0 1000000)) -l "$WAVE"` | extra |

### ★ ≥7s 的 4 条:多出来的部分是名字表扫描 / CLI 路径,不是查询算法慢

* `name-find-sig-wide` **10.90s** / `name-find-sig-precise` **8.19s**:`(find-sig ...)` 必须扫完全部 **17,956,098** 条名字
  (全表子串匹配);同时 RSS 从地板 6.2GB 涨到 **9.41GB / 9.27GB** —— 命中列表被物化在堆上。
* `cli-count` **8.19s** / RSS **9.27GB**:`count <wave> <sig> <val>` 是 **CLI 子命令路径**(与表达式内 `count` 同语义,
  但走 CLI 自己的参数/名字表/结果物化路径),所以它比表达式形式的同类查询贵一档。
* `meta-scope-count` **7.25s** / RSS 6260MB:比加载地板(5.37~6.52s)高约 **0.7s**,是 `(length SCOPES)` 遍历
  **1,620,912** 个 scope 的开销,**与名字表无关**。

### 其余 27 条:**5.37 ~ 6.52s**,几乎全是加载

其中 `cli-sigs` 6.38s(RSS 6194MB,只多 0.2GB)说明 `sigs` 的懒遍历没有物化整表;
`edges-list` 5.37s 是所有条目里最低的(单纯读变更列)。

**读 mode A 的正确姿势**:两两之间的差(< 1s)小于加载抖动(见 §7),所以 **mode A 用来发现"加载回归"**,
不要拿它判断"某个查询变快了 5%"。要判查询侧,看 mode B。

---

## 5. mode B —— 会话内边际(按中位边际降序)

探针序号 = `probes.wal` 里的行序(`session.<n>.log` 的 `[prof] trace probe #k` 与之对应)。

| 功能 | 会话内边际中位 (s) | 探针 # |
|---|---:|---:|
| `name-find-sig-wide` | 3.961496 | 7 |
| `name-find-sig-precise` | 2.340186 | 6 |
| `meta-scope-count` | 0.464267 | 2 |
| `index-find` | 0.051151 | 22 |
| `index-get` | 0.002514 | 9 |
| `index-specials` | 0.002015 | 4 |
| `change-count` | 0.001756 | 12 |
| `edge-window` | 0.001369 | 16 |
| `time-at` | 0.001185 | 8 |
| `index-sample-at` | 0.000997 | 10 |
| `edge-rising` | 0.000923 | 14 |
| `cond-and-count` | 0.000759 | 21 |
| `metric-latency-len` | 0.000683 | 23 |
| `metric-ipc` | 0.000602 | 28 |
| `edge-falling` | 0.000569 | 15 |
| `level-count` | 0.000532 | 20 |
| `edges-list` | 0.000368 | 17 |
| `change-list` | 0.000289 | 13 |
| `time-search` | 0.000125 | 19 |
| `meta-signal-count` | 0.000057 | 1 |
| `meta-timescale` | 0.000032 | 3 |
| `metric-histogram` | 0.000028 | 27 |
| `name-signal-width` | 0.000023 | 5 |
| `metric-stats` | 0.000023 | 24 |
| `assert-eq-t0` | 0.000021 | 29 |
| `metric-percentile` | 0.000018 | 25 |
| `nav-step` | 0.000017 | 18 |
| `metric-median` | 0.000015 | 26 |
| `value-initial` | 0.000011 | 11 |

### 会话总账(3 次取中位)

| 项 | 中位 | 3 次原始值 | 出处 |
|---|---:|---|---|
| `wall-total`(进程总墙钟) | 13.48s | 14.26 / 13.45 / 13.48 | `session.csv` |
| `session`(阶段账 session) | 13.322s | 14.081 / 13.295 / 13.322 | `session.1..3.log` |
| `load`(= open + names + index) | 6.397s | 6.436 / 6.282 / 6.397 | 阶段账 |
| ├ `fsdb:open` | 5.251s | 5.299 / 5.148 / 5.251 | 阶段账 |
| ├ `fsdb:names` | 0.128s | 0.137 / 0.126 / 0.128 | 阶段账(页缓存**暖**) |
| ├ `fsdb:index` | 0.962s | 0.962 / 0.960 / 0.981 | 阶段账 |
| └ `fsdb:npi_init` | 0.000s | 0.000 / 0.000 / 0.000 | 阶段账 |
| `probe`(29 条合计) | 6.773s | 7.341 / 6.773 / 6.678 | 阶段账 |
| `probe-mean` | 0.233552s/次 | 0.253138 / 0.233552 / 0.230276 | 阶段账 |
| 会话峰值 RSS | 9381 MB | 9381 / 9379 / 9381 | `session.csv` |

### 这张账怎么对平

* `load` 的四个子段之和 = 5.251 + 0.128 + 0.962 + 0.000 = **6.341s**,与 `load` 6.397s 差 **0.056s**
  (未单列的缓存打开/校验开销);
* `session` 13.322s − (`load` 6.397 + `probe` 6.773) = **0.152s** 是探针之间的解释器/REPL 开销;
* `wall-total` 13.48s − `session` 13.322s = **0.158s** 是进程启动/收尾(含 NPI 库加载);
* 29 条逐探针中位之和 = **6.832s**,与阶段账 `probe` 的 6.773s 差 0.059s
  ("各轮中位之和" vs "合计后再取中位"的固有差异,不是缺项);
* **两条最重的探针占 92%**:`name-find-sig-wide` 3.961496s + `name-find-sig-precise` 2.340186s = 6.301682s,
  占逐探针中位之和的 **92.2%**;
* 除这两条 + `meta-scope-count`(0.464s)+ `index-find`(0.051s)外,其余 **25 条都在 1.4ms 以下**;
* 会话峰值 RSS **9381MB**,与 mode A 里物化名字表的那几条(9267~9408MB)同量级 —— 会话里 `find-sig` 的命中列表同样留在堆上。

---

## 6. 跳过与缺测(不填推测值)

### `cli-topsig` 为什么跳过

`topsig <wave> [n]` 要对**全部 17,956,098 个信号各解析一次句柄并扫一遍变更流**。按仓库在真波形上已标定的代价模型
(NPI 逐条 `next()`≈1.45µs / 变更记录;暖路径按名字解析句柄≈550µs / 信号 ≈ 2.75 小时 —— 这两个系数是仓库在真波形上
已标定的代价模型,见 `AGENTS.md` / [`bench/RESULTS.md`](RESULTS.md)),这一条是**分钟~小时级**,
会让整套基线失去意义,所以 `bench/features.tsv` 里标 `heavy=yes`,装置**默认跳过**;
要用它在小夹具上跑,加 `--include-heavy`。

### 缺测清单

| 项 | 状态 | 说明 |
|---|---|---|
| `cli-topsig` mode A | **跳过**(非缺测) | 见上;`single.failures.tsv` 为 0 行,不是失败 |
| `cli-count` / `cli-sigs` / `cli-topsig` mode B | **缺测** | `probes.wal` 只由 `features.tsv` 里 `kind=expr` 的 29 行生成,CLI 子命令没有会话内形式 |
| 逐探针 RSS(mode B) | **缺测** | 采集只有会话级峰值 9381MB |
| 冷页缓存下的 mode A / mode B | **缺测** | 本次全部为暖态(`fsdb:names` 0.126~0.137s);冷态见 §7 与 [`bench/RESULTS.md`](RESULTS.md) |
| VCD / FST 夹具 | **缺测** | 本基线只覆盖这份 FSDB;VCD/FST 的既有数字在 [`bench/perf-history.csv`](perf-history.csv) 与 [`bench/RESULTS.md`](RESULTS.md) |
| 时间线冷建 / `-j` 并行 / scope 分片 | **缺测** | 不在本装置范围(查询不建时间线);见 [`bench/RESULTS.md`](RESULTS.md) 的 FSDB 段 |

另外提示一个**来源细节**,免得读者以为缺一行:mode A 的 `edges-list` 中位在
`.tools/bench/baseline/extra/single.median.csv`(主表 `single.median.csv` 未含),本文件已并入 §4。

---

## 7. 已知噪声源(比数字之前先确认)

1. **页缓存冷热**:同一二进制同一命令,暖态 10.5~11.1s、冷态 14.0~17.2s([`bench/RESULTS.md`](RESULTS.md))。
   本基线的 mode B 是**暖态**(`fsdb:open` 5.15~5.30s、`fsdb:names` 0.126~0.137s)。
2. **许可通道状态**:`.tools/vm/license_up.sh` 可能报"通道可用"而端口在一分钟内又关;
   装置默认 `--license auto`,每次测量前在同一进程内 source 环境并重跑配方(共 3 次机会)。
   症状与判读见 [`docs/fsdb-env.md`](../docs/fsdb-env.md) §5。
3. **`-j` 并行度**:冷建时间线按 `-j N|auto`(`--jobs`)/ `WAL_FSDB_TL_JOBS` 并行。本基线不建时间线,
   但如果哪天 `fsdb:index` 或 `load` 突然变大,先怀疑并行度/机器负载,再怀疑代码。
4. **rayon 线程数**:`fsdb:index` 段的名字 FNV 哈希是 rayon 并行(仓库既有记录:2.85GB 名字串行哈希 ~4s → 并行 ~0.4s),
   改 `RAYON_NUM_THREADS` 会直接动这一段。
5. **同机并发**:FSDB 受许可席位限制,同一时刻只允许一个 FSDB 进程;装置用 `.tools/bench/baseline/.bench.lock` 串行化自身。
   所以**测这份基线时不要让别的 FSDB 进程/重活同时跑**(本仓库的教训:并行跑会直接 `SIGSEGV` 或把墙钟冲成噪声)。

---

## 8. 回归判定规则与闸的用法

**规则(仓库既有约定,与 `scripts/check_bench_regression.py` 一致):**

> **同一个 op 相对基线 > 20% 即视为回归**;必须在提交信息 / CHANGELOG 里写明原因。

闸只做一件事:把 >20% 的地方红出来并以**非 0** 退出(默认阈值 20%,只比 wall;RSS 默认只报,加 `--gate-rss` 才参与)。

```bash
python3 scripts/check_bench_regression.py --baseline <V> --new <V>
python3 scripts/check_bench_regression.py --baseline 0.14.49 --new 0.14.51            # mode A 那 31 个 op
python3 scripts/check_bench_regression.py --baseline 0.14.50 --new 0.14.51 --op session  # 只看会话类 op
python3 scripts/check_bench_regression.py --threshold 15 --gate-rss --json           # 更严 + RSS 参与 + 机器可读
```

**基线版本号怎么选**:

* mode A 的 31 个 `fsdb-feat-<功能>` op ⇒ `--baseline 0.14.49`;
* mode B 的 29 个 `fsdb-feat-session-<功能>` op ⇒ `--baseline 0.14.50`(它们只在 0.14.50 出现,
  闸的默认口径"每个 op 最早的版本"也落在 0.14.50)。

**哪个数说话**:mode A 是端到端(含加载),加载回归会**淹没**功能回归;要判"某个功能变慢"必须看 mode B 的会话内边际。两套一起看。

**一个必须知道的局限**:闸默认 `--min-wall 0.05`(低于 50ms 的 op 不判)。mode B 的 29 行里只有 4 行 ≥ 50ms
(`name-find-sig-wide` 3.961s、`name-find-sig-precise` 2.340s、`meta-scope-count` 0.464s、`index-find` 0.0512s),
其余 **25 行对闸不产生判定力** —— 它们的价值是"留证 + 人工比对"。mode B 真正的回归哨兵是会话级的
`fsdb:names` / `fsdb:index` / `load` / `fsdb-feat-session`,那几条都在 0.1~6.4s 量级。

---

## 9. 入库约定(这 29 行是一次性的)

`bench/perf-history.csv` 在 0.14.50 一次性追加了 **29 行** `fsdb-feat-session-<功能>`(会话内边际中位)。
这是**只给基线版本**的入库:`append-csv` 的默认行为**不包含**这 29 行 —— 它只写
(a) `single.median.csv` 里每条功能一行(**30 行**;本基线的 `edges-list` 来自 `extra/` 表,需另行追加)、
(b) 3 条会话汇总行(`fsdb-feat-session` / `-session-load` / `-session-probe-mean`)。
也就是每版本约 **33 行**量级,而不是 +29 个新 op。

以后若确实要逐版本跟踪会话边际,再给装置加 `--append-marginals`(或 `session-marginals`)之类的开关,按需开启;
在此之前,**请把 0.14.50 那 29 行当作"基线快照"读,不要期望每个版本都有一份**。

---

## 10. 数据来源与行数核对(全部为只读本地产物)

| 文件 | 行数 | 用途 |
|---|---|---|
| [`bench/features.tsv`](features.tsv) | 32 条(29 expr + 3 CLI) | **权威**功能清单 |
| [`bench/probes.wal`](probes.wal) | 29 条 probe | mode B 输入 |
| `.tools/bench/baseline/single.csv` | 90 数据行(30 功能 × 3) | mode A 原始 |
| `.tools/bench/baseline/extra/single.csv` | 3 数据行(`edges-list` × 3) | mode A 原始(补) |
| `.tools/bench/baseline/single.median.csv` | 30 数据行 | mode A 中位 |
| `.tools/bench/baseline/extra/single.median.csv` | 1 数据行 | mode A 中位(补) |
| `.tools/bench/baseline/session.csv` | 114 数据行(3 轮 × 38:每轮 1 `wall-total` + 8 阶段 + 29 `session-probe`) | mode B |
| `.tools/bench/baseline/session.1..3.log` | 3 份 `[prof]` 阶段账 | mode B 原始 |
| `.tools/bench/baseline/single.failures.tsv` | 0 行 | mode A 无失败 |

以上数字**全部**逐字取自这些产物(`0.14.50` 这次没有重测)。任何一处对不上,以 `bench/perf-history.csv` 为准并回来改本文件。


## 10. 使用禁忌与独立复测结论(Lead 复核后补写,2026-10-17)

本基线由**另一个人独立复测**过(3 条 5 次复测 + 全量 31 条单次 + 自跑一遍完整会话 + mode A↔B 正确性对拍)。
结论:**有条件可信** —— 数据立得住,但必须按下面的条件引用。

### 10.1 引用条件(违反会得出错误结论)

1. **mode A 与 mode B 不许相减/互相推算**。两半是**跨批次**测的:本次复测里 mode A 比基线偏高 +6%、
   mode B 偏低 −5.8%(方向相反各漂约 6%);基线自身也有 25/30 条 mode A 的**最小** wall 低于同一份
   session 的**最大** load(6.44s)—— 而 mode A 定义上 = 加载 + 查询。要比较就**同批比**。
2. **会话内边际只用于"同一会话内的相对排序"**,不能当"这个功能独立跑要多久"。对物化整张名字表的
   功能(`find-sig`)尤其如此:会话内边际系统性低于独立成本(写/读页缓存已被前面的探针预热)。
3. **`edges-list` 的 mode A 中位在 `.tools/bench/baseline/extra/single.median.csv`**(主表漏收,属装置口径不对称);
   取数时别当"缺测"。
4. **名字物化类功能跨批次可漂 ±25%,普通查询 ±14%** ⇒ 判定回归必须**同批**;且 mode A 建议 **≥5 次**
   (本次基线是 3 次,3 次的小离散是批次内假象:`find-sig-wide` 3 次离散 0.6%,而两次 5 次批测中位差 7%)。
5. **逐探针 RSS 是缺测**(只有会话级 9381 MB);冷页缓存口径、VCD/FST、时间线冷建/`-j`/scope 分片都不在本装置范围。

### 10.2 装置缺陷(已知,用之前先看这条)

`.tools/bench/baseline/session.<n>.log` 是 `OUT=$(… 2>&1)` **合流**出来的:stdout 的 `=>` 值行与 stderr 的
`[prof] trace` 行会因管道刷新竞争而**乱序**,**不能**用它做逐 probe 归因(复测同事是自己另做"带标记 + 两路分开"
的会话才完成对拍的)。要用 `WAL_PROF_TRACE` 做逐功能归因,请:值写 stdout 文件、阶段账写 stderr 文件、并在 probe 里插标记。

### 10.3 基线暴露出的第一个可优化点(建议另开单)

`cli-count` 的 mode A RSS **9273 MB**,而 `cli-sigs` 只有 6193 MB;复测独立复现为 9271 MB。根因:
`src/main.rs` 的 `cmd_count` 里 `let sigs = tr.signals();` —— **物化整张 1796 万名字表(+约 3.1 GB)**;
同文件的 `cmd_sigs` 早已改成 `signal_count()/signal_at(i)` 懒遍历(并留了注释)。
这也是仓库自己的规矩("求值热路径禁止 `signals()`")在 CLI 路径上漏掉的一处。

* 复测全套原始产物在 `.tools/bench/verify/`(`REPORT.md` / `wall_compare.csv` / `session_verify.csv` / `correctness.csv`),**本地产物、不入库**;
  本条把它的结论固化进版本库,避免"复测做了但没人看见"。
* 正确性对拍结论:**跨进程值完全确定**(会话 29 条输出逐行相同);**mode A ↔ mode B:MATCH=29 / DIFF=0 / NA=3**(CLI 不适用)。
