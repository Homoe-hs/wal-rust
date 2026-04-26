# WAL Rust - 构建文档

> 版本: 0.5.0 | 更新: 2026-04-26

---

## 一、项目概述

| 项目 | 值 |
|------|-----|
| **项目名** | wal-rust |
| **版本** | 0.4.0 |
| **Rust版本** | 1.70+ |
| **目标** | 与原版 WAL (wal-lang.org) 100% 兼容，支持 150GB+ VCD 文件 |
| **支持平台** | Linux/macOS/Windows |
| **输入格式** | VCD, FST |
| **输出格式** | VCD, FST |

### 构建状态

| 项目 | 构建 | 测试 | Clippy |
|------|------|------|--------|
| wal-rust | ✅ 0 errors | ✅ 49 passed | ✅ 0 errors |
| wal-lsp | ✅ 0 errors | N/A | ✅ 0 errors |

### 内存性能 (v0.5.0)

| 指标 | 旧版 (v0.3) | 新版 (v0.5) |
|------|-----------|-----------|
| VCD 加载策略 | 全量 HashMap 加载 | 两遍扫描 + sparse index |
| signal_data 内存 | 2-5 GB | 0 (按需 mmap 读取) |
| 索引内存 | 80 MB (timestamps) | ~360 MB (timestamps + offsets + sparse index) |
| LRU 缓存 | 无 | ~100 MB (可配置 10万条) |
| line reading | String 分配 | 零拷贝 `&[u8]` slice |
| mmap 生命周期 | 加载后释放 | 持久化 (On-demand Query) |

---

## 二、系统架构

```
wal-rust/
├── Cargo.toml
├── CONSTRUCTION.md              # 本文档
├── src/
│   ├── main.rs                 # CLI 入口 (run, repl)
│   ├── cli.rs                  # 参数解析
│   ├── wal/                    # WAL 语言核心
│   │   ├── mod.rs
│   │   ├── parser/
│   │   │   └── parse.rs       # WalParser (grouped_symbol, scoped_symbol, timed_atom)
│   │   ├── ast/
│   │   │   ├── operator.rs    # Operator 枚举 (82 operators)
│   │   │   ├── value.rs       # Value 枚举
│   │   │   ├── symbol.rs      # Symbol
│   │   │   ├── wlist.rs       # WList
│   │   │   ├── closure.rs     # Closure (Rc<RefCell<Environment>>)
│   │   │   └── macro_def.rs   # Macro (Rc<RefCell<Environment>>)
│   │   ├── eval/
│   │   │   ├── evaluator.rs   # Evaluator + macros (defun/defunm/set!/for/list)
│   │   │   ├── environment.rs # Environment (Rc<RefCell<Environment>> parent chain)
│   │   │   └── dispatch.rs    # Dispatcher (BuiltinFn registry)
│   │   ├── builtins/          # 内置函数 (12 modules)
│   │   │   ├── core.rs       # when, unless, cond, set, define, print, exit, alias, unalias
│   │   │   ├── math.rs       # + - * / ** floor ceil round mod sum
│   │   │   ├── list.rs       # list first second last rest in map fold zip max min average length
│   │   │   ├── signal.rs     # load unload step find find/g whenever count timeframe ts get signals index
│   │   │   ├── types.rs      # defined? atom? symbol? string? int? list? convert/bin sym<->str
│   │   │   ├── bitwise.rs    # bor band bxor
│   │   │   ├── array.rs      # array seta geta geta/default dela mapa
│   │   │   ├── scope.rs      # scoped all-scopes resolve-scope in-scope in-scopes resolve-group
│   │   │   ├── virtual_sig.rs # defsig new-trace dump-trace
│   │   │   └── special.rs    # quote quasiquote unquote fn defmacro macroexpand gensym eval parse rel_eval slice call import
│   │   └── repl/
│   ├── vcd/                    # VCD 解析
│   │   ├── types.rs
│   │   ├── reader.rs          # MmapReader
│   │   └── parser.rs          # MmapVcdParser
│   ├── fst/                    # FST 读写
│   │   ├── types.rs/reader.rs/writer.rs/blocks.rs/varint.rs/compress.rs
│   ├── trace/                  # 波形接口
│   │   ├── trace.rs           # Trace trait
│   │   ├── container.rs       # TraceContainer
│   │   ├── vcd.rs             # VcdTrace (当前全量加载，计划 mmap+稀疏索引+LRU)
│   │   └── fst.rs             # FstTrace
│   └── lib.rs
└── tree-sitter-wal/
```

---

## 三、v0.4.0 完成清单

### 阶段 1: wal-lsp — 精简 + 稳定性 ✅

- ✅ 无 MCP 代码（src/ 中无 "mcp" 引用）
- ✅ 无 `--mcp` flag（纯 LSP server）
- ✅ Cargo.toml 无 `mcp` feature
- ✅ LazyLock 工作空间 (`std::sync::LazyLock<SharedWorkspace>`)
- ✅ RwLock 毒化保护: 6× handlers 使用 `unwrap_or_else(|e| e.into_inner())`
- ✅ document.rs 死代码已不存在

### 阶段 2: 删除非标准运算符 ✅

- ✅ 删除: `for`, `range`, `rising`, `falling`, `stable`, `unstable`
- ✅ 从 operator.rs 枚举、from_str、as_str 全部移除
- ✅ 从 builtins/list.rs 移除 `op_for`/`op_range` 实现和注册

