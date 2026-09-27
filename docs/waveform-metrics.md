# 波形指标与统计(延迟 / IPC / 分布)

> ✅ 现行 —— 算子实现在 `src/wal/builtins/metrics.rs`;本文是**语义约定**与用例。
> 闸:`tests/metrics_test.rs`(手算期望值的 VCD 夹具)。

## 0 为什么要这一层

硬件团队要的是**分布**, 不是单个数字:"握手平均多少拍"没有意义,"p50/p99/最坏多少"才有意义;
IPC 同理, 不只看均值, 还要看随时间的波动。所以这一层把中位数/分位数/标准差/直方图
做成一等算子, 而不是让用户自己写循环。

## 1 算子一览

| 算子 | 签名 | 说明 |
|---|---|---|
| `median` | `(median xs)` | 中位数(p50, 线性插值) |
| `percentile` | `(percentile xs p)` | 任意分位, `p ∈ 0..100`;越界**报错**不静默 |
| `stddev` | `(stddev xs)` | **样本**标准差(n-1);n<2 → 0 |
| `stats` | `(stats xs)` | 打印一行摘要, 返回 `(count min p50 mean p90 p99 max stddev)` |
| `histogram` | `(histogram xs [bins=10])` | 打印条图, 返回 `(lo hi count …)`(边界 ×1000) |
| `latency` | `(latency <start> <end> [#restart])` | 每次 start→end 的间隔(**原生时间单位**) |
| `ipc` | `(ipc <events> <cycles>)` | 事件数 / 时钟周期数 |

均值用已有的 `average`;求和用 `sum`。

## 2 语义约定(先定死, 免得各人各解)

* **空列表不报错**:`median`/`percentile`/`stddev`/`stats` 返回 NaN / 0 ——
  "这类事件一次都没发生"也要能跑完并打印 `n=0`。
* `percentile` 用**线性插值**(与 `numpy.percentile` 默认口径一致):`[1,2,3,4]` 的 p50 = 2.5、p90 = 3.7。
* `stddev` 是**样本**标准差(n-1)。
* 一切按 f64 算;`stats` 的 count 是整数;列表里的非数值元素**报错**(不静默跳过)。
* **`latency` 单位是波形原生时间单位**(ps/ns/…, 与 `getwave`/`at` 一致), 不是拍数。
  要拍数:查时钟周期再除 —— `(period "clk")`, 或用 `(ipc …)` 拿吞吐。
* **`latency` 默认 FIFO 配对**:先到的 start 配下一个 end;选项 `#restart` 表示"新 start 直接
  作废还没配上的旧 start"(重试/覆盖语义)。配不上的两端**直接丢弃**, 不编造负延迟;
  想核对丢弃量就比 `(count (rising "start"))` 与 `(len (latency …))`。
* 边沿口径与查询引擎一致:**x→1 算 `changes`, 不算 `rising`**。
* **`latency`/`ipc` 的参数是条件表达式**(`(rising "req")`), 走的是**未求值 AST** 那条路
  (与 `count`/`find`/`whenever` 相同), 不是普通函数参数。
* `latency`/`ipc` 只在**时间域**取变更列(`change_points_time`), **不建索引空间** ——
  在千万时间戳的大波形上不会触发"全文件物化时间线"。

## 3 用例

```bash
# 一次加载, 多个探针(stdin 会话)
target/release/wal-rust --stdin -l aicore_smoke_test_000.fsdb <<'EOF'
(latency (rising "tb.dut.req") (rising "tb.dut.ack"))
(stats   (latency (rising "tb.dut.req") (rising "tb.dut.ack")))
(histogram (latency (rising "tb.dut.req") (rising "tb.dut.ack")) 20)
(ipc (rising "tb.dut.inst_retire") (rising "tb.dut.clk"))
EOF
```

组合即分析:延迟列表能直接喂给统计 —— `(stats (latency …))` / `(median (latency …))`。

真实 case study(321MB / 1796 万信号 core 级 smoke test, 时钟 670ps)见
[`bench/RESULTS.md`](../bench/RESULTS.md) 末节:写响应往返 28.8ns(≈43 cycle)、
11 条 barrier/sync 操作用了 601 cycle(≈55 cycle/op)、整段 IPC 1.7e-4 ——
结论是"上电连通性 smoke test", 不是性能测试。

## 4 多文件行为

`latency`/`ipc` 的两个信号各自按**加载顺序 first-match** 选源(与其它查询一致);
两个信号可以来自不同波形(此时"延迟"跨文件, 只在两边时间基相同时才有物理意义)。

## 5 还没做的

* `latency` 的**窗口**与**多事件**版本(`(latency a b :window T0 T1)`);
* `#index` 单位(拍/索引差)—— 需要索引空间, 大波形上代价高, 先不做;
* 逐拍/逐窗口的 IPC(现在是整段一个数;要分布就先按窗口切, 再各自 `ipc`)。
