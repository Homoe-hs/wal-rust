# FSDB 读取:借 Verdi 的 NPI,纯 Rust FFI

> 一句话:**FSDB 我们不猜格式,直接调用 Synopsys 正版 Verdi 的 NPI 读库**(`libNPI.so`);
> 而调用方式是 `dlopen` + `dlsym` 手写 FFI —— **不需要 C++ 编译器,也不需要 fsdb2vcd 转换**。
> 没有 Verdi 的机器上,FSDB 依旧是"明确报错 + 提示",VCD/FST 通路完全不受影响。

## 1 为什么不是别的路

| 路线 | 结论 |
|---|---|
| **自己逆向 FSDB 二进制** | 不做。写者(FSDB 版本)一直在变,单变量差分成本极高,收益不确定 |
| **fsdb2vcd 转换后再读** | 不做(用户明确否掉):多一次落盘、丢掉 FSDB 独有信息、还要外部工具 |
| **FFR(`libnffr.so` + C++ 垫片)** | 做过并跑通(见 `src/trace/fsdb_shim/`),但要 C++ 编译链;产品通路改走 NPI |
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

**实践建议**(与内网实测结论一致): FSDB 后端适合**中小波形、边沿/时间类查询、
一次性全扫**; 大波形的高频随机 `find`/电平查询仍建议用 VCD/FST。
下一步可做的是把时间线落盘缓存(对齐 VCD 的旁挂列缓存), 让"需要索引空间"的
查询在第二次以后也变成毫秒级。

## 7 内网实测环境(2026-09-16)

| 项 | 值 |
|---|---|
| Verdi | **S-2021.09-SP2**(`/tools/synopsys/verdi/S-2021.09-SP2`) |
| NPI | `share/NPI/inc/npi_fsdb.h` + `share/NPI/lib/linux64/libNPI.so`(另有 gcc730/vcst 两份); `npi_fsdb*` 符号 168 个、`TimeBasedVcIter` 13 个 |
| `etc/verdi_version` | **不存在**(别让它当环境事实; 用 `bin/verdi -version`, 但 GUI 路径要 libpng12) |
| 许可 | **只在计算节点可达**(登录节点 27000/27400 全 DROP); 计算节点 27000@lic01/02 TCP 通 |
| FSDB 版本 | 5.9(reader 更新 → 只报 `*WARN* generated using a previous version`, 可读) |

## 8 已知限制 / 后续

* 首次按索引查询要扫一遍**全文件**(算时间线),大 FSDB 上是主要成本;后续可落盘缓存
  (对齐 VCD 的旁挂列缓存),或用 `npiFsdbSigdbCollector` + `npi_waveform_sigdb_open`
  建列式索引库。
* `FSDB_BT_VCD_REAL` 之类的 analog/real 值走 `format=RealVal` 兜底,尚未用真实 analog
  样本验证。
* FSDB 版本比本机 reader 新时无法读取(实测 25A 写的 5.7,2018 的 reader 是 5.6 →
  `npi_fsdb_open` 直接失败),错误信息里已给出这个提示。
