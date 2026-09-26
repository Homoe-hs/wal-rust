# wal-rust — WAL: Waveform Analysis Language

[![ci](https://github.com/Homoe-hs/wal-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/Homoe-hs/wal-rust/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/Homoe-hs/wal-rust)](https://github.com/Homoe-hs/wal-rust/releases)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#许可证)
[![glibc](https://img.shields.io/badge/glibc-%E2%89%A52.17-informational)](#安装)

[WAL](https://wal-lang.org)(Waveform Analysis Language)的高性能 Rust 实现:
**专注波形查询**, 用表达式/脚本从 VCD / FST / FSDB 里直接拿到答案 ——
不生成波形、不做格式转换、不读设计代码。

```bash
$ wal-rust '(count (rising "top.clk"))' -l design.vcd
=> 820965

$ wal-rust '(count (&& (= (get "awvalid") 1) (= (get "awready") 1)))' -l design.fsdb
=> 846848
```

| | |
|---|---|
| 当前版本 | **0.14.32**(发版记录见 [`CHANGELOG.md`](CHANGELOG.md)) |
| 输入格式 | VCD、FST([wellen](https://crates.io/crates/wellen))、**FSDB**(借 Verdi NPI, 纯 Rust FFI) |
| 平台 | Linux x86_64;发布二进制要求 **glibc ≥ 2.17**(CentOS 7 / RHEL 7 / Ubuntu 16.04+) |
| 规模 | 已在 **152GB / 58.7GB** 合成波形与 **5.86GB** 真实波形上验证(冷查询单遍读文件, 见[性能](#性能)) |
| 语义 | 4 状态(x/z)全保留;口径唯一, 见 [`docs/4-state-semantics.md`](docs/4-state-semantics.md) |

## 这是什么 / 为什么用它

波形 debug 的日常是"在几百 GB 的 dump 里找一个条件成立的次数/时刻"。
GUI(如 Verdi/GTKWave)擅长看, 不擅长批量跑与进 CI;通用脚本(python 解析 VCD)慢且各自实现语义。
wal-rust 的做法:

* **一个统一查询引擎**:`count` / `find` / `whenever` / `step` 共用同一条
  区间扫描实现, 语义只有一处定义([`docs/query-engine-design.md`](docs/query-engine-design.md) §1);
* **不做快慢两条路**:边沿/电平/4 状态条件都走同一套边界推进, 再靠"变更点并集 + 值覆盖"提速;
* **正确性优先**:`WAL_NO_ENGINE=1` 提供纯逐拍 oracle, 测试矩阵拿它与引擎对拍;
* **波形只读一遍**:加载只读文件头, 索引与被查询信号的变更列在同一遍扫描里算出来并落盘复用。

## 安装

**方式一: 下发布二进制**(无需 Rust / Verdi)

```bash
# https://github.com/Homoe-hs/wal-rust/releases  (glibc ≥ 2.17)
chmod +x wal-rust && sudo mv wal-rust /usr/local/bin/
wal-rust --version        # → wal-rust 0.14.9 (from /usr/local/bin/wal-rust)
```

**方式二: 源码构建**

```bash
git clone https://github.com/Homoe-hs/wal-rust && cd wal-rust
cargo build --release            # 本机工具链
make ci-fast                     # 或先跑一圈本地 CI 确认环境没问题
bash scripts/install.sh          # 可选: 装到 PATH
```

**方式三: 老 glibc 机器上发版/自建**(需要 zig + cargo-zigbuild, 见 `scripts/install.sh`)

```bash
make dist      # —> target/x86_64-unknown-linux-gnu.2.17/release/wal-rust
```

## 快速开始

```bash
# 1) 一次性查询(不用写 WAL 表达式, 适合 shell/CI)
wal-rust count design.vcd clk 1        # clk==1 的时间戳个数
wal-rust sigs  design.vcd "wstrb" 10   # 名字含 wstrb 的信号(前 10)
wal-rust topsig design.vcd             # 变更最频繁的信号

# 2) WAL 表达式(主力用法;波形用 -l 预加载)
wal-rust '(count (rising "top.clk"))' -l design.vcd
wal-rust '(count (is-x "state"))'     -l design.vcd          # 有多少时间处于 X
wal-rust '(getwave "state")'          -l design.vcd          # 全部变更点 (t v)
wal-rust '(at "state" 1515000)'       -l design.vcd          # 某时刻的值
wal-rust '(assert-eq "sig" 100000 200000 1)' -l design.vcd   # [t0,t1] 内恒为 1?

# 3) 脚本 / REPL
wal-rust run script.wal -l design.vcd
wal-rust repl

# 4) 会话模式: 一次加载, 多条探针(大波形最划算)
printf '(count (changes "clk"))\n(get "state")\n' | wal-rust --stdin -l design.vcd

# 5) CI 用法: 遇错即停, 退出码可判
wal-rust run --halt-on-error script.wal -l design.vcd
```

输入自动识别:以 `(` 开头 → 当表达式;是存在的文件 → 当脚本;没有输入 → REPL。

### 三个容易踩的点

* **时间单位**:`getwave`/`at`/`edges` 用文件原生单位(VCD 的 `$timescale`),
  人读时间用 `(fmt-time T)`,节拍号用 `(fmt-time T "clk")`。
* **`(get s)` 取的是当前 `INDEX`**,不随 `map` 的位置漂移;按时间取值用 `(at s T)`。
* **x/z**:`x` 不等于 0(`(= (get a) 0)` 为假);`x→1` 算 `changes` 但**不算** `rising`;
  索引 0 没有前驱,除非 `$dumpvars` 给了确定初值(此时会有告警提示)。

## 语言速查

完整用法见 [`docs/wal-编程手册.md`](docs/wal-编程手册.md)(2600+ 行);这里只放最常用的:

| 类别 | 用法 |
|---|---|
| 算术/比较 | `+ - * / %`、`== != < <= > >=`、`min max abs`、`(fmt-time T)` |
| 逻辑 | `&& \|\| ! not`;真值:**`#f` / `0` / `()` 为假**,其余为真 |
| 绑定/函数 | `(define x v)`、`(define (f a) ...)`、`(set! x v)`(词法穿透)、`(fn (a) ...)`、闭包 |
| 列表/数组 | `(list …)`、`(map f lst)`、`(length l)`;⚠️ `car/cdr/append/reverse/filter/sort/dotimes/stable` 等**上游标准库形式 wal-rust 未内置**(调用会报未知算子, 见手册 §1.10/§8.5) |
| 宏 | `(defmacro name (args) body)`、`(defun …)`、`(defunm …)` |
| 控制流 | `(if c a b)`、`(when c …)`、`(cond …)`、`(while c …)`(后两者逐拍推进, 大波形上慎用) |
| 波形取值 | `(get s)`、`(at s T)`、`(sample-at s idx)`、`(getwave s)`、`(edges s T0 T1)` |
| 边沿/状态 | `(rising s)`、`(falling s)`、`(changes s)`、`(is-x s)`、`(is-z s)` |
| 计数/查找 | `(count cond)`、`(find cond)`、`(whenever cond …)`、`count/step`、`find/step` |
| 信号名 | `(find-sig "pat")`(支持 `*` `?`)、`(SIGNALS)`、`(signal-width s)` |
| 特殊变量 | `(INDEX)`、`(MAX-INDEX)`、`(TS)`、`(SIGNALS)`、`(TRACE-NAME)`、`(TRACE-FILE)` —— **只认大写** |
| 其它 | `(printf …)`、`(assert-eq s T0 T1 v)`、`(doc "topic")`、`(exit N)` |

> ⚠️ `(TS)` 目前返回的是**当前 INDEX**(不是原生时间戳)。要按时间过滤, 用
> `(at s T)` / `(getwave s)` 配合原生时间;这一点与手册 §8.4 的说明一致, 未来若改为
> 返回时间戳会在 `CHANGELOG` 里作为行为变化标注。

## 波形分析核心口径(必读)

| 规则 | 说明 |
|---|---|
| 值的时间语义 | 某索引处的值 = **该时间戳最后一次写入**(同一时间戳的多次写入折叠) |
| 初值 | `$dumpvars` 快照;没有则 `x`。t0 快照**不是** INDEX 0 的条目 |
| 索引空间 | 所有被引用信号变更时间的**并集**;逐索引查询从 INDEX 0 扫到末尾 |
| 边沿 | 逐索引跳变;`x→1` 是 `changes`,**不是** `rising` |
| 计数入口一致 | `count` 子命令 / `(count …)` / `(find …)` / `(whenever …)` 全入口同一口径 |

细节与全部特例: [`docs/4-state-semantics.md`](docs/4-state-semantics.md)(4 状态)、
[`docs/query-engine-design.md`](docs/query-engine-design.md) §1(查询语义)。

## 多文件与缓存(会影响结果, 请看)

```bash
wal-rust '(count (rising "clk"))' -l a.vcd -l b.fsdb
```

* **同名信号以先 `-l` 的为准**(first-match)。后加载的同名波形不参与回答 —— 该规则自
  0.14.8 起确定化(此前是 HashMap 迭代顺序, 跨进程随机)。
* **索引空间**只由"实际会读值的波形"决定:未被查询引用的波形不会参与, 也不会被拖去
  做全文件扫描(FSDB 的 `max_index` 等于一次全文件扫描)。
* **不做跨波形索引合并**:索引是各波形自己的变更排名,合并出的集合没有意义。
* **缓存**写在实际执行命令的目录下(`./.wal-rust-cache/`,可用 `WAL_CACHE_DIR` 指定):

| 文件 | 内容 |
|---|---|
| `<basename>-<len>-<mtime>-<ctime>-<inode>-v1.wcol` | 加载索引(跳过头解析) |
| `<...>-v1.cols/<fnv(信号名)>.col` | 按信号落盘的变更列(跨进程复用) |
| `WAL_CACHE=off\|auto\|build\|read` | 关闭 / 默认 / 强制构建 / 只读 |
| `WAL_CACHE_MIN_MB`、`WAL_CACHE_DIR` | 缓存阈值、目录 |

缓存 key 含 `ctime` 与 `inode`,所以"同尺寸同 mtime 的改写"不会命中陈旧数据;
缓存写失败(磁盘满 / `ulimit -f`)只提示一次, 不影响结果与退出码。

## 格式支持

| 格式 | 读 | 写 | 说明 |
|---|---|---|---|
| **VCD** | ✅ | ✅(`dump-trace`) | mmap + 懒索引;支持 gzip/bzip2/zlib;别名信号(idcode 复用)已收敛 |
| **FST** | ✅([wellen](https://crates.io/crates/wellen)) | ⚠️ 仅往返测试 | 手写 reader 已从查询路径退役;`dump-trace` 只写 VCD, `.fst` 会明确拒绝 |
| **FSDB** | ✅(借 Verdi NPI) | ❌ | **不逆向、不转换**:运行期 `dlopen` `libNPI.so` + `dlsym`,二进制无 C++ 依赖 |

FSDB 用法与要点(`docs/fsdb-npi.md`):

```bash
export VERDI_HOME=/path/to/verdi      # 或 WAL_NPI_LIB=/path/to/libNPI.so
wal-rust '(count (rising "clk"))' -l design.fsdb
```

* 只用 `npiFsdbTimeBasedVcIter`(归并迭代器)一次拿到时间线与变更列;
* 打开会 checkout 一个 Verdi 许可(与开 Verdi 一样占 seat);
* 边沿/取值/`getwave` 类查询**不需要**物化全局时间线;需要索引空间的查询有落盘缓存
  (`WAL_FSDB_TL_JOBS=N` 可并行冷建);
* 找不到库时 FSDB 给出明确报错, VCD/FST 通路不受影响。

## 架构

```
wal-rust/
├── src/
│   ├── main.rs / cli.rs / lib.rs   # 入口(表达式/脚本/REPL 自动识别)、clap 参数
│   ├── wal/                        # WAL 语言核心
│   │   ├── ast/                    # Operator 枚举(154 个变体)、Value、Symbol、WList、Closure、Macro
│   │   ├── lexer/ parser/          # tree-sitter 语法 + @/#/~ 变换
│   │   ├── eval/                   # Evaluator、Environment(Rc<RefCell>)、Dispatcher、
│   │   │                           #   统一区间扫描引擎 interval_scan
│   │   ├── builtins/               # 146 个注册算子 / 12 个模块
│   │   └── repl/                   # REPL(rustyline);--stdin 会话在 main.rs
│   ├── vcd/                        # VCD 读器(mmap + memchr + 压缩识别)
│   ├── fst/                        # FST 写器(读走 wellen)
│   └── trace/                      # 波形抽象层
│       ├── trace.rs                # Trace trait: 值/边沿/计数/名字解析的口子
│       ├── container.rs            # TraceContainer(按加载顺序的选源与索引空间规则)
│       ├── vcd.rs                  # VcdTrace: 懒索引 + 变更列 + 旁挂缓存
│       ├── fst.rs                  # FstTrace(wellen)
│       └── fsdb.rs                 # FsdbTrace(NPI 纯 Rust FFI)
├── tree-sitter-wal/                # WAL 语法(含生成的 parser.c… 见 tree-sitter-wal/build.rs)
├── tests/                          # 11 个测试文件: 语义矩阵 51 + golden 4 + 随机差分 3 + 集成/单元
├── scripts/                        # ci.sh(本地 CI) / release.sh / check_docs.py / 生成器
├── docs/                           # 文档, 入口是 docs/README.md
└── bench/                          # 样本生成、性能脚本与历史数字
```

### 关键设计取舍

| 取舍 | 原因 |
|---|---|
| 只做查询, 不做转换 | 转换链会引入"到底谁的语义"的争论;格式各用最好的读法(wellen / NPI) |
| 加载只读文件头, 索引惰性构建 | 152GB 波形上"加载"不该等于"整读一遍";谁需要 dump 区数据谁付那遍扫描 |
| 统一区间扫描引擎(不分快慢路) | 快慢两条路必然语义漂移;用变更点并集 + 值覆盖把一次扫描做对 |
| 纯逐拍 oracle 常驻 | `WAL_NO_ENGINE=1` 让测试有独立参照, 引擎改动不再是"自己验自己" |
| FSDB 借 NPI 而不是逆向 | 正确性由 Synopsys 保证;代价是运行期依赖许可与库 |
| 缓存 key 含 ctime/inode | 脚本反复生成同名波形是常态, size+mtime 会命中陈旧数据 |

## 性能

数据采集于 v0.13.2(本机 16 核 / NVMe, 合成波形);**同表内对比才有意义**,绝对秒数受页缓存影响很大。

**58.7GB 合成波**(3.5M 信号 / 1.5M 时间戳 / ~1.65G 变更)

| 指标 | v0.11.11 | v0.13.0 | v0.13.2 |
|---|---|---|---|
| 加载(`load` / `(SIGNALS)`) | 360s | 82s 冷 / 42s 暖 | **1.1s**(只读文件头) |
| 首次查询(冷) | 674s | 216s | **72.6s**(单遍读 + 解析) |
| 同进程第二次同信号 | 208s | ≈0s | **≈0s** |
| 新进程同信号(缓存命中) | 114s | 2.5s | **2.5s** |

**FSDB(借 NPI)** 与 VCD 的口径不同:见 [`docs/fsdb-npi.md`](docs/fsdb-npi.md) §6;
真实 5.86GB 波形上边沿计数 1.4s、电平条件 0.27s、`is-x` 由 394s/4GB 降到 0.25s/126MB。

**测量口径(重要)**:RSS 含 mmap 文件页(`smaps_rollup` 的 Private_Dirty 才是堆);
比较必须在同一轮内 A/B(页缓存会让同一条命令在 42s–82s 间波动)。
复现: `bash scripts/perf_history.sh <wave> <sig> <tag>`;历史与生成方法见 [`bench/`](bench/)。

## 文档与开发

| 你想…… | 去哪 |
|---|---|
| 查文档总览/权威口径 | [`docs/README.md`](docs/README.md) |
| 学会 WAL 语言 | [`docs/wal-编程手册.md`](docs/wal-编程手册.md) |
| 参与开发、跑本地 CI、发版 | [`CONTRIBUTING.md`](CONTRIBUTING.md) |
| 看每个版本改了什么 | [`CHANGELOG.md`](CHANGELOG.md) |
| 给 AI 代理的项目说明 | [`AGENTS.md`](AGENTS.md) |

```bash
make help        # 所有常用任务
make ci          # 本地 CI(与 GitHub Actions 同一份 scripts/ci.sh)
make gates       # 语义冻结闸: 矩阵 + 随机差分 + 旧版语义对拍
make release VERSION=0.15.0
```

编辑器:配套 LSP(wal-lsp)提供诊断/补全/hover;

```
wal-lsp: 语法与语义诊断、补全、hover 文档、跳转定义、文档符号
```

## 许可证

双许可 **MIT OR Apache-2.0**(见 [`LICENSE-MIT`](LICENSE-MIT)、[`LICENSE-APACHE`](LICENSE-APACHE)),
与 Rust 生态惯例一致:你可以任选其一。除非明确声明,提交到本仓库的补丁都按上述双许可分发。
