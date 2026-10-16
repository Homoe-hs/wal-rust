# 性能剖析手册:阶段账 + 火焰图

> 状态:✅ 可用(`WAL_PROF=1` 零依赖;火焰图需要 `--features prof`)。
> 目的:回答"**时间花在哪个阶段、哪个函数**",而不是靠猜。改动都在 `dev/profiling` 分支。

## 1. 两条命令

```bash
# ① 阶段账(普通 release 二进制即可, 零额外依赖)
WAL_PROF=1 ./target/release/wal-rust --stdin -l big.fsdb < probes.wal

# ② 火焰图(需要带符号的二进制 + pprof-rs)
CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=false \
  cargo build --release --features prof
WAL_PROF=1 WAL_PROF_SAMPLE=session WAL_PROF_OUT=.tools/prof \
  ./target/release/wal-rust --stdin -l big.fsdb < probes.wal
```

* 采样器是 **pprof-rs(用户态 SIGPROF + backtrace)**:这台机器没有 `perf`/`valgrind`,也没有 root,
  它照样能用 —— 不需要 `perf_event_open`。
* `WAL_PROF_TRACE=1`:**每一次**阶段调用都打一行 `[prof] trace <阶段> #<第几次> <ms>ms`
  —— 会话模式下与 `probes.wal` 的行序一一对应,于是能**逐探针归因**(不必为每条功能各起一个进程)。
  只想逐探针归因时,不必再设 `WAL_PROF`。
* `WAL_PROF_SAMPLE=<阶段名,...>` 决定**哪些阶段**生成火焰图(不设就一个都不生成, 只计时);
  同一时刻只允许一个采样器(嵌套时采被选中的那一层)。
* 输出:`<WAL_PROF_OUT|.wal-rust-prof>/flamegraph-<阶段>.svg`(inferno 格式, 浏览器直接打开)。

## 2. 阶段账长什么样(真波形 336MB / 1796 万信号)

`--stdin` 会话, 600 个探针(`(count (changes "sig"))` × 600):

```
[prof] 阶段账(墙钟, 累计 ...):
[prof]   session                         10.360s  ×1        35.0%
[prof]   load                             9.293s  ×1        31.4%
[prof]   fsdb:open                        4.434s  ×1        15.0%   ← NPI init+open+文件属性
[prof]   fsdb:names                       3.982s  ×1        13.4%   ← 名字就位(.fnames 解码)
[prof]   fsdb:index                       0.840s  ×1         2.8%   ← 名字索引(32 线程)
[prof]   probe                            0.711s  ×600       2.4%   ← 每探针 ≈1.2ms
```

> ⚠️ `session` / `load` 是**外层**,`fsdb:*` 是它的**明细**,列出来的"累计"会把两层相加,别相加当总量。

要点(与之前所有测量一致):**加载占 95%,600 个探针合计 0.71s(每个 ≈0.95~1.2ms)**。

## 3. 火焰图怎么读(一个必须知道的陷阱)

`WAL_PROF_SAMPLE=session` 的图会把**所有线程**的样本混在一起:32 个 rayon worker 在
`WorkerThread::wait_until` 里自旋,会把它们的占比放大、把你的主线程工作冲淡。
所以:

* **墙钟归属看阶段账**(精确);
* **火焰图只看"阶段内的函数排序"**,而且最好**单线程 + 只采一个阶段**:
  `RAYON_NUM_THREADS=1 WAL_PROF_SAMPLE=load`。

单线程、只采 `load` 段(3471 个样本,真波形)的排序(两种口径都稳定):

| 函数 | 说明 |
|---|---|
| `Vec<wal_rust::trace::fsdb::Sig>::push` / `push_mut` / `core::ptr::write::<Sig>` | **构建 `sigs: Vec<Sig>`(1796 万个)** —— 加载期最大的一块 |
| `try_load_tree_cache` / `decode_tree_file` | **`.fnames` 解码**(含 `from_utf8`、`Vec<usize>::push` 的位宽数组) |
| `mi_arenas_page_regular_alloc` / `mi_page_extend_free` | mimalloc 分配 |
| `core::str::converts::from_utf8` | 名字 UTF-8 校验(**很小**, 与之前"换 unchecked 无收益"的实验一致) |

> 口径说明:上表的百分比是**含子帧**的采样占比(同一函数在多处出现会重复计入),
> 只用来看**排序**,不要当精确的耗时比例。

## 4. 由这份剖面直接得到的两条结论

1. **加载期的两个热点,都是"把 1796 万信号整体物化"的代价**:解码整张名字表 + 建 `Vec<Sig>`。
   它们不在查询路径上 —— 也就是说**只要查询用不到全表,这两块就该消失**(惰性/零加载,
   与 `.fcol` 按信号列、`.ftl` 时间线配合)。这与"缓存格式 v3(mmap + 零拷贝 arena)"是同一条路。
2. `from_utf8`、`Vec<usize>` 的 push 之类都是小头;真正的大头是**数据规模本身**,
   不是某个算法的常数 —— 所以优化方向是"**别物化**",不是"把某个循环写快 10%"。

## 5. 复现与约束

* `--features prof` 只在显式开启时编译 pprof-rs;默认构建**不引入任何新依赖**。
* 剖析二进制带 debuginfo(`CARGO_PROFILE_RELEASE_DEBUG=1 STRIP=false`),**它的墙钟数字不要当性能基线**,
  性能数字一律用普通 release 二进制 + `WAL_PROF=1` 的阶段账。
* 阶段打点位置:`src/main.rs`(`session`/`load`/`probe`)、`src/trace/fsdb.rs`(`fsdb:begin` / `fsdb:npi_init` / `fsdb:pre_open` / `fsdb:open` / `fsdb:names` / `fsdb:index`)。
