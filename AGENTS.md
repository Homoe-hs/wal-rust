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
| `src/trace/` | `Trace` trait, `VcdTrace`, `FstTrace`, `TraceContainer` | 2,200 |
| `tests/` | Rust integration & correctness tests (6 files, ~900 lines) | 900 |
| `test_data/` | VCD/FST test files (counter.vcd 11K, pyvcd_100M 107MB, edge cases) | — |
| `tree-sitter-wal/` | WAL grammar (`grammar.js`), compiled to `parser.c` via `build.rs` | — |
| `docs/` | 设计/复盘: `query-engine-design.md`(统一引擎), `waveform-io-plan.md`(IO-1..6), `152gb-round.md`(P0 轮), `internal-feedback-review.md` | — |

## Architecture notes

- **tree-sitter parser**: `build.rs` compiles `tree-sitter-wal/src/parser.c`. First build compiles C code.
- **FST read backend**: wellen (`wellen::simple::read` in `src/trace/fst.rs`); legacy hand-rolled reader retired (writer stays: `src/fst/writer.rs`).
- **Dispatcher pattern** for builtins: (1) handler in `src/wal/builtins/xxx.rs` (2) register in `builtins/mod.rs::register_all()` (3) optional `Operator` variant in `ast/operator.rs`.
- **Global allocator**: `mimalloc` in `src/main.rs`.
- **VCD trace loading** (v0.12.0): PASS-1a header (sequential, also captures `$dumpvars` initial snapshot). PASS-1b parallel chunks emit **flat triples `(sig, ts, off)`** (file order) which merge into lazy per-signal `Vec<(ts,off)>` sparse anchors (no more BTreeMap per signal). One `madvise(DONTNEED)` at load end.
- **Signal value reads**: `read_signal_value_at()` uses sparse anchors (`partition_point` on the Vec) + memchr jump scan.
- **Query semantics (0.12.x, single authoritative definition)**: value AT an index = LAST write in that timestamp (delta cycles collapse); initial value = `$dumpvars` snapshot else x; edges = per-index transitions (x→1 is Changed, never Rising/Falling); count/find always scan the full timeline from INDEX 0 and restore the cursor. See `docs/query-engine-design.md` §1.

## Performance-sensitive paths

| Path | Mechanism |
|------|-----------|
| `VcdTrace::find_indices()` | Parallel chunk scan (Rayon), collects all changes for `signal_cache` |
| **warm same-signal query** | `find_indices` answers from the cached full-scan change list (O(C), no file rescan) — 58.7GB warm-2nd query 208s → ms |
| `VcdTrace::find_indices_batch()` | Single pass over VCD dump for N signals |
| `signal_cache` | `find_indices` writes per-signal change history; both `signal_value` (O(log C)) and warm `find_indices` consume it; capped at `MAX_DECODED_SIGNALS` (256) |
| `count` fast path | `(= (get "sig") 1)` uses `find_indices` directly |
| `count &&` decomposition | `(count (&& a b) ...)` → `BatchEntry::And` → single pass |
| `whenever` do decomposition | → independent `count` calls |
| **统一区间扫描引擎** | `interval_scan`(变更点并集边界 + 解释器值覆盖): count/find/whenever/count/step 同一实现;含边沿谓词时"边界真值 + 区间内部真值(边沿强制 false)"两段计入 |
| **纯逐拍 oracle** | `WAL_NO_ENGINE=1` 让引擎直接返回 None → 全部走逐拍;矩阵在子进程里用它做独立对拍 |

> 统一查询引擎(变更点并集区间扫描)已落地(docs/query-engine-design.md §IntervalSweep):
> 三个硬约束——①每个边界都要推进 prev ②区间内部边沿恒假、电平按区间长累加
> ③&&/|| 分解不得丢无法解析的谓词。矩阵 `tests/regression_matrix.rs` 是语义冻结闸。

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
版本线: 0.12.x(0.12.0 已发布);pre-commit 钩子自动 bump 补丁号(**Cargo.toml 已暂存时不 bump**——版本变更与代码同 commit 提交)。
