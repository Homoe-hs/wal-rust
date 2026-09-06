# wal-rust 0.8 → 0.11 行为变更表

面向从上代 WAL 用法迁移的用户；每项均以 0.11.x 实测为准。

## CLI / 安装

| 旧 | 新 |
|---|---|
| `wal-rust '(expr)' -l f` | 不变；新增 `count/sigs/topsig` 子命令、`--halt-on-error` |
| `--version` 只报版本 | 附安装路径（`wal-rust 0.11.x (from /path/wal-rust)`） |
| release glibc ≥ 2.34 | **0.11.4 起 release 为 glibc ≥ 2.17**（CentOS 7 / RHEL 7 / Ubuntu 16.04+） |

## 语言语义（与官方 WAL 的差异，已显式化）

| 项 | 行为 |
|---|---|
| `count`/`find` **默认** | **信号变化点采样**（快路径，mmap 单遍）；官方“每个索引”语义 = `count/step` `find/step`（显式，较慢） |
| `(whenever ...)` | 保持官方逐拍语义；`(whenever "changed" sig cond ...)` = 变化点采样模式 |
| `&&`/`||` 输出 | **布尔** `true/false`（官方为 1/0；`#t/#f`/`1/0` 输入等价） |
| `(get)` 含 x/z | 返回**位串**（全 x → `"x"`、全 z → `"z"`、部分 x/z → 逐位小写，如 `"10x1"`；无 x/z 仍为 int）。判定用 `(is-x)/is-z`；**x ≠ 0、x ≠ 1**（与 count/find 一致） |
| `(signal-width)` / `(sample-at)` | 接受**字符串或符号**（作用域全名如 `"tb.dut.sig [7:0]"`） |
| `(timescale)` | 已实现（打印 `1ns`/`?`）；多行 `$timescale` 亦解析 |
| 列表打印 | 有界渲染 `...(N items)`（9 万信号不爆终端） |
| 未知操作符/信号 | 给最近候选（`Closest matches` / `Did you mean`） |
| `SIGNALS/SCOPES` 等 | 一等 list，可用 `slice`/`take`/`first` 直接处理 |

## 保持兼容（未移除）

- `groups / in-group / resolve-group / in-scope / in-scopes / all-scopes`、`CG`/`CS`、`@`/`#`/`~` 语法糖均保留
- `slice` 对列表按 Python 语义；`(for/list [x xs] body+)`（含多绑定 zip）
- `save` 导出 CSV 保留 x/z 原样位串（唯一“原样”面板，get 见上表）

## 已知边界（诚实标注）

- `count/step` 逐拍扫描在大波形（>10 万索引）较慢（每次 `get` 为索引查找）
- 常量/初始值信号：有 var 声明但 dump 中无该信号值行的场景如需 guarantee，请用 `(is-x)`/`(get)` 结合确认（0.11.4 测试报告的 fix3.vcd 特例待样本复现）
- 带 `x` 值的向量在统计口径中“非 0 非 1”（4-state 语义）；若需要“把 x 当 0 折叠”的旧口径，用 `(count (= (get …) 0))` 之外的组合请显式按 `(is-x)` 过滤