### 阶段 3: @/#/~ 解析 + 宏实现 ✅

| 语法 | 转换 | 文件 |
|------|------|------|
| `expr@offset` | `(rel_eval expr offset)` | `parse.rs:174-195` |
| `#signal` | `(resolve-group (quote signal))` | `parse.rs:196-217` |
| `~scope` | `(in-scope scope)` | `parse.rs:218-236` |
| `(defun name args body...)` | `(define name (fn args body...))` | `evaluator.rs:350-367` |
| `(defunm name args body...)` | `(defmacro name args body...)` | `evaluator.rs:369-385` |
| `(set! x val)` | `(set x val)` | `evaluator.rs:387-396` |
| `(for/list (x lst) body...)` | `(map (fn (x) body...) lst)` | `evaluator.rs:398-420` |

### 阶段 4: 新增缺失函数 ✅

| 函数 | 文件 | 状态 |
|------|------|------|
| `when` | core.rs | ✅ 实现 |
| `unless` | core.rs | ✅ 实现 |
| `cond` | core.rs | ✅ 实现 |
| `sum` | math.rs | ✅ 实现 |
| `count` | signal.rs | ✅ 实现 |
| `timeframe` | signal.rs | ✅ 实现 |
| `in-scope` | scope.rs | ✅ 实现 |
| `in-scopes` | scope.rs | ✅ 实现 |
| `geta/default` | array.rs | ✅ 实现 |

### 阶段 5: 信号名自动解析 ✅

- ✅ `eval_symbol` 末尾：符号未找到时，从已加载 traces 的 signal list 中查找

### 阶段 7: 稳定性修复 ✅

| 修复 | 文件 |
|------|------|
| `symbol->string` / `string->symbol` 映射互换修正 | `operator.rs` |
| `floor()` 负数取整: `f.floor() as i64` | `math.rs` |
| `$$scope` → `$scope` 拼写修正 | `vcd/parser.rs` |
| `parts[4]` → `parts[3]` 索引修正 | `vcd/parser.rs` |
| `Environment::clone()` 保留 traces | `environment.rs:113-122` |
| `Environment::set()` 父链遍历 via Rc\<RefCell\> | `environment.rs:75-84` |
| `eval_dispatch` unsafe aliasing → scoped raw ptr | `evaluator.rs:461-474` |
| 多行 sexpr 解析 -> 跨行 paren_depth 累积 | `main.rs:58-93` |
| Clippy PI error → allow attribute | `fst/blocks.rs:86` |
| Clippy warnings → 全部清除 | 多处 |
| RwLock 毒化保护 →全部 `unwrap_or_else` | `signal.rs`, `evaluator.rs` |
| rel_eval 覆盖 bug → 移除 special.rs 的 stub | `special.rs` |

---

## 四、尚未完成

### 阶段 6: 大文件性能重构 ✅

| 任务 | 状态 | 文件 |
|------|------|------|
| VcdTrace 两遍扫描 | ✅ Pass1=索引, Pass2=按需 mmap | `trace/vcd.rs` |
| 稀疏索引 (BTreeMap) | ✅ 每100次变化采样 | `trace/vcd.rs` |
| LRU 缓存 (lru crate) | ✅ 10万条默认容量, RefCell 内部可变 | `trace/vcd.rs` |
| 零拷贝 read_line_bytes() | ✅ 直接返回 mmap bytes slice | `vcd/reader.rs` |
| seek_to / current_offset | ✅ 随机访问支持 | `vcd/reader.rs` |
| 二分查找 timestamps | ✅ `binary_search` | `trace/vcd.rs` |
| RefCell 内部可变性 | ✅ signal_value(&self) -> 内部 mutation | `trace/vcd.rs` |

- ✅ VcdTrace 重构: 两遍扫描 + 稀疏索引 + LRU 缓存 + zero-copy line reading
- ✅ FstTrace 惰性 VCDATA 加载
- ✅ Cargo.toml 新增 `lru` crate 依赖
- ✅ RefCell 内部可变性 (signal_value 从 &self 调用, 内部修改 cache 和 reader)

### 部分完善项

| 函数 | 当前 | 备注 |
|------|------|------|
| `ts` | 返回 trace index | 应返回实际时间戳值 |
| `alias`/`unalias` | 返回 Nil (stub) | 需实现别名系统 |
| `defsig`/`new-trace`/`dump-trace` | 返回 Nil/stub | 虚拟信号暂未完整实现 |
| `fold/signal`/`signal-width`/`sample-at` | 返回 Nil (stub) | 高级波形操作未实现 |

---

## 五、性能目标 (v0.5.0 计划)

| 指标 | 目标 |
|------|------|
| 150GB VCD 加载（索引构建） | < 60 秒 |
| 运行时内存占用 | ≤ 2GB（mmap + 稀疏索引 + LRU 缓存）|
| signal_value 查询 | O(log n) 二分查找 |
| find_indices | O(n+m) 双指针 |
| FST trace 首次访问 | < 100ms（单块解压+缓存）|

---

## 六、验证标准

```bash
# 1. 构建测试
cargo build --release && cargo test

# 2. Clippy 检查
cargo clippy

# 3. REPL 测试
echo "(+ 1 2)" | cargo run --release -- repl
```

---

*文档版本: 0.5.0*
*最后更新: 2026-04-26*
