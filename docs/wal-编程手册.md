# WAL 编程手册

> Waveform Analysis Language — 波形分析语言
> 基于 wal-rust 实现

## WAL 是什么

WAL (Waveform Analysis Language) 是面向**硬件波形分析**的领域特定语言。它不是通用的日志分析或软件 trace 工具——它专门处理**数字电路仿真产生的 VCD/FST 波形文件**。

### 硬件波形分析 vs 软件 Trace 分析

| 维度 | 软件 Trace | 硬件波形 (WAL 的领域) |
|------|-----------|-------------------|
| **值空间** | 字符串/数字 | **多值逻辑**: `0` `1` `X` `Z`，以及**位宽向量** |
| **时序** | 事件时间戳 | **仿真时间步** + 相对偏移 (`signal@+1`) |
| **信号关系** | 顺序执行 | **组合逻辑**: 多信号同时变化，值取决于信号组合 |
| **边沿** | 值变化 | **上升沿/下降沿**: `(rising clk)` 检测时钟跳变 |
| **传播** | N/A | **传播延迟**: 信号变化经过组合逻辑需要时间步 |

### WAL 的核心思想

WAL 将硬件分析中最常用的概念直接作为语言一等公民：

1. **信号即变量** — `clk`、`data_bus` 直接可读，其值自动跟随仿真时间
2. **时间步即迭代** — `(step)` 推进仿真时间，`INDEX` 定位当前时刻
3. **边沿即谓词** — `(rising clk)`、`(falling rst)` 表达边沿检测
4. **组合即组合** — `(&& (= clk 1) (! rst))` 表达信号组合条件
5. **相对时间即 `@`** — `data@-1` 表示上一个时钟周期的数据值

### 典型硬件分析场景

**1. 协议握手检查**
```wal
(find (&& (= tb.valid 1) (= tb.ready 1)))
```
在波形中找到所有 valid 和 ready 同时为高的时刻——完成握手事件定位。

**2. 时序违反检查**
```wal
(find (&& (= tb.req 1) (= (reval tb.grant 1) 0)))
```
在 `req` 拉高后的下一个时间步 `grant` 仍未拉高——检测响应超时。

**3. 状态覆盖率**
```wal
(defsig fsm_state (get "top.u_core.state"))
(length (find (= fsm_state 3)))  ;; IDLE 状态的 tick 数
```

**4. 组合逻辑分析**
```wal
(sample-at (get "top.u_alu.result")
  (first (find (&& (= (get "top.u_alu.op") 5) (= (get "top.u_alu.valid") 1)))))
```

### WAL 不擅长什么

- 软件函数调用链分析（用 perf/BPFTrace）
- 分布式系统日志聚合（用 Elastic/Kibana）
- 性能 Profiling（用 Valgrind/perf）
- 内存分析（用 AddressSanitizer/Valgrind）

这些场景的数据模型与硬件波形完全不同。WAL 的设计围绕**数字电路仿真波形**——带位宽的信号、仿真时间步、边沿检测、组合逻辑条件。

---

## 内容地图 (MOC)

