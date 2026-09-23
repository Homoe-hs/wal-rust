# 更新日志

本项目从 **0.14.7** 起手工维护本文件(遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.0.0/)
的组织方式, 版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/))。

* 0.14.6 及更早: 见 [GitHub Releases](https://github.com/Homoe-hs/wal-rust/releases) 与 `git log`;
  那些版本号由 pre-commit 钩子自动递增, 多数没有独立发布说明, 这里不回溯编造。
* 每次发版前必须先在 `## [未发布]` 下写清"用户能感知的变化", 再执行 `make release`。
* 面向使用者的分类: **Added**(新增) / **Changed**(行为变化, 需注意) / **Fixed**(修复) /
  **Performance**(性能) / **Docs**(文档) / **Internal**(内部/工程, 使用者可忽略)。

## [未发布]

### Added
- **FSDB 时间线的并行/集群预计算**: 新增两个正式子命令 ——
  `wal-rust fsdb-timeline-map <file> <shard> <shards> <out.part>`(算一片)与
  `wal-rust fsdb-timeline-merge <file> <part>...`(归并 → 安装 `.ftl`)。
  集群上用 `scripts/lsf_fsdb_prewarm.sh <file.fsdb> [shards]` 一条命令提交/等待/归并
  (`bsub -n 1` × N, 无 LSF 时 `--local` 本机并发, `--dry-run` 只打印提交命令)。
  `WAL_FSDB_TL_JOBS` 支持 `auto`(= min(额度, 8))。
  **归并产物与单进程写出的 `.ftl` 逐字节一致**(单测 + 真机 `cmp` 双重验证), 分片怎么切、
  归并顺序如何都不影响结果。
- **`bsub -n N` + stdin 会话**: `scripts/lsf_wal_run.sh <wave> <probes.wal> [slots]`
  把 `wal-rust --stdin -l wave < probes.wal`(一次加载、多探针)提交成 `bsub -n N`
  (`--queue/--wall/--wait/--dry-run`, 默认加 `-R "span[hosts=1]"`)。
- `scripts/bench_fsdb.sh`(`make bench-fsdb FSDB=x.fsdb [SIG=tb.clk]`): FSDB 查询基准,
  冷建/暖查询/只加载分开计时并追加到 `bench/perf-history.csv`; CI `perf` 阶段在设了
  `WAL_FSDB_BENCH=<file.fsdb>` 时自动跑(判据: 暖查询应接近"只加载")。

### Performance
- **批处理里按申请的 slot 自动并行冷建**: `bsub -n N` 会导出 `LSB_DJOB_NUMPROC`, wal-rust
  现在按它(其次 `LSB_MCPU_HOSTS`, 再其次核数)决定时间线 worker 数(封顶 8), 不用再手设
  `WAL_FSDB_TL_JOBS`;只在检测到批处理变量时才这样(登录节点仍默认单进程, 不会偷偷吃许可),
  自动开时往 stderr 打一行提示。实测(客机模拟 `LSB_DJOB_NUMPROC=8`, 200 万时间戳夹具,
  一个进程跑两条 stdin 探针): **21.5s**(单进程 40.1s)。
- **FSDB 逐信号变更列有了跨进程缓存(`.fcol`)**: FSDB 取某信号的变更列只能重走一遍
  NPI 变更流(`npiFsdbTimeBasedVcIter`), 以前**每个新进程都要为该查询用到的信号重扫一遍**,
  与查询复杂度无关 —— 大波形上就是"每次查询都慢"。现在冷扫描后把列(t0 初值放在文件头部,
  取值不必解整列)落盘, 下一个进程直接命中;`prepare()` 声明的多信号查询同样先吃缓存。
  实测(客机, 200 万时间戳 / 100 万沿夹具, TCG 模拟下偏保守):
  电平计数暖查询 8.6s → **4.1s**(2.1×)、沿计数 8.0s → **3.6s**(2.2×), 冷查询 48.5s → **36.6s**;
  暖查询已逼近"只加载"的 3.2s 底线。
- **单机并行冷建对"信号少、时间戳多"的波形也生效了**: 并行判据曾是"信号数 ≥ 2048",
  于是"一组计数器打满时间轴"(几百毫秒级别的信号数、几千万时间戳)这类波形永远走单进程。
  改成"每个 worker 至少分到一个信号"。实测冷建 40.1s → 24.8s(`TL_JOBS=4`)/ 21.6s(`TL_JOBS=8`)。
- **时间线冷建不再逐块拷贝主表**: 每 4096 信号一块, 以前每块结束都把已累积的时间线整份
  拷贝一遍(O(块数 × 主表长) —— 1.88M 信号 = 459 块 × 上千万时间点 = 几十 GB memcpy),
  现在收齐分片后一次归并。

### Internal
- `tests/fsdb_diff.rs` 新增三个闸: `fsdb_col_cache_hit_matches_cold`(需 Verdi, 同一查询在
  "不用缓存 / 建缓存 / 命中缓存"三次运行下必须同答且 `.fcol` 落盘)、
  `timeline_map_reduce_matches_single_process_encoding`(map/reduce 的 `.ftl` 与单进程
  逐字节一致 + 归并顺序无关 + 坏分片报错)、`timeline_jobs_parsing_is_conservative`;
  `src/trace/fsdb.rs` 增加列缓存编解码往返单测(含 4-state、初值缺失、指纹失效、损坏)。
- 并行写的缓存临时文件带 PID 后缀: N 个 worker 同时走 `FsdbTrace::load` 会写同一份
  `.fnames`, 固定 tmp 名会互相 rename 走半份文件。

<!-- 新一轮变更写在这里(下面直接写 ### Added / ### Fixed / ...)。
     发版时把本标题改成 `## [x.y.z] - YYYY-MM-DD`, 并在文件顶部新开一个「未发布」小节 ——
     只留本注释, 不要留占位条目:`scripts/check_docs.py` 与 `scripts/release.sh` 都会拒绝
     只有模板的版本节(曾发生: v0.14.25/v0.14.26 的 release notes 发成了空模板)。 -->

## [0.14.26] - 2026-09-21

### Performance
- **名字解析不再每次求值克隆整张名字表(188 万信号 FSDB 上的致命项)**。裸符号求值走
  "自动解析信号名", 它调用 `traces.signals()`, 而 `FsdbTrace::signals()` 返回
  `sig_names.clone()` —— 统一引擎在每个边界都重新求值条件, 于是 O(N)×边界数:
  现场表现为"`get` 名字解析慢到 3 分钟不出结果"。改为调用后端 `resolve_name`
  (索引式 + 缓存)。实测(200k 信号 VCD, 5 个裸符号的 `||` 条件):
  逐拍 117.3s → **8.2s**(14×), 引擎路径 0.27s → **0.06s**, 结果一致。
- FSDB 短名解析加**叶子名排序索引**(`leaf_order`, 只存 u32, O(log N) 查找, 首次用时懒建):
  此前每个不同拼写的短名都要线性扫全部信号名(188 万 → 几十~几百毫秒/次)。

- `scripts/fsdb_vcd_quickcheck.sh <fsdb> <vcd> [sig…]`: 一分钟内回答"两份波形是不是同一份仿真"
  —— ① 信号集(数量/同名交集/各自独有) ② 索引空间长度 ③ 典型信号的边沿/电平/变更计数,
  逐项 ✅/❌ 并给出"是不是 fsdb2vcd 匹配问题"的结论。
- `scripts/bench_name_resolution.sh [信号数] [时间戳数]`: 名字解析/裸符号求值的可复跑基准
  (引擎 / 逐拍 / 字符串对照三条路径), 专门盯"O(N) 每次求值"回归。

### Internal
- CI 的 `perf` 阶段接入名字解析基准(>240s 判退化);`AGENTS.md` 增加硬约束"求值热路径禁止 `signals()`"。

## [0.14.25] - 2026-09-21

### Fixed
- **裸信号符号做电平比较会静默错值**: `(= clk 1)` / `(! rst)` 这类**手册在教**的写法被判成
  "不引用信号", 于是常量折叠在索引 0 求值一次套用到全部索引 ——
  `(count (= clk 1))` → 20、`(count (= clk 0))` → 0、`(count (< clk 1))` → 0(真值都是 10)。
  现在裸符号(既不是变量、又能被已加载波形解析)一律算作信号引用, 引擎与纯逐拍对拍一致;
  变量条件(`(= x 1)`)的常量折叠不受影响。回归: `matrix_bare_signal_symbols_count_correctly`。
- `wal-rust run -c <表达式>` 不再要求同时给 `<FILE>`(给了 FILE 时 `-c` 优先);
  两个都不给时报错并给出用法(退出码 2)。
- `--help` 里 "125 named operators" 更正为 146(实际注册数), 并加单元测试守门:
  帮助文案与注册表不一致就红。
- 文档门禁的测试数口径改为 cargo 口径(`2×单元 + 集成`), 不再把 295 误报成 223。
- `cargo build --release` **零告警**(此前 37 条): 清掉未用 import/赋值与死代码,
  旧的手写 FST 读器(`src/fst/reader.rs`, 查询路径已改用 wellen)整体标注 `#![allow(dead_code)]`
  并说明保留原因, 不再淹没真实告警。

### Internal
- `scripts/release.sh`: ssh 推送失败时回退到 gh 的 HTTPS 凭据, 并刷新 `origin/main` 跟踪引用。

## [0.14.21] - 2026-09-21

### Changed
- **CLI 一次性表达式里"多顶层形式"被当成函数调用(静默错值)**: `parse_expr` 把程序交给
  `eval_list` 时, 若首元素求值成 Closure/Macro 就走 IIFE 分支、把其余顶层形式当作实参 ——
  `(define add5 ((fn (n) (fn (x) (+ x n))) 5)) (add5 3)` 答 **13**(应为 8);
  `defun` 返回闭包报 Arity error;`(twice (print "hi"))` 打印 **4** 次(应 2 次);
  `macroexpand` 会连带求值。现在多顶层形式显式包成 `(list ...)`, 按书写顺序求值。
  (脚本模式与 `--stdin` 逐条求值本来就不受影响, 所以只在"命令行一把梭"时踩到。)
  回归: `matrix_cli_multiform_program_forms`。
- **词法**: `1e3` / `1.5e3` / `1.5E-3` 等科学计数法现在被识别(此前拆成 `1` + 符号 `e3`,
  报 "Undefined symbol: e3");`#timeout`(合法分组符号 `#name`)不再被拆成 `#t` + `imeout`
  (此前报 "Undefined symbol: imeout"), 找不到组时给明确语义错误。
  回归: `matrix_lexer_scientific_and_sharp_symbols`。
- **变参宏(`defmacro`/`defunm` 单个符号作参数表)只绑到第一个实参**: 例如
  `(defunm m args (length args)) (m 1 2 3)` 报 `length expects list or string`(应为 3)。
  根因是构造宏对象时漏置 `variadic` 标志;顺带修掉 `defunm` 把 body 多包一层列表的问题
  (`(defunm m args (length args))` 曾返回 `(3)` 而不是 `3`)。
  回归: `matrix_variadic_macros_bind_all_args`。
- **`(sample-at s idx)` 浮点索引不再静默截断**: `(sample-at s 4.5)` 曾取索引 4 的值, 现在明确报错
  (整数除法用 `(div a b)`)。回归: `matrix_sample_at_rejects_float_index`。
- `test_samples/verify_vcd.wal` 从"只打印不断言"改为**断言式**自检: 9 项检查, 任一不成立即退出码 1。
- `tests/fsdb_diff.rs` 的两个 Verdi 门改为 `#[ignore = "需要 Verdi/NPI…"]`: 没 Verdi 时如实显示
  `ignored` 而不是"5 passed"(以前是静默 return, 造成门禁跑过的假象);有样本时
  `make gates` 会自动 `--include-ignored` 真跑。

## [0.14.17] - 2026-09-20

### Added
- **工程外壳**: LICENSE(MIT/Apache-2.0)、CHANGELOG、CONTRIBUTING、SECURITY、issue/PR 模板、
  `rust-toolchain.toml`、`.editorconfig`、`.gitattributes`。
- **本地 CI**: `make ci`(`scripts/ci.sh`)—— 环境自检 / 文档一致性 / fmt / clippy / build / test /
  语义冻结闸 / 打包冒烟 / 性能冒烟, 本机与 GitHub Actions 跑同一份脚本;失败的阶段会给出日志路径与复跑命令。
- **文档一致性检查**(`python3 scripts/check_docs.py`, CI 的 `docs` 阶段): 本地路径引用是否还存在、
  `docs/README.md` 索引是否收录全部文档、文档里声称的测试数是否与实际一致、面向当前用户的文档是否还在讲旧版本。
- **发版脚本** `scripts/release.sh`(`make release VERSION=x.y.z`): 校验工作区/分支/tag/CHANGELOG →
  跑本地 CI → glibc 2.17 交叉构建 → 推送 → 创建 release;`make release-dry` 可演练。
- **CI 徽章与文档地图**: README 重写为项目门面(安装/上手/速查/口径/架构/性能),`docs/README.md` 作为文档唯一入口。
- 历史归档目录 `docs/history/` 与"历史文档不追改"的约定(带横幅的文档, 其过时路径由 CI 降级为提示)。

### Fixed
- **`(slice x start end)` 在 `start >= end` 时崩溃**: `(slice (list 0 1 2 3 4 5 6 7) 3 0)` 直接
  panic(slice index starts at 3 but ends at 0);字符串分支还会错取(`(slice "hello" 3 1)` → `"lo"`)。
  现在按空区间返回 `()`/`""`。回归: `matrix_slice_reversed_range_is_empty`。
- **含 `%` 的源码里中文被双重编码(mojibake)**: `%` 归一化(`%` → `mod `)的实现逐字节
  `push(b as char)`,把 UTF-8 的多字节序列映射成 Latin-1 —— 于是 `(printf "信号: %s\n" 42)`
  打印 `ä¿¡å·: 42`,而**不带 `%` 的中文正常**(所以长期没被发现)。现在按字节切片拷贝原串。
  回归: `matrix_percent_normalize_keeps_utf8`(同时冻结 `(% a b)` ≡ `(mod a b)` 与 `%%` 行为)。
- 多文件/缓存/打包相关的三处修复见 [0.14.8] 与 [0.14.9]。

### Changed
- `Cargo.lock` 纳入版本控制(二进制项目需要可复现构建)。
- 仓库根目录清理: 散落的实验脚本归位(`examples/wal/`、`bench/`、`test_samples/`),
  删掉无人引用的 `verify.sh`(旧的工程结构检查, 已被 `make ci` 取代)、
  误命名的 `analysis`(内容其实是个 VCD 夹具)、以及**已废弃的 C++ 垫片**
  `src/trace/fsdb_shim/`(404 行, 纯 Rust FFI 取代后无人引用;垫片源码见历史提交 `b993d8c`)。
- `test_samples/` 重写为**真正能跑**的脚本: 旧入口引用了两个不存在的 `.wal` 与不存在的
  `test_data/`, 必然失败;现在自包含脚本直接跑, 需要波形的脚本用 `-l`/`WAVE=` 提供。

### Docs
- 全量文档审计(93 条: 断链 15 / 版本过时 8 / 与实现不符 31 / 已完成却写成 TODO 8 / 自相矛盾 15 /
  重复 6 / 结构 10)并逐条修复: 见 `docs/README.md` 的状态约定与 CHANGELOG 下方各版本条目。
- 重点纠正的事实错误: `0` 是假值(README 原写"0 为真")、多文件同名信号**先 `-l` 的为准**
  (README 原写"以后加载的为准")、`count`/`find` 是**逐索引**语义(迁移文档原写"变化点采样")、
  FSDB 自 0.14.x 起**已支持**(复盘文档仍写"被拒绝")、`TS` 返回的是 **INDEX** 而非时间戳、
  手册里 22 个"标准库宏"其实**未随 wal-rust 发布**(`std/` 不存在)、`(signals)` 等小写形式不存在(只认大写)。
- `docs/README.md`: 10 篇文档的索引(性质/状态/读者);新增文档必须登记。
- 手册 MOC 修正(补 8.3、锚点错位)、`convert` 从"现存操作符"移除、`&&`/`||` 返回值更正为 `true`/`false`。

## [0.14.9] - 2026-09-17

### Fixed
- **相对路径加载 FSDB 不再"一个缓存都不写"**。NPI 沙箱会把工作目录切到
  `<cache>/npi/`, 而缓存 key 需要 stat 波形文件本身; 之前 trace 里存的是用户给的相对路径,
  于是 `-l design.fsdb`(最常见写法)每次都重养一遍全文件扫描(全局时间线)。
  现在统一记绝对路径, NPI open 与缓存 key 用同一个。
  新增回归 `tests/fsdb_diff.rs::fsdb_cache_written_for_relative_path`(需 Verdi, 否则自动跳过)。

## [0.14.8] - 2026-09-17

### Changed(行为变化, 多文件同时加载时必须注意)
- **选源由 `-l` 顺序决定**: `TraceContainer` 改为按加载顺序迭代。此前用 `HashMap` 存 trace,
  迭代顺序每个进程都不同 —— 两条波形都有同名信号时(同一设计导出 FSDB + VCD 是常态),
  同一条命令会在两条波形之间**随机**选一条(实测 6 次里 5 次一个答案、1 次另一个)。
  现在**先 `-l` 的先算**, 结果可复现。同时禁止跨波形合并索引集合(索引是各自的变更排名),
  取值/取索引路径统一 first-match。
- 查询的**索引空间**只由"实际会读值的波形"决定; 不引用信号的查询(常量条件 / `INDEX` / `TS`)
  取主波形(第一条 `-l`)。未参与查询的波形不再被问 `max_index`。
- 缓存 key 由 `basename+size+mtime` 改为 `basename+len+mtime+**ctime**+**inode**`:
  修掉"同一秒内等长改写 + 改回 mtime"会导致**热查询返回旧列**的问题(首尾 64KB 指纹拦不住中段改动)。
- 缓存写失败(`ulimit -f` / 磁盘满)不再杀进程: 启动即忽略 `SIGXFSZ`, 降级为"不写缓存"并提示一次,
  结果与退出码不受影响。

### Fixed
- 逐拍回退(`step_scan`)与统一引擎对多文件的索引空间规则一致; `INDEX`/`TS` 读"正在被推进的那条 trace"。

## [0.14.7] - 2026-09-17

### Added
- **FSDB 直读(借助 Verdi NPI)**: 运行期 `dlopen` `libNPI.so` + Itanium 符号, 纯 Rust FFI,
  不需要 C++ 垫片。支持 `-l design.fsdb` 查询、`(load)`/`sigs`/`count` 等全部现有功能。
- 时间线 / 名字树落盘缓存(`.ftl` / `.fnames`), 冷查询之后第二次起毫秒级。
- `WAL_FSDB_TL_JOBS=N`: 冷构建全局时间线时按信号轮转分片并行(每个 worker 一个 Verdi 许可)。

### Performance
- 边沿 / 初值 / `getwave` / `at` 类查询不再物化索引空间(跳过全文件扫描)。
- 电平条件只数区间长度, 不展开逐索引。
- 内网 174MB FSDB 实测: `is-x` 394s/4GB → 0.25s/126MB; 冷时间线 400.5s → 175.1s(`TL_JOBS=4`);
  热缓存 0.28s。细节与全部数字见 `docs/fsdb-npi.md`。

### Docs
- `docs/fsdb-npi.md`: NPI 逆向结论、缓存与并行构建设计、实测表格、内网环境与坑位。

---

## 更早版本(0.14.6 及以前)

| 版本线 | 主题 |
|---|---|
| 0.14.0 – 0.14.6 | 开发期(未单独发布): 口径 A(t0 初值永远是快照, VCD/FSDB 索引空间对齐)、FFR 垫片探索后废弃、VCD 别名信号收敛 |
| 0.13.x | 两段式 VCD 加载(懒索引 + 变更列融合)、统一区间扫描引擎、旁挂列缓存 |
| 0.12.x | 查询语义冻结(值 = 该时间戳最后写入 / 初值 = `$dumpvars` / 边沿按逐索引跳变)、WAL 语言四大写语义 |
| ≤0.11 | FST/VCD 读写、WAL 解释器、REPL、tree-sitter 语法 |

细节: `git log --oneline` 与 [Releases 页](https://github.com/Homoe-hs/wal-rust/releases);
设计取舍见 `docs/` 下各篇(索引见 `docs/README.md`)。
