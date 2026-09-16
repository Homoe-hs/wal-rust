# AGENTS.md — wal-rust

## Quick commands

```bash
cargo test                          # all Rust tests (~232)
cargo test --test wal_integration_test  # integration tests only
cargo test test_vcd_pyvcd_verify_strobe -- --nocapture  # single test + stdout
cargo build --release               # release build
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17  # glibc 2.17+ compatible
target/release/wal-rust '(expr)' -l trace.vcd   # eval expression
target/release/wal-rust run script.wal -l file  # run script
target/release/wal-rust repl        # interactive REPL
test_samples/run_tests.sh           # WAL script test runner
bash scripts/diff_find.sh .tools/wal-rust.old target/release/wal-rust   # find semantic diff gate
WAL_NO_ENGINE=1 target/release/wal-rust '(count (&& (rising "c") (= (get "d") 3)))' -l x.vcd  # 禁用统一引擎(纯逐拍)= 独立 oracle
cargo test --test fuzz_vcd_fst_diff   # 随机波形差分: VCD↔FST 等价 + 引擎↔逐拍(可调 WAL_FUZZ_N/WAL_FUZZ_SEED)
cargo run --release --example npi_probe -- design.fsdb clk   # NPI 探针(不需要 wal-rust 全功能)
WAL_FSDB_TEST_FILE=x.fsdb WAL_FSDB_TEST_VCD=x.vcd cargo test --release --test fsdb_diff  # FSDB↔VCD 差分门(需 Verdi)
```

## CLI input auto-detect

No subcommand needed for common cases:
- starts with `(` → eval as expression
- existing file path → run as script
- no input → REPL

Subcommands: `run`, `repl`, `count <wave> <sig> [v]`, `sigs <wave> <pat> [n]`, `topsig <wave> [n]`.
Flags: `-l <waveform>` (repeatable), `-c <code>` (inline override), `--halt-on-error` (stop at first script error).

## Source layout

| Dir | Content | ~Lines |
|-----|---------|--------|
| `src/wal/` | AST, tree-sitter parser, Evaluator, builtins (11 modules) | 5,300 |
| `src/vcd/` | VCD parser (mmap + memchr + two-pass) | 2,100 |
| `src/fst/` | FST writer (wellen reads via `src/trace/fst.rs`; legacy reader retired) | 2,700 |
| `src/trace/` | `Trace` trait, `VcdTrace`, `FstTrace`, `FsdbTrace`, `TraceContainer` | 3,200 |
| `tests/` | Rust integration & correctness tests (6 files, ~900 lines) | 900 |
| `test_data/` | VCD/FST test files (counter.vcd 11K, pyvcd_100M 107MB, edge cases) | — |
| `tree-sitter-wal/` | WAL grammar (`grammar.js`), compiled to `parser.c` via `build.rs` | — |
| `docs/` | 设计/复盘: `query-engine-design.md`(统一引擎), `waveform-io-plan.md`(IO-1..6), `152gb-round.md`(P0 轮), `internal-feedback-review.md` | — |

## Architecture notes

- **tree-sitter parser**: `build.rs` compiles `tree-sitter-wal/src/parser.c`. First build compiles C code.
- **FST read backend**: wellen (`wellen::simple::read` in `src/trace/fst.rs`); legacy hand-rolled reader retired (writer stays: `src/fst/writer.rs`).
- **FSDB read backend**: `src/trace/fsdb.rs` —— 运行期 `dlopen` Synopsys NPI(`libNPI.so`)+
  Itanium mangled 符号,纯 Rust FFI,无 C++ 垫片。只用 `npiFsdbTimeBasedVcIter`(用过后
  `npi_fsdb_create_vct` 会失效);`npiFsdbValue.format` 是**入参**;库路径来自 `$VERDI_HOME`/
  `$WAL_NPI_LIB`。细节见 `docs/fsdb-npi.md`。差分门 `tests/fsdb_diff.rs` 由
  `WAL_FSDB_TEST_FILE` + `WAL_FSDB_TEST_VCD` 开启(没 Verdi 自动跳过)。
- **Dispatcher pattern** for builtins: (1) handler in `src/wal/builtins/xxx.rs` (2) register in `builtins/mod.rs::register_all()` (3) optional `Operator` variant in `ast/operator.rs`.
- **Global allocator**: `mimalloc` in `src/main.rs`.
- **VCD trace loading** (0.13.2 起两段式, 懒索引):
  - **load** = 只读文件头 PASS-1a($scope/$var/$dumpvars 初值快照)。58.7GB 的 `(SIGNALS)` 现在 1s 级。
  - **dump 区索引**(`DumpIndex`: 时间戳表 / 采样锚点 / 事件点)由 `dump()` 懒构建(OnceCell),
    谁需要 dump 区数据谁付这一遍扫描。
  - 查询前能声明信号时(`Trace::prepare` ← `interval_scan` / `find_indices`)索引与这些信号的
    **完整变更列**在**同一次遍历**里算出并写进 `signal_cache` + 旁挂列缓存 —— 冷启动只读一遍文件。
    PASS-1b 并行分块仍产 flat triples `(sig, ts, off)`, 合并成每信号 `Vec<(ts,off)>` 锚点。
  - 冷文件按 64MB 窗口 `madvise(MADV_WILLNEED)` 预读; 扫完 `madvise(DONTNEED)` 释放页。
