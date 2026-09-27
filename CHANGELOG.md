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
- **波形指标与统计算子(延迟/吞吐/分布)**: `median` / `percentile` / `stddev` / `stats` /
  `histogram`(纯数值列表)与 `latency` / `ipc`(波形侧配对)。
  `(stats (latency (rising "req") (rising "ack")))` 这种组合就是"握手延迟分布"的完整答案。
  语义写死在 `docs/waveform-metrics.md`:latency 用**原生时间单位**、默认 **FIFO 配对**
  (`#restart` 让新 start 作废未配对的旧 start)、配不上的两端丢弃、边沿口径与查询引擎一致
  (x→1 是 `changes` 不是 `rising`);两个波形算子只在**时间域**取变更列, **不建索引空间** ——
  千万时间戳的大波形上不会触发"全文件物化时间线"。闸:`tests/metrics_test.rs`(手算期望值)。
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
- **`-j N|auto` / `--jobs`: 一个波形的并行度**(默认 `auto`, 不需要 LSF 也能用 —— 单机就是多核)。
  默认 = min(可用额度, 8), 额度优先取 `bsub -n` 分配的 slot 数, 否则核数;
  **小波形自动退回单进程**(`WAL_FSDB_TL_MIN_MB` 默认 32MB, 或信号数 ≥ 16384), 因为每个
  worker 各有一次 NPI 初始化开销;`-j 1` 强制单进程, 显式数字一律照办。VCD 侧同步按该数建
  线程池。实测(200 万时间戳夹具, 冷缓存): 默认(小文件)39.1s / `-j 8` **21.0s**。
- `scripts/bench_fsdb.sh`(`make bench-fsdb FSDB=x.fsdb [SIG=tb.clk]`): FSDB 查询基准,
  冷建/暖查询/只加载分开计时并追加到 `bench/perf-history.csv`; CI `perf` 阶段在设了
  `WAL_FSDB_BENCH=<file.fsdb>` 时自动跑(判据: 暖查询应接近"只加载")。

### Changed
- **大波形的并行冷建不再需要人工配环境变量**: 波形文件 ≥ `WAL_FSDB_WALK_SCAN_MB`(默认 128MB)时,
  每个 `fsdb-timeline-map` worker **自动**启用"边走边扫"(`WAL_FSDB_WALK_SCAN=1`), 并在多分片下
  **自动选切分层**(`WAL_FSDB_SCOPE_SPLIT` 留空/`auto`): 在"子树数 ≥ 分片数"的层里取**每 worker
  认领的子树数最接近 256**(`WAL_FSDB_SCOPE_TARGET_PER_WORKER`)的那层。于是直接
  `make fsdb-prewarm FSDB=big.fsdb SHARDS=8 LOCAL=1` 就是内存安全且接近最优的那条路 ——
  以前必须自己 `export WAL_FSDB_WALK_SCAN=1 WAL_FSDB_SCOPE_SPLIT=5`, 忘了就会让 8 个 worker
  各走整棵树。小文件行为不变;想回到旧口径: `WAL_FSDB_WALK_SCAN=0`、
  `WAL_FSDB_SCOPE_SPLIT=0|off`, 显式数字仍然照办(想固定层做 A/B 就用它)。
  * 甜点 256 是真实波形上量出来的(321MB / 1796 万信号, 同机同天, 端到端
    `make fsdb-prewarm SHARDS=8 LOCAL=1`): depth4(42 棵/worker) **668s**、
    depth5(265 棵) **636s**、depth8(4595 棵) **760s**。depth8 的信号数其实是三档里
    **最均衡**的(每片恰好 ≈12.3%, 即"完美平衡"), 但每 worker 要认领 4595 棵树 ——
    真正决定耗时的是**变更密度**(时钟类信号每条 13 万次变化, 逻辑信号常常几次),
    均匀切信号数不等于均匀切工作量。所以选层用"目标棵数"而不是"按信号数算均衡度"。
  * 统计"每层多少棵子树、每棵多大"要一次只数数的树遍历(这份 1796 万信号波形 ~44s,
    NPI 驻留 ~10GB): **8 路并发走这一遍会 OOM** —— 首版实测 8 个 worker 同时统计时 NPI 直接
    `SIGSEGV`(`[fhdb][fatal] Can not get user data` 刷屏)。现在用 **flock 串行化**:
    第一个 worker 走一遍并把结果写进 `<cache>/<file_identity>-v1.fscope`
    (魔术字 `WSCP2` + 每层子树数与每棵子树大小, 原子 rename), 其余 worker 在锁上等 ~44s 后
    直接命中, 之后的每次运行 0 代价。缓存目录不可写时**不猜也不走树**, 直接按最深一层分片
    (子树最小 = 每 worker 最省内存; 而"不切分"意味着每个 worker 走整棵树)。
    自动选中的层 `WAL_DEBUG_FSDB=1` 会打印。闸: 单测比对选层规则、缓存往返(含文件身份失效)。
  * **归并产物与分片口径无关**: depth4 / depth5 / depth8 三条口径的 `.ftl` 都是
    `md5 ca1cb095…`(320,204 个时间点, 460,014 B) —— 自动选层换了口径也不会动产物。

