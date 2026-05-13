# AGENTS.md — wal-rust

## Quick commands

```bash
cargo test                          # all Rust tests (112)
cargo test --test wal_integration_test  # integration tests only
cargo test test_vcd_pyvcd_verify_strobe -- --nocapture  # single test + stdout
cargo build --release               # release build
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.34  # glibc 2.34+ compatible
target/release/wal-rust '(expr)' -l trace.vcd   # eval expression
target/release/wal-rust run script.wal -l file  # run script
target/release/wal-rust repl        # interactive REPL
test_samples/run_tests.sh           # WAL script test runner
```

## CLI input auto-detect

No subcommand needed for common cases:
- starts with `(` → eval as expression
- existing file path → run as script
- no input → REPL

Subcommands: `run`, `repl`. Flags: `-l <waveform>` (repeatable), `-c <code>` (inline override).

## Source layout

| Dir | Content | ~Lines |
|-----|---------|--------|
| `src/wal/` | AST, tree-sitter parser, Evaluator, builtins (11 modules) | 5,200 |
| `src/vcd/` | VCD parser (mmap + memchr + two-pass) | 2,100 |
| `src/fst/` | FST reader/writer (LE/BE auto-detect, LZ4/zlib) | 2,700 |
| `src/trace/` | `Trace` trait, `VcdTrace`, `FstTrace`, `TraceContainer` | 2,150 |
| `tests/` | Rust integration & correctness tests (5 files, ~860 lines) | 860 |
| `test_data/` | VCD/FST test files (counter.vcd 11K, pyvcd_100M 107MB, edge cases) | — |
| `tree-sitter-wal/` | WAL grammar (`grammar.js`), compiled to `parser.c` via `build.rs` | — |

## Architecture notes

- **tree-sitter parser**: `build.rs` compiles `tree-sitter-wal/src/parser.c`. First build compiles C code.
- **FST endian auto-detect**: PI bytes → LE, e bytes → BE. No user config.
- **Dispatcher pattern** for builtins: (1) write handler in `src/wal/builtins/xxx.rs` (2) register in `builtins/mod.rs::register_all()` (3) optional `Operator` enum variant in `ast/operator.rs`.
- **Global allocator**: `mimalloc` in `src/main.rs` (no `#[global_allocator]` elsewhere).
- **VCD trace loading**: two-pass. Pass 1a scans header (sequential). Pass 1b scans dump in parallel chunks (Rayon). Builds sparse index per signal.
- **Signal value reads**: `read_signal_value_at()` uses `timestamp_offsets` + memchr jump scan (not line-by-line `read_line_bytes()`).

## Performance-sensitive paths

| Path | Mechanism |
|------|-----------|
| `VcdTrace::find_indices()` | Parallel chunk scan (Rayon), collects all changes for `signal_cache` |
| `VcdTrace::find_indices_batch()` | Single pass over VCD dump for N signals (N× faster than N individual calls) |
| `count` fast path | `(= (get "sig") 1)` uses `find_indices` directly |
| `count &&` decomposition | `(count (&& a b) ...)` → `BatchEntry::And` → single pass |
| `count` multi-arg batch | `(count cond1 cond2 ...)` → single `find_indices_batch` call |
| `whenever` do decomposition | `(whenever (= 1 1) (do ...))` → independent `count` calls |
| `signal_cache` | `find_indices` writes per-signal change history; `signal_value` uses it for O(log C) lookups |

## Builtin naming convention

- **Uppercase special variables** (bare or function-call): `SIGNALS`, `MAX-INDEX`, `TS`, `INDEX`, `TRACE-NAME`, `TRACE-FILE`, `CG`, `CS`, `SCOPES`
- **Lowercase operators** (always function-call): `count`, `find`, `whenever`, `get`, `step`, `define`, `set!`, `printf`, `+`, `-`, `&&`, `||`, `=`, `!=`, `first`, `rest`, `map`, `fold`, `length`
- Both `(SIGNALS)` and bare `SIGNALS` work and return the same list.

## Test data notes

- `test_data/test_pyvcd_150G.vcd` (155GB) may not exist on all clones (LFS-managed). Tests skip gracefully with `if !p.exists() { return; }`.
- `test_data/test_pyvcd_100M.vcd` (107MB) is required for strobe/counter pyvcd tests.
- `test_data/counter.vcd` (11KB) is the primary small test waveform (6 signals, 523 timestamps).

## GitHub Release

```bash
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.34
gh release create <tag> --title "v0.x.x" target/x86_64-unknown-linux-gnu/release/wal-rust
```

Binary requires glibc ≥ 2.34 (compatible with RHEL 9, Ubuntu 22.04+).
