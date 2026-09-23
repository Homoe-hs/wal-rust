# FSDB 读取:借 Verdi 的 NPI,纯 Rust FFI

> ✅ **现行设计 + 实测记录** —— FSDB 后端(借 Verdi NPI, 纯 Rust FFI)的设计、缓存策略与实测数字;
> 多文件/缓存一致性规则见 §9。

> 一句话:**FSDB 我们不猜格式,直接调用 Synopsys 正版 Verdi 的 NPI 读库**(`libNPI.so`);
> 而调用方式是 `dlopen` + `dlsym` 手写 FFI —— **不需要 C++ 编译器,也不需要 fsdb2vcd 转换**。
> 没有 Verdi 的机器上,FSDB 依旧是"明确报错 + 提示",VCD/FST 通路完全不受影响。

## 1 为什么不是别的路

| 路线 | 结论 |
|---|---|
| **自己逆向 FSDB 二进制** | 不做。写者(FSDB 版本)一直在变,单变量差分成本极高,收益不确定 |
| **fsdb2vcd 转换后再读** | 不做(用户明确否掉):多一次落盘、丢掉 FSDB 独有信息、还要外部工具 |
| **FFR(`libnffr.so` + C++ 垫片)** | 做过并跑通, 但要拖一条 C++ 编译链(垫片源码见历史提交 `b993d8c`);产品通路改走 NPI 纯 Rust FFI |
| **NPI + 纯 Rust FFI** ✅ | 只需要目标机已装 Verdi;二进制里没有 C++ 依赖,运行期发现库、缺库优雅退化 |

## 2 纯 Rust 调 C++ 库:三个实测结论

NPI 的头文件是 C++(`npi_fsdb.h` 里有 `#include <map>`、引用参数),但**导出的函数可以直接
`dlsym` 调用**,因为:

1. **C 风格 API 的参数全是 C 类型** —— 所有 handle 都是 `typedef void*`。函数名是 Itanium
   mangled(如 `_Z13npi_fsdb_openPKc`),Rust 侧按名字查符号 + `extern "C"` 声明即可。
   `npi_init(int& argc, char**& argv)` 的引用在 ABI 上就是指针,直接 `*mut c_int` /
   `*mut *mut *mut c_char`。
2. **`npiFsdbValue.format` 是入参**(要什么格式),不是出参。官方
   `share/NPI/example/via_examples/NPI_Models/FSDB_Model/npi_fsdb_vct_value/demo.cpp`
   里先 `val.format = npiFsdbBinStrVal` 再调用,成功(返回非 0)后从 `val.value.str` 取
   **4 态位串**。按出参读会得到 `format=-1 / value=0`。
3. **归并迭代器 `npiFsdbTimeBasedVcIter` 是 C++ 类,但布局只有一个 `Impl* m_impl`**(8 字节)。
   纯 Rust 分配一块带对齐的缓冲、调构造/析构符号,就能当对象用:
   - 构造 `_ZN22npiFsdbTimeBasedVcIterC1Ev`
   - `add(sig)` `_ZN22npiFsdbTimeBasedVcIter3addEPv`
   - `iter_start(begin,end)` / `iter_next(&t,&sig)` / `get_value(&v)` / `iter_stop()`
   2018(O-2018.09) 与 25A(X-2025.06) 两代 `libNPI.so` 都导出这些符号。

   ⚠️ **硬约束**:同一个进程里用过 `npiFsdbTimeBasedVcIter` 之后,`npi_fsdb_create_vct`
   就返回 0(实测,顺序反过来也一样)。所以本后端**全程不碰 `create_vct`**,所有
   取值/扫列都走归并迭代器。

## 3 运行期环境(交给用户的只有两个变量)

```bash
export VERDI_HOME=/path/to/verdi          # 或者直接 WAL_NPI_LIB=/path/to/libNPI.so
export SNPSLMD_LICENSE_FILE=27000@licsrv  # 内网许可;NPI 在 npi_fsdb_open 时才 checkout
wal-rust -l design.fsdb '(count (rising "clk"))'
```