### Performance
- **大波形时间线: 按 scope 子树的多进程并行(以及一个靠"三种分片对拍"抓到的真 bug)**。
  `WAL_FSDB_SCOPE_SPLIT=<depth>` 让每个 worker 只走自己那几棵子树 —— NPI 遍历期内存随子树
  下降(实测每 worker **2.8~3.9GB**, 而单 walker 是 ~12GB), 于是 8 路并行在这台 27GB 机器上
  可行。选层靠一次标定遍历(`WAL_FSDB_SCOPE_STATS=<depth>`): 这份 core 级设计 depth1/2 都只有
  1 个子树(切了等于没切), **depth3 = 7 棵**, depth4 = 334 棵(最大 13.7%)。
  * ⚠️ **抓到的 bug**: 用了 scope 分片后我仍然按"信号下标 % N"过滤信号, 两套分片口径叠加
    导致每个 worker 丢掉自己子树里 7/8 的信号 —— 表现是"三条分片合并出来的时间线并集少了
    43 个时间点"。是"不同分片口径必须产出同一并集"这条对拍(而不是单测)抓到的, 已修:
    scope 分片时信号级轮转自动关闭。
  * 实测(1796 万信号真波形): **depth5/8 片 494s**、depth4/8 片 570s(单 walker 外推 ~2700s),
    每 worker 4~8GB; 合并后 `.ftl` 装好, **暖查询 11.4s / 6.35GB**(之前 20 分钟跑不完的那条)。
  * 正确性: depth3/7、depth4/8、depth5/8 **三种独立分片口径的并集逐点相同(320,204 个时间点)**。
- **大波形的时间线冷建换路: "边走边扫"**。常规路径在这类波形上要么先按名字把句柄一个个
  解析出来(实测 **550µs/个** → 18M 信号 ≈ 2.75 小时), 要么把 18M 句柄全攒着(NPI 遍历期
  自己涨到 ~10GB)。新路径"走到一批信号就扫一批、扫完丢句柄"(`timeline_by_walk_split`):
  不建名字表、不攒句柄数组, 峰值内存与信号总数解耦。
  * 信号数 ≥ `WAL_FSDB_WALK_SCAN_MIN`(默认 2,000,000)自动启用, `WAL_FSDB_WALK_SCAN=0` 关闭;
  * **产物与常规 worker 逐字节相同**(cal1m 上 `cmp` 验证);
  * 1796 万信号真波形实测: **1/8 分片(约 224 万信号)369s / 11.9GB**, 产出 318,809 个时间点
    —— 单 walker 全量按此线性外推 **~45 分钟**(每变更记录 ~1.45µs 是 NPI 的硬成本, 换路只
    免掉了"按名字解析 18M 次句柄"的那两小时, 并没有免掉逐条扫描);
  * 另支持按 **scope 子树**分片(`WAL_FSDB_SCOPE_SPLIT=<depth>`)以便多进程并行时内存随
    子树大小下降; 这份设计只有一个顶层 scope 切不动, 留给别的设计。
- **真波形的时间线冷建: 代价量化 + "只取句柄"的 worker**。在 1796 万信号 / 4377 万时间戳的
  core 级波形上把"整文件过一遍"拆开量: `iter.start()` 装载 ≈0.2µs/条、`iter.next()` 逐条
  ≈1.25µs/条 → **≈1.45µs / 变更记录**(代价跟"记录条数"走, 不跟信号数走 —— 时钟类信号
  独占大头, 单个 adapter clk 就 130,590 次变化)。块大小有甜点(65536 最好, 4096 慢 35%,
  262144 更慢), `iter_next(time&)` 单参数重载**不提速**(开销在 NPI 内部逐条处理)。
  **暖路径的句柄解析是致命项**: `.fnames` 命中时句柄按名字懒解析, 实测 **550µs/信号**
  (18M 信号 ≈ 2.75 小时) → 新增 `WAL_FSDB_HANDLES_ONLY=1`(时间线 worker 默认开):
  跳过名字 arena/索引/scope, 走树只留句柄, `handle_of` 归零, 代价变成一次树遍历(~41s)。
  `WAL_DEBUG_FSDB=1` 多了"时间线扫描剖析"一行(`handle_of / add / start / next×N / 时间点`),
  下一轮的优化目标(按 scope 边走边扫 / 直接读 FSDB 全局时间表)在 `bench/RESULTS.md` 里记着。
