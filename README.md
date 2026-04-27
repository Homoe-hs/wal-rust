# wal-rust — WAL: Waveform Analysis Language

High-performance Rust implementation of [WAL](https://wal-lang.org), supporting VCD/FST waveform analysis at scale. **155GB VCD loaded in 9 minutes on 16-core CPU.**

```bash
$ wal-rust run -l trace.vcd analyze.wal
```

---

## Features

### Full WAL Language Support (82 operators)

| 类别 | 运算符 |
|------|--------|
| 数学 | `+`, `-`, `*`, `/`, `**`, `floor`, `ceil`, `round`, `mod`, `sum` |
| 逻辑 | `not`, `=`, `!=`, `>`, `<`, `>=`, `<=`, `&&`, `\|\|` |
| 位运算 | `bor`, `band`, `bxor` |
| 控制流 | `print`, `printf`, `set!`, `define`, `let`, `if`, `case`, `when`, `unless`, `cond`, `while`, `do`, `exit` |
| 列表 | `list`, `first`, `second`, `last`, `rest`, `in`, `map`, `fold`, `zip`, `max`, `min`, `average`, `length` |
| 类型 | `defined?`, `atom?`, `symbol?`, `string?`, `int?`, `list?`, `convert/bin`, `string->int`, `bits->sint`, `symbol->string`, `string->symbol`, `int->string` |
| 波形 | `load`, `unload`, `step`, `signals`, `index`, `max-index`, `ts`, `trace-name`, `trace-file`, `find`, `find/g`, `whenever`, `count`, `timeframe`, `get`, `call` |
| 作用域 | `scoped`, `all-scopes`, `resolve-scope`, `groups`, `in-group`, `in-groups`, `in-scope`, `in-scopes`, `resolve-group` |
| 数组 | `array`, `seta`, `geta`, `geta/default`, `dela`, `mapa` |
| 特殊形式 | `quote`, `quasiquote`, `unquote`, `eval`, `parse`, `rel_eval`, `slice`, `import` |

### Macros & Syntax Sugar

| 语法 | 展开 |
|------|------|
| `(defun f (x) body)` | `(define f (fn (x) body))` |
| `(defunm m (x) body)` | `(defmacro m (x) body)` |
| `(set! x val)` | `(set x val)` |
| `(for/list (x xs) body)` | `(map (fn (x) body) xs)` |
| `expr@offset` | `(rel_eval expr offset)` |
| `#signal` | `(resolve-group 'signal)` |
| `~scope` | `(in-scope scope)` |

### VCD/FST Waveform Engine

```
Two-pass scan architecture:
  Pass 1: sparse index (timestamps + offsets + BTreeMap<signal, time→offset>)
  Pass 2: on-demand mmap seek + zero-copy read_line_bytes + LRU cache
  
16-way parallel (rayon): 155GB VCD indexed in 9 minutes
Memory: ~460 MB for 155GB file (index only, no data loaded)
```

### Macro System

`defun`, `defunm`, `set!`, `for/list` implemented as syntactic transformations in the evaluator — no separate macro expander needed.

### Environment

`Rc<RefCell<Environment>>` parent chain with mutable traversal — `set!` modifies variables through the scope chain correctly.

---

## Performance

### 155GB VCD Benchmark

| 指标 | 值 |
|------|-----|
| 文件大小 | 155 GB |
| 信号数 | 103 |
| 时间戳数 | 385,314,044 |
| **加载时间** | **9 分 05 秒** |
| 并行度 | 16 核 (rayon), 562% CPU |
| 内存占用 | ~910 MB (PID RSS at peak) |
| I/O 吞吐 | 284 MB/s (接近 SSD 极限) |

### Stress Test Results

| 测试 | 规模 | 结果 |
|------|------|------|
| 嵌套深度 | 1,000 层 | ✅ |
| WAL 行数 | 10,000,000 行 | ✅ 85s |
| 单行参数 | 333,333 args | ✅ 0.89s |
| 并发文件 | 100 个 | ✅ 0.8s |
| VCD 加载 | 100MB / 1GB / 10GB / 155GB | ✅ |

---

## Installation

### Prerequisites

- Rust 1.70+
- Linux/macOS/Windows (Windows: mmap path untested)

### Build

```bash
git clone https://github.com/hesheng/wal-rust.git
cd wal-rust
cargo build --release
cp target/release/wal-rust ~/.local/bin/
```

### Verify

```bash
$ wal-rust --version
wal-rust 0.5.0

$ wal-rust run -c '(+ 1 2)'
=> 3
```

---

## Usage

### Quick Start

```bash
# Interactive REPL
wal-rust repl

# Run a script
wal-rust run script.wal

# Run with VCD preloading
wal-rust run -l dump.vcd script.wal

# Execute single expression
wal-rust run -c '(load "dump.vcd") (signals)'
```

### Example: Bus Protocol Analyzer

`tilelink.wal` — Auto-detects TileLink or AXI bus signals and computes:

```
  Bus Protocol Performance Analyzer
  ├── Protocol Detection (TileLink a_*/d_* or AXI axi_*)
  ├── Handshake Count (valid && ready)
  ├── Bus Utilization (stall ratio)
  ├── Bandwidth Estimation (data_beats × width / time)
  ├── Transaction Type Breakdown (Get/Put opcodes)
  └── Latency Sampling (A→D, AR→R, AW→W response time)
```

```bash
wal-rust run -l core_sim.vcd tilelink.wal
```

### WAL Script Reference

```lisp
;; Variables
(define x 42)
(set! x 100)

;; Functions
(defun sq (x) (* x x))
(sq 5)  ;; => 25

;; Waveform queries
(load "sim.vcd")
(signals)           ;; list all signal names
(step 10)           ;; advance 10 timestamps
(ts)                ;; current timestamp
(get "clk")         ;; signal value at current time
(find (= (get "clk") 1))  ;; all indices where clk=1

;; Lists
(map (fn (x) (* x 2)) (list 1 2 3))  ;; => (2 4 6)
(fold + 0 (list 1 2 3 4))            ;; => 10

;; Control flow
(if (> x 0) "positive" "negative")
(when true (print "always runs"))
(unless false (print "also runs"))
(cond ((= x 1) "one") (true "other"))

;; Syntax sugar
clk@100           ;; rel_eval clk 100
#rst_n            ;; resolve-group rst_n
~top             ;; in-scope top
```

---

## Architecture

```
wal-rust/
├── src/
│   ├── main.rs              # CLI entry (run, repl)
│   ├── cli.rs               # clap argument parsing
│   ├── wal/                 # WAL language core
│   │   ├── ast/             # Operator, Value, Symbol, WList, Closure, Macro
│   │   ├── parser/          # WalParser (tree-sitter + @/#/~ transforms)
│   │   ├── eval/            # Evaluator, Environment (Rc<RefCell>), Dispatcher
│   │   ├── builtins/        # 82 operators across 12 modules
│   │   └── repl/            # Interactive REPL (rustyline)
│   ├── vcd/                 # VCD parsing
│   │   ├── reader.rs        # MmapReader (madvise + memchr + zero-copy)
│   │   ├── parser.rs        # MmapVcdParser
│   │   └── types.rs         # VcdEvent, VcdValue
│   ├── fst/                 # FST read/write
│   │   ├── reader.rs        # FstReader
│   │   ├── writer.rs        # FstWriter (blocks, varint, compress)
│   │   └── types.rs         # BlockType, FstHeader
│   └── trace/               # Waveform interface
│       ├── trace.rs          # Trace trait
│       ├── container.rs     # TraceContainer (Arc<RwLock<>>)
│       ├── vcd.rs           # VcdTrace (parallel two-pass + sparse index + LRU)
│       └── fst.rs           # FstTrace
├── tree-sitter-wal/         # WAL grammar
├── stress_tests/            # Generated stress test files
└── tilelink.wal             # Bus protocol analyzer script
```

### Key Design Decisions

| 决策 | 说明 |
|------|------|
| **tree-sitter parser** | 共享 wal-lsp grammar，支持 @/#/~ 语法糖 |
| **mmap + on-demand** | Pass1 仅建索引（~460MB），Pass2 按需查询 |
| **par_iter** | rayon 并行分块扫描，16 核 562% CPU 利用率 |
| **madvise(MADV_SEQUENTIAL)** | 内核 2MB 预读，减少 page fault |
| **Rc<RefCell<Environment>>** | 父作用域可修改，支持 set! 跨 scope |
| **macro-as-special-form** | defun 等宏在 eval_list 中内联展开，无独立展开器 |

---

## Editor Integration

wal-rust pairs with [wal-lsp](https://github.com/hesheng/wal-lsp) for IDE features:

```
wal-lsp provides:
  - Syntax error diagnostics (real-time)
  - Semantic error checking (unknown functions, wrong arity)
  - 125+ completion items
  - Hover documentation
  - Go-to-definition
  - Document symbols
```

Configure in `~/.config/opencode/opencode.json`:
```json
{
  "lsp": {
    "wal": {
      "command": ["/path/to/wal-lsp"],
      "extensions": [".wal"]
    }
  }
}
```

---

## License

MIT OR Apache-2.0