后端自己处理:

* **库发现**:`WAL_NPI_LIB` → `$VERDI_HOME/share/{NPI/lib/linux64,vcst/linux64}/libNPI.so`
  → 从 `PATH` 上的 `verdi/fsdbdebug` 反推 `VERDI_HOME`。
* **资源目录**:NPI 要在 `LD_LIBRARY_PATH` 里找到 `etc/`(否则
  `[NPI ERROR] Failed to find Verdi resource directory (etc/)`)。后端自动把
  `…/share/NPI/lib/linux64`(其下 `etc` 软链到 `$VERDI_HOME/etc`)补进 `LD_LIBRARY_PATH`,
  **用户不需要手工设**。
* **输出干净**:NPI 会把版权 banner / 加载警告打到 **stdout**,`wal-rust ... | grep` 会坏掉。
  后端在 `npi_init`/`npi_fsdb_open` 期间把 fd 1 指向 `/dev/null`(`WAL_DEBUG_FSDB=1`
  时不静音,方便排障)。
* **日志目录**:NPI 会在 CWD 建 `wal-rustLog/`。后端调用期间临时 `chdir` 到
  `./.wal-rust-cache/npi/`,结束后还原(切不进去的只读目录就放弃隔离)。

## 4 语义映射(与 VCD 后端逐条对齐)

| 概念 | VCD 后端 | FSDB 后端 |
|---|---|---|
| 时间线(INDEX 空间) | dump 区所有 `#T`(规则 A:t0 dumpvars 不算 INDEX) | **所有信号变更时间的并集**(t>0),分块扫一遍归并迭代器 |
| 初值(t0 快照) | `$dumpvars` 条目 | 信号的 **t=0 条目** |
| 索引 0 之前 | 初值,无条目 → x | 同上 |
| 同索引多次写入 | 最后一次 | 最后一次(归并流里折叠) |
| 边沿/电平判定 | `eval_change_list` | 同一套语义重写为 `ScalarValue`(`eval_column`) |

`Trace::prepare(names)` 会把这些信号的变更列与**时间线构建合并成一次遍历**
(和 VCD 侧"懒索引 + 变更列融合"同一个设计)。

## 5 验证

1. **探针**(`examples/npi_probe.rs`):独立小工具,可单独交叉编译后拿到内网机器上跑,
   只用 `VERDI_HOME` 就能打印树/值变化,是"内网到底能不能读"的最小凭据。
2. **同源差分门** `tests/fsdb_diff.rs`(环境变量开启,没 Verdi 自动跳过):
   `WAL_FSDB_TEST_FILE=x.fsdb WAL_FSDB_TEST_VCD=x.vcd cargo test --release --test fsdb_diff`
   比对:时间线(逐索引原生时间) / 信号集 / 位宽 / 确定初值 / **每个索引的取值** / 变更点。
   实测 `verilog.fsdb ↔ verilog.vcd`:**179 个信号 × 406 个索引全等**(仅 2 组位炸开
   向量按上面说明跳过)。
3. **对 Verdi 复核**:宿主 `fsdbdebug -vc -vidcode N` 的输出与我们的读数一致
   (例如 t=0 条目 `xtag:(0 0) val:0`)。

### 两类**已解释**的差异(不是解析错)

* **位炸开向量**:VCS 写 VCD 时把某些总线拆成 `CH [4] … CH [0]` 五个单 bit `$var`,
  而 NPI 把它们归成一个 `CH [4:0]` 的 5 bit 信号。差分门单独统计并跳过(必须打印出来)。
* **t=0 采样时机**:VCD 的 `$dumpvars` 抓的是**仿真开始前**的快照(连续赋值还没结算 → x),
  FSDB 记的是 **t=0 delta 之后**的值。于是 `i_ALUB.clock` 这类信号 VCD 侧初值是 x、
  FSDB 侧是 0,`(count (rising …))` 会差 1(146 vs 147)。**FSDB 侧与 Verdi 一致**,
  所以这是两代写者的差异,不是我们读错;差分门只允许"VCD x → FSDB 确定值"这一个方向。

