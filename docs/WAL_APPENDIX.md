# WAL Language Specification Appendix

> **Date**: 2026-05-09
> **WAL Version**: 0.8.2 (wal-lang.org documentation)
> **wal-rust Version**: 0.8.4
> **Original WAL (Python)**: 0.8.7 (PyPI)

---

## 1. Test Methodology

Each documented feature was tested on BOTH interpreters using `test_samples/spec_audit.wal` (67 structured checks) and `test_samples/spec_test.wal` (83 assert-based checks).

- **Original interpreter**: `pip install wal-lang==0.8.7` run via `/tmp/wal-env/bin/wal`
- **Rust interpreter**: `target/debug/wal-rust`

---

## 2. Compliance Matrix

### 2.1 Core Language

| Doc Ref | Feature | Original | wal-rust | Notes |
|---------|---------|----------|----------|-------|
| 1.1 | `(+ expr*)` | ✅ | ✅ | |
| 1.1 | `(- expr*)` | ✅ | ✅ | |
| 1.1 | `(* expr*)` | ✅ | ✅ | |
| 1.1 | `(/ a b)` | ✅ | ✅ | Returns float |
| 1.1 | `(** base exp)` | ✅ | ✅ | Returns float |
| 1.1 | `(floor n)` | ✅ | ✅ | Added (not in spec 0.8.2) |
| 1.1 | `(ceil n)` | ✅ | ✅ | Added |
| 1.1 | `(round n)` | ✅ | ✅ | Added |
| 1.1 | `(mod a b)` | ✅ | ✅ | Added |
| 1.2 | `(! expr)` | ✅ | ✅ | |
| 1.2 | `(&& expr*)` | ✅ | ✅ | |
| 1.2 | `(|| expr*)` | ✅ | ✅ | |
| 1.2 | `(= expr*)` | ✅ | ✅ | List/string/symbol supported |
| 1.2 | `(!= expr*)` | ✅ | ✅ | |
| 1.2 | `(> a b)` | ✅ | ✅ | |
| 1.2 | `(< a b)` | ✅ | ✅ | |
| 1.2 | `(>= a b)` | ✅ | ✅ | |
| 1.2 | `(<= a b)` | ✅ | ✅ | |
| 1.3 | `(let (expr) body)` | ✅ | ✅ | Bindings are sequential (spec-compliant) |
| 1.3 | `(define id expr)` | ✅ | ✅ | Returns the bound value (spec-compliant) |
| 1.3 | `(set! id expr)` | ✅ | ✅ | Returns the new value |
| 1.4 | `(defun name args body)` | ✅ | ✅ | Includes variadic form |
| 1.4 | `(fn args body)` | ✅ | ✅ | Closures supported |
| 1.5 | `(do body+)` | ✅ | ✅ | |
| 1.5 | `(when cond body+)` | ✅ | ✅ | |
| 1.5 | `(unless cond body+)` | ✅ | ✅ | |
| 1.5 | `(if cond then else)` | ✅ | ✅ | |
| 1.5 | `(cond (guard expr)+)` | ✅ | ✅ | Supports `else` keyword |
| 1.5 | `(case key (val expr)+)` | ✅ | ✅ | Supports `default` keyword |
| 1.6 | `(print args*)` | ✅ | ✅ | |
| 1.6 | `(printf format args*)` | ✅ | ✅ | |

### 2.2 Waveform Handling

| Doc Ref | Feature | Original | wal-rust | Notes |
|---------|---------|----------|----------|-------|
| 2.1 | `(load file id?)` | ✅ | ✅ | Auto-generates t0, t1... |
| 2.1 | `(unload id)` | ✅ | ✅ | |
| 2.2 | `(step id amount)` | ✅ | ✅ | Supports negative steps |
| 2.3 | `(alias name signal)` | ✅ | ✅ | |
| 2.3 | `(unalias name)` | ✅ | ✅ | |
| 2.4 | `(whenever cond body+)` | ✅ | ✅ | |
| 2.5 | `(find cond)` | ✅ | ✅ | |
| 2.6 | `(count cond)` | ✅ | ✅ | |
| 2.7 | `(timeframe body+)` | ✅ | ✅ | |

### 2.3 Accessing Signals

| Doc Ref | Feature | Original | wal-rust | Notes |
|---------|---------|----------|----------|-------|
| 3.1 | `(get signal)` | ✅ | ✅ | Accepts symbol |
| 3.2 | `(slice s upper lower)` | ✅ | ✅ | List, string, and int bit-slicing |
| 3.3 | `(reval expr offset)` | ✅ | ✅ | Returns #f if out of bounds |
| 3.4 | `expr@offset` | ✅ | ✅ | Parsed to `rel_eval` |

