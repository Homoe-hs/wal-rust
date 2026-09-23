# AGENTS.md — wal-rust

## Quick commands

```bash
cargo test                          # all Rust tests (~290)
cargo test --test wal_integration_test  # integration tests only
cargo test test_vcd_pyvcd_verify_strobe -- --nocapture  # single test + stdout
cargo build --release               # release build
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17  # glibc 2.17+ compatible
target/release/wal-rust '(expr)' -l trace.vcd   # eval expression
target/release/wal-rust run script.wal -l file  # run script
target/release/wal-rust repl        # interactive REPL
make ci                             # 本地 CI(与 GitHub Actions 同一份 scripts/ci.sh)
WAVE=x.vcd test_samples/run_tests.sh  # WAL 脚本层冒烟(自包含脚本无需波形)
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
Flags: `-l <waveform>`(可重复), `--halt-on-error`(遇错即停)。
`-c/--code <表达式>` 是 **`run` 子命令**的选项(`wal-rust run -c '(+ 1 2)'`, 可省略 FILE);
顶层 `wal-rust '(...)'` 的表达式靠"以 `(` 开头"自动识别, 没有全局 `-c`。

## Source layout

| Dir | Content | ~Lines |
|-----|---------|--------|
| `src/wal/` | AST, tree-sitter parser, Evaluator, builtins (11 modules) | 10,300 |
| `src/vcd/` | VCD parser (mmap + memchr + two-pass) | 2,200 |
| `src/fst/` | FST writer (wellen reads via `src/trace/fst.rs`; legacy reader retired) | 2,900 |
| `src/trace/` | `Trace` trait, `VcdTrace`, `FstTrace`, `FsdbTrace`, `TraceContainer` | 6,800 |
| `tests/` | Rust integration & correctness tests(11 个文件) | 4,200 |
| `src/tests_fixtures.rs` | **测试夹具的来源**: 内嵌合成波形, 测试不依赖 `test_data/`(该目录不存在, 也不该存在) | — |
| `tree-sitter-wal/` | WAL 语法(`grammar.js`), 构建时生成解析器 C 代码并编译 | — |
| `docs/` | 文档(10+ 篇): **入口是 [`docs/README.md`](docs/README.md)** —— 有索引、状态与权威性标注, 不要在这里枚举 | — |

## Architecture notes

- **tree-sitter parser**: `tree-sitter-wal/build.rs` 用 `cc` 编译 `tree-sitter-wal/src/parser.c`;
  该文件**入库**(与 `grammar.js` 一起提交, 否则干净 clone/build.rs 直接失败)。
  **改语法必须重新生成**: 见 `tree-sitter-wal/README.md`(`tree-sitter generate` + CLI 版本固定 + 提交生成物)。
  ⚠️ `token(...)` 里不能引用规则(`$.base_symbol`), 只能内联正则 —— 所以字符类提取成 `BASE_SYMBOL_RE` 共用。
- **FST read backend**: wellen (`wellen::simple::read` in `src/trace/fst.rs`); legacy hand-rolled reader retired (writer stays: `src/fst/writer.rs`).
- **FSDB read backend**: `src/trace/fsdb.rs` —— 运行期 `dlopen` Synopsys NPI(`libNPI.so`)+
  Itanium mangled 符号,纯 Rust FFI,无 C++ 垫片。只用 `npiFsdbTimeBasedVcIter`(用过后
  `npi_fsdb_create_vct` 会失效);`npiFsdbValue.format` 是**入参**;库路径来自 `$VERDI_HOME`/
  `$WAL_NPI_LIB`。细节见 `docs/fsdb-npi.md`。差分门 `tests/fsdb_diff.rs` 由
  `WAL_FSDB_TEST_FILE` + `WAL_FSDB_TEST_VCD` 开启(没 Verdi 自动跳过)。
- **多文件语义(0.14.8 起)**: ①`TraceContainer` 按**加载顺序**迭代(`order: Vec<TraceId>`)
  —— 两条波形都有同名信号时**先 `-l` 的先算**, 不许退化成 HashMap 迭代顺序(跨进程随机,
  实测同一条命令 6 次里 5 次答 30000、1 次答 40); ②查询的**索引空间 = 实际会读值的波形**
  (first-match: 第一条 `-l` 里能解析出该名字的那条)的 `max_index`,
  不引用信号的查询(常量条件/INDEX-only)取**主波形**(第一条 `-l`)。未参与查询的波形绝不进
  索引空间 —— 否则 `-l vcd -l fsdb` 查一个两条都有的信号会先物化 FSDB 全局时间线
  (内网 >900s, 单加载秒级)。③**禁止跨波形合并索引集合**: 索引是各自的变更排名,
  合并出来的既不是这条也不是那条(实测 `-l s1 -l s2` 同名信号答出 s2 的数);
  所有取值/取索引路径统一 first-match。逐拍(`step_scan`)与统一引擎必须用同一规则;
  `INDEX`/`TS` 读"正在被推进的那条 trace"(`Evaluator::scan_trace`)。
- **CLI 多顶层形式 = `(list ...)`**: `parse_expr` 把"一个参数里写多个顶层形式"包成 `(list form1 form2 ...)`
  (求值顺序 = 书写顺序, 结果回显各形式的值)。**绝不能**让它以裸列表进 `eval_list` —— 那里的 IIFE 分支
  会在首元素求值成 Closure/Macro 时把其余顶层形式当成它的实参, 于是
  `(define add5 ((fn (n) (fn (x) (+ x n))) 5)) (add5 3)` 静默答 13、`defun` 返回闭包报 Arity error、
  `(twice (print …))` 打印 4 次。回归: `matrix_cli_multiform_program_forms`。
- **词法边界不能靠"最长前缀"**: `#t`/`#f` 是关键字, 但 `#name` 是合法的分组符号 ——
  若 `grouped_symbol` 写成 `seq("#", $.base_symbol)`(两个 token), 词法器先匹配到 `#` 就输给 `#t`,
  于是 `#timeout` 被拆成 `#t` + `imeout`(报错指到 `imeout`)。现在 `grouped_symbol` 是**单 token**,
  按最长匹配赢过 `#t`;`#t` 单独出现时仍是布尔。回归: `matrix_lexer_scientific_and_sharp_symbols`。
- **裸信号符号必须算作信号引用**: `(= clk 1)` / `(! rst)` 里的裸符号在 WAL 里是"当前 INDEX 的值"。
  判定"是否引用信号"的 `collect_cond_signals` 只认 `(get s)`/`(rising s)` 这类显式形式 ——
  裸符号漏判会被**常量折叠**吃掉, `count` 只在索引 0 求值一次就套用到全部索引(实测
  `(count (= clk 1))` 给 20/0, 真值 10)。现在用 `collect_cond_signals_with_bare`: 符号既不是
  变量、又能被某条波形解析 → 算信号引用(变量优先, 不影响常量折叠)。回归: `matrix_bare_signal_symbols_count_correctly`。
- **求值热路径禁止 `signals()`**: `Trace::signals()` 返回 `Vec<String>`(FSDB 是
  `sig_names.clone()`, 188 万信号 = 每次克隆一整张表)。裸符号自动解析曾经这么干, 而统一引擎
  **在每个边界都重新求值条件** → O(N)×边界数, 实测 200k 信号 VCD 的逐拍路径 117s → 修复后 8.2s
  (引擎路径 0.27s → 0.06s; 188 万信号 FSDB 上就是"3 分钟不出结果")。
  规则: 热路径一律用 `resolve_name`/`contains`(后端索引式);需要整表的地方只有
  `(SIGNALS)`/`sigs`/`find-sig` 这类**显式**请求。
  FSDB 短名解析另有懒建的叶子名排序索引(`leaf_order`, 只存 u32, O(log N)), 不再线性扫全表。
  基准: `./scripts/bench_name_resolution.sh [信号数] [时间戳数]`。
- **FSDB 变更列必须走旁挂缓存(`.fcol`)**: FSDB 取"某信号的变更列"只能重走一遍 NPI 变更流
  (`npiFsdbTimeBasedVcIter`), 没有 VCD 那种内存映射索引 —— 于是**每个新进程**都要为该查询
  用到的信号重扫一遍, 与查询复杂度无关(现场 265MB / 3700 万时间戳上表现为"每次查询都慢")。
  现在 `column_time`/`prepare` 先吃 `<cache>/<file_identity>-v1.fcol/<fnv(全名)>.col`
  (列 + **t0 初值放在文件头部**: `initial_of()` 只读前 64KB, 不必解整列), 冷扫描后落盘;
  指纹(`wave_fingerprint`)+位宽 + 文件身份 key 三重校验, 不匹配一律视为未命中。
  实测: 暖查询 8.6s → 4.1s(电平)/8.0s → 3.6s(沿), 冷查询 48.5s → 36.6s(200 万时间戳夹具)。
  基准: `make bench-fsdb FSDB=x.fsdb [SIG=tb.clk]`(或 `WAL_FSDB_BENCH=x.fsdb ./scripts/ci.sh --only perf`)。
- **时间线冷建别逐块拷主表**: `FsdbTrace::scan` 每 4096 信号一块, 曾把已累积的时间线
  每块整份拷贝一次 → O(块数 × 主表长)(1.88M 信号 = 459 块 × 上千万时间点 = 几十 GB memcpy)。
  现在分片收齐后一次归并(收完再平衡归并)。
- **帮助文案里的数字要有测试守门**: `--help` 曾写 "125 named operators" 而实际 146
  (`scripts/check_docs.py` 不扫 help 文本)。现在 `src/cli.rs` 有单元测试比对
  `builtins::registered_operator_count()`, 改注册表不同步改文案就会红。
- **索引参数不许静默截断**: `(sample-at s 4.5)` 曾经取索引 4 的值(静默错值), 现在明确报"必须是整数"。
- **缓存 key 必须含 ctime+inode**: `trace::vcd::file_identity`(basename+len+mtime+ctime+inode)
  —— 只按 size+mtime 会漏掉"同一秒内等长改写"(脚本反复生成同名波形是常态), 而首尾 64KB
  指纹拦不住中段改动; ctime 用户改不回去。`.wcol`/`.cols`/`.fnames`/`.ftl` 统一用它。
  缓存写失败只提示一次、绝不影响结果与退出码; `main` 启动就忽略 **SIGXFSZ**(否则
  `ulimit -f`/磁盘满 → rc=153 + core)。
- **索引空间是"要不要全文件扫描"的分水岭**: `change_points`/`find_indices` 走索引空间,
  对时间优先后端(FSDB)意味着物化全局时间线 = 全文件扫描; `Trace::change_points_time()`
  与 `Trace::count_matches()` 让 `getwave`/`at`/边沿计数跳过它。新增/修改这类查询时:
  ① `count_matches` 必须与 `find_indices(..).len()` **逐条一致**(含 `Changed` 在索引 0 的
  特例); ② 后端的 `set_index`/`max_index` 不要变成隐藏的全扫(引擎每次查询都会恢复游标)。
- **Dispatcher pattern** for builtins: (1) handler in `src/wal/builtins/<module>.rs` (2) register in `src/wal/builtins/mod.rs::register_all()` (3) 可选 `Operator` variant in `src/wal/ast/operator.rs`。
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
| **多文件选源与索引空间收口** | 加载顺序决定选源(first-match), 且 `interval_scan`/`step_scan`/`find_indices` 只对**会读值的**波形取 `max_index`; 禁止跨波形合并索引集合(内网 #32: `-l vcd -l fsdb` 查同名信号 >900s → 秒级) |
| **旁挂列缓存(跨进程)** | 冷扫描后按信号落盘 `<cache>/<file_identity>-v1.cols/<fnv(name)>.col`(key 含 ctime+inode);下一个进程 `anchored_changes` 直接命中(58.7GB 同查询 113.8s → 3.35s) |
| **FSDB 列缓存(`.fcol`)** | 同构但独立的一套(FSDB 只能靠 NPI 重扫): 列 + t0 初值在头部, 指纹/位宽校验; 暖查询 8.6s→4.1s, 冷 48.5s→36.6s(200 万时间戳夹具); `make bench-fsdb` |
| **纯逐拍 oracle** | `WAL_NO_ENGINE=1` 让引擎直接返回 None → 全部走逐拍;矩阵在子进程里用它做独立对拍 |

> 统一查询引擎(变更点并集区间扫描)已落地(docs/query-engine-design.md §IntervalSweep):
> 五条硬约束——①每个边界都要推进 prev ②区间内部边沿恒假、电平按区间长累加
> ③&&/|| 分解不得丢无法解析的谓词 ④名字解析所有读取路径一致(短名/叶子名) ⑤引用 INDEX/TS 的条件必须放弃引擎(走逐拍)。矩阵 `tests/regression_matrix.rs` 是语义冻结闸。

## Language notes (0.12.x)

- **set! 词法穿透**: 绑定是共享 cell;`(set! x v)` 写 lookup 找到的那个绑定,闭包内修改对捕获方可见(累加器可用);fn 局部 `define` 每次调用独立。未知变量 set! 报错。
- **四大写语义**: `(get/at)` 初值 = `$dumpvars` 快照;per-index 最后写入;`(at s T)` 首变化前返回 `(0 初值)`。
- 大写特殊变量 `(SCOPES)`/`(CG)` 零参调用形式与裸符号等价。

## Test data notes

- **仓库里不放波形夹具**: 单元/集成测试用 `src/tests_fixtures.rs` 现场生成的合成波形;
  需要真实大波形的门(FSDB↔VCD、性能)由环境变量开启, 没有样本时自动跳过。
- `test_data/`、`bench/data/`、`.tools/` 都是 **gitignored** 的本地产物:
  `test_data/` 已不存在(别引用), `bench/data/` 需要时用 `scripts/gen_big_vcd.py` 生成,
  `.tools/` 放手工夹具(x/z、glitch、dumpvars、alias、45-bit)、vcd2fst 二进制与旧版二进制(语义门)。
- 历史大样本(11.5GB / 58.7GB)已清理;数字与复现方法见 `bench/README.md` 与 `bench/RESULTS.md`。

## GitHub Release

**不要再手敲发布命令** —— 用脚本(它会检查工作区/tag/CHANGELOG 并跑本地 CI):

```bash
make release-dry VERSION=0.15.0   # 演练
make release     VERSION=0.15.0   # 正式(改版本号 → CI → zigbuild → push → gh release)
make dist                         # 只构建 glibc2.17 二进制, 不发布
```

Binary requires glibc ≥ 2.17 (CentOS 7 / RHEL 7 / Ubuntu 16.04+ compatible).
版本线: 0.14.x —— **`Cargo.toml` 是唯一版本来源**, 发版走 `make release VERSION=x.y.z`(见 `CONTRIBUTING.md` §4);
`.githooks/pre-commit` 自动 bump 补丁号(**Cargo.toml 已暂存时不 bump** —— 版本变更与代码同 commit 提交)。