## 6 性能: 索引空间是"要不要付一遍全文件扫描"的分水岭

NPI 没有"全局时间表"API。要拿到索引空间(所有信号变更时间的并集)只能把
**每一条 `(时间, 信号)` 变更**都从 NPI 拉出来 —— 实测一个 12k 信号 / 75KB 的
FSDB, 归并流就有 **171 万条**, 只为算出 **423** 个时间点。

两件事把"每个查询都白扫一遍"变成了"只有真需要的查询才扫":

1. **`set_index` / `max_index` 的隐藏全扫**(最重要的一个)。`interval_scan` 等
   引擎路径在查询结束后会**恢复游标**, 而 `set_index` 要拿 `max_index()` 做边界
   检查 —— 于是每个查询(哪怕只是 `(initial s)`)都会触发一次全文件扫描。
   修法: 后端记住"已知有效的最大索引", 恢复游标这类**已知合法**的写入直接短路。
2. **时间基列 + 两个新 trait 方法**。`(getwave s)`/`(at s T)` 本来只想要
   "信号自己的变更点 + 原生时间", 却绕道索引空间(索引→时间回环)→ 现在
   `Trace::change_points_time()` 直接给时间, 不建时间线。`count` 只要个数,
   边沿类条件与全局时间线无关 → `Trace::count_matches()`; 电平/取值条件要按
   区间长度累加, 仍然走索引空间(这是语义决定的, 不是实现偷懒)。

   注意 `count_matches` 必须复刻 `Changed` 在**索引 0** 的特例(首个变更恰好是
   全局最早那次 + 没有确定初值时, 索引 0 没有前驱)。为此加了
   `global_first_change_time()`(分块只读每块第一条 `t>0` 的条目), 而且只在
   "真的可能差"时才去问它 —— 差分门的 24 条对拍就是盯这个的。

结果(12,005 信号 FSDB; 机器是 TCG 模拟的 QEMU, 绝对值比真机慢好几倍,
看**结构**不看绝对值):

| 查询 | 修前 | 修后 |
|---|---|---|
| `(count (rising s))` | 4.9s | **3.0s**(= 加载地板) |
| `(count (changes s))` | 4.4s | **2.9s** |
| `(getwave s)` | 5.8s | **2.5s** |
| `(at s T)` | — | **2.5s** |
| `(initial s)` / 树遍历 | 2.4s | 2.4s(本来就只加载) |
| `(count (= (get s) v))` 电平 | 5.3s | 5.7s(需要索引空间) |
| `(find (rising s))` | — | 4.9s(需要索引空间) |

对 174MB / 随机值那类波形, 原来的"`(count (rising tvalid))` >300s、峰值 17.2GB"
就是被上面第 1 条拖进全扫的; 现在这条查询只读一个信号, 内存也回到几十 MB 量级。

### 6.1 落盘缓存(0.14.3 起)

索引空间与名字树都**只跟波形内容有关, 与查询无关**, 所以都落盘复用
(`WAL_CACHE=off|auto|build|read`, 目录 `./.wal-rust-cache/`, key = 文件名+长度+mtime+首尾
64KB 指纹):

| 缓存 | 内容 | 命中效果(60k 信号实测) |
|---|---|---|
| `<wave>-<len>-<mtime>-v1.ftl` | 全局时间线(delta-varint) + 全局最早变更时间 | `find`/电平条件不再全文件扫描: 5.3s → 2.5s |
| `<wave>-<len>-<mtime>-v1.fnames` | 名字 + 位宽 + scope(2.85MB/60k 信号) | **完全跳过树遍历**: 3.3s → 2.4s(遍历 800ms → 0) |

两个缓存都**原子写**(`.tmp` + rename), 指纹不符/截断一律拒收(有单测盯着)。
mtime 变了(仿真还在写盘)或换名字 → 自动失效重建。

句柄**不进缓存**(跨进程无效): 恢复后用 `npi_fsdb_sig_by_name(全名)` 按需解析,
实测 2000 次 114ms 全中, 而树遍历是按信号数线性增长的。

