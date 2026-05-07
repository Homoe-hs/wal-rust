# wal-rust — WAL: Waveform Analysis Language

High-performance Rust implementation of [WAL](https://wal-lang.org), supporting VCD/FST waveform analysis at scale. **155GB VCD loaded in 9 minutes on 16-core CPU.**

```bash
$ wal-rust '(+ 1 2)'
=> 3

$ wal-rust '(load "trace.vcd") (signals)'
```

---

## Quick Start

```bash
# Install
cargo build --release
cp target/release/wal-rust ~/.local/bin/

# Evaluate expressions inline (auto-detected)
wal-rust '(+ 1 2)'
wal-rust '(load "dump.vcd") (signals)'

# Run a script file
wal-rust script.wal

# Interactive REPL
wal-rust repl

# Explicit subcommands (still work)
wal-rust run -l dump.vcd script.wal
wal-rust run -c '(signals)'
```

**Auto-detection:** input starts with `(` → evaluated as WAL expression.
Input is a file path → executed as WAL script. No input → REPL.

---

## WAL Language Reference

### Math

| Expression | Result |
|:-----------|:-------|
| `(+ 1 2 3)` | `6` |
| `(- 10 3)` | `7` |
| `(* 2 3 4)` | `24` |
| `(/ 10 3)` | `3.333...` |
| `(** 2 10)` | `1024` |
| `(mod 10 3)` | `1` |
| `(sum (list 1 2 3 4))` | `10` |
| `(average (list 1 2 3 4))` | `2.5` |
| `(max 3 7 1)` | `7` |
| `(min 3 7 1)` | `1` |

### Comparison

| Expression | Result |
|:-----------|:-------|
| `(= 5 5)` | `true` |
| `(!= 5 3)` | `true` |
| `(> 5 3)` | `true` |
| `(< 3 5)` | `true` |
| `(>= 5 5)` | `true` |
| `(<= 3 5)` | `true` |

### Logic

| Expression | Result |
|:-----------|:-------|
| `(&& #t #t #f)` | `false` |
| `(\|\| #f #f #t)` | `true` |
| `(! #f)` | `true` |
| `(bor 1 2 4)` | `7` |
| `(band 7 3)` | `3` |
| `(bxor 5 3)` | `6` |

### Variables & Functions

```lisp
;; Define
(define x 42)
(define name "world")

;; Let bindings — supports both formats:
(let ([x 10] [y 20]) (+ x y))   ;; vector pair format
(let (x 10 y 20) (+ x y))        ;; flat format

;; Set!
(set! x 100)

;; Named function
(defun sq [x] (* x x))
(sq 5)  ;; => 25

;; Anonymous function (inline call)
((fn [x] (+ x 1)) 5)  ;; => 6

;; Variadic function
(defun sum-all [xs] (fold + 0 xs))

;; Closures
(defun make-adder [n] (fn [x] (+ x n)))
(define add5 (make-adder 5))
(add5 3)  ;; => 8
```

### Control Flow

```lisp
;; If — falsy values: #f and ()
(if (> x 0) "positive" "negative")
(if () "yes" "no")     ;; => "no"
(if 0 "yes" "no")     ;; => "yes" (0 is truthy in WAL/Lisp)

;; Cond
(cond
  ((= x 1) "one")
  ((= x 2) "two")
  (#t "other"))

;; Case — supports default keyword
(case x
  (1 "one")
  (2 "two")
  (default "other"))

;; When / Unless
(when #t (print "always runs"))
(unless #f (print "also runs"))

;; Do (sequential evaluation)
(do (print "step 1") (print "step 2"))

;; While
(define i 0)
(while (< i 5)
  (print i)
  (set! i (+ i 1)))
```

### Strings

```lisp
(string-append "a" "b" "c")     ;; => "abc"
(printf "Value: %d" 42)          ;; prints "Value: 42"
(printf "hex: %x, bin: %b" 255 255)
(print "hello" " " "world")
(int->string 42)                 ;; => "42"
(string->int "42")               ;; => 42
(string->symbol "foo")           ;; => foo
(symbol->string 'foo)            ;; => "foo"
```

### Lists

```lisp
(list 1 2 3)              ;; => (1 2 3)
(first (list 10 20 30))   ;; => 10
(second (list 10 20 30))  ;; => 20
(last (list 10 20 30))    ;; => 30
(rest (list 1 2 3))       ;; => (2 3)
(in 2 (list 1 2 3))       ;; => true
(length (list 1 2 3 4))   ;; => 4

;; Map — accepts fn closures and operator symbols
(map (fn [x] (+ x 1)) (list 1 2 3))    ;; => (2 3 4)
(map + (list 1 2 3) (list 4 5 6))      ;; => (5 7 9)

;; Fold / Reduce
(fold + 0 (list 1 2 3 4))              ;; => 10
```

### Arrays (Key-Value Maps)

```lisp
;; Create — supports flat and vector pair formats
(define a (array ["x" 10] ["y" 20]))
(define a (array "x" 10 "y" 20))       ;; same result

;; Access
(geta a "x")                ;; => 10
(geta/default a 0 "z")      ;; => 0 (default if not found)
(seta a "z" 30)             ;; => ("x" 10 "y" 20 "z" 30)
(dela a "x")                ;; => ("y" 20)
(mapa a (fn [v] (* v 2)))   ;; => ("x" 20 "y" 40)
```

### Type Checking & Conversion

```lisp
(defined? 'x)          ;; check if x is defined
(atom? 42)             ;; true (non-list)
(symbol? 'foo)         ;; true
(string? "hello")      ;; true
(int? 42)              ;; true
(list? (list 1 2))     ;; true
(null? ())             ;; true
(empty? ())            ;; true

;; Conversions
(convert/bin 10)       ;; => "1010"
(convert/bin 10 8)     ;; => "00001010" (padded to 8 bits)
(string->int "42")     ;; => 42
(int->string 42)       ;; => "42"
(bits->sint 1)         ;; => -1  (2's complement)
```

---

## Waveform Analysis

### Loading

```lisp
;; Load VCD or FST (auto-detected by extension)
(load "sim.vcd")
(load "waveform.fst")

;; Load with custom trace ID
(load "sim.vcd" "trace_a")

;; Unload
(unload "trace_a")
```

### Navigation

```lisp
;; Current timestamps
index           ;; current position (0-based)
max-index       ;; last position
ts              ;; current simulation timestamp
trace-name      ;; trace ID
trace-file      ;; file path

;; Step forward/backward
(step 10)            ;; advance 10 steps
(step -5)            ;; go back 5 steps

;; Signal value access
(signals)            ;; list all signal names
(get "clk")          ;; signal value at current index
(get "data_bus")     ;; vector signal value

;; Relative time access (syntax sugar)
clk@+1               ;; value of clk 1 step ahead
data_bus@-2          ;; value of data_bus 2 steps back

;; Signal metadata
(signal-width "clk")    ;; bit width
(signal? "clk")         ;; true if signal exists
```

### Search & Find

```lisp
;; Find all indices matching a condition
(find (= (get "clk") 1))              ;; rising edges
(find (&& (= (get "clk") 1) (= (get "rst") 0)))

;; Count matching indices
(count (> (get "counter") 100))

;; Global find (across all known scopes)
(find/g (= (get "clk") 1))

;; Combo: check 5 steps ahead where signal was high
(whenever (= clk@+1 1)
  (print "next cycle will be high"))

;; Sample at specific index
(sample-at "clk" 100)

;; Fold over time
(fold signal expr init method)
```

### Scopes & Groups

```lisp
;; Named scopes
(scoped "top.sub" (get "counter"))
(all-scopes expr)
(resolve-scope "counter")

;; Groups (signal name prefixes)
(groups "_clk" "_data")               ;; find common prefixes
(in-group "mem" (get "addr"))         ;; evaluate in group context
(in-groups (list "mem" "cpu") (signals))

;; Syntax sugar
~top.sub        ;; equivalent to (in-scope "top.sub")
#clk            ;; equivalent to (resolve-group 'clk)
```

### Bus Analysis

The built-in `tl-handshakes`, `tl-latency`, `tl-bandwidth` operators
analyze TileLink bus protocols:

```lisp
(load "soc.vcd")
(tl-handshakes "soc.bus")       ;; handshake stats
(tl-latency "soc.bus")          ;; A→D transaction latency
(tl-bandwidth "soc.bus")        ;; bandwidth utilization
```

---

## FST Format Support

| Format | Encoding | Status |
|:-------|:---------|:-------|
| walconv (standard) | Little-endian | ✅ Full: signal names, hierarchy, VCDATA, ZWRAP |
| Icarus Verilog | Big-endian | ✅ Full: gzip HIER after GEOM, signal names, scopes |
| GTKWave examples | Big-endian | ✅ Verified: des.fst, transaction.fst, 10 test files |
| vcd2fst (GTKWave) | Big-endian | ⚠️ HDR+GEOM read OK, HIER signal names not decoded ** |
| ZWRAP (gzip) | Both | ✅ Auto-detect gzip vs zlib compression |

** vcd2fst uses a compact/packed HIER encoding that differs from the Icarus gzip format. No crash — gracefully returns 0 signals.

---

## Architecture

```
wal-rust/
├── src/
│   ├── main.rs              # CLI entry (auto-detect expr/file/repl)
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
│   │   ├── reader.rs        # FstReader (LE/BE auto-detect, Icarus HIER)
│   │   ├── writer.rs        # FstWriter (blocks, varint, compress)
│   │   └── types.rs         # BlockType, FstHeader
│   └── trace/               # Waveform interface
│       ├── trace.rs          # Trace trait, ScalarValue, FindCondition
│       ├── container.rs     # TraceContainer (Arc<RwLock<>>)
│       ├── vcd.rs           # VcdTrace (parallel two-pass + sparse index + LRU)
│       └── fst.rs           # FstTrace (on-demand block decompress + LRU)
├── tree-sitter-wal/         # WAL grammar
└── stress_tests/            # Generated stress test files
```

### Key Design Decisions

| Decision | Rationale |
|:---------|:----------|
| **Auto-detect expr/file/repl** | No subcommand needed for common cases |
| **tree-sitter parser** | Shared grammar with wal-lsp, supports @/#/~ syntax |
| **mmap + on-demand** | Pass 1 builds index (~460MB for 155GB), Pass 2 queries |
| **par_iter scanning** | 16-way rayon parallel chunk scanning |
| **madvise(MADV_SEQUENTIAL)** | 2MB kernel readahead reduces page faults |
| **Rc<RefCell\<Environment\>>** | Parent chain mutable for `set!` traversal |
| **macro-as-special-form** | defun/defunm expanded inline in eval_list |
| **FST endian auto-detect** | PI→LE, e→BE; no user configuration needed |
| **Icarus gzip HIER** | Reverse-engineered from GTKWave fstapi.c source |

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

## Performance

### 155GB VCD Benchmark

| Metric | Value |
|:-------|:------|
| File size | 155 GB |
| Signals | 103 |
| Timestamps | 385,314,044 |
| **Load time** | **9 min 05 sec** |
| Parallelism | 16 cores (rayon), 562% CPU |
| Memory | ~910 MB (PID RSS peak) |
| I/O throughput | 284 MB/s (SSD bound) |

### Stress Tests

| Test | Scale | Result |
|:-----|:------|:-------|
| Nesting depth | 1,000 levels | ✅ |
| WAL lines | 10,000,000 lines | ✅ 85s |
| Single-line args | 333,333 args | ✅ 0.89s |
| Concurrent files | 100 files | ✅ 0.8s |
| VCD loading | 100MB / 1GB / 10GB / 155GB | ✅ |

---

## License

MIT OR Apache-2.0
