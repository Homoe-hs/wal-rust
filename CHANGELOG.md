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
- (下一次发版前在这里写;分类见文件头)

## [0.14.14] - 2026-09-20

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