还剩一个坑值得记一笔: 缓存路径必须在**进 NPI 沙箱之前**算成绝对路径 ——
沙箱为了不让 NPI 的 `wal-rustLog/` 落到用户目录会把 CWD 切到
`./.wal-rust-cache/npi/`, 期间用相对路径写缓存会写进沙箱里(表现为"文件不见了")。

### 6.2 电平条件: 只数区间长度, 不展开索引(0.14.5)

内网 174MB 波形实测 `(count (is-x tdata_i))` = **394s / RSS 峰值 4GB`** —— 全样本为 x
的信号, 匹配区间覆盖上亿个索引, 而旧实现把这些索引逐个 `push` 进 `Vec`。
现在判定与展开彻底分开:

* `eval_column_spans()` 产出 **(边沿单点, 电平区间 [start,end))**;
* `count_matches()` 电平条件 = 区间长度求和 + 边沿计数 —— **内存与匹配索引数无关**;
* 只有真正要索引的 `find` 才展开成逐个索引(语义不变)。

两者共用同一套判定代码, 所以"计数"与"`find` 的长度"不会漂移 —— VM 上用
`count` / `count/step`(逐拍 oracle) 逐信号对过。

### 6.3 内网复测(2026-09-17, hpc163)

样本 `dcache_cacheable_random_test_000` fsdb 174MB / vcd 5.86GB:

| 查询 | FSDB(0.14.4) | 旧版 | 对照 VCD |
|---|---|---|---|
| `count(rising clk)` | **1.40s** | >300s 超时 / 17GB | 60.8s |
| `count(=tvalid 1)` | **0.27s** | — | 0.52s |
| `count(changes tdata_i)` | **0.25s**(缓存后) | 冷 56s | — |
| `getwave clk` | 惰性(只读该信号) | — | >400s 未完成 |
| `count(is-x tdata_i)` | 394s / 4GB → **本版已修**(见 §6.2) | — | — |

一致性: 5 类查询与 VCD **逐值全等**(820965 / 11793 / 1060269 / 846848 …)。

### 6.4 冷构建全局时间线: 多进程并行(0.14.6, `WAL_FSDB_TL_JOBS`)

冷启动的全局时间线要"把整个 FSDB 的每条变更过一遍", 是唯一还在几百秒量级的操作。
它**是纯并集, 天然可并行** —— 实测同一文件两个进程同时冷建: 串行 20.6s+23.2s,
并行墙钟 23.8s(≈2x 吞吐)。

实现选**多进程 exec**(不是 fork/线程): NPI 的线程/fork 安全性没有保证, exec 出来的
worker 是全新进程, 走正常的 `npi_init/open`, 每个 worker 只算一段信号的变更时间并
落盘, 父进程线性归并。worker 入口就是正式子命令 `fsdb-timeline-map`(见 §6.6)。

| 60k 信号 FSDB 冷建 | 墙钟 | 说明 |
|---|---|---|
| 单进程 | 23.5s | 默认 |
| `WAL_FSDB_TL_JOBS=4` | 9.5s | 构建本身 6.9s |
| `WAL_FSDB_TL_JOBS=8` | 7.1s | 构建本身 4.6s |

内网 174MB 样本(全局时间线更大): 单进程 400.5s → `TL_JOBS=4` **175.1s**。
分片用**轮转**(`idx % jobs`)而不是连续区间: 热点信号比冷信号贵几个数量级,
连续切会让一个 worker 独自扛下整条时钟树; 轮转把冷热混在一起, 且**不需要增加
worker 数**(每个 worker 一次 NPI 初始化 ~2s + 一个 Verdi 许可, 细分成池子反而更慢
—— 实测 16 片/4 并发比 4 片慢 40%)。

串行与并行产出的 `.ftl` **逐字节一致**(已做 A/B 校验; 头部的 first_change 提示两条
路径都填)。任何一个 worker 失败 → 自动回退单进程, 不改变结果。

⚠️ **每个 worker 是一次独立的 NPI 会话, 会各占一个 Verdi 许可**; 所以默认关闭
(`WAL_FSDB_TL_JOBS=1`)。许可池够用时再开, 比如 `WAL_FSDB_TL_JOBS=4`。

还剩下的量级问题只有**第一次**构建全局时间线(冷查询) —— 它必须把整个 FSDB 的每条
变更过一遍; 之后所有进程直接读 `.ftl` 缓存。把这次构建并行化(多进程按信号分片)
是后续唯一可能再上一个数量级的点。

**实践建议**(与内网实测结论一致): FSDB 后端适合**中小波形、边沿/时间类查询、
一次性全扫**; 大波形的高频随机 `find`/电平查询仍建议用 VCD/FST。
> 注: 时间线/名字树落盘缓存**已在 0.14.3 落地**(见 §6.1), 上面这句是当时的状态描述。
> 仍需付全文件扫描的只有"第一次冷查" —— 之后所有进程直接读 `.ftl`。

### 6.5 逐信号变更列的旁挂缓存 `.fcol`(现场"每次都慢"的根因)

现场 265MB / 3700 万时间戳的 FSDB: 即使 `.ftl` 命中, **同一条查询每次运行还是慢**。
原因是 FSDB 与 VCD 的结构性差别: VCD 有内存映射索引, 一次扫描后同进程复用
(`signal_cache`)+跨进程落盘(`.cols`); 而 FSDB 拿"某信号的变更列"**只能重走一遍
NPI 变更流**(`npiFsdbTimeBasedVcIter`), 每次进程启动都要为这次查询用到的每个信号
重扫一遍 —— **与查询本身多复杂无关, 只与该信号的变更条数成正比**。

修法与 VCD 同构, 但独立一套(FSDB 侧没有 `.cols`):

* 路径 `<cache>/<file_identity>-v1.fcol/<fnv1a(信号全名)>-v1.col`
  (与 `.ftl`/`.fnames` 共用同一身份 key: basename+len+mtime+ctime+inode);
* 格式 `WALFCOL2` + `wave_fingerprint` + 位宽 + **init 编码** + 条数 + [delta 时间 varint, 值];
* **t0 初值放在头部**: `initial_of()` 被 `count_matches`/`interval_scan` 无条件调用,
  以前为一个初值就要扫完整条变更流 —— 现在只读前 64KB;
* `prepare(names)` 也先吃缓存(把多信号查询合成一次 NPI 扫描的收益保留), 扫完一起落盘;
* 三重校验(身份 key / 内容指纹 / 位宽)任一不符 → 视为未命中并重建; `WAL_CACHE=off`
  完全不读写; 写回阈值与 VCD 旁挂缓存一致(`WAL_CACHE_MIN_MB`, 默认 8MB);
  单列超过 `WAL_FSDB_COL_MAX`(默认 5000 万点)不落盘。

实测(客机 TCG 模拟, 比真机慢一个量级, 结论方向一致; 夹具 = 100 万时钟沿 / 200 万时间戳,
`make bench-fsdb FSDB=…`):

| 查询 | 修复前 冷 | 修复前 暖 | 修复后 冷 | 修复后 暖 |
|---|---|---|---|---|
| 只加载 `(length (SIGNALS))` | 3.4s | 3.5s | 3.4s | 3.2s |
| 沿计数 `(count (rising clk))` | 8.2s | 8.0s | 7.6s | **3.6s** |
| 电平计数(统一引擎) | 48.5s | 8.6s | **36.6s** | **4.1s** |

读法: 暖查询已经压到"只加载"的底线(3.2s = NPI 初始化 + open + 名字树);
冷查询的下降来自 t0 初值不再单独扫一遍 + 时间线分片一次性归并(见下)。

**顺带修掉的时间线冷建二次方拷贝**: `scan()` 每 4096 信号一块, 以前每块结束都把已累积的
时间线整份 `merge_sorted_unique` 一次 → O(块数 × 主表长)(188 万信号 = 459 块 × 上千万
时间点 = 几十 GB memcpy)。现在分片收齐后一次平衡归并。

回归闸: `tests/fsdb_diff.rs::fsdb_col_cache_hit_matches_cold`(需 Verdi)—— 同一查询在
"不用缓存 / 建缓存 / 命中缓存"三次运行下必须同答, 且 `.fcol` 确实落盘;
`src/trace/fsdb.rs::col_cache_tests` 覆盖编解码往返(4-state / 初值缺失 / 指纹失效 / 损坏)。

### 6.6 并行冷建: 单机多进程 + 集群(LSF)

冷建时间线是**唯一**还在"整文件过一遍"量级的操作(§6.5 之后查询侧已经压到"只加载"),
而它是纯并集 —— 天然可并行, 且**分片方式不影响结果**。三条路径:

**① 单机多进程**: `WAL_FSDB_TL_JOBS=N`(或 `auto` = min(核数, 8))。
每个 worker 是独立进程(一次 NPI 初始化 ~2s + 一个 Verdi 许可), 扫 `idx % N == k`
那批信号的变更时间, 父进程归并。

**② LSF(集群)**: `scripts/lsf_fsdb_prewarm.sh <file.fsdb> [shards] [--queue Q] [--wall HH:MM]`

```bash
# 在共享文件系统上(FSDB、二进制、--work、--cache-dir 都要所有节点可见)
./scripts/lsf_fsdb_prewarm.sh /shared/wave/design.fsdb 16 --queue normal --cache-dir /shared/wave/.wal-rust-cache
# 内部 = bsub -n 1 跑 N 个 `wal-rust fsdb-timeline-map design.fsdb $k N tl.$k.part`
#        → 等全部结束 → `wal-rust fsdb-timeline-merge design.fsdb tl.*.part`
```

之后普通查询把 `WAL_CACHE_DIR` 指向同一个共享目录即可直接命中。
没有 LSF 时 `--local` 等价于 ①;`--dry-run` 只打印将要提交的 bsub 命令。

**③ 手动两步**(批处理系统自己调度的场合):

```bash
wal-rust fsdb-timeline-map   design.fsdb 0 16 tl.0.part    # 每片一个 job
wal-rust fsdb-timeline-merge design.fsdb tl.*.part         # 归并 → .ftl
```

**正确性契约**(有闸盯着):

| 不变量 | 闸 |
|---|---|
| `.ftl` 与单进程产出的缓存**逐字节一致** | `timeline_map_reduce_matches_single_process_encoding`(还验了归并顺序无关) + 真机 `cmp` 实测 |
| 轮转分片无重叠、无遗漏 | `timeline_round_robin_partition_covers_all` |
| 分片失败/截断/空分片 → 报错, 不写半份缓存 | 同上单测 |
| `WAL_FSDB_TL_JOBS` 默认 1(不能偷偷并行吃许可) | `timeline_jobs_parsing_is_conservative` |

**实测**(200 万时间戳夹具, 客机 TCG 8 vCPU —— 比真机保守得多):

| 分片 | 冷建墙钟 |
|---|---|
| 1(默认) | 40.1s |
| 4 | 24.8s(1.6×) |
| 8 | 21.6s(1.9×) |
| LSF `--local 4` + reduce | 17s(参照单进程含取列共 40.7s) |

内网 174MB 样本(真机, 4 进程): 400.5s → **175.1s**。

⚠️ 两个**必然踩**的坑:
* **许可**: 每个 worker 各占一个 Verdi 许可。shards 要看许可池余量, 不确定先 4;
* **共享文件系统**: 集群 job 在别的节点上跑, FSDB / 二进制 / `--work` / `--cache-dir`
  必须都是共享路径, 否则 job 直接找不到文件。

另外: 计数用的"每个 worker 至少一个信号"判据曾经写成 `信号数 ≥ 2048`, 于是
"信号很少但时间戳几千万"的波形(一组计数器打满时间轴)永远不会并行 —— 现在放开了,
轮转分片对任何信号数都成立。

## 7 内网实测环境(2026-09-16)

| 项 | 值 |
|---|---|
| Verdi | **S-2021.09-SP2**(`/tools/synopsys/verdi/S-2021.09-SP2`) |
| NPI | `share/NPI/inc/npi_fsdb.h` + `share/NPI/lib/linux64/libNPI.so`(另有 gcc730/vcst 两份); `npi_fsdb*` 符号 168 个、`TimeBasedVcIter` 13 个 |
| `etc/verdi_version` | **不存在**(别让它当环境事实; 用 `bin/verdi -version`, 但 GUI 路径要 libpng12) |
| 许可 | **只在计算节点可达**(登录节点 27000/27400 全 DROP); 计算节点 27000@lic01/02 TCP 通 |
| FSDB 版本 | 5.9(reader 更新 → 只报 `*WARN* generated using a previous version`, 可读) |

## 8 已知限制 / 后续

* 首次按索引查询要扫一遍**全文件**(算时间线),大 FSDB 上是主要成本 —— **已有落盘缓存**
  (`.ftl`/`.fnames`, 0.14.3 起, 见 §6.1),冷建可用 `WAL_FSDB_TL_JOBS` 并行;
  更彻底的做法是 `npiFsdbSigdbCollector` + `npi_waveform_sigdb_open` 建列式索引库(未做)。
* `FSDB_BT_VCD_REAL` 之类的 analog/real 值走 `format=RealVal` 兜底,尚未用真实 analog
  样本验证。
* FSDB 版本比本机 reader 新时无法读取(实测 25A 写的 5.7,2018 的 reader 是 5.6 →
  `npi_fsdb_open` 直接失败),错误信息里已给出这个提示。

## 9 多文件语义与缓存一致性(2026-09-17 内网反馈三连)

### 9.1 谁回答: 加载顺序, 不是 HashMap 顺序(#32 的真凶之一)

`TraceContainer` 存 trace 用 `HashMap<TraceId, _>`, 而 Rust 的 `HashMap` 每个进程
的迭代顺序都不同 —— 于是**两条波形都含同名信号**时(内网场景正是"同一设计导出
FSDB + VCD"), 同一条命令会在两条波形之间**随机**挑一条:

```
$ wal-rust '(count (rising "top.clk"))' -l wal.fsdb -l big.vcd   # VM 实测 6 次
=> 40      # 命中 FSDB(短时间线)
=> 30000   # 命中 VCD
=> 30000  ...
```

结果、耗时都不可复现。修法: `TraceContainer` 增加 `order: Vec<TraceId>` 维护**加载
顺序**, 所有"要挑一条"的读取(`trace_ids`/`first_trace`/`traces_iter`/`prepare`/
`source_ids`/`indices`)都按它迭代 → **先 `-l` 的先算**, 后加载的同名波形不参与回答
(`traces_iter_mut` 这类批量写操作顺序无关, 保持原样)。

同时**禁止跨波形合并索引集合**: `find_indices` 原本把"能解析出该名字的所有波形"的
索引并起来(`decompose_and_count`/`find_indices_all_traces` 亦然)。索引是各自变更时间的
**排名**, 两条波形的索引空间不是一回事 —— 实测 `-l s1 -l s2`(同名信号)会答出 s2 的数字。
现在所有取值/取索引路径都统一为 **first-match 选源**(第一条 `-l` 里能解析出该名字的
波形), 与解释器逐拍一致。

### 9.2 索引空间: 只由"查询引用到的波形"决定(#32 的性能真凶)

时间优先后端(FSDB)的 `max_index` = 索引空间长度 = **全文件所有信号变更时间的并集**,
只能全文件扫描得到。而统一引擎原先对**所有已加载 trace** 取 `max_index` 的最大值 ——
于是 `-l fsdb -l vcd` 查 VCD 信号 = 先物化一遍 FSDB 全局时间线(内网 174MB 样本冷建
400s+), 用户看到 >900s; 只加载 VCD 则是秒级。

现在的规则(逐拍回退与统一引擎**一致**):

| 查询形态 | 索引空间 |
|---|---|
| 引用了某信号 | **实际会读值的那条波形**(first-match: 第一条 `-l` 里能解析出该名字的波形)的 `max_index` |
| 不引用信号(常量条件 / `INDEX`/`TS`-only) | **主波形**(第一条 `-l`) |

注意是"**会读值**"而不是"**能解析**": `-l vcd -l fsdb` 查一个两条都有的信号(同一设计
导出两份是常态)时, 值只从 VCD 读 —— 若把"能解析"的 FSDB 也算进索引空间, 就会为了一个
VCD 查询先扫一遍 FSDB 全局时间线(内网 >900s 的另一半原因)。

**没出现在查询里的波形不该改变查询的索引空间** —— 既省掉无关的全文件扫描, 语义上
也更可解释。逐拍扫描同时收口(`step_scan` 只推进参与集合; `INDEX`/`TS` 改为读"正在
被推进的那条 trace", 否则多文件时会读到原地不动的波形而恒为 0)。

### 9.3 缓存 key 必须含 `ctime` + `inode`(#8 陈旧命中)

缓存 key 原为 `basename + size + mtime`, 内容指纹只覆盖首尾 64KB。脚本反复生成同名
波形时"**同一秒内等长改写**"会让 size/mtime 都不变, 中段(>64KB 以外的)改写又躲过
指纹 → 热查询直接返回**旧列**(内网实测: 174MB 波形改 10 处后仍答旧值 40473)。

修法: key 增加 `ctime`(+纳秒)与 `inode` —— `ctime` 在内容或元数据任何改动时都会变,
且普通用户**改不回去**(改 `mtime` 容易, 改 `ctime` 不行); `inode` 挡住"删了重建"。
`.wcol`/`.cols`(VCD)与 `.fnames`/`.ftl`(FSDB)统一走 `trace::vcd::file_identity`。
首尾 64KB 指纹保留作第二道防线(`WALCOL02` 格式)。

### 9.4 缓存写不下的降级(#50)

`ulimit -f` / 磁盘满时, 写缓存超限会收到 **SIGXFSZ** —— 默认处置是直接杀进程
(`rc=153` + core), 用户看到的是"查个波形崩了"。现在:

* `main` 一开始就把 `SIGXFSZ` 设为 `SIG_IGN` → `write()` 返回 `EFBIG`, 进程继续;
* 缓存写失败**只提示一次**(`wal-rust: 缓存写入失败, 本次会话不再尝试写缓存…`),
  查询结果与退出码不受影响。

回归: `tests/regression_matrix.rs` 的 `matrix_cache_write_over_limit_degrades`
(用 python3/perl 垫一层把 `SIGXFSZ` 复位成 `SIG_DFL`, 否则父进程若已忽略它, 测试会
在"永远通过"的假象里)。

### 9.5 一致性诊断与性能回归工具

* `./scripts/fsdb_vcd_quickcheck.sh design.fsdb design.vcd [信号…]` —— 一分钟回答
  "同一设计导出的 FSDB 与 VCD 是不是同一份仿真": ① 信号集(数量/同名交集/各自独有)
  ② 索引空间长度 ③ 典型信号的边沿/电平/变更计数;逐项 ✅/❌, 不一致时给出
  "先确认导出选项/层次前缀"这类可操作结论。
* `./scripts/bench_name_resolution.sh [信号数] [时间戳数]` —— 名字解析/裸符号求值基准。
  188 万信号 FSDB 上"每次求值克隆整张名字表"曾经让 `get` 解析慢到分钟级;这条基准用来防它回归。

### 9.6 这三个修复对应的回归测试

| 测试 | 保证 |
|---|---|
| `matrix_multitrace_source_is_load_order` | 同一条命令跨进程可复现; 第一条 `-l` 优先 |
| `matrix_multitrace_index_space_follows_query` | 多加载无关(long)波形不改变结果(引擎与逐拍两条路都对拍) |
| `matrix_sidecar_stale_content_same_size_mtime` | 等长中段改写 + mtime 装回去 → 必须重算(去掉 ctime/inode 即失败) |
| `matrix_cache_write_over_limit_degrades` | `ulimit -f` 下 rc=0 + 正确结果 + 一次提示(不忽略 SIGXFSZ 即失败) |