### 1. 核心语言
- [算术](#11-算术): `+` `-` `*` `/` `**` `floor` `ceil` `round` `mod` `abs` `sum`
- [位运算](#12-位运算): `bor` `band` `bxor`
- [逻辑与比较](#13-逻辑与比较): `!` `=` `!=` `>` `<` `>=` `<=` `&&` `||`
- [程序状态](#14-程序状态): `define` `set` `set!` `let`
- [函数](#15-函数): `defun` `fn`
- [控制流](#16-控制流): `do` `when` `unless` `if` `cond` `case` `while`
- [打印输出](#17-打印输出): `print` `printf`
- [元编程](#18-元编程): `quote` `quasiquote` `unquote` `defmacro` `macroexpand` `gensym` `eval` `parse`
- [工具函数](#19-工具函数): `exit` `type` `eval-file` `require` `import` `call` `repl`
- [标准库宏](#110-标准库宏): 完整宏清单

### 2. 波形处理
- [加载与卸载](#21-加载与卸载): `load` `unload` `loaded-traces` `step`
- [信号别名](#22-信号别名): `alias` `unalias`
- [条件遍历](#23-条件遍历): `whenever` `find` `find/g` `count` `timeframe`
- [信号存在检查](#24-信号存在检查): `signal?`
- [信号位宽与采样](#25-信号位宽与采样): `signal-width` `sample-at` `fold/signal`
- [裁剪](#26-裁剪): `trim-trace`
- [虚拟信号](#27-虚拟信号): `defsig` `new-trace` `dump-trace`

### 3. 访问信号
- [直接信号访问](#31-直接信号访问): 信号名即变量
- [get](#32-get-signal): `get`
- [slice](#33-slice-signal-upper-lower): `slice`
- [reval](#34-reval-expr-offset): `reval`
- [@ 语法糖](#35--语法糖): `expr@offset`
- [特殊变量](#36-特殊变量): `INDEX` `MAX-INDEX` `TS` `SIGNALS` `CS` `CG` `LOCAL-SIGNALS` `VIRTUAL-SIGNALS` `TRACE-FILE` `TRACE-NAME` 等

### 4. 组与作用域
- [组](#41-组): `groups` `in-groups` `in-group` `resolve-group` `#name`
- [作用域](#42-作用域): `in-scope` `resolve-scope` `~name` `set-scope` `unset-scope` `all-scopes` `in-scopes`
- [特殊变量](#43-特殊变量): `CS` `CG` `LOCAL-SIGNALS` `SCOPES`

### 5. 列表
- [列表构建](#51-列表构建): `list`
- [列表访问](#52-列表访问): `first` `second` `third` `last` `rest`
- [成员检查](#53-成员检查): `in`
- [聚合](#54-聚合): `min` `max` `sum` `average` `length`
- [高阶函数](#55-高阶函数): `map` `fold` `zip`
- [范围生成](#56-范围生成): `range`
- [切片](#57-切片): `slice`
- [列表推导](#58-列表推导): `for/list`
- [宏定义列表函数](#59-宏定义列表函数): `car` `cdr` `cadr` `append` `reverse` `filter` `sort` `partition`

### 6. 数组
- [数组操作](#61-数组操作): `array` `seta` `geta` `geta/default` `dela` `mapa`

### 7. 类型与转换
- [类型谓词](#71-类型谓词): `atom?` `symbol?` `string?` `int?` `list?` `defined?` `signal?` `null?` `empty?` `type`
- [类型转换](#72-类型转换): `convert/bin` `string->int` `int->string` `string->symbol` `symbol->string` `bits->sint` `string-append` `convert`

### 8. wal-rust 差异
- [扩展操作符](#81-扩展操作符golden-无): `abs` `string-append` `third` `null?` `empty?`
- [已移除的非核心操作符](#82-已移除的非核心操作符): TileLink 分析、VCD→FST 转换
- [内置特殊形式](#83-内置特殊形式golden-中为-stdlib-宏): `when` `unless` `cond` `count` `timeframe` `sum`
- [行为差异](#84-行为差异): wal-rust 与 golden 行为对照表
- [特殊变量](#85-特殊变量): wal-rust 特殊变量列表

---

## 1. 核心语言

### 1.1 算术

#### `+`

**签名**: `(+ expr* ...)`  
**参数**:
- `expr* ...` (`any?`, 可变) — 要相加的值。0 个参数时返回 0。
**返回值**: 
- 所有参数为 `int?` 时 → `int?`
- 任一参数为 `float?` 时 → `float?`（整型自动提升为浮点）
- 任一参数为 `list?` 时 → `list?`（列表拼接）
- 任一参数为 `string?` 时 → `string?`（字符串拼接）
**说明**: 
- 支持混合类型算术：int + float → float
- 列表拼接：`(+ '(1 2) '(3 4))` → `(1 2 3 4)`
- 字符串拼接：`(+ "ab" "cd")` → `"abcd"`
- 非数值/非列表/非字符串类型会导致 `Invalid type for addition` 错误
**示例**:
```wal
(+ 1 2 3)
```
```
6
```
```wal
(+ 1.5 2)
```
```
3.5
```
```wal
(+ '(1 2) '(3 4))
```
```
(1 2 3 4)
```
```wal
(+)
```
```
0
```

#### `-`

**签名**: `(- n ...)`  
**参数**:
- `n*` (`int?` | `float?`, 至少 1 个) — 被减数及减数
**返回值**: `int?` 或 `float?`（取决于参数类型）
**说明**: 
- 单个参数：返回取负值 `(- 5)` → `-5`
- 多个参数：依次相减 `(- 10 3 2)` → `5`
- 参数类型必须一致（全 int 或全 float），混合类型返回 float
- 非数字类型返回 `Invalid type for subtraction` 错误
**示例**:
```wal
(- 10 3)
```
```
7
```
```wal
(- 5)
```
```
-5
```

#### `*`

**签名**: `(* expr* ...)`  
**参数**:
- `expr* ...` (`int?` | `float?`, 可变) — 要相乘的值。0 个参数时返回 1。
**返回值**: `int?` 或 `float?`
**说明**: 
- `(*)` 返回 1（与 golden 不同，golden 中 `*` 至少需 2 参数）
- 所有参数为 int 时返回 int；含 float 时返回 float
- 非数字类型返回 `Invalid type for multiplication` 错误
**示例**:
```wal
(* 2 3 4)
```
```
24
```
```wal
(*)
```
```
1
```

#### `/`

**签名**: `(/ a b)`  
**参数**:
- `a` (`int?` | `float?`) — 被除数
- `b` (`int?` | `float?`) — 除数
**返回值**: `float?`
**说明**: 
- 总是返回浮点数结果
- 除数为 0 时返回 `Division by zero` 错误
- 固定 2 参数
**示例**:
```wal
(/ 10 3)
```
```
3.3333333333333335
```
```wal
(/ 7.0 2)
```
```
3.5
```

#### `**`

**签名**: `(** base exp)`  
**参数**:
- `base` (`int?` | `float?`) — 底数
- `exp` (`int?` | `float?`) — 指数
**返回值**: `int?`（当 base 和 exp 均为 int 且 exp 在 0~20 范围内且不溢出时），否则 `float?`
**说明**: 
- 当两个参数均为 int 且 exp 在 0~20 范围内时，尝试 `checked_pow` 返回 int
- 超出范围或溢出时回退到浮点 `powf`
- 固定 2 参数
**示例**:
```wal
(** 2 10)
```
```
1024
```
```wal
(** 2.0 3)
```
```
8.0
```

#### `floor`

**签名**: `(floor n)`  
**参数**:
- `n` (`float?` | `int?`) — 数值
**返回值**: `int?`
**说明**: 向下取整。int 参数直接返回自身。
**示例**:
```wal
(floor 3.14)
```
```
3
```

#### `ceil`

**签名**: `(ceil n)`  
**参数**:
- `n` (`float?` | `int?`) — 数值
**返回值**: `int?`
**说明**: 向上取整。int 参数直接返回自身。
**示例**:
```wal
(ceil 3.14)
```
```
4
```

#### `round`

**签名**: `(round n)`  
**参数**:
- `n` (`float?` | `int?`) — 数值
**返回值**: `int?`
**说明**: 四舍五入。int 参数直接返回自身。
**示例**:
```wal
(round 3.6)
```
```
4
```

#### `mod`

**签名**: `(mod a b)`  
**参数**:
- `a` (`int?`) — 被除数
- `b` (`int?`) — 除数
**返回值**: `int?`
**说明**: 整数取模。除数为 0 时返回 `Modulo by zero` 错误。固定 2 参数。
**示例**:
```wal
(mod 10 3)
```
```
1
```

#### `abs`

**签名**: `(abs n)`  
**参数**:
- `n` (`int?` | `float?`) — 数值
**返回值**: `int?` 或 `float?`（与参数类型一致）
**说明**: 返回绝对值。wal-rust 扩展操作符，golden 无此函数。
**示例**:
```wal
(abs -5)
```
```
5
```
```wal
(abs -3.5)
```
```
3.5
```

#### `sum`

**签名**: `(sum xs)`  
**参数**:
- `xs` (`list?`) — 数字列表
**返回值**: `int?` 或 `float?`
**说明**: 
- 对列表中所有数字求和
- 空列表返回 0
- 含 float 时返回 float
- 列表中含非数字类型返回错误
**示例**:
```wal
(sum '(1 2 3))
```
```
6
```

### 1.2 位运算

#### `bor`

**签名**: `(bor int+ ...)`  
**参数**:
- `int+ ...` (`int?`, 至少 2 个) — 整数参数
**返回值**: `int?`
**说明**: 按位或。从左到右依次运算。
**示例**:
```wal
(bor 1 2 4)
```
```
7
```

#### `band`

**签名**: `(band int+ ...)`  
**参数**:
- `int+ ...` (`int?`, 至少 2 个) — 整数参数
**返回值**: `int?`
**说明**: 按位与。从左到右依次运算。
**示例**:
```wal
(band 7 3)
```
```
3
```

#### `bxor`

**签名**: `(bxor int+ ...)`  
**参数**:
- `int+ ...` (`int?`, 至少 2 个) — 整数参数
**返回值**: `int?`
**说明**: 按位异或。从左到右依次运算。
**示例**:
```wal
(bxor 5 3)
```
```
6
```

### 1.3 逻辑与比较

#### `!`

**签名**: `(! expr+ ...)`  
**参数**:
- `expr+ ...` (`any?`, 至少 1 个) — 表达式
**返回值**: `bool?`
**说明**: 
- 如果任意参数为真值，返回 `#f`；否则返回 `#t`
- 真值规则：`0`、`#f`、空列表 `()` 为假；其他所有值为真
- wal-rust 中支持任意类型，不限于整数
**示例**:
```wal
(! #f)
```
```
#t
```
```wal
(! 0 0)
```
```
#t
```
```wal
(! 0 1)
```
```
#f
```

#### `=`

**签名**: `(= expr+ ...)`  
**参数**:
- `expr+ ...` (`any?`, 至少 2 个) — 要比较的值
**返回值**: `bool?`
**说明**: 
- 比较所有参数是否全部相等
- 列表比较按元素逐一比较
- int 与 float 比较时自动转换：`(= 5 5.0)` → `#t`
**示例**:
```wal
(= 5 5)
```
```
#t
```
```wal
(= '(1 2) '(1 2))
```
```
#t
```

#### `!=`

**签名**: `(!= expr+ ...)`  
**参数**:
- `expr+ ...` (`any?`, 至少 2 个) — 要比较的值
**返回值**: `bool?`
**说明**: 
- 如果任意参数与第一个参数不相等，返回 `#t`
- 相当于 `not all equal`
**示例**:
```wal
(!= 5 3)
```
```
#t
```

#### `>`

**签名**: `(> n+ ...)`  
**参数**:
- `n+ ...` (`int?` | `float?`, 至少 2 个) — 数值
**返回值**: `bool?`
**说明**: 链式比较：每个元素必须严格大于下一个。`(> 5 4 3)` → `#t`。
**示例**:
```wal
(> 5 4 3)
```
```
#t
```
```wal
(> 5 5)
```
```
#f
```

#### `<`

**签名**: `(< n+ ...)`  
**参数**:
- `n+ ...` (`int?` | `float?`, 至少 2 个) — 数值
**返回值**: `bool?`
**说明**: 链式比较：每个元素必须严格小于下一个。`(< 1 2 3)` → `#t`。
**示例**:
```wal
(< 1 2 3)
```
```
#t
```

#### `>=`

**签名**: `(>= n+ ...)`  
**参数**:
- `n+ ...` (`int?` | `float?`, 至少 2 个) — 数值
**返回值**: `bool?`
**说明**: 链式比较：每个元素必须大于或等于下一个。`(>= 5 5 3)` → `#t`。
**示例**:
```wal
(>= 5 5 3)
```
```
#t
```

#### `<=`

**签名**: `(<= n+ ...)`  
**参数**:
- `n+ ...` (`int?` | `float?`, 至少 2 个) — 数值
**返回值**: `bool?`
**说明**: 链式比较：每个元素必须小于或等于下一个。`(<= 1 2 2)` → `#t`。
**示例**:
```wal
(<= 1 2 2)
```
```
#t
```

#### `&&`

**签名**: `(&& expr+ ...)`  
**参数**:
- `expr+ ...` (`any?`, 至少 1 个) — 表达式
**返回值**: `bool?`（`1` 或 `0`）
**说明**: 
- wal-rust 中为非短路求值：所有参数均被求值后再判断
- 所有参数均为真值时返回 `1`，否则返回 `0`
- 真值规则同 `!`
**示例**:
```wal
(&& #t #t #f)
```
```
0
```

#### `||`

**签名**: `(|| expr+ ...)`  
**参数**:
- `expr+ ...` (`any?`, 至少 1 个) — 表达式
**返回值**: `bool?`（`1` 或 `0`）
**说明**: 
- wal-rust 中为非短路求值：所有参数均被求值后再判断
- 任一参数为真值时返回 `1`，否则返回 `0`
**示例**:
```wal
(|| #f #f #t)
```
```
1
```

### 1.4 程序状态

#### `define`

**签名**: `(define id expr)`  
**参数**:
- `id` (`symbol?`) — 变量名
- `expr` (`any?`) — 值表达式
**返回值**: `any?`（expr 求值结果）
**说明**: 
- 特殊形式，`id` 不预求值（直接作为字面符号）
- 将 expr 求值结果绑定到 id，在新作用域定义变量
- 如果 id 已存在，返回重新定义错误
- 也支持函数定义形式：`(define (name args) body)`
**示例**:
```wal
(define x 10)
```
```
10
```

#### `set`

**签名**: `(set id expr)` 或 `(set (id expr)+ ...)`  
**参数**:
- `id` (`symbol?`) — 变量名
- `expr` (`any?`) — 值表达式
**返回值**: `any?`（最后一个赋值的值）
**说明**: 
- 特殊形式，id 不预求值
- 单模式：`(set x 10)` — 设置单个变量
- 多模式：`(set (x 1) (y 2))` — 同时设置多个变量
- 变量必须已定义（通过 `define` 或 `let`），否则返回错误
**示例**:
```wal
(define x 10)
(set x 20)
```
```
20
```
```wal
(set (x 1) (y (+ x 1)))
```

#### `set!`

**签名**: `(set! id expr)`  
**参数**:
- `id` (`symbol?`) — 变量名
- `expr` (`any?`) — 值表达式
**返回值**: `any?`
**说明**: wal-rust 内置宏，展开为 `(set id expr)`。
**示例**:
```wal
(define x 10)
(set! x (+ x x))
```
```
20
```

#### `let`

**签名**: `(let (binding+ ...) body+ ...)`  
**参数**:
- `binding+ ...` — 绑定列表，支持两种格式：
  - 扁平格式：`(x 1 y 2)`
  - 嵌套格式：`((x 1) (y 2))`
- `body+ ...` (`any?`) — 求值体
**返回值**: `any?`（最后一个 body 表达式的结果）
**说明**: 
- 特殊形式，bindings 不预求值
- 创建新作用域，按顺序求值绑定（后面的绑定可以使用前面的值）
- 求值 body，返回最后一个表达式结果
**示例**:
```wal
(let ((x 10) (y (+ x 5))) (+ x y))
```
```
25
```

### 1.5 函数

#### `fn`

**签名**: `(fn (args+ ...) body+ ...)`  
**参数**:
- `args+ ...` (`list?` | `symbol?`) — 参数列表。单个符号表示变参函数
- `body+ ...` (`any?`) — 函数体
**返回值**: `closure?`
**说明**: 
- 特殊形式，参数和函数体不预求值
- 创建闭包，捕获当前环境
- 变参：`(fn args body)` — 所有参数作为列表传递给 args
- 多个 body 表达式自动包装为 `do` 块
- wal-rust 中 `fn` 后可直接跟调用参数：`((fn (x) (* x 2)) 5)` → `10`
**示例**:
```wal
(fn (x) (* x 2))
```

#### `defun`

**签名**: `(defun name (args+ ...) body+ ...)`  
**参数**:
- `name` (`symbol?`) — 函数名
- `args+ ...` (`list?` | `symbol?`) — 参数列表
- `body+ ...` (`any?`) — 函数体
**返回值**: `closure?`
**说明**: 
- wal-rust 内置宏，展开为 `(define name (fn (args) body...))`
- 支持变参（参数为单个符号时）
- 多个 body 表达式自动包装为 do 块
**示例**:
```wal
(defun double (x) (* x 2))
(double 5)
```
```
10
```
```wal
(defun sum-all xs (fold + 0 xs))
(sum-all 1 2 3)
```
```
6
```

### 1.6 控制流

#### `do`

**签名**: `(do body+ ...)`  
**参数**:
- `body+ ...` (`any?`) — 按顺序求值的表达式
**返回值**: `any?`（最后一个表达式的结果）
**说明**: 特殊形式，body 不预求值。按顺序求值所有 body 表达式，返回最后一个。
**示例**:
```wal
(do (print "hello") (+ 1 2))
```
```
hello
3
```

#### `when`

**签名**: `(when cond body+ ...)`  
**参数**:
- `cond` (`any?`) — 条件表达式
- `body+ ...` (`any?`) — 条件为真时求值的表达式
**返回值**: `any?`（最后一个 body 的结果）或 `nil`
**说明**: 
- 特殊形式，条件不预求值
- 在 wal-rust 中为内置操作符（golden 中为标准库宏）
- cond 为真值时求值 body，否则返回 `nil`
**示例**:
```wal
(when #t (print "yes"))
```
```
yes
```

#### `unless`

**签名**: `(unless cond body+ ...)`  
**参数**:
- `cond` (`any?`) — 条件表达式
- `body+ ...` (`any?`) — 条件为假时求值的表达式
**返回值**: `any?`（最后一个 body 的结果）或 `nil`
**说明**: 
- 特殊形式，条件不预求值
- 在 wal-rust 中为内置操作符（golden 中为标准库宏）
- cond 为假值时求值 body，否则返回 `nil`
**示例**:
```wal
(unless #f (print "yes"))
```
```
yes
```

#### `if`

**签名**: `(if cond then else?)`  
**参数**:
- `cond` (`any?`) — 条件
- `then` (`any?`) — 条件为真时的表达式
- `else?` (`any?`, 可选) — 条件为假时的表达式
**返回值**: `any?`
**说明**: 
- 特殊形式，所有分支不预求值
- cond 为真值时求值 then 并返回
- cond 为假值且提供了 else 时求值并返回 else
- 无 else 且 cond 为假时返回 `nil`
- `then` 和 `else` 均为单个表达式，多用 `do` 包装
**示例**:
```wal
(if (> 3 2) "yes" "no")
```
```
yes
```

#### `cond`

**签名**: `(cond (guard expr+ ...)+ ...)`  
**参数**:
- `(guard expr+ ...)+ ...` (`list?`) — 子句列表
  - `guard` (`any?`) — 守卫表达式，`else` 或 `#t` 作为默认分支
  - `expr+ ...` (`any?`) — 守卫为真时求值的表达式
**返回值**: `any?` 或 `nil`
**说明**: 
- 特殊形式，子句不预求值
- 在 wal-rust 中为内置操作符（golden 中为标准库宏）
- 依次求值每个子句的 guard，第一个为真值时求值对应的 exprs
- 返回选中子句中最后一个 expr 的结果
- 无子句匹配时返回 `nil`
- `else` 符号作为 guard 时始终匹配
**示例**:
```wal
(cond ((= 1 2) "no")
      ((= 1 1) "yes")
      (else "maybe"))
```
```
yes
```

#### `case`

**签名**: `(case key (value expr+ ...)+ ...)`  
**参数**:
- `key` (`any?`) — 键表达式
- `(value expr+ ...)+ ...` (`list?`) — 子句列表
  - `value` (`any?`) — 匹配值；`default` 符号作为默认分支
  - `expr+ ...` (`any?`) — 匹配成功时求值的表达式
**返回值**: `any?` 或 `nil`
**说明**: 
- 特殊形式，key 和 value 均不预求值
- 求值 key，遍历子句，对每个子句求值 value 并与 key 比较
- 匹配时求值对应 exprs，返回最后一个结果
- `default` 符号作为子句值时始终匹配
- 无子句匹配时返回 `nil`
**示例**:
```wal
(case 2 (1 "one") (2 "two") (default "many"))
```
```
two
```

#### `while`

**签名**: `(while cond body)`  
**参数**:
- `cond` (`any?`) — 条件表达式
- `body` (`any?`) — 循环体
**返回值**: `any?`（最后一次 body 的结果）或 `nil`
**说明**: 
- 特殊形式，cond 和 body 不预求值
- 每次迭代前求值 cond，cond 为真值时求值 body
- cond 为假时终止，返回最后一次 body 的结果
- 若从未进入循环，返回 `nil`
**示例**:
```wal
(define i 0)
(while (< i 3) (print i) (set i (+ i 1)))
```
```
0
1
2
```

### 1.7 打印输出

#### `print`

**签名**: `(print expr* ...)`  
**参数**:
- `expr* ...` (`any?`, 可变) — 要打印的值
**返回值**: `nil`
**说明**: 
- 参数预求值后逐个打印
- 字符串输出时不带引号，符号输出时不带前缀
- 自动追加换行符
**示例**:
```wal
(print "hello" " " "world")
```
```
hello world
```

#### `printf`

**签名**: `(printf format args* ...)`  
**参数**:
- `format` (`string?`) — 格式字符串，支持 `%d` `%s` `%f` `%x` `%i` `%%`
- `args* ...` (`any?`, 可变) — 格式化参数
**返回值**: `nil`
**说明**: 
- 第一个参数必须为字符串
- 格式说明符：`%d`/`%i`（整数）、`%s`（字符串）、`%f`（浮点）、`%x`（十六进制）
- 支持宽度和对齐标志：`%04d`、`%-10s`
- 支持精度：`%.2f`
- 支持转义序列：`\n`、`\t`、`\"`
- 格式串不含 `%` 时，`{0}` `{1}` 等会被替换为对应参数
**示例**:
```wal
(printf "value = %d, pi = %.2f" 42 3.14159)
```
```
value = 42, pi = 3.14
```

### 1.8 元编程

#### `quote`

**签名**: `(quote expr)`  
**参数**:
- `expr` (`any?`) — 任意表达式
**返回值**: `any?`（原始 AST）
**说明**: 
- 特殊形式，expr 不预求值
- 阻止求值，返回表达式本身
- `'expr` 是 `(quote expr)` 的简写
**示例**:
```wal
(quote (+ 1 2))
```
```
(+ 1 2)
```
```wal
'(+ 1 2)
```
```
(+ 1 2)
```

#### `quasiquote`

**签名**: `(quasiquote expr)`  
**参数**:
- `expr` (`any?`) — 带模板的表达式
**返回值**: `any?`
**说明**: 
- 特殊形式，expr 不预求值
- 与 quote 类似，但允许通过 `unquote`（`,expr`）和 `unquote-splice`（`,@expr`）选择性求值
- `` `expr `` 是 `(quasiquote expr)` 的简写
- `unquote` 在 quasiquote 外使用时报错
**示例**:
```wal
(define x 5)
`(+ 1 ,x)
```
```
(+ 1 5)
```

#### `unquote`

**签名**: `(unquote expr)`  
**参数**:
- `expr` (`any?`) — 欲求值的表达式
**返回值**: 无（直接求值报错）
**说明**: 只能在 quasiquote 上下文中使用，直接调用报 `unquote outside quasiquote` 错误。

#### `defmacro`

**签名**: `(defmacro name (args+ ...) body+ ...)`  
**参数**:
- `name` (`symbol?`) — 宏名
- `args+ ...` (`list?` | `symbol?`) — 参数列表或变参符号
- `body+ ...` (`any?`) — 宏展开体
**返回值**: `macro?`
**说明**: 
- 特殊形式，参数和 body 不预求值
- 定义在编译时展开代码的宏
- 宏参数接收原始 AST（未求值），返回新 AST
- 支持变参（参数为单个符号时）
**示例**:
```wal
(defmacro twice (x) `(do ,x ,x))
(twice (print "hi"))
```
```
hi
hi
```

#### `macroexpand`

**签名**: `(macroexpand expr)`  
**参数**:
- `expr` (`any?`) — 宏调用表达式（列表）
**返回值**: `any?`（展开后的 AST）
**说明**: 
- 参数预求值，结果必须是列表 `(macro-name args...)`
- 查找宏定义，展开并返回展开结果（不进一步求值）
**示例**:
```wal
(macroexpand '(when #t (print "hi")))
```

#### `gensym`

**签名**: `(gensym)`  
**参数**: 无
**返回值**: `symbol?`
**说明**: 生成全局唯一的符号，名称格式为 `GENSYM_0`、`GENSYM_1` 等。
**示例**:
```wal
(gensym)
```

#### `eval`

**签名**: `(eval expr)`  
**参数**:
- `expr` (`any?`) — 表达式
**返回值**: `any?`
**说明**: 
- 参数预求值，将求值结果作为 WAL 表达式再次求值
- 相当于两层求值
**示例**:
```wal
(eval '(+ 1 2))
```
```
3
```

#### `parse`

**签名**: `(parse str)`  
**参数**:
- `str` (`string?`) — 包含 WAL 代码的字符串
**返回值**: `any?`（解析后的 AST）
**说明**: 将字符串解析为 WAL 表达式并返回 AST。
**示例**:
```wal
(parse "(+ 1 2)")
```

### 1.9 工具函数

#### `exit`

**签名**: `(exit code?)`  
**参数**:
- `code?` (`int?`, 可选) — 退出码，默认 0
**返回值**: 不返回（终止程序）
**说明**: 以指定退出码终止程序。无参数时默认返回 0。
**示例**:
```wal
(exit 1)
```

#### `type`

**签名**: `(type x)`  
**参数**:
- `x` (`any?`) — 任意值
**返回值**: `string?`
**说明**: 
- 返回类型的字符串表示
- wal-rust 返回格式：`"<class 'int'>"`、`"<class 'str'>"`、`"<class 'bool'>"`、`"<class 'float'>"`、`"<class 'symbol'>"`、`"<class 'list'>"`、`"<class 'NoneType'>"` 等
- golden 返回 Python type 对象，wal-rust 返回字符串
**示例**:
```wal
(type 42)
```
```
<class 'int'>
```

#### `eval-file`

**签名**: `(eval-file path)`  
**参数**:
- `path` (`string?`) — 文件路径
**返回值**: `any?`
**说明**: 读取文件内容并作为 WAL 代码求值。文件不存在或无法读取时返回错误。
**示例**:
```wal
(eval-file "script.wal")
```

#### `require`

**签名**: `(require name)`  
**参数**:
- `name` (`symbol?`) — 模块名
**返回值**: `any?`
**说明**: 
- 在搜索路径中查找 `<name>.wal` 文件并求值
- 搜索路径：`.`、`/usr/local/share/wal/stdlib`、`/usr/share/wal/stdlib`
- 模块未找到时返回错误
**示例**:
```wal
(require std)
```

#### `import`

**签名**: `(import path)`  
**参数**:
- `path` (`string?`) — 文件路径
**返回值**: `any?`
**说明**: 
- wal-rust 中加载 WAL 源文件（与 golden 不同，golden 加载 Python 模块）
- 读取文件并作为 WAL 代码求值
**示例**:
```wal
(import "utils.wal")
```

#### `call`

**签名**: `(call fn args* ...)`  
**参数**:
- `fn` (`any?`) — 可调用的函数/宏/符号
- `args* ...` (`any?`) — 调用参数
**返回值**: `any?`
**说明**: 动态调用函数或宏。第一个参数求值后应为闭包、宏或可调用的符号。
**示例**:
```wal
(call (fn (x) (* x 2)) 5)
```
```
10
```

#### `repl`

**签名**: `(repl)`  
**参数**: 无
**返回值**: 不返回
**说明**: wal-rust 中交互式 REPL 不可用，返回错误。

### 1.10 标准库宏

以下宏定义在标准库 `std/std.wal` 中或在 wal-rust 中直接内置：

| 宏 | 签名 | 说明 |
|------|------|------|
| `defun` | `(defun name [args] body+)` | 定义函数，展开为 `(define name (fn [args] body...))`（wal-rust 内置） |
| `when` | `(when cond body+)` | 条件为真时求值 body（wal-rust 内置） |
| `unless` | `(unless cond body+)` | 条件为假时求值 body（wal-rust 内置） |
| `cond` | `(cond [guard expr+]+)` | 多分支条件（wal-rust 内置） |
| `case` | 见上文 | 值匹配分支（特殊形式） |
| `for/list` | `(for/list [sym data] body+)` | 列表推导，展开为 `map`（wal-rust 内置） |
| `for` | `(for [sym data] body+)` | 列表遍历（无返回值，副作用，标准库宏） |
| `dowhile` | `(dowhile body... cond)` | 至少执行一次 body 后检查 cond（标准库宏） |
| `until` | `(until cond body+)` | 循环直至 cond 为真（标准库宏） |
| `step-until` | `(step-until condition)` | 步进直到 condition 满足（标准库宏） |
| `step-while` | `(step-while condition)` | 步进直到 condition 不满足（标准库宏） |
| `always` | `(always body+)` | 等价于 `(whenever #t body...)`（标准库宏） |
| `inc` | `(inc sym ...)` | 变量自增 1，支持多个变量（标准库宏） |
| `dec` | `(dec sym ...)` | 变量自减 1（标准库宏） |
| `rising` | `(rising expr)` | 上升沿检测：expr 从 0→1（标准库宏） |
| `falling` | `(falling expr)` | 下降沿检测：expr 从 1→0（标准库宏） |
| `stable` | `(stable expr)` | 信号稳定检测（标准库宏） |
| `unstable` | `(unstable expr)` | 信号不稳定检测（标准库宏） |
| `signed` | `(signed signal)` | 将有符号整数转为补码形式（标准库宏） |
| `timeframe` | `(timeframe body+)` | 保存/恢复 INDEX（wal-rust 内置） |
| `count` | `(count cond)` | 计算条件为真的次数（wal-rust 内置） |
| `sum` | `(sum xs)` | 列表求和（wal-rust 内置） |
| `car` | `(car xs)` | 等价于 `first`（标准库宏） |
| `cdr` | `(cdr xs)` | 等价于 `rest`（标准库宏） |
| `cadr` | `(cadr xs)` | 等价于 `(car (cdr xs))`（标准库宏） |
| `append` | `(append xs x)` | 列表末尾追加（标准库宏） |
| `reverse` | `(reverse xs)` | 反转列表（标准库宏定义函数） |
| `filter` | `(filter p xs)` | 列表过滤（标准库宏定义函数） |
| `sort` | `(sort xs)` | 数字列表排序（标准库宏定义函数） |
| `partition` | `(partition p xs)` | 列表分区（标准库宏） |
| `set!` | `(set! key value)` | 展开为 `(set key value)`（wal-rust 内置） |
| `defunm` | `(defunm name [args] body)` | 定义可变参数宏（wal-rust 内置） |
| `symbol-add` | `(symbol-add args...)` | 符号名拼接（标准库宏定义函数） |
| `groups-excluding` | `(groups-excluding (including ...) (excluding ...))` | 过滤组（标准库宏） |
| `inc-define` | `(inc-define sym ...)` | 不存在则初始化为 1 再自增（标准库宏） |

---

## 2. 波形处理

### 2.1 加载与卸载

#### `load`

**签名**: `(load file id?)`  
**参数**:
- `file` (`string?`) — 波形文件路径
- `id?` (`string?`, 可选) — 波形的注册 ID
**返回值**: `nil`
**说明**: 
- 从文件加载波形，使用 `id` 注册
- 未提供 `id` 时自动生成 `t0`、`t1`、`t2`... 方案选择
- 底层加载由 TraceContainer 实现
**示例**:
```wal
(load "trace.vcd")
(load "trace.vcd" "my_trace")
```

#### `unload`

**签名**: `(unload id)`  
**参数**:
- `id` (`string?`) — 要卸载的波形 ID
**返回值**: `nil`
**说明**: 从内核中移除指定 ID 的波形。
**示例**:
```wal
(unload "t0")
```

#### `loaded-traces`

**签名**: `(loaded-traces)`  
**参数**: 无
**返回值**: `list?`（字符串列表）
**说明**: 返回所有已加载波形的 ID 列表。
**示例**:
```wal
(loaded-traces)
```

#### `step`

**签名**: `(step id? amount?)`  
**参数**:
- `id?` (`string?`, 可选) — 追踪 ID
- `amount?` (`int?`, 可选, 默认 1) — 步进步数
**返回值**: `bool?`
**说明**: 
- `(step)` — 所有追踪步进 1 步
- `(step amount)` — 所有追踪步进 amount 步
- `(step id amount)` — 指定追踪步进 amount 步
- 负值后退：`(step -1)` 后退 1 步
- 所有加载追踪均未到达末尾时返回 `#t`，任一到达末尾返回 `#f`
**示例**:
```wal
(step 1)
```

### 2.2 信号别名

#### `alias`

**签名**: `(alias name signal)`  
**参数**:
- `name` (`symbol?`) — 别名符号
- `signal` (`symbol?`) — 目标信号名
**返回值**: `nil`
**说明**: 
- 特殊形式，name 直接作为字面符号（不预求值）
- 为信号引入别名，使其可通过 name 引用
- 别名与组和作用域兼容
**示例**:
```wal
(alias clk top.clk)
```

#### `unalias`

**签名**: `(unalias name)`  
**参数**:
- `name` (`symbol?`) — 要移除的别名
**返回值**: `nil`
**说明**: 
- 特殊形式，name 直接作为字面符号
- 移除别名。别名不存在时返回错误
**示例**:
```wal
(unalias clk)
```

### 2.3 条件遍历

#### `whenever`

**签名**: `(whenever cond body+ ...)`  
**参数**:
- `cond` (`any?`) — 条件表达式
- `body+ ...` (`any?`) — 条件为真时求值的表达式
**返回值**: `any?`（最后一个匹配位置上的最后一个 body 的结果）
**说明**: 
- 特殊形式，cond 和 body 不预求值
- 从当前 INDEX 开始遍历所有时间点
- 在每个 cond 求值为真的索引上求值 body
- 遍历完成后恢复原始 INDEX
- 有快速路径：`(= (get "sig") val)` 形式的简单条件使用索引扫描优化
**示例**:
```wal
(whenever (= clk 1) (print INDEX))
```

#### `find`

**签名**: `(find cond max-results?)`  
**参数**:
- `cond` (`any?`) — 条件表达式
- `max-results?` (`int?`, 可选, 默认无限制) — 最大结果数
**返回值**: `list?`（整数索引列表）
**说明**: 
- 特殊形式，cond 不预求值
- 返回 cond 求值为真的所有 INDEX 列表
- 结果排序并去重
- 可选的 max-results 参数限制返回数量
- 有快速路径：`(= (get "sig") val)` 形式使用索引扫描
- 遍历完成后恢复原始 INDEX
**示例**:
```wal
(find (= clk 1))
```

#### `find/g`

**签名**: `(find/g cond)`  
**参数**:
- `cond` (`any?`) — 条件表达式
**返回值**: `list?`
**说明**: 
- 特殊形式，cond 不预求值
- 全局查找：跨所有加载的追踪同步步进
- 每个满足条件的点上，返回所有追踪的当前 INDEX
- 如果只有一个追踪，返回单一整数；否则返回列表
- 遍历完成后恢复原始 INDEX
**示例**:
```wal
(find/g (= clk 1))
```

#### `count`

**签名**: `(count cond)`  
**参数**:
- `cond` (`any?`) — 条件表达式
**返回值**: `int?`
**说明**: 
- 特殊形式，cond 不预求值
- 返回 cond 求值为真的 INDEX 数量
- 有快速路径：`(= (get "sig") val)` 形式使用索引扫描
- 遍历完成后恢复原始 INDEX
**示例**:
```wal
(count (= clk 1))
```

#### `timeframe`

**签名**: `(timeframe body+ ...)`  
**参数**:
- `body+ ...` (`any?`) — 临时求值的表达式
**返回值**: `any?`（最后一个 body 的结果）
**说明**: 
- 特殊形式，body 不预求值
- 在求值 body 前保存每个加载追踪的当前 INDEX
- body 求值后恢复所有 INDEX
- 允许在 body 内进行步进等时间操作而不丢失位置
**示例**:
```wal
(timeframe
  (while (! ready) (step))
  (print INDEX))
```

### 2.4 信号存在检查

#### `signal?`

**签名**: `(signal? name)`  
**参数**:
- `name` (`symbol?`) — 信号名
**返回值**: `bool?`
**说明**: 检查 name 是否为已加载波形中存在的信号名。
**示例**:
```wal
(signal? clk)
```

### 2.5 信号位宽与采样

#### `signal-width`

**签名**: `(signal-width name)`  
**参数**:
- `name` (`symbol?`) — 信号名
**返回值**: `int?`
**说明**: 
- 返回信号 name 的位宽
- 先在第一个追踪中查找，然后在其他追踪中查找
- 未找到时默认返回 1
**示例**:
```wal
(signal-width data_bus)
```

#### `sample-at`

**签名**: `(sample-at name index)`  
**参数**:
- `name` (`symbol?`) — 信号名
- `index` (`int?`) — 要读取的时间索引
**返回值**: `int?` 或 `nil`
**说明**: 
- 读取指定信号在给定索引处的值
- golden 中此函数设置采样点，wal-rust 中直接读取指定索引的信号值（行为相反）
- 未找到信号时返回 `nil`
**示例**:
```wal
(sample-at clk 100)
```

#### `fold/signal`

**签名**: `(fold/signal f acc stop signal)`  
**参数**:
- `f` (`any?`) — 折叠函数，接受 `(acc signal_value)` 两个参数
- `acc` (`any?`) — 初始累加器
- `stop` (`any?`) — 停止条件表达式
- `signal` (`symbol?`) — 信号名
**返回值**: `any?`（最终累加器值）
**说明**: 
- 从当前 INDEX 开始遍历
- 每一步：检查 stop 条件，为真时停止；否则读取 signal 值并用 `(f acc val)` 更新累加器
- 遍历完成后恢复原始 INDEX
- 到达追踪末尾时自动停止
**示例**:
```wal
(fold/signal (fn (acc val) (+ acc val)) 0 (= INDEX 100) data)
```

### 2.6 裁剪

#### `trim-trace`

**签名**: `(trim-trace start end)`  
**参数**:
- `start` (`int?`) — 起始索引
- `end` (`int?`) — 结束索引
**返回值**: `nil`
**说明**: 
- wal-rust 中为 stub 实现：接收参数但不执行实际裁剪操作
- golden 中为 `(trim-trace tid new-max-index)` 签名
**示例**:
```wal
(trim-trace 0 1000)
```

### 2.7 虚拟信号

#### `defsig`

**签名**: `(defsig name expr)`  
**参数**:
- `name` (`symbol?`) — 虚拟信号名
- `expr` (`any?`) — 定义虚拟信号值的表达式
**返回值**: `nil`
**说明**: 
- 定义虚拟信号，其值由 expr 在每次求值时动态计算
- expr 被存储在环境中并注册为虚拟信号
- 通过 `VIRTUAL-SIGNALS` 可列出所有已定义的虚拟信号
**示例**:
```wal
(defsig my_sig (+ clk reset))
```

#### `new-trace`

**签名**: `(new-trace name)`  
**参数**:
- `name` (`symbol?`) — 追踪名
**返回值**: `nil`
**说明**: wal-rust 中为 stub 实现，接收参数但不执行实际创建操作。
**示例**:
```wal
(new-trace "virtual")
```

#### `dump-trace`

**签名**: `(dump-trace path)`  
**参数**:
- `path` (`string?` | `symbol?`) — 输出文件路径
**返回值**: `string?`
**说明**: 
- 将所有已定义的虚拟信号和波形信号导出为 VCD 格式文件
- 遍历所有时间点，记录每个虚拟信号的值变化
- 导出完成后恢复原始 INDEX
- 未定义虚拟信号时返回错误
**示例**:
```wal
(dump-trace "output.vcd")
```

---

## 3. 访问信号

### 3.1 直接信号访问

WAL 的核心思想：加载波形中的信号可以直接通过其名称读取。信号名即变量，其值取决于当前 INDEX。

信号名解析顺序：
1. 环境变量查找
2. 操作符名查找
3. 特殊变量（INDEX、MAX-INDEX、TS 等）
4. 信号名自动查找（精确匹配 → 添加作用域前缀 → 添加组前缀 → 模糊匹配）

##### 示例:

```wal
(load "trace.vcd")
clk
```

```
1
```

### 3.2 `(get signal)`

#### `get`

**签名**: `(get name)`  
**参数**:
- `name` (`symbol?` | `string?`) — 信号名
**返回值**: `int?` | `float?`
**说明**: 
- 返回指定信号在当前 INDEX 的值
- 信号名首先精确匹配，然后添加作用域前缀 (`CS + name`)，再添加组前缀 (`CG + name`)
- 模糊回退：后缀匹配 → 最后组件匹配 → 子串匹配
- 模糊匹配歧义时记录警告
- 完全未找到时返回错误及可用信号列表
**示例**:
```wal
(get clk)
```

### 3.3 `(slice signal upper lower)`

#### `slice`

**签名**: `(slice val idx)` 或 `(slice val upper lower)`  
**参数**:
- `val` (`int?` | `list?` | `string?`) — 要切片的值
- `idx` (`int?`) — 单元素索引
- `upper` (`int?`) — 上界（高位）
- `lower` (`int?`) — 下界（低位）
**返回值**: 取决于输入类型
**说明**: 
- **整数位提取（2 参数）**: `(slice n bit)` → 提取第 bit 位的值（0 或 1）
- **整数位提取（3 参数）**: `(slice n upper lower)` → 提取 `[upper:lower]` 位范围，bit 索引 0..63
- **列表切片（2 参数）**: `(slice lst idx)` → 取第 idx 个元素（越界返回 `nil`）
- **列表切片（3 参数）**: `(slice lst start end)` → 取 `lst[start..end]` 子列表
- **字符串切片（2 参数）**: `(slice str idx)` → 取第 idx 个字符
- **字符串切片（3 参数）**: `(slice str start end)` → 取 `str[start..end]` 子串
**示例**:
```wal
(slice 15 0)
```
```
1
```
```wal
(slice 15 2 0)
```
```
7
```

### 3.4 `(reval expr offset)`

#### `reval`

**签名**: `(reval expr offset)`  
**参数**:
- `expr` (`any?`) — 要求值的表达式
- `offset` (`int?`) — 相对于当前 INDEX 的偏移
**返回值**: `any?`（expr 求值结果）或 `#f`（越界）
**说明**: 
- 特殊形式，expr 不预求值
- 将所有追踪的 INDEX 临时偏移 offset，求值 expr，然后恢复 INDEX
- 任意追踪越界时返回 `#f`（不执行求值）
- 偏移可以是负数
**示例**:
```wal
(reval clk -1)
```

### 3.5 `@` 语法糖

**签名**: `expr@off`  
**说明**: `@` 宏转换为对 `reval` 的调用。wal-rust 中仅支持 `atom@atom` 形式。
**示例**:
```wal
INDEX@-1
```

### 3.6 特殊变量

| 变量 | 类型 | 说明 |
|------|------|------|
| `INDEX` | `int?` | 当前时间索引（0-based）|
| `MAX-INDEX` | `int?` | 最大有效 INDEX 值 |
| `TS` | `int?` | 当前仿真时间戳（同 INDEX）|
| `SIGNALS` | `list?` | 所有信号名称列表 |
| `SIGNALS-NO-ALIAS` | `list?` | 无别名的信号列表 |
| `CS` | `string?` | 当前作用域 (Current Scope) |
| `CG` | `string?` | 当前组 (Current Group) |
| `LOCAL-SIGNALS` | `list?` | 当前作用域的本地信号列表 |
| `LOCAL-SCOPES` | `list?` | 当前上下文的本地作用域列表 |
| `SCOPES` | `list?` | 所有可用作用域列表 |
| `VIRTUAL-SIGNALS` | `list?` | 所有虚拟信号列表 |
| `TRACE-FILE` | `string?` | 当前波形文件路径 |
| `TRACE-NAME` | `string?` | 当前波形名称 (ID) |

---

## 4. 组与作用域

### 4.1 组

#### `groups`

**签名**: `(groups posts* ...)`  
**参数**:
- `posts* ...` (`symbol?` | `string?`, 可变) — 后缀条件列表
**返回值**: `list?`（字符串列表）
**说明**: 
- 返回所有信号名前缀 `pre`，使得对每个 `post` 都有 `pre + post` 是有效的信号名
- 空参数列表返回空列表
- 用于按模式查找信号组
**示例**:
```wal
(groups valid ready)
```

#### `in-group`

**签名**: `(in-group group expr+ ...)`  
**参数**:
- `group` (`symbol?` | `string?`) — 组名
- `expr+ ...` (`any?`) — 在组上下文中求值的表达式
**返回值**: `any?`（最后一个表达式的结果）
**说明**: 
- 特殊形式，group 不预求值
- 设置当前组（CG）为指定组名
- 同步更新当前作用域（CS）为组名的前缀（去掉尾部点号）
- 在组上下文中对 `#name` 形式的信号引用，会自动添加组前缀
**示例**:
```wal
(in-group "top.in_" (print #ready))
```

#### `in-groups`

**签名**: `(in-groups groups body+ ...)`  
**参数**:
- `groups` (`list?`) — 组名列表
- `body+ ...` (`any?`) — 求值体
**返回值**: `any?`（最后一个组的最后一个 body 结果）
**说明**: 
- 特殊形式，groups 不预求值
- 遍历组名列表，在每个组中求值 body
- 返回最后一个组的最后一个 body 结果
**示例**:
```wal
(in-groups '("top.in_" "top.out_")
  (print CG ":")
  (whenever (&& clk #valid #ready) (print INDEX)))
```

#### `resolve-group`

**签名**: `(resolve-group name)`  
**参数**:
- `name` (`symbol?`) — 信号名
**返回值**: `int?`（信号值）
**说明**: 
- 在当前组（CG）上下文中解析信号名
- 信号全名 = CG + name
- 未找到信号时返回错误
- `#name` 是此函数的简写
**示例**:
```wal
(resolve-group ready)
```

#### `#name` — resolve-group 简写

**说明**: `#name` 是 `(resolve-group name)` 的语法糖，在当前组上下文中查找信号。

### 4.2 作用域

#### `in-scope`

**签名**: `(in-scope scope expr+ ...)`  
**参数**:
- `scope` (`symbol?` | `string?`) — 作用域名
- `expr+ ...` (`any?`) — 在作用域上下文中求值的表达式
**返回值**: `any?`（最后一个表达式的结果）
**说明**: 
- 特殊形式，scope 不预求值
- 设置当前作用域（CS）为指定作用域
- 信号名相对于该作用域解析（信号名前加 `scope.` 前缀）
**示例**:
```wal
(in-scope "top.sub" (print clk))
```

#### `scoped`

**签名**: `(scoped name expr)`  
**参数**:
- `name` (`symbol?` | `string?`) — 作用域名
- `expr` (`any?`) — 表达式
**返回值**: `any?`
**说明**: 
- wal-rust 中 `scoped` 是 `in-scope` 的别名
- 特殊形式，name 不预求值
- 在指定作用域中求值单个表达式
**示例**:
```wal
(scoped "top.sub" clk)
```

#### `resolve-scope`

**签名**: `(resolve-scope name)`  
**参数**:
- `name` (`symbol?` | `string?`) — 信号名
**返回值**: `int?`
**说明**: 
- 在当前作用域（CS）上下文中解析信号名
- 信号全名 = CS + name
- 未找到信号时返回错误
- `~name` 是此函数的简写
**示例**:
```wal
(resolve-scope clk)
```

#### `~name` — resolve-scope 简写

**说明**: `~name` 是 `(resolve-scope name)` 的语法糖，在当前作用域上下文中查找信号。

#### `set-scope`

**签名**: `(set-scope scope)`  
**参数**:
- `scope` (`symbol?` | `string?`) — 作用域名
**返回值**: `nil`
**说明**: 设置当前作用域（CS）为指定作用域。影响后续所有未限定信号名的解析。
**示例**:
```wal
(set-scope "top.sub")
```

#### `unset-scope`

**签名**: `(unset-scope)`  
**参数**: 无
**返回值**: `nil`
**说明**: 重置当前作用域为空字符串，后续信号名不添加作用域前缀。
**示例**:
```wal
(unset-scope)
```

#### `all-scopes`

**签名**: `(all-scopes expr)`  
**参数**:
- `expr` (`any?`) — 在每个作用域中求值的表达式
**返回值**: `list?`
**说明**: 
- 遍历所有加载追踪中的所有作用域
- 在每个作用域中设置 CS 并求值 expr
- 返回所有结果的列表
- 跳过重复作用域
**示例**:
```wal
(all-scopes clk)
```

#### `in-scopes`

**签名**: `(in-scopes scopes body+ ...)`  
**参数**:
- `scopes` (`list?`) — 作用域名列表
- `body+ ...` (`any?`) — 求值体
**返回值**: `any?`（最后一个作用域中最后一个 body 的结果）
**说明**: 
- 特殊形式，scopes 不预求值
- scopes 列表在子环境中求值
- 在每个作用域中求值 body
- 返回最后一个作用域中最后一个 body 的结果
**示例**:
```wal
(in-scopes (list "top.sub1" "top.sub2") (print clk))
```

### 4.3 特殊变量

| 变量 | 说明 |
|------|------|
| `CS` | 当前作用域 (Current Scope)，字符串 |
| `CG` | 当前组 (Current Group)，字符串 |
| `LOCAL-SIGNALS` | 当前作用域的本地信号列表 |
| `SCOPES` | 所有可用作用域列表 |

---

## 5. 列表

### 5.1 列表构建

#### `list`

**签名**: `(list expr* ...)`  
**参数**:
- `expr* ...` (`any?`, 可变) — 元素表达式
**返回值**: `list?`
**说明**: 返回包含所有参数求值结果的列表。
**示例**:
```wal
(list 1 2 3)
```
```
(1 2 3)
```

### 5.2 列表访问

#### `first`

**签名**: `(first xs)`  
**参数**:
- `xs` (`list?`) — 列表
**返回值**: `any?`
**说明**: 返回列表第一个元素。空列表时返回错误。
**示例**:
```wal
(first '(1 2 3))
```
```
1
```

#### `second`

**签名**: `(second xs)`  
**参数**:
- `xs` (`list?`) — 列表
**返回值**: `any?`
**说明**: 返回列表第二个元素。列表长度不足 2 时返回错误。
**示例**:
```wal
(second '(1 2 3))
```
```
2
```

#### `third`

**签名**: `(third xs)`  
**参数**:
- `xs` (`list?`) — 列表
**返回值**: `any?`
**说明**: 返回列表第三个元素。列表长度不足 3 时返回错误。wal-rust 扩展操作符，golden 无。
**示例**:
```wal
(third '(1 2 3))
```
```
3
```

#### `last`

**签名**: `(last xs)`  
**参数**:
- `xs` (`list?`) — 列表
**返回值**: `any?`
**说明**: 返回列表最后一个元素。空列表时返回错误。
**示例**:
```wal
(last '(1 2 3))
```
```
3
```

#### `rest`

**签名**: `(rest xs)`  
**参数**:
- `xs` (`list?`) — 列表
**返回值**: `list?`
**说明**: 返回除第一个元素外的所有元素。空列表返回空列表。
**示例**:
```wal
(rest '(1 2 3))
```
```
(2 3)
```

### 5.3 成员检查

#### `in`

**签名**: `(in check+ ... container)`  
**参数**:
- `check+ ...` (`any?`, 至少 1 个检查值) — 要检查的值
- `container` (`list?` | `string?`) — 容器
**返回值**: `bool?`
**说明**: 
- 检查所有 `check` 值是否都在 `container` 中
- 容器的最后一个参数是列表或字符串
- 字符串容器时，所有检查值必须为字符串
- 所有检查值均找到时返回 `#t`，否则返回 `#f`
**示例**:
```wal
(in 3 '(1 2 3))
```
```
#t
```
```wal
(in "lo" "hello")
```
```
#t
```

### 5.4 聚合

#### `min`

**签名**: `(min xs)`  
**参数**:
- `xs` (`list?`) — 数字列表
**返回值**: `int?` | `float?`
**说明**: 返回列表中的最小元素。空列表返回错误。混合 int/float 可比较。
**示例**:
```wal
(min '(3 1 2))
```
```
1
```

#### `max`

**签名**: `(max xs)`  
**参数**:
- `xs` (`list?`) — 数字列表
**返回值**: `int?` | `float?`
**说明**: 返回列表中的最大元素。空列表返回错误。混合 int/float 可比较。
**示例**:
```wal
(max '(3 1 2))
```
```
3
```

#### `average`

**签名**: `(average xs)`  
**参数**:
- `xs` (`list?`) — 数字列表
**返回值**: `float?`
**说明**: 返回列表元素的平均值。空列表返回错误。
**示例**:
```wal
(average '(1 2 3))
```
```
2.0
```

#### `length`

**签名**: `(length x)`  
**参数**:
- `x` (`list?` | `string?`) — 列表或字符串
**返回值**: `int?`
**说明**: 返回列表的长度或字符串的字符数。
**示例**:
```wal
(length '(1 2 3))
```
```
3
```

### 5.5 高阶函数

#### `map`

**签名**: `(map f xs)`  
**参数**:
- `f` (`closure?` | `symbol?`) — 函数（闭包或操作符符号）
- `xs` (`list?`) — 列表
**返回值**: `list?`
**说明**: 
- 对列表 xs 中的每个元素应用函数 f
- f 可以是闭包、操作符符号（如 `+`）或其他可调用值
- 对操作符符号的特殊处理：通过 eval 间接调用
- 对闭包的调用：被应用的元素用 `quote` 包装后传入
**示例**:
```wal
(map (fn (x) (* x 2)) '(1 2 3))
```
```
(2 4 6)
```

#### `fold`

**签名**: `(fold f acc xs)`  
**参数**:
- `f` (`closure?` | `symbol?`) — 折叠函数
- `acc` (`any?`) — 初始累加器
- `xs` (`list?`) — 列表
**返回值**: `any?`
**说明**: 
- 左折叠：从初始值 acc 开始，依次对每个元素应用 `(f acc x)`
- f 可以是闭包、操作符符号或其他可调用值
- acc 和 x 用 `quote` 包装后传入闭包
**示例**:
```wal
(fold + 0 '(1 2 3))
```
```
6
```

#### `zip`

**签名**: `(zip xs ys)`  
**参数**:
- `xs` (`list?`) — 第一个列表
- `ys` (`list?`) — 第二个列表
**返回值**: `list?`
**说明**: 将两个列表配对组合，结果长度等于较短的列表长度。
**示例**:
```wal
(zip '(1 2) '(a b))
```
```
((1 a) (2 b))
```

### 5.6 范围生成

#### `range`

**签名**: `(range end)` 或 `(range start end)` 或 `(range start end step)`  
**参数**:
- `end` (`int?`) — 结束值（不包含）
- `start` (`int?`) — 起始值（包含）
- `step` (`int?`, 可选, 默认 1) — 步长
**返回值**: `list?`
**说明**: 
- 1 参数：`(range 5)` → `(0 1 2 3 4)`
- 2 参数：`(range 1 5)` → `(1 2 3 4)`
- 3 参数：`(range 0 10 2)` → `(0 2 4 6 8)`
- 负步长：`(range 5 0 -1)` → `(5 4 3 2 1)`
**示例**:
```wal
(range 1 5)
```
```
(1 2 3 4)
```

### 5.7 切片

参见 [`slice`](#33-slice-signal-upper-lower)（列表切片功能复用同一函数）。

### 5.8 列表推导

#### `for/list`

**签名**: `(for/list (sym data) body+ ...)`  
**参数**:
- `(sym data)` (`list?`) — 绑定说明符，sym 为迭代变量，data 为数据列表表达式
- `body+ ...` (`any?`) — 循环体
**返回值**: `list?`
**说明**: 
- wal-rust 内置宏，展开为 `(map (fn (sym) (do body...)) data)`
- 对 data 中每个元素绑定到 sym，求值 body，返回结果列表
**示例**:
```wal
(for/list (x '(1 2 3)) (* x 2))
```
```
(2 4 6)
```

### 5.9 宏定义列表函数

以下函数通过标准库宏定义（在 `std/std.wal` 中）：

| 函数 | 签名 | 说明 |
|------|------|------|
| `car` | `(car xs)` | 等价于 `first` |
| `cdr` | `(cdr xs)` | 等价于 `rest` |
| `cadr` | `(cadr xs)` | 等价于 `(car (cdr xs))` |
| `append` | `(append xs x)` | 在列表末尾追加元素 x |
| `reverse` | `(reverse xs)` | 反转列表 |
| `filter` | `(filter p xs)` | 过滤列表，保留满足谓词 p 的元素 |
| `sort` | `(sort xs)` | 对数字列表排序 |
| `partition` | `(partition p xs)` | 将列表分为满足和不满足谓词 p 的两部分 |

---

## 6. 数组

在 WAL 中，数组是一种哈希表数据结构，底层实现为键值对扁平的列表 `(k1 v1 k2 v2 ...)`。

### 6.1 数组操作

#### `array`

**签名**: `(array pairs* ...)`  
**参数**:
- `pairs* ...` (`any?`, 可变) — 键值对，支持：
  - 扁平格式：`k1 v1 k2 v2`
  - 嵌套格式：`(k1 v1) (k2 v2)`
**返回值**: `list?`（键值对扁平列表）
**说明**: 
- 构造一个用键值对数据初始化的数组
- 键始终存储为字符串（显示时用字符串）
- 打印时用花括号 `{}` 显示
**示例**:
```wal
(array)
```
```
{}
```
```wal
(array 'x 10 'y 20)
```
```
{("x" 10) ("y" 20)}
```

#### `seta`

**签名**: `(seta array key value)`  
**参数**:
- `array` (`list?`) — 数组
- `key` (`any?`) — 键
- `value` (`any?`) — 值
**返回值**: `list?`（新数组）
**说明**: 
- 键转换为字符串后插入/更新
- 返回新数组（非破坏性）
- 键已存在时更新值；键不存在时追加
**示例**:
```wal
(seta (array) 'x 10)
```
```
{("x" 10)}
```

#### `geta`

**签名**: `(geta array key)`  
**参数**:
- `array` (`list?`) — 数组
- `key` (`any?`) — 键
**返回值**: `any?`
**说明**: 
- 键转换为字符串后查找
- 键不存在时返回错误
**示例**:
```wal
(geta (array 'x 10) 'x)
```
```
10
```

#### `geta/default`

**签名**: `(geta/default array default key)`  
**参数**:
- `array` (`list?`) — 数组
- `default` (`any?`) — 键不存在时的默认值
- `key` (`any?`) — 键
**返回值**: `any?`
**说明**: 
- 键转换为字符串后查找
- 键存在时返回对应值；不存在时返回 default
**示例**:
```wal
(geta/default (array 'x 10) 5 'y)
```
```
5
```

#### `dela`

**签名**: `(dela array key)`  
**参数**:
- `array` (`list?`) — 数组
- `key` (`any?`) — 键
**返回值**: `list?`（新数组）
**说明**: 
- 键转换为字符串后移除对应条目
- 返回新数组（非破坏性）
- 键不存在时返回原数组
**示例**:
```wal
(dela (array 'x 10 'y 20) 'x)
```
```
{("y" 20)}
```

#### `mapa`

**签名**: `(mapa array f)`  
**参数**:
- `array` (`list?`) — 数组
- `f` (`closure?`) — 函数，接受 `(key value)` 两个参数
**返回值**: `list?`（键值对扁平列表）
**说明**: 
- 对数组中每个 (键 值) 对应用函数 f
- f 返回的值替换原值（键保持不变）
- f 接受两个参数：key 和 value
**示例**:
```wal
(mapa (array 'x 10 'y 20) (fn (k v) (* v 2)))
```
```
{("x" 20) ("y" 40)}
```

---

## 7. 类型与转换

### 7.1 类型谓词

#### `atom?`

**签名**: `(atom? expr+ ...)`  
**参数**:
- `expr+ ...` (`any?`, 至少 1 个) — 要检查的值
**返回值**: `bool?`
**说明**: 
- 检查所有参数是否均为原子
- 原子定义：`nil`、`bool`、`int`、`float`、`string`、`symbol`、`closure`、`macro`、空列表、`unquote`、`unquote-splice`
- 多参数时所有参数必须均为原子才返回 `#t`
**示例**:
```wal
(atom? 42)
```
```
#t
```

#### `symbol?`

**签名**: `(symbol? expr+ ...)`  
**参数**:
- `expr+ ...` (`any?`, 至少 1 个) — 要检查的值
**返回值**: `bool?`
**说明**: 检查所有参数是否均为符号。多参数时全部为符号才返回 `#t`。
**示例**:
```wal
(symbol? 'x)
```
```
#t
```

#### `string?`

**签名**: `(string? expr+ ...)`  
**参数**:
- `expr+ ...` (`any?`, 至少 1 个) — 要检查的值
**返回值**: `bool?`
**说明**: 检查所有参数是否均为字符串。多参数时全部为字符串才返回 `#t`。
**示例**:
```wal
(string? "hello")
```
```
#t
```

#### `int?`

**签名**: `(int? expr+ ...)`  
**参数**:
- `expr+ ...` (`any?`, 至少 1 个) — 要检查的值
**返回值**: `bool?`
**说明**: 检查所有参数是否均为整数。多参数时全部为整数才返回 `#t`。
**示例**:
```wal
(int? 42)
```
```
#t
```

#### `list?`

**签名**: `(list? expr+ ...)`  
**参数**:
- `expr+ ...` (`any?`, 至少 1 个) — 要检查的值
**返回值**: `bool?`
**说明**: 检查所有参数是否均为列表。多参数时全部为列表才返回 `#t`。
**示例**:
```wal
(list? '(1 2))
```
```
#t
```

#### `null?`

**签名**: `(null? x)`  
**参数**:
- `x` (`any?`) — 要检查的值
**返回值**: `bool?`
**说明**: 
- 检查值是否为 `nil` 或空列表
- wal-rust 扩展操作符
**示例**:
```wal
(null? '())
```
```
#t
```
```wal
(null? nil)
```
```
#t
```

#### `defined?`

**签名**: `(defined? name)`  
**参数**:
- `name` (`symbol?`) — 变量名
**返回值**: `bool?`
**说明**: 检查变量是否在当前环境中定义。
**示例**:
```wal
(define x 10)
(defined? 'x)
```
```
#t
```

#### `type`

**签名**: `(type x)`  
**参数**:
- `x` (`any?`) — 任意值
**返回值**: `string?`
**说明**: 返回类型的字符串表示（如 `"<class 'int'>"`）。
**示例**:
```wal
(type 42)
```
```
<class 'int'>
```

### 7.2 类型转换

#### `convert/bin`

**签名**: `(convert/bin num width?)`  
**参数**:
- `num` (`int?` | `string?`) — 要转换的数字（或可解析为数字的字符串）
- `width?` (`int?`, 可选) — 输出宽度，不足时补零
**返回值**: `string?`
**说明**: 
- 将整数转换为二进制字符串表示
- width 指定最小宽度，不足时左补零
- 输入为字符串时，先解析为整数再转换
**示例**:
```wal
(convert/bin 5 8)
```
```
00000101
```

#### `string->int`

**签名**: `(string->int str)`  
**参数**:
- `str` (`string?`) — 数字字符串
**返回值**: `int?`
**说明**: 将字符串解析为十进制整数。输入为整数时直接返回。
**示例**:
```wal
(string->int "42")
```
```
42
```

#### `bits->sint`

**签名**: `(bits->sint bits)`  
**参数**:
- `bits` (`string?`) — 二进制字符串（仅含 0 和 1）
**返回值**: `int?`
**说明**: 
- 将二进制字符串转换为有符号整数（补码表示）
- 最高位为 1 时取反加 1 得负数
- 空字符串返回错误，含非 0/1 字符返回错误
**示例**:
```wal
(bits->sint "101")
```
```
-3
```

#### `symbol->string`

**签名**: `(symbol->string sym)`  
**参数**:
- `sym` (`symbol?` | `string?`) — 符号（或字符串）
**返回值**: `string?`
**说明**: 将符号名称转换为字符串。输入为字符串时直接返回。
**示例**:
```wal
(symbol->string 'hello)
```
```
hello
```

#### `string->symbol`

**签名**: `(string->symbol str)`  
**参数**:
- `str` (`string?` | `symbol?`) — 字符串
**返回值**: `symbol?`
**说明**: 将字符串转换为符号。输入为符号时直接返回。
**示例**:
```wal
(string->symbol "hello")
```

#### `int->string`

**签名**: `(int->string n)`  
**参数**:
- `n` (`int?` | `float?`) — 数值
**返回值**: `string?`
**说明**: 将整数或浮点数转换为字符串表示。
**示例**:
```wal
(int->string 42)
```
```
42
```

#### `string-append`

**签名**: `(string-append str+ ...)`  
**参数**:
- `str+ ...` (`any?`, 至少 2 个) — 要拼接的值
**返回值**: `string?`
**说明**: 
- 将所有参数强制转换为字符串后拼接
- wal-rust 扩展操作符，golden 无
- 非字符串参数自动格式化
**示例**:
```wal
(string-append "hello" " " "world")
```
```
hello world
```

#### `convert`

**签名**: `(convert input output compression?)`  
**参数**:
- `input` (`string?`) — 输入 VCD 文件路径
- `output` (`string?`) — 输出 FST 文件路径
- `compression?` (`string?`, 可选, 默认 `"lz4"`) — 压缩方式（`"lz4"` 或 `"zlib"`）
**返回值**: `string?`
**说明**: 
- 将 VCD 波形文件转换为 FST 格式
- wal-rust 独占功能
**示例**:
```wal
(convert "trace.vcd" "trace.fst" "zlib")
```

---

## 8. wal-rust 实现差异

wal-rust 是 WAL 语言的高性能 Rust 实现。以下是与 golden (Python) 参考实现的主要差异。

### 8.1 扩展操作符（golden 无）

wal-rust 新增了以下操作符（golden 中无对应实现）：

| 操作符 | 签名 | 说明 |
|--------|------|------|
| `abs` | `(abs n)` | 绝对值，参数为 `int?` 或 `float?` |
| `string-append` | `(string-append str+ ...)` | 字符串拼接，至少 2 参数 |
| `third` | `(third xs)` | 返回列表第三个元素，不足 3 个元素时报错 |
| `null?` | `(null? x)` | 检查是否为 `nil` 或空列表 |
| `empty?` | `(empty? x)` | `null?` 的别名 |
| `convert` | `(convert input output compression?)` | VCD 转 FST 格式 |

### 8.2 已移除的非核心操作符

以下操作符曾存在于 wal-rust 早期版本，但因属于协议/工具特定功能，已从语言核心中移除：

| 操作符 | 说明 | 替代方案 |
|--------|------|---------|
| `tl-handshakes` | TileLink 握手计数 | `(count (&& (rising (get req)) (= (get grant) 0)))` |
| `tl-latency` | TileLink 平均延迟 | 可通过 `whenever` + 状态变量实现 |
| `tl-bandwidth` | TileLink 带宽计算 | `(* (count (= (get valid) 1)) (/ data-width 8))` |
| `convert` | VCD→FST 格式转换 | 外部工具（如 gtkwave 的 vcd2fst）|

#### `tl-latency`

**签名**: `(tl-latency req grant data?)`  
**参数**:
- `req` (`symbol?`) — 请求信号名
- `grant` (`symbol?`) — 授权信号名
- `data?` (`symbol?` | `string?`, 可选) — 数据信号名
**返回值**: `float?`
**说明**: 
- 计算请求到授权的平均延迟（周期数）
- 检测 `req` 上升沿到 `grant` 上升沿的周期数
- 无完成握手时返回 NaN
**示例**:
```wal
(tl-latency tl.req tl.grant)
```

#### `tl-bandwidth`

**签名**: `(tl-bandwidth channel valid width)`  
**参数**:
- `channel` (`symbol?`) — 通道信号名
- `valid` (`symbol?`) — valid 信号名
- `width` (`int?`) — 数据位宽
**返回值**: `int?`（字节数）
**说明**: 计算 TileLink 总线总带宽：`transfer_count * data_width / 8`（字节）。
**示例**:
```wal
(tl-bandwidth tl.channel tl.valid 32)
```

### 8.3 信号变量作为函数

wal-rust 中以下特殊变量可作为**无参数函数**调用：

```wal
(signals)       ↦ 所有信号列表
(index)         ↦ 当前时间索引
(max-index)     ↦ 最大索引
(ts)            ↦ 当前时间戳
(trace-name)    ↦ 当前波形名称
(trace-file)    ↦ 当前波形路径
```

（在 golden 中这些是大写变量名 `INDEX`、`SIGNALS` 等，不通过函数调用）

### 8.4 内置特殊形式（golden 中为 stdlib 宏）

wal-rust 将以下 golden 标准库宏实现为内置操作符：

- `(when cond body+)` — 条件执行
- `(unless cond body+)` — 条件不执行时执行
- `(cond (guard expr+)+)` — 多分支条件
- `(count cond)` — 计数
- `(timeframe body+)` — 时间范围
- `(sum xs)` — 列表求和
- `(set! key value)` — 展开为 `(set key value)`
- `(defun name args body+)` — 展开为 `(define name (fn args body...))`
- `(for/list (sym data) body+)` — 展开为 `map`
- `(defunm name args body)` — 展开为 `(defmacro name args body)`

### 8.5 行为差异

| 操作 | golden | wal-rust |
|------|--------|----------|
| `(*)` | 报错（需 ≥2 参数） | 返回 1 |
| `(&& expr ...)` / `(|| expr ...)` 求值 | 短路求值 | 参数预求值后再逻辑判断（**非**短路，有副作用的参数总会执行） |
| `(! expr)` | 支持 ≥1 参数，仅接受 int | 固定 ≥1 参数，任意类型 |
| `(import module)` | 加载 Python 模块 | 加载 WAL 源文件 |
| `(require module)` | 模块绑定导入（支持 rename-in/only-in/prefix-in） | 读文件系统 `.wal` 文件 |
| `(type x)` | Python type 对象（`<class 'int'>`） | 字符串（`"<class 'int'>"`） |
| `(sample-at ...)` | 设置采样点 | 读取指定索引的信号值（相反操作） |
| `(trim-trace ...)` | 实际裁剪 | stub 实现，无效果 |
| `(expr)@offset` | 支持任意 sexpr | 仅支持 `atom@atom` |
| `(call ...)` | 调用 Python 模块函数 | 调用 WAL 函数/宏 |
| `(scoped ...)` | `(scoped scope expr)` 固定 2 参数 | `(scoped scope body+)` 可变 body |
| `(step)` | 无参数 | 支持 `(step)`、`(step amount)`、`(step id amount)` 三种形式 |
| `(define ...)` | 仅符号绑定 | 支持 `(define x val)` 和 `(define (fn args) body)` 两种形式 |
| `(let ...)` | 仅支持 `(let ((x 1)) body)` 形式 | 支持 `(let (x 1) body)` 和 `(let ((x 1)) body)` 两种形式 |

### 8.6 特殊变量

所有 golden 的特殊变量在 wal-rust 中均已实现：

| 变量 | 说明 |
|------|------|
| `INDEX` | 当前时间索引（0-based），int |
| `MAX-INDEX` | 最大有效 INDEX 值，int |
| `TS` | 当前仿真时间戳，int |
| `SIGNALS` | 所有信号名称列表，list |
| `SIGNALS-NO-ALIAS` | 无别名的信号列表，list |
| `CS` | 当前作用域 (Current Scope)，string |
| `CG` | 当前组 (Current Group)，string |
| `LOCAL-SIGNALS` | 当前作用域的本地信号列表，list |
| `LOCAL-SCOPES` | 当前上下文的本地作用域列表，list |
| `SCOPES` | 所有可用作用域列表，list |
| `VIRTUAL-SIGNALS` | 所有虚拟信号列表，list |
| `TRACE-FILE` | 当前波形文件路径，string |
| `TRACE-NAME` | 当前波形名称 (ID)，string |
