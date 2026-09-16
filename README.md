# wal-rust — WAL: Waveform Analysis Language

High-performance Rust implementation of [WAL](https://wal-lang.org), supporting VCD/FST/FSDB waveform analysis at scale.

**当前(v0.13.2)** — 58.7GB 合成波(3.5M 信号 / 1.5M 时间戳 / 1.65G 变更):

| 场景 | 代价 |
|:--|:--|
| 加载(`load` = 只读文件头 + mmap) | **约 1s**(0.13.1 前要 82s 冷 / 42s 暖 —— 那时加载会整扫一遍 dump) |
| 任意表达式 `count`/`find`(冷,首次查询) | **72.6s**(旧版 216s):懒建索引与变更列**同一次遍历**,只读一遍文件 |
| 同一信号再次查询(同进程) | **≈0s**(已解码变更列复用) |
| 同一信号再次查询(新进程) | **2.5s**(索引 + 列缓存命中;旧版 114s 全扫) |
| 多探针工作流 | `--stdin` 会话模式:一次加载,几十个探针不再重复加载/全扫 |
| 信号头解析(4M 信号) | **1.6s / 0.97GB RSS**(0.12.45:2.35s / 1.21GB) |

> **`load` 的语义**:`(load "x.vcd")` 与 `-l` 只读文件头(scope/`$var`/`$dumpvars`
> 初值快照)+ mmap;**时间戳表/采样锚点/事件点按需构建**(第一次真正需要 dump 区
> 数据的查询来付这一遍扫描)。所以"加载"不再是几十秒,冷成本落在第一次查询上。
> 若查询前能声明信号(`count`/`find`/`whenever` 等会通过 `Trace::prepare` 声明),
> 索引与这些信号的完整变更列在**同一次遍历**里一起算出并缓存。

> 内存口径:RSS 含 mmap 文件页驻留;进程**堆**是 O(信号数 + 被查询信号的变更列)。
> 详见 [Performance](#performance) 与 [docs/waveform-io-plan.md](docs/waveform-io-plan.md)。

```bash
$ wal-rust '(+ 1 2)'
=> 3

$ wal-rust '(load "trace.vcd") (SIGNALS)'
```

---

## Quick Start

```bash
# Install
cargo build --release
cp target/release/wal-rust ~/.local/bin/

# Evaluate expressions inline (auto-detected)
wal-rust '(+ 1 2)'
wal-rust '(load "dump.vcd") (SIGNALS)'

# Run a script file
wal-rust script.wal

# Interactive REPL
wal-rust repl

# Explicit subcommands (still work)
wal-rust run -l dump.vcd script.wal
wal-rust run -c '(SIGNALS)'

# One-shot queries (no WAL expression needed — shell/CI friendly)
wal-rust count dump.vcd clk 1                        # count signal==VALUE timestamps
wal-rust sigs dump.vcd "wdata" 20                    # signal names containing pattern
wal-rust topsig dump.vcd                             # most-active signals (change count)

# Session mode: one load, many probes (大波形上的推荐用法)
printf '(count (changes "clk"))\n(get "state")\n' | wal-rust --stdin -l dump.vcd

# Stop at the first script error (CI-friendly; default continues)
wal-rust run --halt-on-error script.wal -l dump.vcd

# Or install with automatic PATH registration:
bash scripts/install.sh
```

**Auto-detection:** input starts with `(` → evaluated as WAL expression.
Input is a file path → executed as WAL script. No input → REPL.

---

## Binary Quickstart

The GitHub release ships one self-contained binary (`wal-rust`, x86_64,
**glibc ≥ 2.17** — CentOS 7 / RHEL 7 / Ubuntu 16.04+). No Rust or waveform
toolchain needed: copy it, chmod +x, done.

```bash
# download from https://github.com/Homoe-hs/wal-rust/releases
chmod +x wal-rust && sudo mv wal-rust /usr/local/bin/   # or ~/.local/bin
wal-rust --version        # → wal-rust 0.12.0 (from /path/to/wal-rust)
```

### Three ways to use it

```bash
# 1. One-shot queries (no WAL expression needed — shell/CI friendly)
wal-rust count dump.vcd clk 1              # timestamps where clk == 1
wal-rust sigs dump.vcd "wstrb" 10          # names containing "wstrb" (first 10)
wal-rust topsig dump.vcd                   # most-active signals by change count

# 2. WAL expressions (the workhorse; wave pre-loaded with -l)
wal-rust -l sim.vcd '(take 5 (find-sig "clk"))'          # first 5 clk-named signals
wal-rust -l sim.vcd '(find-sig "*valid")'                # 通配: * 任意串 / ? 单字符
wal-rust -l sim.vcd '(count (is-x "sig"))'               # how much of sig is X
wal-rust -l sim.vcd '(getwave "clk")'                    # all change points (t v)
wal-rust -l sim.vcd '(edges "clk" 100000 200000)'        # change times in a window
wal-rust -l sim.vcd '(at "sig" 1515000)'                 # value at time t
wal-rust -l sim.vcd '(fmt-time 1515000)'                 # → "1.51us" human time
wal-rust -l sim.vcd '(assert-eq "sig" 100000 200000 1)'  # sig == 1 throughout [t0,t1]?
wal-rust -l sim.vcd '(count (&& (= (get "awvalid") 1) (= (get "awready") 1)))'  # handshakes

# 3. Scripts / REPL
wal-rust script.wal -l sim.vcd                # multi-line WAL scripts (define/map/fold…)
wal-rust run --halt-on-error script.wal -l sim.vcd   # CI: stop at first error
wal-rust repl                                 # interactive
```

### Gotchas

- **Time units**: `getwave/at/edges` return raw values in the file's native
  unit (VCD `$timescale`); use `(fmt-time T)` for human time,
  `(fmt-time T "clk")` for beat numbers.
- **Signal names**: `(find-sig "p")` or `sigs` for lookups; not-found errors
  give nearest-name hints. `SIGNALS` prints bounded (`...(N items)`), so
  90k-signal waves stay terminal-friendly.
- **x/z 语义(权威口径)**: [docs/4-state-semantics.md](docs/4-state-semantics.md)。
  要点:`x` 不等于 0(`(= (get a) 0)` 假、`(!= (get a) 0)` 真);`x→1` 算变化但
  **不算上升沿**;索引 0 没有前驱,除非 `$dumpvars` 给了确定初值(此时工具会告警提示);
  `count`/`count/step`/`count` 子命令/`find` 全入口同口径。
- **游标语义**:`(get s)` 取**当前 `INDEX`** 处的值,不随 `map`/遍历位置变化;
  按时间取值用 `(at s T)` 或 `(sample-at s idx)`。
- **样本 vs 初值**:变更点(样本)只来自 `#` 段;`$dumpvars` 快照是"索引 0 之前的持值",
  不进 `getwave` —— 所以"只有初值、之后不变"的信号 `(getwave s)` 为空(预期),
  读它的 t0 状态用 `(get s)`/`(at s 0)`/`(initial s)`。`(at s T)` 在首变化之前返回
  `(0 初值)`;首时间戳 >0 时索引 0 的值属于那个时间戳,不代表 t0。
- **多探针**:`--stdin` 会话模式在单进程内复用加载与列缓存;跨进程则靠
  `./.wal-rust-cache/` 下的索引 + 列缓存(默认 `WAL_CACHE=auto`,只写执行目录)。
- **多文件**:可 `-l a.vcd -l b.vcd` 同时加载;但**同名信号以后加载的为准**
  (按名字解析,没有 join/前缀区分),跨文件同名请用完整层次名或 `(in-scope ...)`。
- **退出码(CI 可判)**:`0` = 成功;`1` = 脚本/表达式有错误、或 `assert-eq` 失败;
  `(exit N)` = 立刻结束并返回 `N`。脚本模式默认**遇错继续执行**(后续行照跑),
  退出码仍为非 0,`--halt-on-error` 则在第一处错误停下。REPL 不受影响。
- **Full reference**: `(doc "edges")` one-liner for any command,
  `(doc <任意名字>)`(未知主题会列出全部可用主题),`(help)` 总览。

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

### Macros

```lisp
;; 定义宏: 参数不求值, 体是模板(quasiquote ` + 展开 , / ,@)
(defmacro twice (x) `(do ,x ,x))
(twice (print "hi"))                  ;; 打印 hi 两次

;; 只展开不求值(调试宏的利器)
(macroexpand '(twice (print "hi")))   ;; => (do (print "hi") (print "hi"))

;; 卫生宏: (gensym) 生成不冲突的符号
(gensym)                              ;; => GENSYM_0

;; 类函数宏: 调用处展开(展开结果是列表)
(defunm sq [x] (* x x))
(sq 5)                                ;; => (25)
```

`(help "defmacro")` / `(doc "macroexpand")` 查签名与说明;

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
(take 2 (list 1 2 3))     ;; => (1 2)  — first N elements (works on SIGNALS too)
(in 2 (list 1 2 3))       ;; => true
(length (list 1 2 3 4))   ;; => 4

;; Map — accepts fn closures and operator symbols
(map (fn [x] (+ x 1)) (list 1 2 3))    ;; => (2 3 4)
(map + (list 1 2 3) (list 4 5 6))      ;; => (5 7 9)

;; Comprehension (official WAL docs style; multi-binding = zip)
(for/list [x (list 1 2 3)] (* x 2))          ;; => (2 4 6)

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
(boolean? #t)          ;; true

;; Conversions
(convert/bin 10)       ;; => "1010"
(convert/bin 10 8)     ;; => "00001010" (padded to 8 bits)
(string->int "42")     ;; => 42
(int->string 42)       ;; => "42"
(bits->sint 1)         ;; => -1  (2's complement)
```

---

## Waveform Analysis

### 四值语义与游标(先读这一节)

- 权威口径:[docs/4-state-semantics.md](docs/4-state-semantics.md);`(doc semantics)` 可取摘要。
- `x` **不是** 0:`(= (get a) 0)` 为假、`(!= (get a) 0)` 为真;含 x/z 的值以**位串**返回(`"00x1"`)。
- 边沿:`x→1` / `x→0` **不算** rising/falling(但算一次 `changes`);
  索引 0 没有前驱,除非 `$dumpvars` 给了确定初值 —— 这种情况工具会打印一次性 warning。
- `(get s)` 取**当前 INDEX** 处的值,不随 `map`/遍历位置变化;按时间用 `(at s T)` / `(sample-at s idx)`。
- 各聚合入口同口径:`count` / `count/step` / `count` 子命令 / `find` / `getwave` / `whenever`。

### Loading

```lisp
;; Load VCD / FST / FSDB (auto-detected by extension and magic)
(load "sim.vcd")
(load "waveform.fst")
(load "waveform.fsdb")   ;; 需要 Verdi 的 NPI 读库, 见 "FSDB Format Support"

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
(SIGNALS)            ;; list all signal names
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

;; Find signal names by substring — the first step of any debug session
(find-sig "clk")                       ;; all names containing "clk"
(find-sig "*valid")                   ;; 通配: `*` 任意串, `?` 单字符(无通配=子串)
(take 5 (find-sig "wstrb"))            ;; first 5 matches

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

### Time-aware & Verification Queries

These work with actual timestamps (in the waveform's native unit; use
`(fmt-time T)` for human-readable time, `(fmt-time T "clk")` for beat numbers):

```lisp
(getwave "clk")                     ;; ((t v) ...) all change points
(edges "clk" 0 1000000)             ;; change timestamps in a window
(at "clk" 5000)                     ;; value at time t
(wave "clk" t0 t1)                  ;; windowed change points (held value first)
(assert-eq "sig" 100000 200000 1)   ;; true if sig equals v throughout [t0,t1]
(count (is-x "sig"))                ;; how much of the signal is unknown
(search "sig" "101" t0 t1)          ;; bit-pattern occurrence timestamps
(period "clk")                      ;; average clock period (seconds)
(freq "clk")                        ;; clock frequency (Hz)
```

`(count cond)` / `(find cond)` 求值**每一个索引**(官方 WAL 语义),由统一区间扫描引擎
实现:区间内部值恒定的部分按**区间长度**累加,含边沿谓词时只在边界取值 + 区间内部按
"边沿恒假"再求值一次 → 成本 O(变更点 × 表达式),而不是逐索引解释执行。
`(count/step cond)` / `(find/step cond)` 是**逐索引等价形式**(用于对拍/教学;大波形上慢很多,
因为每个索引真的求值一次)。`WAL_NO_ENGINE=1` 可强制走逐拍路径,作为独立 oracle。
`(whenever cond body)` 在每个命中索引处执行 body;`(whenever "changed" "sig" cond body)`
只在信号变化点采样。

Big list results are rendered bounded (`(...(N items))`), so `(print SIGNALS)`
on a 90k-signal wave stays terminal-friendly.

### Scopes & Groups

```lisp
;; Named scopes
(scoped "top.sub" (get "counter"))
(all-scopes expr)
(resolve-scope "counter")

;; Groups (signal name prefixes)
(groups "_clk" "_data")               ;; find common prefixes
(in-group "mem" (get "addr"))         ;; evaluate in group context
(in-groups (list "mem" "cpu") (SIGNALS))

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

> **注**: TileLink 分析操作符（`tl-handshakes`、`tl-latency`、`tl-bandwidth`）与 VCD→FST 转换器
> 已从 wal-rust 核心移除（协议/工具特定功能不适合放在语言核心；转换也违背"只专注波形"的定位）。
> 数值转换算子仍在: `(convert/bin x)`、`(int->string x)`、`(string->int s)`。

## FST Format Support

**Read backend: [wellen](https://crates.io/crates/wellen)** (`wellen::simple::read` in
`src/trace/fst.rs`) — the legacy hand-rolled reader is retired from the query path.
**Write backend: hand-rolled `FstWriter`** (`src/fst/`) — 用于往返测试与实验性导出,
**不是转换器**(wal-rust 不做格式转换;`dump-trace` 只写 VCD,`.fst` 路径会明确拒绝)。
真实 VCS 波形已验证 VCD≡FST 一致(含 512/1024 位向量、x/z、`$dumpoff`/`$dumpon` 窗口)。

| Format | Encoding | Status |
|:-------|:---------|:-------|
| walconv (standard) | Little-endian | ✅ Full: signal names, hierarchy, VCDATA, ZWRAP |
| Icarus Verilog | Big-endian | ✅ Full: gzip HIER after GEOM, signal names, scopes |
| GTKWave examples | Big-endian | ✅ Verified: des.fst, transaction.fst, 10 test files |
| vcd2fst (GTKWave) | Big-endian | ✅ Verified (2026-09-06): names/values decode via wellen; width suffix normalized (`sig [7:0]` → `sig` + width metadata) |

---

## FSDB Format Support(借 Verdi 的 NPI,纯 Rust FFI)

FSDB **不做逆向、不做转换** —— 直接调用 Synopsys 正版 Verdi 的 NPI 读库 `libNPI.so`,
调用方式是运行期 `dlopen` + `dlsym`:`wal-rust` 的二进制里**没有 C++ 依赖**,构建期也不需要
Verdi;找不到库时 FSDB 给出明确报错,VCD/FST 通路完全不受影响。

```bash
export VERDI_HOME=/path/to/verdi     # 或 WAL_NPI_LIB=/path/to/libNPI.so
wal-rust -l design.fsdb '(count (rising "clk"))'
wal-rust -l design.fsdb '(find (is-x "state"))'
```

要点(细节与实测结论见 `docs/fsdb-npi.md`):

* **只用归并迭代器** `npiFsdbTimeBasedVcIter`:一次遍历同时得到"全局时间线"(所有信号
  变更时间的并集)与被查询信号的变更列;`npi_fsdb_create_vct` 用过它之后就失效,所以全程不碰。
* **规则 A 一致**:t=0 条目是初值快照,不是 INDEX;索引 0 的值 = 该索引最后一次写入,
  首变化之前 = 初值(无条目 → x)。
* **环境全自动**:库路径从 `$VERDI_HOME` 推、`etc/` 资源目录自动补进 `LD_LIBRARY_PATH`、
  NPI 的 stdout banner 静音、日志目录挪进 `./.wal-rust-cache/npi/`。
* **许可**:NPI 在 `npi_fsdb_open` 时 checkout Verdi 许可(和打开 Verdi 一样占 seat)。
* **验证**:`tests/fsdb_diff.rs` 是环境变量开启的同源差分门(没 Verdi 自动跳过);
  实测 `verilog.fsdb ↔ verilog.vcd` **179 信号 × 406 索引全等**,并用 Verdi 自带的
  `fsdbdebug -vc -vidcode N` 复核过 t=0 取值。
* **两类已解释差异**:①VCD 把某些总线位炸开(`CH [4]…CH [0]`)而 NPI 归成一个 5bit 信号;
  ②`$dumpvars` 抓的是 delta 之前的 x、FSDB 记的是 t=0 结算后的值(FSDB 侧与 Verdi 一致)。

---

## Architecture

```
wal-rust/
├── src/
│   ├── main.rs              # CLI entry (auto-detect expr/file/repl)
│   ├── cli.rs               # clap argument parsing
│   ├── lib.rs               # Crate library root, re-exports
│   ├── wal/                 # WAL language core
│   │   ├── ast/             # Operator (125 个名字), Value, Symbol, WList, Closure, Macro
│   │   ├── lexer/           # Tokenizer: Token, TokenKind, Position
│   │   ├── parser/          # WalParser (tree-sitter + @/#/~ transforms, ==/% 归一化)
│   │   ├── eval/            # Evaluator, Environment (Rc<RefCell>), Dispatcher,
│   │   │                    #   统一区间扫描引擎 interval_scan(值覆盖 + 边沿区间语义)
│   │   ├── builtins/        # 145 个注册算子 / 12 个模块
│   │   └── repl/            # 交互 REPL(rustyline);--stdin 会话模式在 main.rs
│   ├── vcd/                 # VCD parsing
│   │   ├── reader.rs        # MmapReader (madvise + memchr + zero-copy + compression detection)
│   │   ├── parser.rs        # MmapVcdParser + VcdParser
│   │   └── types.rs         # VcdEvent, VcdValue
│   ├── fst/                 # FST writer (read happens via wellen in trace/fst.rs)
│   │   ├── writer.rs        # FstWriter (blocks, varint, compress)
│   │   ├── blocks.rs        # FST block types and parsing
│   │   ├── compress.rs      # Compression/decompression (gzip, zlib, LZ4)
│   │   ├── varint.rs        # Variable-length integer encoding/decoding
│   │   └── types.rs         # FstHeader, ScopeType, VarType, SignalDecl
│   └── trace/               # Waveform interface
│       ├── trace.rs          # Trace trait, ScalarValue, FindCondition, TraceId
│       ├── container.rs     # TraceContainer, SharedTraceContainer (Arc<RwLock<>>)
│       ├── vcd.rs           # VcdTrace: 并行两遍扫描 + 稀疏锚点 + 列/旁挂缓存 + LRU
│       └── fst.rs           # FstTrace via wellen(惰性解码 + 安全包装)
├── tree-sitter-wal/         # WAL grammar
├── tests/                   # 语义矩阵(38) + golden 小波(3) + 随机差分(3) + 集成测试
├── scripts/                 # diff_find.sh(语义对拍门禁) / perf_history.sh / 生成器
└── docs/                    # 4-state-semantics(四值口径) / query-engine-design /
                             #   waveform-io-plan(缓存设计) / 152gb-round(现场复盘)
```

### 缓存与一致性地基

| 机制 | 作用 |
|:--|:--|
| 同进程变更列缓存 | 同一信号第二次查询 ≈0s(`anchored_changes` 先查已解码列) |
| `./.wal-rust-cache/<wave>-<len>-<mtime>-v1.wcol` | 跨进程索引(跳过加载),写在实际执行命令的目录 |
| `<...>-v1.cols/<fnv(名字)>.col` | 按信号落盘的列缓存(只缓存被查询过的信号) |
| `WAL_COL_CACHE_MB` | 加载时按预算预建列,之后所有信号查询免扫 |
| `tests/regression_matrix.rs` | 快路径 == 逐拍 oracle;含子进程 `WAL_NO_ENGINE=1` 独立对拍 |
| `tests/golden_tiny.rs` | 手算黄金值(表达式 + CLI 子命令 + 会话模式) |
| `tests/fuzz_vcd_fst_diff.rs` | 随机波形:VCD↔FST 等价 + 引擎↔逐拍 + 缓存路径 |
| `scripts/diff_find.sh` | 与上一版二进制逐值对拍(语义零回归门禁) |

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
| **FST read backend = wellen** | battle-tested parser; hand-rolled reader retired from query path |
| **Icarus gzip HIER** | Reverse-engineered from GTKWave fstapi.c source (writer/roundtrip) |

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

### 大波形基准(2026-09-10,v0.13.2)

合成波形与本机(16 核 / NVMe)实测。**同表内对比才有意义**(绝对秒数受页缓存影响很大):

**58.7GB**(3.5M 信号 / 1.5M 时间戳 / ~1.65G 变更)

| 指标 | v0.11.11 基线 | v0.13.0 | v0.13.2 |
|:-----|:-------------|:--------|:--------|
| 加载(`load`/`(SIGNALS)`) | 360s | 82s(冷) / 42s(暖) | **1.1s**(只读文件头) |
| 冷查询(首个 count) | 674s | 216s | **72.6s** |
| 同进程第二次同信号查询 | 208s | ≈0s | **≈0s** |
| 新进程同信号查询 | 114s | 2.5s | **2.5s** |
| 峰值 RSS | 11.4GB | 3.0GB(加载) / 7.0GB(扫描中) | 同左(含 mmap 文件页) |

冷查询的物理下限:本机 NVMe 顺序读实测 **1.2-1.6GB/s**(`dd` 4 路并行也一样),
58.7GB 读一遍就是 37-49s;150GB 冷查询因此 ≥94s —— **≤60s 只能靠"不整读文件"**
(旁挂列缓存命中 2.5s,或后续把边沿列也做成常驻索引)。72.6s ≈ 单遍读 + 解析,
已经没有重复 IO。

**构造成本画像(1GB 真实 VCS 波形,300k 拍 × 260 信号)**

| 路径 | 代价 |
|:--|:--|
| 采样加载 | ~2.5s/GB(冷)/ 0.4s/GB(暖) |
| 冷扫描(页缓存暖) | ~2.7GB/s |
| 全量建列 `WAL_COL_CACHE_MB=2000` | 12s/GB + 4.5GB RSS,之后**所有信号查询免费** |
| 旁挂列缓存命中(第二次进程) | 0.03s |

**多信号 / 头解析(4M 信号,131MB header;交错 A/B 两轮)**

| 指标 | 0.12.45 | 0.13.0 |
|:--|:--|:--|
| 4M 信号头解析 | 2.35–2.90s / 1.21GB | **1.59–1.82s / 0.97GB** |
| 1M 信号查询(83MB) | 0.63s | **0.45s** |
| 1M 信号 + `$dumpvars` 初值快照 | 0.63s / 347MB | **0.45s / 282MB** |
| 更早的 0.12.41 基线 | 头解析 3.5–4.7s,1M 信号 0.86s | — |

手段:`$var` 计数预估容量、零分配解析、scope 前缀缓存、FxHash;
**0.13.0 起**:名字/ID 全部落在 arena(`names_blob` + `name_meta`)与开放寻址哈希表
(`OpenIndex`,探针用 `mix64` 打散),初值快照存紧凑 blob —— 每信号不再有独立
`Arc<str>` / `HashMap` 条目,内存和解析时间同时下降(4M 信号省 ~250MB)。

### 测量口径(重要)

- **RSS ≠ 堆**:波形经 mmap 读取,内核把映射页也算进 RSS;
  进程堆是 O(信号数 + 被查询信号的变更列)。看堆用 `smaps_rollup` 的 Private_Dirty。
- 页缓存状态会让同一条命令在 42s–82s 之间波动;比较时请在同一轮内 A/B。
- 复现脚本:`bash scripts/perf_history.sh <wave> <sig> <tag>`(结果追加到
  `bench/perf-history.csv`),回归闸:`bash scripts/diff_find.sh`。

### 早期基准(历史)

| 指标 | 值 |
|:-----|:---|
| 155GB VCD 加载(旧实现) | 9 min 05 sec |
| 152GB 实际 dump(0.11.x) | count 数分钟–数十分钟;0.12.x 起同信号重复查询≈0,跨进程 3.35s |
| 17.5GB 实际 dump(0.12.36 现场) | 单探针 35–130s(冷/暖);`--stdin` 会话模式与列缓存可显著摊薄 |

### Stress Tests(保持)

| Test | Scale | Result |
|:-----|:------|:-------|
| Nesting depth | 1,000 levels | ✅ |
| WAL lines | 10,000,000 lines | ✅ 85s |
| Single-line args | 333,333 args | ✅ 0.89s |
| Concurrent files | 100 files | ✅ 0.8s |
| VCD loading | 100MB / 1GB / 10GB / 58.7GB(合成) | ✅ |

---

## Build & Releases

```bash
cargo build --release                                # host build
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17   # old-glibc (≥2.17) binary
```

GitHub releases ship a **glibc ≥ 2.17** binary (CentOS 7 / RHEL 7 / Ubuntu 16.04+
compatible; `cargo zigbuild` cross-build). Local builds require the host toolchain.

---

## License

MIT OR Apache-2.0