- **改扫描/索索引路径后必须重跑 `tests/regression_matrix.rs`**: 融合路径的"廉价预筛"(比行尾字节 /
  `<id>` 结尾)必须用**解析出的 ID** 复核 —— 行 `1 11` 以 `1` 结尾且前一字符是值字符, 但 ID 是 `11`。
  这类 bug 只会在真实大文件上表现为数字对不上(77629 → 30741), 闸跑得少就会漏。
- **Signal value reads**: `read_signal_value_at()` uses sparse anchors (`partition_point` on the Vec) + memchr jump scan.
- **Query semantics (0.12.x, single authoritative definition)**: value AT an index = LAST write in that timestamp (delta cycles collapse); initial value = `$dumpvars` snapshot else x; edges = per-index transitions (x→1 is Changed, never Rising/Falling); count/find always scan the full timeline from INDEX 0 and restore the cursor. See `docs/query-engine-design.md` §1.

## Performance-sensitive paths

| Path | Mechanism |
|------|-----------|
| `VcdTrace::find_indices()` | Parallel chunk scan (Rayon), collects all changes for `signal_cache` |
| **warm same-signal query** | `anchored_changes` 先查 `signal_cache`(已解码变更列, full_scan)→ 同进程第 2 次起 O(1) 复用;58.7GB: 第二次同信号查询 ~85s → ≈0s(3 次 count 合计 227s ≈ 加载+首次扫描) |
| `VcdTrace::find_indices_batch()` | Single pass over VCD dump for N signals |
| `signal_cache` | `find_indices` writes per-signal change history; both `signal_value` (O(log C)) and warm `find_indices` consume it; capped at `MAX_DECODED_SIGNALS` (256) |
| `count` fast path | `(= (get "sig") 1)` uses `find_indices` directly |
| `count &&` decomposition | `(count (&& a b) ...)` → `BatchEntry::And` → single pass |
| `whenever` do decomposition | → independent `count` calls |
| **懒索引 + 变更列融合** | `(load)` 只读头; 首次查询声明信号(`Trace::prepare`)→ 索引与变更列同一次遍历; 58.7GB 冷查询 216s→73s, `(load)` 63s→1s |
| **统一区间扫描引擎** | `interval_scan`(变更点并集边界 + 解释器值覆盖): count/find/whenever/count/step 同一实现;含边沿谓词时"边界真值 + 区间内部真值(边沿强制 false)"两段计入 |
| **旁挂列缓存(跨进程)** | 冷扫描后按信号落盘 `<cache>/<wave>-<len>-<mtime>-v1.cols/<fnv(name)>.col`;下一个进程 `anchored_changes` 直接命中(58.7GB 同查询 113.8s → 3.35s) |
| **纯逐拍 oracle** | `WAL_NO_ENGINE=1` 让引擎直接返回 None → 全部走逐拍;矩阵在子进程里用它做独立对拍 |

> 统一查询引擎(变更点并集区间扫描)已落地(docs/query-engine-design.md §IntervalSweep):
> 五条硬约束——①每个边界都要推进 prev ②区间内部边沿恒假、电平按区间长累加
> ③&&/|| 分解不得丢无法解析的谓词 ④名字解析所有读取路径一致(短名/叶子名) ⑤引用 INDEX/TS 的条件必须放弃引擎(走逐拍)。矩阵 `tests/regression_matrix.rs` 是语义冻结闸。

## Language notes (0.12.x)

- **set! 词法穿透**: 绑定是共享 cell;`(set! x v)` 写 lookup 找到的那个绑定,闭包内修改对捕获方可见(累加器可用);fn 局部 `define` 每次调用独立。未知变量 set! 报错。
- **四大写语义**: `(get/at)` 初值 = `$dumpvars` 快照;per-index 最后写入;`(at s T)` 首变化前返回 `(0 初值)`。
- 大写特殊变量 `(SCOPES)`/`(CG)` 零参调用形式与裸符号等价。

## Test data notes

- `test_data/test_pyvcd_150G.vcd` (155GB) may not exist on all clones (LFS-managed). Tests skip gracefully.
- `test_data/test_pyvcd_100M.vcd` (107MB) required for strobe/counter pyvcd tests.
- `test_data/counter.vcd` (11KB): primary small fixture (6 signals, 523 timestamps).
- `.tools/` (gitignored): handwritten fixtures (x/z、glitch、dumpvars、alias、45-bit), vcd2fst binaries, old binaries for the diff gate.
- `bench/data/` (gitignored): synthetic large waveforms (76MB / 11.5GB / 58.7GB), `bench/RESULTS.md` has the numbers.

## GitHub Release

```bash
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17
gh release create <tag> --title "v0.x.x" target/x86_64-unknown-linux-gnu/release/wal-rust
```

Binary requires glibc ≥ 2.17 (CentOS 7 / RHEL 7 / Ubuntu 16.04+ compatible).
版本线: 0.13.x(0.13.0 已发布);pre-commit 钩子自动 bump 补丁号(**Cargo.toml 已暂存时不 bump**——版本变更与代码同 commit 提交)。
