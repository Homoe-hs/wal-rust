# wal-rust 查询引擎设计(初版方案)

> ✅ **现行设计** —— §1 的查询语义是**权威定义**(与 `4-state-semantics.md` 的四值口径配套);
> 后续章节保留当初的方案与阶段表, 落地状态在文内标注。

> 目标: **消灭"快路径/慢路径"分裂**,用一个统一引擎服务 count/find/whenever/
> count-rise/at/change_points,并把任意表达式的查询性能做到与 `(= (get "sig") N)`
> 同一量级(150GB 单表达式 ≤ 60s)。
> 参照: DuckDB 向量化执行 + [扫描下推任意表达式](https://github.com/duckdb/duckdb/pull/17213);
> Arrow/Gandiva [表达式→向量内核](https://github.com/lynnyuan-arch/gandiva);
> [vectorized vs compiled](https://github.com/samyama-ai/dbms_research/blob/fa2f8afb108c38b9564e8a0c8cf1a2d4624437a4/topics/13-column-stores-olap/vectorized-vs-compiled.md)。

## 0. 为什么是数据库路线

波形查询 = "对全表(全部时间戳)做表达式谓词扫描"。数据库对这类问题的
成熟结论: ①扫描下推表达式(一次 IO 完成解析/投影/过滤/聚合);②表达式编译成
批量向量内核(或向量化解释器);③稀疏列/RLE 天然契合"多拍不变化"的波形。
wellen 的每信号变更点横格 = 现成的稀疏列;VCD 侧我们已经有每信号变更点缓存。
**不需要把波形物化成列存:变更点即列。**

## 1. 参考语义(唯一权威定义)

引擎所有入口共享同一份语义,禁止各自实现:

| 条目 | 定义 |
|---|---|
| 值 @ 索引 i | 该索引内**最后一次**写入的值;索引内无写入 = 保持此前值 |
| 初值 | `$dumpvars` 快照中该信号的值;无快照/无该信号 → x |
| 首变化前 | 保持初值(`(get s 0)`、`(at s T)`、is-x 计数一致) |
| 边沿 | **相邻索引值跳变**(x→1 是 change;x→0/1 不是 rising/falling;1→0 是 falling 等) |
| count/find/whenever | 全程从 INDEX 0 扫描;不读入也不改动游标 |
| count 区间语义 | 电平类条件持有区间按长度计入;边沿类仅命中点计入 |
| `(at s T)` | 返回值 = T 时刻保持值;首变化前返回 (0, 初值) |
| x/z | IEEE 1364-1995 §14.1.1.4:get 位串;比较语义按 4-state(见 §4) |

**一致性测试矩阵**(P1 建立,后续每步回归):
fixtures ∈ {小(≤1k 拍), 大(≥300k 拍), 标量, 向量, x/z, 毛刺(delta 双写),
共享 id 别名, 初值快照} × backend ∈ {VCD, FST} × 表达式 ∈ {全部条件构造}。
期望值 = 参考语义手工金标准(少量) + 引擎自洽性质(两条路径互相校验前只对拍旧实现)。

## 2. 总体架构

```
               ┌──────────────────────────────────────────────┐
               │     QueryEngine (src/wal/eval/, interval_scan) │
               │  Expr(编译后表达式树) · IntervalSweep(区间扫描) │
               │  count/find/whenever/edge/at  — 全部入口        │
               └───────▲─────────────────────┬───────────────┘
                       │ Column(稀疏列视图)    │ 匹配区间/命中列表
        ┌──────────────┴──────────┐   ┌──────┴──────────────┐
        │  VcdTrace::columns()    │   │  FstTrace::columns() │
        │  单遍并行扫描,一次产出    │   │  wellen 变更横格零拷贝 │
        │  所有被引用信号的列        │   │  包装为 Column        │
        └─────────────────────────┘   └─────────────────────┘
```

### Column(稀疏列)

```rust
pub struct Column {
    pub name: String,
    pub width: usize,
    pub initial: ScalarValue,          // $dumpvars 快照值, 无 → x(全宽)
    /// 变更点,升序,索引语义 = 全局时间表索引
    pub change_indices: Vec<u32>,
    pub values: Vec<ScalarValue>,      // 与 change_indices 一一对应
}
```

- **VCD**: `columns(names)` 一次并行分块扫描,收集全部被引用信号的变更点
  (现 `find_indices_batch` 的扫描骨架改造成"收集列"输出);每块重播前值。
- **FST**: wellen `iter_changes` 折叠同索引组(取最后值)后直接包装,零额外 IO。
- 列缓存放 trace 内(带容量上限),多查询复用同一列。

### 表达式编译

`(compile cond)` — AST → 扁平节点数组(数据/引用分离,常量折叠):

```rust
pub enum Node {
    Const(Value),                    // 常量(含位串)
    Col(u32),                        // 列引用 → 当前值数组下标
    PrevCol(u32),                    // 上一索引值(edge 节点用)
    Not(usize), UnaryMinus(usize),
    Add(usize, usize), Sub(..), Mul(..), Div(..), Mod(..),
    Cmp(usize, usize),               // =/!=/</>/<=/>=, 4-state 感知
    And(usize, usize), Or(usize, usize), Xor(..),
    IsX(u32), IsZ(u32),              // 列直接判
    Rising(u32), Falling(u32), Changed(u32),  // 边界处用 PrevCol/Cur 求值
}
```

求值环境是一个小的"当前值元组" `Cur(Vec<ScalarValue>)`,每次边界处整体刷新。
节点求值为单一 match,**无 env 查找、无字符串解析**——这就是"慢路径"的
全部开销来源,编译后只剩纯算术。

### IntervalSweep(区间扫描)

```
1. name → column: 收集表达式引用的全部列(其变更点并集 = 扫描边界)
2. 边界序列 S = merge(各列 change_indices)  (+ 首索引 0, 若 0 ∉ S)
3. 游标 = 0; last = 0
   对每个边界 p:
     cur 刷新到 p 处各列值(每列在 p 处若有变更则取该值/否则不动)
     if eval(expr, cur): 区间 [last, p) 计入(电平) / p 计入(边沿)
     last = p
   终止: [last, max_index] 计入
4. count = Σ命中区间长;find = 命中边界/区间起点;whenever = 命中区间上执行 body
```

成本 **O(变更点并集 × E)** ——稀疏信号 C ≪ T;稠密(clk 级)退化为 O(T×E)
但每拍只做常数级节点求值,无解释器/环境开销。**快路径 = 单列特例**,自然消失。

#### 边界处求值的五条硬约束(0.12.x 实测踩坑记录)

1. **每个边界都要推进 `prev`**。覆盖表存 `(prev, cur)`;`cur` 只在信号自身
   变更处更新,但 `prev` 必须在**每个**边界先写成"上一索引的 `cur`"。
   否则 `(rising a)` 会在"由 b 的变化引入的边界"上拿陈旧 `prev` 误报
   (经典查询 `(count (&& (rising clk) (= (get state) 3)))` 过计数)。
2. **区间内部边沿恒为假**。边界之间各信号值恒定 ⇒ 边沿谓词只可能在边界为真。
   混合条件(`(|| (rising c) (= (get d) 3))`)的**电平部分**在整段区间内保持为真,
   必须按区间长度累加;只计边界会漏计区间内部索引。
   实现:区间内部用 `edge_off` 标志把 rising/falling/changes 强制 false 再求值一次,
   命中则 `count += next - b - 1` / `find` 展开 `(b+1)..next`。
3. **分解优化不得丢谓词**。`(&& ...)`/`(|| ...)` 的"只取可解析子条件"优化
   (count 的 `decompose_and_count`、find 的 `decompose_and_find_indices`)
   必须对**无法解析的子条件直接放弃分解**;否则 `(find (&& (rising c) (= (get d) 3)))`
   会静默返回电平段。count 侧靠 `parse_edge_condition` 覆盖边沿谓词,
   find 侧不再分解含边沿谓词的条件(交给引擎)。

> 注意:0.11.x(**无引擎**)本身也带这两处缺陷——`(find (&& (rising c) (= (get d) 3)))`
> 在 v0.11.4 返回电平段 `(0 1 2)`,`(count (&& (rising c) (= (get d) 15)))` 返回 2(正确 0)。
> 因此 `scripts/diff_find.sh` 的"与旧版对拍"**不是**这类条件的 oracle;
> 权威 oracle 是纯逐拍路径(下节)与手算黄金值。

4. **名字解析必须所有读取路径一致**。`op_get` 走 `resolve_signal_name`(短名/叶子名/
   子串),而 `op_rising`/`op_falling`/`op_changes`/`is-x`/`is-z` 曾直接按字面名读 →
   短名场景下边沿/X 谓词静默给 false(`count/step`、`find` 回退、`whenever` 回退全中招)。
   现在解析下沉到 `VcdTrace::resolve_idx` / `FstTrace::resolve_cached`(带缓存,
   一次 O(N) 扫描后 O(1)),所有路径共用。
5. **INDEX/TS 依赖的条件必须放弃引擎**。区间内部"值恒定"的前提只对信号成立;
   `INDEX`/`TS` 随索引变化,`(&& (= (get c) 1) (= INDEX 3))` 在引擎里会被
   按边界游标求值 → 漏计。`collect_cond_signals` 检测到 INDEX/TS 直接返回
   `Ok(None)` 走逐拍(此类条件本身就必须 O(索引数),无性能损失)。

**后续优化(P6 候选)**:`(&& 信号条件 INDEX条件)` 可先让引擎算出信号侧命中集,
再只在该集合上求值 INDEX 部分 → O(命中数) 而非 O(索引数)。当前为保证语义先直接
回退逐拍(正确性优先,性能后补)。

6. **0 个信号引用 = 常量条件**。不引用信号/INDEX 且无副作用的条件在每个索引取值相同,
   `count`/`find`/`count/step` 直接 O(1) 给出结果(全索引或空集),不再逐索引求值。
   纯度判定:`collect_cond_signals` 为空且 AST 无 `set!`/`print`/`while`/… 且无闭包调用;
   引用信号的表达式绝不折叠(与逐拍 oracle 对拍在矩阵 `matrix_constant_condition_fold`)。

7. **多 trace 必须跨 trace 查找**。信号可能只存在于其中一条 trace:
   - `interval_scan` 逐个 trace 解析,用**含该信号的 trace** 的变更点;时间线长度取
     各 trace `max_index` 的最大值(与逐拍"任一还能步进就继续"一致);
   - `&&`/`||` 分解优化必须**跳过**不含该信号的 trace(曾用原始名去查 → 报错 → 塞空集
     → `&&` 恒 0),并把含它的各 trace 结果取并集;
   - `getwave`/`wave`/`at` 改为跨 trace 查找(`with_signal_trace`),错误信息列出全部
     trace 的候选。
   - 名字解析统一走 `Trace::resolve_name`(后端带缓存实现),避免每次查询都
     `signals()` 分配整张信号表(3.5M 信号 ≈ 数十 MB/次)。

#### 独立 oracle:`WAL_NO_ENGINE=1`

引擎与"逐拍路径"若共享同一套实现,对拍就是自证。`WAL_NO_ENGINE=1` 让
`interval_scan` 直接返回 `None`(全部走纯逐拍),回归矩阵在**子进程**里用它做
独立 oracle(`matrix_engine_matches_step_oracle_subprocess`,22 个条件 × count/find)。

### 向量化(P5,后续深入)

- 边界批量:一次取 V=4096 个边界,Cur 变为 V 宽小列,节点求值 SIMD
  (gandiva 式 `kernel(vector)->vector`);标量分支 → 把 x/z 谓词单列化。
- VCD 行解析:SIMD memmem + 逐块 madvise(已有计划),并行分块不变。
- 评估是否晚引入 batch 化解释器 vs LLVM codegen:先向量化解释器
  (与 DuckDB 选择一致),全程可对拍、可回退。

## 3. 入口映射

| 入口 | 引擎调用 |
|---|---|
| `count cond` | Sweep + 区间长求和 |
| `find cond [limit]` | Sweep,收集命中索引 |
| `count/step` `find/step` | 同上显式逐点(语义相同,返回逐拍) |
| `whenever [changed] sig cond body` | Sweep,命中处 set INDEX 后 eval body(现有语义) |
| `count-rise/fall/edges` | 边沿节点 + 区间求和 |
| `(get/at/sample-at)` | Column 直查(O(log C)),无扫描 |
| `change_points`/`getwave`/`wave` | Column 变更列表 + 初值 |
| `save/CSV` | 列 + 区间展开(现状保留,输出层) |

## 4. 4-state 比较语义

**权威定义在 [`4-state-semantics.md`](4-state-semantics.md)**(本节不再重复, 避免两处不一致)。
摘要: `=`/`!=` 在含 x/z 时按位串比(任一 x/z 位相异即不等);真值为假的是 `#f`/`0`/`()`(注意 **`x` 不是假值**,
它是"未知", 与 0 的关系是不相等);Rising/Falling 只认 0↔1, Changed 按语义相等(全 x = x)。
> 历史注: 本文早期版本写的"`0`/`x`/`z` → false"与实现不符(实现在 `src/wal/ast/value.rs::is_truthy`),
> 已按权威文档改正。

## 5. 落地阶段

| 阶段 | 内容 | 验收 |
|---|---|---|
| P1 | 语义冻结 + 一致性矩阵(小/大×标量/向量/xz/glitch/alias × VCD/FST × 表达式集) | 矩阵跑通,旧行为全部对拍 |
| P2 | `Column` 抽象 + `Trace::columns()`(VCD 单遍收集、FST 打包) | 同信号列 == 旧 change_points 语义(含 delta 折叠/初值快照) |
| P3 | Expr 编译 + IntervalSweep;`count`/`find` 切换到引擎 | diff gate(scripts/diff_find.sh)ALL MATCH + 矩阵全绿 |
| **P3 ✅ 已落地(0.12.x)** | **实现变体**: 不另写表达式编译器,而是"同一解释器 + 信号值覆盖"——`interval_scan` 收集引用信号,取变更点并集为边界,在边界处给 `op_get`/边沿谓词安装值覆盖后调用**现有解释器**求值;无边缘谓词按区间长度计入,含边缘谓词只计边界。`count`/`find` 回退前先试引擎。矩阵用例 `matrix_interval_engine_equals_oracle` 全绿 |
| **P4 ✅ 已落地(0.12.x)** | whenever/`count/step`/`find/step`/`at` 收敛到引擎(count/find/whenever/step 四个入口同一实现) | 矩阵 20/20 全绿(含子进程独立 oracle) |
| P5 ⏳ **未落地** | 批量向量化 + SIMD 行解析 | 部分达成: 并行分块/`memchr` 已有, 绝对秒数见 README「性能」;SIMD 未做 |
| P6 ⏳ **未落地** | 删除旧 `FindCondition` 匹配路径(仅保留 CLI/导出视图) | `FindCondition` 仍在 `src/trace/trace.rs`;`Trace::columns()` 也没引入(`grep -rn 'fn columns' src/trace/` 为空) |

每阶段独立可发布;P1-P3 是主体(正确性),P5 是性能冲刺。

## 6. 风险与对策

- ~~**初值快照($dumpvars)** 现 VCD 读器未捕获(B11: 初值 0 丢)~~ → **已解决**(0.13.x: load 只读文件头时抓
  `$dumpvars` 初值快照, 矩阵用例 `matrix_dumpvars_initial`/`matrix_dumpvars_first_edge_at_index_zero` 冻结)。
  原始条目保留在下方仅作记录:
- **初值快照($dumpvars)** 当时 VCD 读器未捕获(B11: 初值 0 丢) → P2 在
  头解析时读取 dumpvars 块,存 per-signal initial。
- **索引域偏移**:FST 时间表可能与 VCD 的 #T 列表差 1 位(§8.4 内测现象,
  取决于转换器)→ P1 明确"以 VCD #T 顺序为准"的索引定义;FST 列按时间值对齐
  (用 timestamp 匹配而非索引位置),差分门禁兜底。
- **内存**:列全量驻留对 3.5M 信号的 150GB 不现实 → 查询级列引用计数 +
  LRU(WAL 只扫被引用信号,常规查询 ≤ 几十列)。
- **兼容**:P3 切换时旧路径保留一个版本做对拍基准,确认语义零回归后移除。
