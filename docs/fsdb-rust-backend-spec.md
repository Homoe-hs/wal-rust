# 纯 Rust 读 FSDB 全局时间线:交付范围与验收标准

> 🟡 **计划(未实现)**: 本文定义"用纯 Rust 取代 NPI 建全局时间线"这件事**做到什么程度算完成**,
> 以及可以逐条执行的验收门。**当前 G0 是红的**(见 §1 实测),所以现在还不能用,别按"现成能力"宣传。

## 0 为什么值得做

FSDB 侧唯一还是百秒级的操作就是**冷建全局时间线**(NPI 的 `npiFsdbTimeBasedVcIter` 约
**1.45µs/变更记录**): 真实波形(336MB / 1796 万信号 / 4377 万时间戳)上 8 路并行 **636s**、
单 walker 外推 **~2700s**;而其余查询/加载都已在 10~17s / 6.2GB 一档(见
[`../bench/RESULTS.md`](../bench/RESULTS.md) 的性能快照)。绕开 NPI 直接读文件里的时刻数据,
是这个量级差唯一可能的来源。

## 1 现状(2026-09-28 实测, 别重复踩)

| 事实 | 证据 |
|---|---|
| `fsdb-parser`(本地 `/home/hesheng/Projects/fsdb-parser`,700 轮逆向)已有 `FsdbFile::time_points()`("所有信号变化时刻并集,升序去重")、`fe` 时间表块解析、以及一份**现成的 wal-rust 集成补丁集**(wal-integration 目录:纯 Rust 版 FsdbTrace + 集成测试 + patched 三处改动,其 README 4 步可应用) | 读源码 + wal-integration 的 README;补丁集当时只被"wal-rust 挂载只读"挡住 |
| **但这个写者族还没吃下来**:`open_meta()` 读真文件 0.54s 返回 `Ok`,却只有 **60 个名字**、`max_time=0`、tail 索引为空(NPI 看到 17,956,098 个信号) | `.tools/fsp-probe`(本轮新建的探针) |
| VC 区是**大量小块 zlib**:全文件候选流头 `78 01/9c/da` ≥13 万个(26,356 / 98,644 / 4,834);文件头偏移 8 起 `04 03 02 01`(VCS 魔数);Python 顺序扫完 336MB 仅 0.4s | 同轮扫描;格式笔记见 fsdb-parser 仓库的 docs/format-notes.md(现代写者 VC 区 zlib) |
| `fsdb-parser` 已知缺口都在**名字/值**那条线(名字覆盖 3063/3378、值侧金标准 24/705、V-2023.12 gate 0%、">15MB 全解析受 2022 网表名流依赖") | 其 SUMMARY-r18 与逆向案例附录 |

**关键判断**: 全局时间**不需要**名字与值,所以"timeline-only"可以绕开上述缺口 ——
这是这个目标可行的根据,也是本方案刻意**不**做完整后端的原因。

## 2 交付物形态(不是 DSO 插件)

wal-rust 自带的后端模块 + 运行期开关,NPI 保留为 oracle/兜底:

```
src/fsdb_rust/           新增:纯 Rust 后端(mmap + 解压 + 记录解码)
  ├── header.rs          头部/族判别(VCS/Verdi/老库 + 写者版本)
  ├── index.rs           尾部索引 / GMAP / 时间表块
  ├── vc_scan.rs         ★ timeline-only 扫描(只取时刻,不解值、不映射名字)
  └── equiv.rs           与 NPI 的等价比对(供门脚本调用)
```

对外**只有三个接口**(第一版不扩散):

```rust
pub fn timeline(path: &Path) -> Result<Vec<u64>, FsdbErr>;  // 全部变化时刻并集(升序去重)
pub fn max_time(path: &Path) -> Result<u64, FsdbErr>;       // 结构自证用
pub fn timescale_exp(path: &Path) -> Result<i32, FsdbErr>;  // 1ps → -12
```

开关:`WAL_FSDB_BACKEND=npi|rust|auto`(CLI `--fsdb-backend` 别名)。**默认先 `npi`**;
`auto` 的语义是"rust 成功且自证通过就用它,否则回落 NPI 并打一行告警",绝不猜着用。

## 3 里程碑

| 步 | 做什么 | 产出 |
|---|---|---|
| **M0 侦察+结构自证** | 吃下 `VCS X-2025.06-SP2_Full64` 的头部/尾部索引/VC 块清单;先用免许可的 `fsdbdebug -vc/-allvc` 做第三独立 oracle | 头部/索引两个新模块;自证:写者串、timescale、`max_time` 全对 |
| **M1 timeline-only 扫描** | M0 若发现"时刻表是现成的"→直接读(秒级);否则 mmap + zlib inflate + 逐记录 delta 解码,只累积并集 | `fsdb_rust::timeline()`,与 NPI 逐点相同 |
| **M2 门与回归** | 等价比对脚本 + 集成测试 + 写进 `bench/perf-history.csv` | PASS/FAIL 表 + 族级覆盖台账 |
| **M3(可选)** | 每信号变更列(替 `.fcol`)、值解码、名字表 | 会撞上 §1 的已知缺口,**第一版不做** |

## 4 验收门(G1/G2 是硬闸,任一红不许切默认)

锚点值全部是**这台机器上已经量到的**,可以逐条复核。

