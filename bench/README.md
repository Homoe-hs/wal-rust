# 性能基准

> 职责边界: 这里放**样本生成、测量脚本与历史数字**; 每个数字必须能复现(样本 + 命令 + 机器口径)。

## 目录

| 路径 | 内容 |
|---|---|
| `RESULTS.md` | 历史性能数字与对照表(读之前先看它的"测量口径") |
| `perf-history.csv` | 逐次测量的追加记录(`scripts/perf_history.sh` 写入) |
| `benchmark.wal` | 基准用 WAL 脚本 |
| `gen_vcd.rs` | 合成波形生成器(源码; 编译见下) |
| `data/` | 大样本目录(**不入库**, 按需生成) |

## 生成样本

```bash
# 小合成样本(1GB 级): 结构可控, 适合回归对照
python3 scripts/gen_big_vcd.py /tmp/small.vcd 200000 20000 100   # 参数: 输出 信号数 时间戳数 每拍变更数
# C 实现: 更快的生成器(见 gen_vcd.rs 顶部注释)
rustc -O bench/gen_vcd.rs -o bench/gen_vcd && ./bench/gen_vcd --help
```

真实大样本(100GB 级)请用 `docs/152gb-round.md` 里的方法生成; 这些文件**不要**提交到仓库。

## 跑基准

```bash
make perf                       # CI 的性能冒烟(有样本才跑)
bash scripts/bench_wal.sh       # 端到端对照
```

## 记录新数字的要求

1. 写清**机器**(CPU/内存/是否虚拟机)、glibc、二进制来源(哪个 tag / 是否 zigbuild)。
2. 写清**冷/热**(换 `WAL_CACHE_DIR` 目录即可模拟冷启动)。
3. 同一结论至少 **3 次**测量取中位数, 避免被 page cache 与调度噪声骗。
4. 回归 >20% 必须在 `CHANGELOG.md` 的对应版本里写明原因。