### 2.4 Groups and Scopes

| Doc Ref | Feature | Original | wal-rust | Notes |
|---------|---------|----------|----------|-------|
| 4.1 | `(groups posts*)` | ⚠️ | ✅ | Original crashes with `Symbol` args |
| 4.2 | `(in-group group expr)` | ✅ | ✅ | |
| 4.3 | `(in-groups groups expr)` | ✅ | ✅ | |
| 4.4 | `(resolve-group name)` | ✅ | ✅ | Prepends CG |
| 4.5 | `#name` | ✅ | ✅ | |
| 4.6 | `(in-scope scope body+)` | ✅ | ✅ | |
| 4.7 | `(in-scopes scopes body+)` | ❌ | ✅ | Original lacks `in-scopes` |
| 4.8 | `(all-scopes expr)` | ❌ | ✅ | Original lacks `all-scopes` |

### 2.5 Lists

| Doc Ref | Feature | Original | wal-rust | Notes |
|---------|---------|----------|----------|-------|
| 5.1 | `(list expr*)` | ✅ | ✅ | |
| 5.2 | `(first xs)` | ✅ | ✅ | |
| 5.3 | `(second xs)` | ✅ | ✅ | |
| 5.4 | `(last xs)` | ✅ | ✅ | |
| 5.5 | `(rest xs)` | ✅ | ✅ | |
| 5.6 | `(in x xs)` | ✅ | ✅ | |
| 5.7 | `(map f xs)` | ✅ | ✅ | |
| 5.8 | `(fold fn init xs)` | ✅ | ✅ | Requires initial value |
| 5.9 | `(zip xs ys)` | ✅ | ✅ | |
| 5.10 | `(max xs)` | ✅ | ✅ | Takes list, not varargs |
| 5.11 | `(min xs)` | ✅ | ✅ | Takes list, not varargs |
| 5.12 | `(sum xs)` | ✅ | ✅ | |
| 5.13 | `(average xs)` | ✅ | ✅ | |
| 5.14 | `(length xs)` | ✅ | ✅ | |

### 2.6 Arrays

| Doc Ref | Feature | Original | wal-rust | Notes |
|---------|---------|----------|----------|-------|
| 6.1 | `(array [k v]*)` | ✅ | ✅ | |
| 6.2 | `(seta arr k v)` | ✅ | ✅ | Keys are normalized to String |
| 6.3 | `(geta arr k)` | ✅ | ✅ | |
| 6.4 | `(geta/default arr d k)` | ✅ | ✅ | |
| 6.5 | `(dela arr k)` | ✅ | ✅ | Cross-type key matching (via `key_to_string`) |
| 6.6 | `(mapa f arr)` | ✅ | ✅ | Passes (key, value) |

### 2.7 Types and Conversions

| Doc Ref | Feature | Original | wal-rust | Notes |
|---------|---------|----------|----------|-------|
| 7.1 | `(atom? x)` | ✅ | ✅ | |
| 7.2 | `(symbol? x)` | ✅ | ✅ | |
| 7.3 | `(string? x)` | ✅ | ✅ | |
| 7.4 | `(int? x)` | ✅ | ✅ | |
| 7.5 | `(list? x)` | ✅ | ✅ | |
| 7.6 | `(convert/bin x)` | ✅ | ✅ | Width optional |
| 7.7 | `(convert/bin x width)` | ✅ | ✅ | |
| 7.8 | `(string->int s)` | ✅ | ✅ | |
| 7.9 | `(int->string n)` | ✅ | ✅ | |
| 7.10 | `(symbol->string sym)` | ✅ | ✅ | |
| 7.11 | `(string->symbol str)` | ✅ | ✅ | |
| 7.12 | `(bits->sint s)` | ✅ | ✅ | Takes binary **string** (e.g. "1" → -1) |

### 2.8 Special Variables

| Variable | Original (0.8.7) | wal-rust | Notes |
|----------|------------------|----------|-------|
| `INDEX` | ✅ | ✅ | Current time index |
| `MAX-INDEX` | ✅ | ✅ | Also `(max-index)` function |
| `TS` | ✅ | ✅ | Same as INDEX |
| `SIGNALS` | ✅ | ✅ | Full signal list |
| `SIGNALS-NO-ALIAS` | ✅ | ✅ | Signals without aliases |
| `CG` | ✅ | ✅ | Current group |
| `CS` | ✅ | ✅ | Current scope |
| `TRACE-NAME` | ✅ | ✅ | Also `(trace-name)` function |
| `TRACE-FILE` | ✅ | ✅ | Also `(trace-file)` function |
| `SCOPES` | ✅ | ✅ | Scope hierarchy |
| `LOCAL-SIGNALS` | ❌ | ✅ | Rust extension |
| `LOCAL-SCOPES` | ❌ | ✅ | Rust extension |
| `VIRTUAL-SIGNALS` | ❌ | ✅ | Rust extension |