### G0 结构自证(前提门)
写者串 == `Chronologic Simulation VCS Release X-2025.06-SP2_Full64`;`timescale_exp == -12`;
`max_time == 43,776,606`(ps);第二阶段再加 `signal_count == 17,956,098`、`scope_count == 1,620,912`。
**现状:红**(今天只给出 60 个名字、`max_time=0`)—— 这就是 M0 的靶子。

### G1 等价(硬闸)
* A: `fsdb_rust::timeline(real)` 与 NPI 并集**逐点相同**,点数 **320,204**。
* B(更强): 走 rust 后端生成的 `.ftl` 与 NPI 的 **md5 相同** =
  `ca1cb095c9a46800c3a014c06e60a959`(460,014 B)。该 md5 已用 4 种 NPI 分片口径验证是确定性的。
* C: 400 万信号夹具(`.tools/bench/v2f/many4m.fsdb`)点数 == **200**。
* D: 族级语料(VCS `O-2018.09-SP2` 与 `X-2025.06-SP1/SP2`、`1ps/10ps/1ns`、含 x/z、含 glitch、
  单/多 scope,用 VM 生成)逐件相等;**允许"该族不支持 → 显式 Err + 回落 NPI",不允许输出错的时间线**。

### G2 无 NPI / 无许可(硬闸)
`env -u VERDI_HOME -u WAL_NPI_LIB -u SNPSLMD_LICENSE_FILE -u LM_LICENSE_FILE` + **许可 VM 停机**
后仍得到 G1 的 md5;`strace -f -e trace=openat` 不出现 `libNPI.so`;
`src/fsdb_rust/` 源码 grep 不到 `npi`/`dlopen`。

### G3 速度
| 场景 | 现状(NPI) | 硬门槛 | 目标 |
|---|---|---|---|
| 真文件全局时间线, 冷页缓存, 单进程 | 636s(8 路)/ ~2700s(单 walker) | **≤30s** | **≤10s** |
| 同上, 暖页缓存 | — | ≤5s | ≤2s |
| 4M 夹具 | 15.65s(冷 level) | ≤2s | ≤1s |
| M0 若发现现成时刻表 | — | ≤1s | ≤0.3s |

* 参考下界:顺序读 336MB 实测 **0.4s**;NPI 的 1.45µs/记录是当前基线,目标按"每记录 20~50ns"推算(50~100×)。
* 数字必须进 `bench/perf-history.csv`(`op=fsdb-rust-*`)。

### G4 内存
真文件时间线峰值 RSS **≤2GB**(NPI 冷建 ~10GB 遍历 / 13.0GB 冷加载)。
实现约束:mmap + 流式,不许 `fs::read` 整文件、不许建 1796 万条名字/句柄结构。

### G5 反静默(比速度更重要)
* A: 任何未识别块/版本/压缩类型 → **`Err`**;`auto` 下回落 NPI 并告警。
* B: 随机字节翻转 fuzz ≥1000 次,只允许两种结果:**与金标准相同** 或 **`Err`**;出现第三种即 FAIL。
* C("未测到 ≠ 0"): 时间线为空必须报错;解析耗时/记录数/最大时间要打出来供核对。
* D: 确定性自证 —— 同一文件跑两遍逐字节相同。

### G6 覆盖台账 + 族级红线
按**写者族**分行:Verdi(vfast)/ VCS-2018 / **VCS-2025 Full64** / 老库 2009(单列,不计入总分);
每族记文件数、可比对点数,**逐文件只许升不许降**;并写明每道门覆盖了哪些文件。

### G7 默认切换条件
G1–G5 在全部语料(≥1 真文件 + ≥6 受控样本)绿,且**连续两个版本**无回归,才把默认从 `npi` 切 `auto`;
否则一直是 opt-in,随时可退。

## 5 第一版明确不做

值解码(real/x/z 位宽明细)、名字表语义、`scopes()` 树、替换 `.fcol`、写 FSDB、
多文件索引空间语义变更、动态库/ABI 稳定。这些留给 M3。

## 6 待定决策(需要人拍板)

1. **复用 `fsdb-parser`(path 依赖)还是自研**:建议复用(它已有数百测试与 700 轮格式知识),
   wal-rust 只吃 §2 的三个接口。
2. **许可证**:`fsdb-parser` 仓库根**没有** LICENSE 文件,而 wal-rust 是 MIT/Apache-2.0;
   合成一个二进制前必须先定这件事。
3. **要不要 Phase 2**(M3):只做时间线(冷建提速),还是连每信号变更列一起(查询侧也彻底脱开 NPI)。

## 7 现有装置(可直接用)

| 装置 | 用途 |
|---|---|
| `.tools/fsp-probe/`(本地,gitignored) | `open_meta` 探针:一键看纯 Rust 侧认出多少名字 / `max_time` |
| fsdb-parser 仓库的 wal-integration 目录 | 现成的 wal-rust 集成补丁集(4 步可应用,2026-08-29 核过零漂移) |
| `scripts/fsdb_env_check.sh <file>` | 写者 / reader / 许可三段体检 |
| `fsdbdebug -tree/-vc/-allvc` | **免许可**的第三方金标准(名字/值/时刻) |
| `bench/perf-history.csv` | 回归库(FSDB 行口径见 [`../bench/README.md`](../bench/README.md)) |
