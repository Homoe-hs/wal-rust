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
   实测 `verilog.fsdb ↔ verilog.vcd`:**179 个信号 × 406 个索引全等**。
3. **对 Verdi 复核**:宿主 `fsdbdebug -vc -vidcode N` 的输出与我们的读数一致
   (例如 t=0 条目 `xtag:(0 0) val:0`)。

### 两类**已解释**的差异(不是解析错)

* **位炸开向量**:VCS 写 VCD 时把某些总线拆成 `CH [4] … CH [0]` 五个单 bit `$var`,
  而 NPI 把它们归成一个 `CH [4:0]` 的 5 bit 信号。差分门单独统计并跳过(必须打印出来)。
* **t=0 采样时机**:VCD 的 `$dumpvars` 抓的是**仿真开始前**的快照(连续赋值还没结算 → x),
  FSDB 记的是 **t=0 delta 之后**的值。于是 `i_ALUB.clock` 这类信号 VCD 侧初值是 x、
  FSDB 侧是 0,`(count (rising …))` 会差 1(146 vs 147)。**FSDB 侧与 Verdi 一致**,
  所以这是两代写者的差异,不是我们读错;差分门只允许"VCD x → FSDB 确定值"这一个方向。

## 6 已知限制 / 后续

* 首次按索引查询要扫一遍**全文件**(算时间线),大 FSDB 上是主要成本;后续可落盘缓存
  (对齐 VCD 的旁挂列缓存),或用 `npiFsdbSigdbCollector` + `npi_waveform_sigdb_open`
  建列式索引库。
* `FSDB_BT_VCD_REAL` 之类的 analog/real 值走 `format=RealVal` 兜底,尚未用真实 analog
  样本验证。
* FSDB 版本比本机 reader 新时无法读取(实测 25A 写的 5.7,2018 的 reader 是 5.6 →
  `npi_fsdb_open` 直接失败),错误信息里已给出这个提示。
