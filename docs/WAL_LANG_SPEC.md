# WAL Language Specification v0.8.2

> Source: https://wal-lang.org/documentation/core

---

## Core Language

### Arithmetic
`(+ expr*)`, `(- expr*)`, `(* expr*)`, `(/ a b)`, `(** base exponent)`, `(floor n)`, `(ceil n)`, `(round n)`

### Logic
`(! expr)`, `(&& expr*)`, `(|| expr*)`, `(= expr*)`, `(!= expr*)`, `(> a b)`, `(< a b)`, `(>= a b)`, `(<= a b)`

### Program State
`(let ((id expr)+) body)`, `(define id expr)`, `(set! id expr)`

### Functions
`(defun name (args+) body+)` — fixed args
`(defun name arg body+)` — variadic (single symbol)
`(fn (args+) body+)` — anonymous

### Control Flow
`(do body+)`, `(when cond body+)`, `(unless cond body+)`, `(if cond then else)`, `(cond (guard expr+)+)`, `(case key (value expr+)+)`

### Printing
`(print args*)`, `(printf format args*)`

### Utility
`(eval-file file)`, `(exit code)`

---

## Waveform Handling
`(load file id?)`, `(unload id)`, `(step id amount)`, `(alias name signal)`, `(unalias name)`, `(whenever cond body+)`, `(find cond)`, `(count cond)`, `(timeframe body+)`

## Accessing Signals
`(get signal)`, `(slice signal upper lower)`, `(reval expr offset)`, `signal@offset`

## Groups and Scopes
`(groups posts*)`, `(in-groups groups expr)`, `(resolve-group name)`, `#name`, `(in-scope scope body+)`, `~scope`, `(in-scopes (scope+) body+)`, `(all-scopes expr)`

## Lists
`(list expr*)`, `(first xs)`, `(second xs)`, `(last xs)`, `(rest xs)`, `(in x xs)`, `(min xs)`, `(max xs)`, `(sum xs)`, `(average xs)`, `(length xs)`, `(map f xs)`, `(fold fn init xs)`, `(zip xs ys)`

## Arrays
`(array (id expr)*)`, `(seta array key value)`, `(geta array key)`, `(geta/default array default key)`, `(dela array key)`, `(mapa f array)`

## Types
`(atom? x)`, `(symbol? x)`, `(string? x)`, `(int? x)`, `(list? x)`, `(convert/bin x width)`, `(string->int s)`, `(int->string n)`, `(symbol->string sym)`, `(string->symbol str)`, `(bits->sint bits)`

## Special Variables
`INDEX`, `MAX-INDEX`, `TS`, `SIGNALS`, `SIGNALS-NO-ALIAS`, `CG`, `CS`, `TRACE-NAME`, `TRACE-FILE`, `SCOPES`, `LOCAL-SIGNALS`, `LOCAL-SCOPES`, `VIRTUAL-SIGNALS`