- **真实 core 级波形(321MB / 1796 万信号 / 162 万 scope): 加载内存砍半、名字表不再物化**。
  实测(本机原生读, VCS X-2025.06-SP2 写的文件): 冷加载 66.9s/**17.8GB** → 52.9s/**13.6GB**;
  暖加载 13.5s/**11.0GB** → 10.1s/**6.35GB**;`(length (SIGNALS))` 11.3GB → **6.35GB**。三处改动:
  ① **`.fnames` 缓存升 v2**: 定长 48B header 带"信号数 / 名字总字节 / scope 数 / scope 总字节",
     读侧**一次精确 reserve**(老口径按 `n*32` 猜 + 翻倍增长: 2.85GB 名字字节会涨到 4.6GB 容量,
     最后一次扩容旧新两份同时驻留 ≈7GB);
  ② **冷路径边遍历边流式写 `.fnames`**: 遍历期间内存里不攒 arena, 走完按精确容量解码回来;
     顺带把"先拼 3.2GB Vec 再落盘"改成流式(指纹 `seek` 回填)。缓存不可写或流式失败 → 自动
     回退内存 arena(重走一遍树), 绝不影响"能不能读波形";
  ③ **`(length (SIGNALS))` 不再物化整表**: 新增 `Trace::signal_count()/signal_at()`(FSDB/VCD
     都是 O(1)/O(名长)), 求值器给 `(length (SIGNALS))` 加快路, CLI `sigs` 按下标懒遍历 ——
     1796 万信号上少背一张 ≈4.8GB 的 `Vec<String>`。`(SIGNALS)` 整表语义不变。
  `WAL_DEBUG_FSDB=1` 现在按 open 后 / 名字就位 / 索引建完三段打印 RSS, 便于定位内存大头。
- **FSDB 时间线冷建 @ 400 万信号: 112s → 14.2s(单进程) / 69.6s → 12.1s(8 路)**。
  病根不是"把数据过一遍", 而是 `npiFsdbTimeBasedVcIter::start()` 的**每块固定开销
  ~100~140ms**(与块内信号数、文件数据量都无关: 19 信号/200 万时间戳夹具上 18 块比 1 块
  多 2.45s;4M 信号夹具上 977 块比 16 块多 98s) —— 400 万信号按老的 4096/块切 = **977 次
  start = 100s 纯开销**。现在默认块大小随信号数放大 `clamp(signals/64, 4096, 65536)`
  (总块数 ≈64 封顶;并行 worker 也按文件总信号数定块, 否则每片 50 万信号又变 122 块),
  `WAL_FSDB_CHUNK` 仍可显式覆盖做 A/B。峰值 RSS 不变(1.5~2.0GB)。
- **时间线 `.ftl` 的落盘判据不再只看点数**: FSDB 冷建代价 ≈ 信号数 × 每信号开销(~3.5µs),
  与时间点数几乎无关 —— "400 万信号 / 200 个时间点"点数 <256 曾因此**每个新进程都重建一次**
  (75.6s/次, `hit` 与 `cold` 一样慢)。现在"本次建了 ≥150ms"也算数(并行/单进程共用同一
  判据), 暖查询 **75.6s → 4.9s(15.6×)**;电平计数冷查询 75.6s → 21.2s。数字与复现记在
  `bench/RESULTS.md`。
- **4M 信号的"名字路径"实测与两处修补**: 加了离线基准
  `cargo test --release --lib -- --ignored --nocapture fsdb_name_path_4m`(不需要波形文件,
  直接造 400 万名字走 encode/decode/建索引/叶子排序), 一跑就露出两个问题:
  ① **叶子名排序 3.5s**: 比较器里现算 `rsplitn('.')` → 每个名字被重复扫描 O(log N) 次;
  改成"先一遍预计算叶子 (偏移,长度), 排序只比字节切片"后 **0.36s**(9.8×);
  ② **`.fnames` 缓存要额外背 207MB**: 改成从文件**流式**解码进 arena(`decode_tree_file`),
  多花 ~70ms 换掉 207MB 峰值; 截断/损坏仍当未命中回退走 NPI 树(真波形上验过)。
  4M 信号名字路径现状: encode 56ms → 207MB; 流式 decode 127ms; 建索引 558ms; 叶子排序 360ms;
  内存 arena 199MB + spans 30MB + 索引 96MB + 叶子序 15MB ≈ **340MB**(改动前 ≈1.1GB)。
  数字与复现命令记在 `bench/RESULTS.md`。
- **FSDB 名字存储改 arena + 开放寻址索引(4M 信号级的头号开销)**: 改动前同一份名字在
  进程里存了**四遍**(`Sig.name` 叶子名 / `Sig.full` 全名 / `sig_names[]` / `HashMap<String,_>`
  的键), 微基准口径 **280B/信号** → 4M 信号 ≈ **1.1GB**;现在名字进连续 arena(每信号只多
  8B 的 (偏移,长度))、索引只存 64 位哈希 + 下标, **73B/信号** → 4M 信号 ≈ 0.3GB(**省 74%**)。
  读/写 `.fnames` 缓存也改成**直接对 arena 编解码**, 省掉 4M 信号下两次几百 MB 的瞬时峰值。
  闸: `trace::name_store::per_signal_bytes_stay_small`(同时打印新旧口径)。
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

### Fixed
- **许可不可用时别再报"缺 libNPI.so"**: NPI 库加载成功、但 `npi_init` 因 check out 不到
  Verdi 席位而失败时, 原来的报错头一句是"FSDB 需要 Verdi 的 NPI 读库(libNPI.so)…设置
  `$VERDI_HOME`" —— 实测库好得很(`dlopen` 13ms 成功), 照这条线索会白翻安装目录。
  现在按 `npi_init` 与否分成两种文案: 库缺失照旧;初始化失败明确写"是**许可**"(并给出
  `$SNPSLMD_LICENSE_FILE`、`scripts/fsdb_env_check.sh`、并发 worker 各占一个席位三条线索)。

### Docs
- 新增 [`docs/fsdb-env.md`](docs/fsdb-env.md): FSDB 读写环境/许可/版本兼容运行手册 ——
  两套许可(25A 27080 / 2018 27051)身份互斥、宿主机原生读 FSDB 的启动顺序(`run25.sh` 起 VM →
  `license_up.sh` 拉通道, **VM 只当许可服务器, 解析在宿主原生跑**)、以及四个真实坑
  (SERVER 主机名要换 127.0.0.1、vendor daemon 端口要钉、**客机 NixOS 防火墙默认 reject
  会被误判成 FlexLM -16,287**、**起 daemon 前要等旧进程死透否则 lmgrd 只会重试 5 分钟**);
  第 8 节是"现场大波形就位后的接收流程"。
- 新增 `scripts/fsdb_env_check.sh`(`make fsdb-env [FSDB=…]`): 一条命令打出"写者版本 /
  magic / 许可(**含端口可达性探测**) / 真实 open 一次", 拿到别人的波形先跑它。
- 新增 `scripts/fsdb_landing_check.sh`(`make fsdb-land FSDB=…`): 波形到手一条命令做完
  身份 sha256/写者 → 环境与许可 → 磁盘余量 → 冷/建/暖基线(`bench_fsdb.sh`) → 并行轴
  `-j 1|auto|8` → **`.ftl` 逐字节一致性**(并行正确性闸)。`--quick` 只做能读+并行收益。

### Internal
- `trace::name_store`: 把 VCD 后端早就有的 `NameArena`/`OpenIndex` 提成共享类型, FSDB 后端
  迁到同一套;补了 arena 往返、开放寻址扩容/碰撞、每信号字节数的单测。
- `tests/fsdb_diff.rs::fsdb_col_cache_hit_matches_cold` 之前**从未真正跑过**(需要 Verdi)。
  环境打通后第一次跑就暴露两处测试自身的问题: ①缓存落在 CWD 下的 `.wal-rust-cache/`, 断言
  却只看工作目录顶层;②没显式设 `WAL_CACHE_DIR`, 环境里若已有它(CI 会设)缓存就写到别处。
  两条都修掉后, 这个门现在真的在跑(冷/建/命中三次同答 + `.fcol` 落盘)。
- `scripts/bench_fsdb.sh` 修两处: 二进制解析成绝对路径(run 里会 cd, 相对路径会静默 0.00s)、
  查询失败直接报错而不是把 `NA` 混进回归库(并把之前误写的 14 行清掉)。
- **`-j/--jobs` 用环境变量而不是进程内 static 传递**: `src/main.rs` 自己 `mod trace;`
  (二进制 crate 二次编译了一份库代码), 两边的 `OnceLock` static 不是同一个变量 —— 用 static
  传参会静默失效(踩过: `-j 1` 照旧开 8 个 worker)。现在 `-j` 统一写 `WAL_FSDB_TL_JOBS`,
  两份编译与子进程 worker 都读同一个值。
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