---

## 3. wal-rust Extensions (Not in WAL Spec 0.8.2)

| Feature | Description |
|---------|-------------|
| `(abs n)` | Absolute value |
| `(null? x)` | #t if nil or empty list |
| `(empty? x)` | #t if nil or empty list (alias) |
| `(third xs)` | Third element of list |
| `(string-append ...)` | Concatenate strings |
| `(bor a b)` | Bitwise OR |
| `(band a b)` | Bitwise AND |
| `(bxor a b)` | Bitwise XOR |
| `(while cond body)` | Loop while condition truthy |
| `(for/list (var list) body)` | List comprehension |
| `(scoped scope body+)` | Variant of in-scope |
| `(set-scope name)` | Set current scope |
| `(unset-scope)` | Clear current scope |
| `(signal-width signal)` | Get signal bit width |
| `(sample-at signal index)` | Get signal value at index |
| `(fold/signal sig expr init)` | Fold over signal timeline |
| `(trim-trace start end)` | Trim trace to range (stub) |
| `(defsig name expr)` | Define virtual signal |
| `(new-trace name)` | Create virtual trace (stub) |
| `(dump-trace path)` | Dump virtual trace to VCD |
| `(eval ...)` | Dynamic evaluation |
| `(parse ...)` | Parse string to expression |
| `(import path)` | Import WAL file |
| `(require name)` | Load module from search path |
| `(call fn args...)` | Dynamic function call |
| `(defmacro name args body)` | Macro definition |
| `(defunm ...)` | Macro via function syntax |
| `(macroexpand expr)` | Expand macro |
| `(gensym)` | Generate unique symbol |
| `(slice)` | Integer bit-slicing support |
| `(convert input output [comp])` | VCD→FST conversion |

---

## 4. Known Behavioral Differences

### 4.1 `groups` with Symbol Arguments

**Original WAL (0.8.7)**: `(groups '_a '_b)` crashes with `TypeError: cannot use 'Symbol' as a dict key` — Python implementation limitation.

**wal-rust**: Correctly handles both symbol and string arguments.

**Workaround**: Use string arguments: `(groups "_a" "_b")` works on both.

### 4.2 `let` Binding Order

**Original WAL (0.8.7)**: `(let [x 10] [y x] (+ x y))` → `30` — bindings are NOT sequential; `y` cannot see `x`.

**wal-rust**: `(let [x 10] [y x] (+ x y))` → `20` — bindings ARE sequential, matching the spec ("later bindings can use earlier bindings").

### 4.3 Output Format

Differences in how values are printed:

| Value | Original WAL | wal-rust |
|-------|-------------|----------|
| String | `hello` (unquoted) | `"hello"` (quoted) |
| Bool `true` | `true` | `true` |
| Bool `#t` | `#t` (literal) | `#t` (literal) |
| List | `(1 2 3)` | `(1 2 3)` |
| None/nil | `None` | `nil` |

### 4.4 `bits->sint` Argument

**Both**: Arguments must be a **binary string** like `"1"` or `"0001"`, not an integer.

---

## 5. Known Bugs (wal-rust)

| Bug | Status | Details |
|-----|--------|---------|
| FST signal names garbled | ✅ Fixed | vcd2fst inline HIER alignment + 1-based handle |
| `@` offset syntax | ✅ Fixed | `RelEval` as special form prevents pre-evaluation |
| `step -1` | ✅ Fixed | Negative steps via `set_index` |
| `(dela arr key)` cross-type | ✅ Fixed | `key_to_string` instead of `==` |
| `is_truthy` Int(0) | ✅ Fixed | 0 now correctly falsy per spec |
| `(groups)` symbol crash | Not applicable | Original WAL bug, not present in wal-rust |

---

## 6. Command Comparison

| Operation | Original WAL | wal-rust |
|-----------|-------------|----------|
| Run script | `wal script.wal` | `wal-rust script.wal` |
| Run with preload | `wal -l file.vcd script.wal` | `wal-rust run -l file.vcd script.wal` |
| REPL | `wal` | `wal-rust repl` |
| Inline expr | N/A (no -c flag) | `wal-rust '(+ 1 2)'` |
| Run without arg | `wal` (enters REPL) | `wal-rust` (auto-detects REPL) |
