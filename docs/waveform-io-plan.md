# 波形加载/解析效率优化专项(数据库领域方案调研 + 落地计划)

> 关联: `docs/query-engine-design.md`(查询侧统一引擎)。
> 目标: 150GB VCD 加载 ≤ 2-3 分钟(现 58.7GB 加载 360s,线性外推 150GB ≈ 900s),
> 单信号查询 ≤ 60s,解析吞吐 ≥ 2GB/s(现 0.5-0.6GB/s 有效),RSS ≤ 3GB。

## 0. 参考(数据库/文本解析领域先进做法)

- **SIMD 结构化索引**(simdjson stage1 / hyper-scan CSV):先用向量化分类字节
  (引号/分隔符/换行)建立"行/字段边界表",再批量做逐字段语义解析;比逐行
  字节判定快 5-15x — 对应我们 PASS-1b 的"边扫边解"。
  - [simdjson core concepts](https://deepwiki.com/abab2025/simdjson_simdjson_master_d140bc2/3-core-concepts)
- **批量列装配 + 分区归并**(Arrow/DuckDB ingest 的"chunk-local tables + 按分区键
  排序/radix 分区,再 stream-merge"):把 PASS-1b 的 per-signal `BTreeMap`
  (每信号 O(log) 散插、3.5M 信号 × 数十条变更)换成 **分块局部表
  (id, ts_idx, value) → 按 id 桶化排序 → 跨块归并**,单遍、有界内存、可并行。
- **Row-group / zone-map 统计下推**:每 64MB 块记录 per-signal (min_ts, max_ts,
  状态集);查询按列裁剪块(Predicate pushdown)。我们现有 per-signal sparse
  index 已近此——改为**块级 zone map + 列级按需重建**,免去 3.5M 个 BTreeMap。
- **列式紧凑编码**:45 位值打包 7B、x/z 按 2bit/state、事件型只存索引
  (wellen FST 已是此形态)。VCD 文本解析后立即打包,内存 ×10+ 下降;
  change_indices 用 delta 编码(offset: 变长 int)。
- **xevdb**(相邻项目):VCD → 单文件 SQLite,值变更存表,查询走 SQL;验证了
  "波形物化 + 关系查询"路线,但逐行 INSERT 路线吞吐有限,SQLite 不适配 150GB
  级稀疏列 — 我们保留内存列 + 单遍扫描,不做 SQL 物化。
  - [xevdb README](https://github.com/aionhw/xevdb)
- **FastWaveBackend**(UCR Yehowshua,波形数据库后端研究)为同类"后端查询引擎"
  项目,站点被 Cloudflare 屏蔽,未能取到细节;若后续需要可邮件索取
  (git.icanbuildit.io/Yehowshua/FastWaveBackend)。

## 1. 现状瓶颈(实测基线)

| 环节 | 现状 | 瓶颈 |
|---|---|---|
| PASS-1b 解析 | 并行分块逐行 memchr+\# 定位,每行做 id 尾匹配 | 每字节 ~0.5 次分支;行数 = 变更点行数(百万级) |
| 稀疏索引 | per-signal `BTreeMap<ts, offset>` 1/100 采样 | 3.5M 信号 → 3.5M 个小 map,堆分配/指针跟随 |
| 时间戳 | `Timestamps`(Uniform 压缩检测) + strided offsets | 已压缩,OK |
| 值读取 | 按块 memchr 跳扫 + find_signal_in_block(找 id 行) | 每次 get 重访 mmap 页 |
| 内存 | mmap 页驻留(load 后 madvise DONTNEED;查询再触摸) | RSS 11GB @58.7GB |
| 加载 | 360s @58.7GB | 解析 + 每信号 BTreeMap 构建 |

## 2. 方案:分块列装配 + 分区归并(核心改造)

PASS-1b 重写为 **5 段管线**(仍单遍文件、并行分块):

```
[块扫描] 每 64MB 块(rayon):
  a. SIMD 找 '\n' → 行边界表(结构索引,hyper-scan 式)
  b. 每行: '#' 时间戳累进;值行 → (id_hash, ts_idx, value) 三元组
  c. 块局部表: 按 id_hash 桶化(预分配 Vec,哈希分桶) + 每桶排序(ts 增量)
  d. 块统计: per-id (min_ts, max_ts, states_mask) → zone map
[归并] 各块局部表按 (id, ts) 流式归并(多路 k-way,同块内已有序)
  → per-signal Column { change_indices(delta 编码), values(紧凑打包) }
[构建] sparse_index: 不再 per-signal BTreeMap——
  · 块级 zone map: Vec<(block_id, min_ts, max_ts, sig_mask bitvec)> 查询先剪块
  · 列级 change_indices 本身即索引(binary search 定位拍)
[内存] madvise: 每块扫完立即 DONTNEED(分块而非文件尾一次),RSS ≤ 堆大小
```

对应收益:
- 解析: 行边界表把"逐行判定"变 bulk;数值解码(45 位字符串→u64)用查表/SWAR,
  预计 1.5-3x;
- 索引: 3.5M 小 BTreeMap → 数百个块局部表,归并 O(N log K),加载显著下降;
- 查询: zone map 剪块 + 列 binary search;get 不再重扫文件(列缓存);
- RSS: 分块 madvise + 紧凑打包,目标 1-3GB。

## 3. 分阶段落地

| 阶段 | 内容 | 验收 |
|---|---|---|
| IO-1 | PASS-1b 行边界 SIMD 化(只改行分裂,不解值);值行 id 尾匹配查表 | 解析吞吐 ≥1GB/s;门禁 ALL MATCH |
| IO-2 | 稀疏索引 BTreeMap → 块局部表 + 归并(M 保留现有读取逻辑,只换构建) | 加载时间 -40~60%;行为不变 |
| IO-3 | 值紧凑打包(delta 索引 + 2bit x/z) + 列缓存替换 LRU 重扫 | get 同信号二次查询 <0.1s;RSS 下降 |
| IO-4 | 分块 madvise 细粒度化 | RSS ≤3GB @150GB |
| IO-5 | zone map 剪块 + predicate 早退(is-x 列状态集) | 稀疏信号查询再 3-10x |
| IO-6 | 与查询引擎(P1-P6)合流:Column 直喂 IntervalSweep | 单套 IO 路径 |

每阶段独立发布(0.11.x 补丁线),diff gate + 一致性矩阵兜底;IO-1/IO-2 先行
(纯性能、零语义变化)。

## 4. 风险

- **行为等价**:IO-1/IO-2 只影响性能路径,必须与旧稀疏索引逐查询对拍
  (WAL_DEBUG_FIND + diff gate 全 fixture)。
- **内存峰值**:块局部表 = O(块内变更行数 × 16B),64MB 块约 50 万行 → ~10MB/块,
  并行 64 块 ≈ 640MB,可控;可调 block_size。
- **id 哈希碰撞**:块局部表按 hash 桶化,桶内做字节比对(与现 find_indices 一致)。
- **VCD 方言**:Icarus 1-bit `#%` 行、$dumpvars、`r` 实数行 — 行分类表需覆盖,
  用现有语料(测试集 + 152GB dump 采样)回归。

## 7. 追加调研:冷查询(磁盘重读)的治本方案(2026-09-07)

现状量化: 58.7GB 每新信号首次扫描 ≈ 225s(冷盘)/ 82s(缓存热);CPU <5s,
墙钟 = 磁盘重读 ×2(锚定扫描的 '#' 位置遍 + id 遍各扫一遍文件)。
数据库侧对应方案(已核):

| 方案 | 机制 | 代价 | 期望 |
|---|---|---|---|
| A. 双遍合一 | 单次遍历同时产出 '#' 位置与 id 命中(段内 memmem),磁盘读 ×1 | 小改;现有锚定扫描重排 | 冷查询 225s→~110s |
| B. 预算化内存列缓存(推荐) | PASS-1b 反正全量解析,顺手打包 per-signal 变更列(delta 索引 + 打包值);按预算(如 min(24GB, 20%RAM))截停;缓存命中信号 O(C) 零文件读 | 加载期打包 CPU 少(已解析);预算内 RSS | 脚本内多数查询 ms;未缓存信号维持现状扫描 |
| C. 列式 sidecar 文件 | 加载期写 `<wave>.walcol`(Arrow 风格: per-signal RLE + zone map + mmap 惰性读);查询只 mmap 目标列二分 | 一次写盘(≈加载时间);磁盘 +10-15%;新文件格式 | 任意查询毫秒级;150GB ≤60s 可达(但接近"转换",与"不做 convert"原则有张力) |
| D. FST 优先工作流 | vcd2fst(外部)已零扫描;FST 查询本已毫秒级 | 无代码;依赖外部转换 | 内网 152GB 已走此路 |

参考: DuckDB [Sorting on Insert / Row-Group 统计下推](https://duckdb.org.cn/2025/05/14/sorting-for-fast-selective-queries)、
[Row-Group 存储](https://github.com/duckdb/duckdb/pull/1808) — 稀疏列 + 块级 min/max 裁剪;
simdjson [stage1 结构索引/指针](https://deepwiki.com/abab2025/simdjson_simdjson_master_74bb7b2/3.1-two-stage-parsing-architecture)(A 的参照);
Arrow/Parquet RLE+delta + [Lance 关于 Arrow 多 buffer 编码的批评](https://arxiv.org/pdf/2504.15247)(C 的设计注意点)。

建议顺序: A(1-2h,收益 1.5x)→ B(半天,脚本场景体验质变)→ C(如需 150GB≤60s 硬指标)。

## 8. 跨进程索引复用: CWD 缓存文件方案(待讨论)

内测诉求: `sigs/count/topsig` 每次调用全量重扫(17.5GB/42M 信号 ≈ 2m40s-3min/次)。
现有列缓存是**进程内**的;跨进程复用需要落盘一份**可再生的缓存**(不是格式转换)。

**定位**: 缓存 ≠ 转换。缓存是可丢弃、可重建、带版本与身份校验的中间产物;
不改变"VCD 是唯一真值来源"的原则,也不引入第二种波形格式。

**方案**:
- 位置: 默认 **执行命令的当前目录** `./.wal-rust-cache/`(相对 CWD,可用 `WAL_CACHE_DIR` 覆盖)。
  **绝不写在波形所在目录**——波形可能来自只读挂载/共享盘;缓存目录不可写时静默跳过,
  查询结果不受影响。
- 命名: `<wave_basename>.<size>.<mtime>.<format_version>.wcol`。
- 内容(顺序读一次即得,无需解析):
  1. 头部: magic + 版本 + 波形 (size, mtime, 首尾 1MB 指纹);
  2. 信号元数据: 名池(长度前缀)+ id + 宽度 + 初值;
  3. 稀疏锚点(每信号 (ts,offset) 序列);
  4. 变更列: 每信号 (ts_idx 增量编码 + 2bit/位状态,宽度≤64) —— 即现有内存列缓存序列化;
     超预算信号写"偏移索引"(ts_idx + 文件偏移),值仍从 VCD 按需读。
- 加载: 命中且校验通过 → 直接 mmap/顺序读缓存(约为 VCD 的 10-15% 字节)→ 查询毫秒级;
  未命中/失效 → 按现状构建,若 `WAL_CACHE=build`(或 auto 且命中失败)则写回。
- 失效: (size, mtime, 指纹) 任一变化或格式版本不符 → 视为未命中并重建。
- 开关: `WAL_CACHE=off|auto|build|read`(默认 auto: 命中读;**未命中且波形 ≥8MB 时写回**,
  小文件不写以免污染目录;`build` 显式构建,始终写;`read` 只读不建)。
- 目录污染防护(0.12.28): auto 模式设 8MB 阈值;缓存目录不可写时静默跳过(查询结果不受影响)。
- 风险: 磁盘占用(~10-15%);CWD 污染(隐藏目录 + 文档说明);mtime 粒度;多进程并发写(临时文件 + 原子 rename)。

**收益预估**: 首次构建仍 ~2-3 分钟(17.5GB 级);之后 `sigs/count/topsig` 从 2m40s-3min/次
降到秒级-亚秒级(仅读缓存),42M 信号元数据也直接从缓存反序列化。
