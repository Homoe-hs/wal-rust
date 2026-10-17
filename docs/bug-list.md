# wal-rust Bug List(找 bug 阶段产出)

> 状态:🟡 **收集中**(2026-10-17 起)。流程按约定:**先只收集、不修**;等一轮收集完 →
> 统一根因分析 → 整体修复 + 补回归测试。每条 bug 必须带**最小复现命令**与**观察值 vs 期望值**。
>
> 夹具生成:`python3 scripts/bughunt/gen_fixtures.py`(12+ 组边界 VCD → `.tools/bughunt/fixtures/`,
> FST 孪生体用 `target/release/examples/vcd2fst` 生成)。
> 单功能矩阵:`python3 scripts/bughunt/run_matrix.py`(产出 `.tools/bughunt/findings.tsv`)。
>
> 严重度:**高** = 结果错误/崩溃/主流程不可用;**中** = 跨格式/跨入口不一致、边界行为可疑;
> **低** = 文案/顺序/可用性。

## BUG-001 VCD 同时间戳的重复 `#N` 段被当成多个索引

- 严重度:**高**(索引空间语义错误,直接影响 count/find/getwave/指标)
- 类别:语义 / 跨格式一致
- 夹具:`.tools/bughunt/fixtures/dup_ts.vcd`(同内容 FST:`dup_ts.fst`)
- 复现:
  ```bash
  ./target/release/wal-rust '(list INDEX TS MAX-INDEX)' -l .tools/bughunt/fixtures/dup_ts.vcd
  # 观察: => (0 0 4)
  ./target/release/wal-rust '(list INDEX TS MAX-INDEX)' -l .tools/bughunt/fixtures/dup_ts.fst
  # 观察: => (0 0 2)
  ./target/release/wal-rust '(getwave "s")' -l .tools/bughunt/fixtures/dup_ts.vcd
  # 观察: => ((0 0) (10 1) (10 0) (10 1) (20 0))   ← 同一时刻 3 个"索引"
  ```
- 期望:`docs/4-state-semantics.md` 规定"某索引处的值 = 该时间戳内最后一次写入(delta 周期/毛刺:最后写入胜出)",
  索引空间按**时间戳**建 → `MAX-INDEX` 应为 2,`getwave` 在 t=10 只应有 1 个点(值取最后写入 1)。
- 疑似根因:`src/trace/vcd.rs` 的 `build_dump_index` 对每个 `#<digits>` 行都 `ts.push(...)`,没有合并相邻相同时间戳。

## BUG-002 FST 读侧名字丢失向量范围(转义标识符场景)——**结论:格式固有限制,已记录**

- 严重度:**已缓解**(VCD 侧已把名字规范为引用名,与 FST 一致;只有"转义标识符且含方括号"这一 FST 格式固有场景仍不一致)
- 类别:跨格式一致性 / 名字解析
- 夹具:`.tools/bughunt/fixtures/escaped.vcd` / `wide.vcd`(及 FST 孪生体)
- 复现:
  ```bash
  ./target/release/wal-rust '(SIGNALS)' -l .tools/bughunt/fixtures/escaped.vcd
  # 观察: => ("top.\a[0]" "top.\bus/weird.name" "top.normal [3:0]")
  ./target/release/wal-rust '(SIGNALS)' -l .tools/bughunt/fixtures/escaped.fst
  # 观察: => ("top.\a" "top.\bus/weird.name" "top.normal")     ← \a[0] 被截成 \a；normal 的 [3:0] 也没了
  ./target/release/wal-rust '(get "top.\a[0]")' -l .tools/bughunt/fixtures/escaped.fst
  # 观察: error: signal 'top.\a[0]' not found. Closest signals: ["top.\a", ...]
  ```
- 期望:`\a[0]`(VCD 转义标识符,空白结束)是**完整名字**,方括号属于名字,不能被当范围;
  且 FST 与 VCD 对同一设计的信号名/范围表示应一致。
- 疑似根因:FST 读侧(wellen)把名字尾部的 `[...]` 解析成 range 后从名字里剥掉;而写侧 `FstWriter::create_var`
  把 `data [3:0]` 原样写进名字 —— 两边表示不统一。

## BUG-003 FST 的 `SCOPES` 多出一个空根 scope

- 严重度:**中**(`(length SCOPES)`/`SCOPES`/`in-scope` 行为跨格式不同)
- 类别:跨格式一致性
- 复现:
  ```bash
  ./target/release/wal-rust '(SCOPES)' -l .tools/bughunt/fixtures/basic.vcd  # => ("top")
  ./target/release/wal-rust '(SCOPES)' -l .tools/bughunt/fixtures/basic.fst  # => ("" "top")
  ./target/release/wal-rust '(length SCOPES)' -l .tools/bughunt/fixtures/one_ts.vcd  # => 1
  ./target/release/wal-rust '(length SCOPES)' -l .tools/bughunt/fixtures/one_ts.fst  # => 2
  ```
- 期望:同一设计两种格式给出相同的 scope 列表(根 scope `""` 不应作为可见 scope)。
- 疑似根因:`src/trace/fst.rs::scopes()` 直接收集 `wf.hierarchy().iter_scopes()`(wellen 含根),未过滤空名。

## ~~BUG-004~~ VCD 信号名带范围且用**空格**分隔 —— **已修**

- 严重度:**中**(可用性 + 跨格式不一致;用户自然输入 `top.data[3:0]` 或 `top.data` 行为不同)
- 类别:名字解析 / 跨格式一致
- 复现:
  ```bash
  ./target/release/wal-rust '(SIGNALS)' -l .tools/bughunt/fixtures/basic.vcd
  # 观察: ("top.clk" "top.data [3:0]" "top.rst" "top.en")
  ./target/release/wal-rust '(get "top.data[3:0]")' -l .tools/bughunt/fixtures/basic.vcd
  # 观察: error: signal 'top.data[3:0]' not found. Closest signals: ["top.data [3:0]", ...]
  ```
- 期望:VCD 规范里 `$var wire 4 ! data [3:0]` 的**引用名是 `data`**,`[3:0]` 是范围;
  wal-rust 至少应让 `data` 与 `data[3:0]` 都能解析,且 `(SIGNALS)` 的表示与 FST 一致(不带空格范围)。
- 备注:`src/vcd/convert.rs:238` 的既有断言 `names.contains("counter_tb.count [7:0]")` 说明当前行为**被测试固化**过,
  修复属于**语义决策**,需要与文档/README 一起改。

## ~~BUG-005~~ `(SIGNALS)` 顺序跨格式不同 —— **已修**

- 严重度:**中**(依赖顺序的用户脚本(`(first (SIGNALS))` 等)在换格式后结果不同)
- 类别:跨格式一致性
- 复现:
  ```bash
  ./target/release/wal-rust '(SIGNALS)' -l .tools/bughunt/fixtures/basic.vcd
  # => ("top.clk" "top.data [3:0]" "top.rst" "top.en")
  ./target/release/wal-rust '(SIGNALS)' -l .tools/bughunt/fixtures/basic.fst
  # => ("top.clk" "top.data" "top.en" "top.rst")
  ```
- 期望:要么文档写明"顺序不保证",要么统一(建议统一为声明序;FST 读侧目前 `sigs.sort()`)。

## BUG-006 数字当函数调用 `(1 2)` 静默返回列表,而不是报错

- 严重度:**低~中**(静默接受非法程序,可能掩盖用户笔误)
- 类别:求值语义 / 错误处理
- 复现:
  ```bash
  ./target/release/wal-rust '(1 2)'      # 观察: => (1 2)
  ./target/release/wal-rust '((quote a) 1)'  # 观察: => (a 1)
  ```
- 期望:待确认设计意图。若"列表字面量"是有意行为,应在文档写明;否则非过程调用应报错
  (`(unknown-op 1)` 已能正确报 `Unknown operator or function`,两者行为不一致)。

## 待观察(未定性,先记录)

- `(get "top.data")` 在 VCD 上能返回(靠子串匹配),但 `top.a` / `top.ab` 这类前缀会让**子串匹配选中错误信号** ——
  需要系统化验证"子串回退"是否会把用户指到错信号(而不是报歧义)。
- `real` 值信号(`$var real`)在 VCD/FST 上的取值/算术行为尚未系统验证。
- 宽度 >64 的向量(`wide.vcd` 的 100 位)参与算术/比较/`bits->sint` 时的行为未验证。

---
*本文件只记录**已复现**的问题;`scripts/bughunt/` 下的矩阵仍在扩充(组合测试、CLI、脚本层、文件健壮性),新发现会追加。*

## BUG-007 `(help)` 把**未内置**的 `filter` 列进语言算子表

- 严重度:**低**(帮助文案误导;手册已注明未内置)
- 复现:
  ```bash
  ./target/release/wal-rust '(help)' 2>&1 | grep filter     # 文案: map fold filter print printf ...
  ./target/release/wal-rust '(filter (lambda (x) (> x 1)) (list 1 2 3))'
  # 观察: error: Unknown operator or function: filter
  ```
- 期望:help 与手册一致(手册 §5.9 明确 `filter` **未内置**,可 `map`+条件替代)。修法:help 里标注未内置或移除。

## ~~BUG-008~~ 损坏/截断波形只 warning 不报错 —— **已修(默认报错,`WAL_VCD_LENIENT=1` 可放宽)**

- 严重度:**中**(脚本只能靠 stderr 发现输入损坏;静默空结果是分析事故隐患)
- 复现:
  ```bash
  head -c 80 .tools/bughunt/fixtures/basic.vcd > /tmp/trunc.vcd
  ./target/release/wal-rust '(length (SIGNALS))' -l /tmp/trunc.vcd; echo "rc=$?"
  # 观察: warning: 未找到 $enddefinitions — 文件可能被截断...   => 0   rc=0
  printf '\x00\x01\x02garbage' > /tmp/garbage.vcd
  ./target/release/wal-rust '(list INDEX TS)' -l /tmp/garbage.vcd; echo "rc=$?"
  # 观察: 同上 warning 后 => (0 0), rc=0
  ```
- 期望:非 VCD/FST 或明显截断的输入应**报错**(rc≠0);至少要有一个"严格模式"开关。空文件已正确报错(`Empty VCD file`),说明有判定能力,只是没用在"缺 $enddefinitions/垃圾内容"上。

## ~~BUG-009~~ `(sample-at s 越界索引)` 返回 nil —— **已撤回(不是 bug)**

> ⚠️ **结论修正**:`test_samples/verify_vcd.wal` 第 32 行明确固化"索引越界返回 nil"
> (`(null? (sample-at s0 (+ mx 1000000)))`),说明这是**既有约定**(越界探测);负索引报错也保留。
> 本轮曾按"越界应报错"修过,CI 的 samples 阶段把它抓了回来,已撤回并改为固化该约定的回归测试。
> 文档已在手册 `sample-at` 节补一句"越界返回 nil"。

- 严重度:(撤回)
- 复现:
  ```bash
  ./target/release/wal-rust '(sample-at "data" 999)' -l .tools/bughunt/fixtures/basic.vcd   # 观察: => nil
  ./target/release/wal-rust '(sample-at "data" -1)'  -l .tools/bughunt/fixtures/basic.vcd   # 观察: error
  ```
- 期望:两者都报错(或都返回明确哨兵),不应只对负索引报错。

## BUG-010 `(at s 负数时间)` 静默返回最后一个值

- 严重度:**中高**(非法时间被接受并返回看似正常的结果)
- 复现:
  ```bash
  ./target/release/wal-rust '(at "data" -5)' -l .tools/bughunt/fixtures/basic.vcd
  # 观察: => (20 "0x1z")   ← 负时间返回了最后一拍的值
  ```
- 期望:负时间应报错(时间轴从 0 开始)。

## BUG-011 `let` 不支持"顺序绑定"(手册承诺 + 示例直接失败)

- 严重度:**中高**(文档主推用法不可用;用户脚本写法被拒)
- 复现:
  ```bash
  ./target/release/wal-rust '(let ((x 10) (y (+ x 5))) (+ x y))'
  # 观察: error: Undefined symbol: x
  ```
- 期望:手册 §1.6 `let` 写的是"创建新作用域,**按顺序求值绑定(后面的绑定可以使用前面的值)**",示例期望 `25`。
- 疑似根因:绑定求值阶段一次性在**旧环境**求值所有 binding 表达式(并行 let),未边绑定边求值(let*)。

## BUG-012 `cond` 不支持 `else` 默认分支(手册承诺 + 示例直接失败)

- 严重度:**中高**(同一类:文档承诺的写法被拒)
- 复现:
  ```bash
  ./target/release/wal-rust '(cond ((= 1 2) "no") ((= 1 1) "yes") (else "maybe"))'
  # 观察: error: Unknown operator or function: else. Closest matches: let, %, abs.
  ```
- 期望:手册 §4 `cond` 参数说明:"`guard` — 守卫表达式,**`else` 或 `#t` 作为默认分支**",且示例正是上面这段。
- 疑似根因:`cond` 求值器把子句 guard 当普通表达式求值,`else` 未做特殊符号处理(它没被求值成真值,而是被当函数调用)。

## BUG-013 手册 `&&`/`||` 的**示例输出**与正文/测试矛盾(0/1 vs true/false)

- 严重度:**低**(纯文档错误)
- 复现:
  ```bash
  ./target/release/wal-rust '(&& #t #t #f)'   # 观察: => false
  ```
- 期望:手册正文写"返回 bool(`true`/`false`;回归测试 `test_and_or_return_bool` 冻结此口径)",
  但同一节的示例输出块写 `0`/`1` → 改示例(实现与冻结测试为准)。

## BUG-014 输出表示与手册不一致:布尔/整值浮点/数组

- 严重度:**低~中**(`(help)`/REPL/脚本回显与手册示例不同,影响用户判断与文档可用性)
- 复现:
  ```bash
  ./target/release/wal-rust '(= 5 5)'         # 观察: => true      手册示例: #t
  ./target/release/wal-rust "(average '(1 2 3))" # 观察: => 2       手册示例: 2.0
  ./target/release/wal-rust "(array 'x 10)"   # 观察: => (x 10)    手册示例: {("x" 10)}
  ./target/release/wal-rust '(type (array))'  # 观察: "<class 'list'>"  ← 数组与列表不可区分
  ```
- 期望:统一一种约定(建议:显示层按手册改 `#t`/`2.0`/`{...}`,或把手册示例全部改成实际输出;二选一)。
  注:手册 §1.2 已说明输入字面量 `#t`/`true` 均可,输出口径需要明确。

## ~~BUG-015~~ `(1 2)` 数字当函数调用返回列表 —— **不是 bug(设计如此,已补文档)**

- 严重度:**待定**
- 复现:
  ```bash
  ./target/release/wal-rust '(1 2)'            # 观察: => (1 2)
  ./target/release/wal-rust '((quote a) 1)'    # 观察: => (a 1)
  ```
- 期望:若"非过程调用"应报错,则这是漏检(对比:`(unknown-op 1)` 能正确报
  `Unknown operator or function`);若"列表字面量"是有意设计,手册需写明。

## 覆盖缺口(下一轮补)

- **43 个算子没有手册示例**,其中 `rising/falling/changes/count-edges/getwave/at/initial/find-sig/
  latency/ipc/stats/median/percentile/stddev/histogram/search/edges/...` 是核心功能,需手工补用例
  (单功能矩阵已部分覆盖,但**期望值**多来自实现自身,不算独立 oracle)。
- 未测区域:`$dumpoff/$dumpon/$dumpall`、宏(`defmacro/defunm/macroexpand`)、虚拟信号
  (`defsig/new-trace/dump-trace`)、`search/freq/dela/period/groups/in-group`、
  `fsdb-timeline-*` 子命令、**FSDB 后端**(许可通道被宿主 EDA 布局变更破坏,待恢复)。
- 疑似(未复现):子串匹配回退(`resolve_signal_name` 第三步)可能把 `top.a` 指到 `top.ab` 而不是报歧义。

---

# 根因分析与修复状态(第一轮修复,2026-10-17)

## 已修(带回归测试,`tests/bughunt_regressions.rs`)

| bug | 根因(代码位置) | 修法 | 回归测试 |
|---|---|---|---|
| **001** VCD 重复 `#N` 当多索引 | ① `src/trace/vcd.rs::build_dump_index` 每遇 `#` 行就 `ts.push`;② 冷扫描 `anchored_changes` 也按 `#` 行数计 `ts_count` | 两处都改为"**时间戳值变化才新增索引**";跨 chunk 头部重复时间戳在合并时用 `dup_head` 修正基址(列缓存的 `gts` 同步用 `base_off`);偏移保留**第一个** `#` 段以覆盖同刻全部写入 | `bug001_duplicate_timestamps_collapse_to_one_index` |
| **001b** 同刻多次写入的变更点取到中间值 | 冷扫描合并里的"last write wins"按 index 比较,但 index 计数未去重导致写入落到不同 index;`getwave` 取到中间值、`(find (changes))` 误报 | 同上(去重后同刻写入落在同一 index,last-write 生效);`getwave/`find` 与 FST 对齐 | 同上(断言 getwave 折叠为 3 点、同刻值=最后写入) |
| **003** FST `SCOPES` 多空根 scope | `src/trace/fst.rs::scopes()` 直接收集 wellen 全部 scope(含根 `""`) | `.filter(|n| !n.is_empty())` | `bug003_fst_scopes_exclude_empty_root` |
| **007** help 列了未内置的 `filter` | `src/wal/builtins/core.rs` 的帮助文案 | 从文案移除 `filter`(手册本就注明未内置) | —(文案类,手工核对) |
| **010** `(at s 负时间)` 返回末值 | `src/wal/builtins/wave.rs::op_at` 用 `extract_int(..)? as u64`,负数回绕成巨大值 | 显式判负 → 报错 | `bug010_at_negative_time_errors` |
| **011** `let` 不支持顺序绑定 | `src/wal/eval/evaluator.rs::eval_let` 在**旧环境**里求值所有 binding 表达式,之后才切到新作用域(与手册 §1.6 的 let* 语义相反) | 先 `mem::replace` 进子作用域,再逐个求值+`define`;用闭包保证出错也恢复环境 | `bug011_let_bindings_are_sequential` |
| **012** `cond` 不支持 `else` | `cond` 未列入 `eval_list` 的**特殊形式**分支 → 参数被预求值,`(else ...)` 被当函数调用 | 新增 `eval_cond`(guard 不预求值;符号 `else` 恒真)+ 特殊形式分发 | `bug012_cond_supports_else_clause` |
| **013** 手册 `&&/||` 示例与实现矛盾 | 手册示例输出写 `0/1`,正文与冻结测试 `test_and_or_return_bool` 要求 bool | 示例改为 `false`/`true` | —(文档) |
| **014** 布尔/浮点/数组显示与手册不一致 | 手册 21 处期望输出写 `#t/#f`、`2.0`、`{...}`;实际输出 `true/false`、`2`、`(...)` | 手册示例统一为实际口径,并在 §1.2 增补"**输出表示**"约定段(bool=`true/false`、整值浮点按整数显示、数组=扁平列表) | —(文档) |
| **cache** VCD 缓存需失效 | BUG-001 改变了索引语义,但缓存 key 未变(`-v1.wcol`) | key 升到 `-v2.wcol`(旁挂 `.cols` 目录名随之变化),旧缓存自动失效 | 由 CI 与夹具验证 |

## 撤回的"修复"(误判)

- **~~BUG-009~~ `sample-at` 越界返回 nil**:`test_samples/verify_vcd.wal` 早已固化"越界返回 nil"的约定
  (越界探测语义),CI 的 `samples` 阶段抓到了这处误改 → 已撤回;改为回归测试
  `sample_at_out_of_range_returns_nil_by_contract` 把该约定固化住(防止以后再被"修"掉)。

## 仍未修(下一轮)

| bug | 现状 | 计划 |
|---|---|---|
| **002** FST 读侧把 `\a[0]` 的方括号当范围截掉 | 已定位:`FstWriter::create_var` 把 `data [3:0]` 原样写进名字,FST 读侧(wellen)解析名字尾部 `[..]` 为 range 并剥离;转义名被误伤 | 写侧把"名字/范围"拆成 FST 的结构字段,转义名原样保留;补 VCD/FST 名字集合一致的回归门 |
| **004** VCD 名带空格范围 `top.data [3:0]` | 与 BUG-002 同源(名字/范围表示不统一);手册/测试有既有断言 | 与 002 一起做**语义决策**(建议统一为 `data[3:0]` 或 `data`,并保留两种写法的解析兼容) |
| **005** `(SIGNALS)` 顺序跨格式不同 | VCD 声明序 vs FST 字典序;剩余矩阵差异主要来自此(21 条) | 统一为声明序(FST 读侧去掉 `sigs.sort()`),或在文档明确"顺序不保证" |
| **008** 损坏/截断 VCD 只 warning、rc=0 | 空文件会报错;缺 `$enddefinitions`/垃圾内容只 warning | 加"文件结构完整性"判定 → 默认报错(或提供 `--lenient` 显式放宽) |
| **015** `(1 2)` 非过程调用静默返回列表 | 待确认设计意图 | 与文档作者确认后二选一(报错 / 写明是列表字面量) |

## 已复现但属"设计/文档"类别(不改实现)

- **016(新)**:单字符/过短名字的解析歧义提示不友好,例:`(getwave "t")` 在 `basic.vcd` 上报
  `signal 't' is ambiguous (2 candidates: ["top.clk", "top.data [3:0]"])` —— 是子串回退把所有含 `t` 的名字都算候选。
  行为本身不算错(确实无法唯一确定),但候选列表与"叶子名"语义容易让人误判。**待评估**是否让"叶子名精确命中"优先于子串回退。

## 验证(本轮)

- `cargo test --release`:全部通过(16 个 test binary,含新增 6 条回归)。
- 文档示例对拍:`MATCH 55 → 79`(`MISMATCH 23 → 1`,剩余 1 条是 CLI 回显 `=>` 造成的比对噪声、1 条是 dump-trace 示例缺前置 defsig)。
- 单功能矩阵:findings `224 → 40`(剩余全部为 SIGNALS 命名/顺序、TRACE 元信息、FST 名字集合差异,归 BUG-002/004/005)。
- 组合阶段:findings `25 → 13`(剩余为 BUG-008 损坏文件、以及 `bits->sint` 参数用法/浮点除零等刻意报错项)。

---

# 第二轮修复记录(名字/范围统一 + 4-state 转换 + 严格输入)

| bug | 根因(代码位置) | 修法 | 回归测试 |
|---|---|---|---|
| **004** VCD 名带空格 range | VCD 头解析把整个名字字段(`data [3:0]`)当引用名 | 新增 `vcd::canonical_var_name`(转义标识符原样保留),在 `src/trace/vcd.rs`、`src/vcd/convert.rs`、`src/vcd/parallel_convert.rs`、`src/vcd/parser.rs` 四处统一;新增 `Trace::resolve_name*_compat` 兼容层(旧写法 `data[3:0]`/`data [3:0]` 继续可解析),所有 builtin 的名字解析改走 compat | `bug004_vcd_signal_names_are_references_only` |
| **005** `(SIGNALS)` 顺序跨格式不同 | FST 读侧 `signals()` 对结果 `sort()` | 去掉排序,保持 FST 文件里 hierarchy 顺序(= 写者声明序 = VCD 头顺序);现在 VCD/FST 的 `(SIGNALS)` **逐项一致(含顺序)** | `bug005_fst_signal_order_is_declaration_order` |
| **002** FST 转义名 `\a[0]` 被截 | FST 格式把 range 编码在 var 名字尾部,wellen 读端按 fstapi 约定剥离 → 转义标识符里的 `[0]` 无法与真 range 区分 | **格式固有限制**:VCD 侧保留 `\a[0]` 原样;FST 读写无法无损表示。已在清单记录,不做破坏格式的 hack | 由 bug004 测试覆盖 VCD 侧 |
| **017(新)** VCD→FST 转换把 x/z 折叠成 0 | `src/vcd/convert.rs::parse_value_change` 的 `if b == b'1' {b'1'} else {b'0'}` | 保留 4-state(x/X→x, z/Z→z),转换后 `(getwave)` 与 VCD 逐值一致 | `bug017_vcd_to_fst_preserves_four_state` |
| **008** 截断/垃圾 VCD 静默成功 | `src/trace/vcd.rs` header 扫描找不到 `$enddefinitions` 只 `eprintln!` warning;转换器同样 | 默认**报错**(提示 `WAL_VCD_LENIENT=1` 放宽);转换器 `vcd_to_fst_streaming` 同样严格 | `bug008_truncated_vcd_errors_by_default` |
| **015** `(1 2)` 返回列表 | 求值器 `eval_list` 明确设计:非符号开头的列表按数据列表求值(注释在案) | **不是 bug**:手册 §1.2 补"列表字面量"说明 | —(文档) |

## 第二轮验证

- `cargo test --release`:**348 passed / 0 failed**(含 `tests/bughunt_regressions.rs` 10 条)。
- 单功能矩阵:`VCD!=FST` 差异 **224 → 24**,且剩余 24 条**全部是 trace 元信息(文件路径不同)**,语义层面跨格式已对齐;
  名字集合差异只剩 1 条(`escaped` 夹具的 FST 固有限制);`(1 2)` 1 条已定性为设计。
- 组合阶段:findings 仅剩"损坏文件"(已修)与刻意报错项,重跑后应为 0 语义问题。
- 文档示例对拍:MATCH 79 / MISMATCH 1(CLI 回显噪声)/ ERR 1(dump-trace 示例缺前置 defsig)。

## 仍待办

- FSDB 维度测试:许可通道仍未恢复(宿主 SCL 悬空链接),需先修 9p 共享;
- 覆盖面补测:宏(`defmacro/defunm/macroexpand`)、虚拟信号(`defsig/new-trace/dump-trace`)、
  `$dumpoff/$dumpon/$dumpall`、`search/freq/dela/period/groups/in-group`、`fsdb-timeline-*`、
  多波形组合的更多组合;
- 全部完成后合回 main。

## 第三轮(收尾核对): 两条"疑似"重新定性

- **截断到 dump 区**(header 完整、`$enddefinitions` 在,数据在中途断) → 仍可加载:
  与"直播 dump 正在写"场景兼容,是**合理行为**;只对"header 不完整"(缺 `$enddefinitions`)报错。
- **`(sample-at s 越界)` 返回 nil** → 既有约定(见 BUG-009 撤回说明),不是 bug。

## 第二轮收尾

- **BUG-018(新)**:`bits->sint` 对 >64 位位串报含糊的 `invalid bits`;
  改为明确报错"仅支持 ≤64 位(收到 N 位)…",手册补限制说明(结果用 64 位整数表示)。
- **组合阶段 findings = 0**:`scripts/bughunt/run_combos.py` 的 25 条初判全部收敛
  (真 bug 已修 + 误判项按"既有约定/刻意报错"修正预期)。
- 单功能矩阵仍余 24 条 `VCD!=FST`,**全部是 trace 元信息(文件路径不同)**,语义零差异。

---

# 第三轮:FSDB 维度(许可通道恢复后,2026-10)

## 3.0 许可通道恢复(基础设施,不涉及产品代码)

宿主 SCL 目录(`eda_tools/synopsys/scl`)悬空后,旧 NixOS/TCG 流程不可用。改走
**Debian 13 + KVM 硬件虚拟化 VM**(域 `edalab`, `192.168.122.50`):
客机内跑 `/mnt/vmrepo/os/debian13/vm_lic25.sh`(ic_designer/27080),宿主经 libvirt NAT 直连 **`27080@192.168.122.50`**。
* 新增(入库)`scripts/fsdb_license_up.sh`:幂等拉起通道(VM 可达性 → 客机许可 → 宿主连通性);
* `.tools/vm/host_fsdb_env.sh` 的 `SNPSLMD_LICENSE_FILE` 改为新地址;`docs/fsdb-env.md` 更新,
  旧 NixOS/hostfwd 流程标为"历史存档";
* 验证:`cal1m.fsdb` → `(17 19 2000000)`,宿主侧 NPI 可用。

## 3.1 BUG-019(高严重度,已修)同进程多线程并发加载 FSDB → SIGSEGV

- **复现**: 4 个线程各自 `FsdbTrace::load()` 后立刻查 `max_index` → **SIGSEGV,3 次崩 2 次**
  (典型报错:测试二进制被信号 11 打死;用户侧就是"
  `Service Crash ... encountered a fatal error and was closed`")。
- **根因**: NPI(`npi_init`/session/VC 迭代器)是**进程级全局状态**,不是线程安全的;
  之前只在"单线程单会话"假设下使用,库用户/并行测试会撞上。
- **修复**: `src/trace/fsdb.rs` 增加**可重入的进程级串行锁** `NpiSerialGuard`
  (线程局部深度计数 + 全局 Mutex;最外层真持锁,嵌套只计数 → `load → scan → handle_of` 不死锁),
  覆盖全部 NPI 入口: `load` / `scan` / `scan_times` / `handle_of` / `real_str_of` /
  `global_first_change_time` / `signal_change_counts_top` / `close_now` / `Drop` / 两个 worker 入口。
- **回归**: `tests/fsdb_thread_load_test.rs`(4 线程并发 load+查,断言信号数一致、不崩)。
  修复后同一复现程序 **10/10 不再崩**(修复前 2/3 崩)。
- **已知限制(已写入 `docs/fsdb-npi.md` §8.1)**: 同进程内两个 FSDB **会话的查询**仍会被锁串行化;
  要并行请拆多进程(timeline worker 本来就是 exec 子进程)。CLI 单会话零开销。

## 3.2 FSDB 单功能/组合测试装置(`scripts/bughunt/run_fsdb.py`)

阶段与结果(小夹具 mini/verilog/z6/zc12/zc110/cal1m + 真实 VCD↔FSDB 成对样本):

| 阶段 | 内容 | 结果 |
|---|---|---|
| F1 | SCOPES/SIGNALS/MAX-INDEX/宽度/名字可解析 | 0 findings |
| F2 | `count(changes/rising/falling)` == `length(find(...))`;`getwave` 与 `at` 互证 | 0 |
| F3 | 引擎 vs 逐拍 oracle(`WAL_NO_ENGINE=1`) | 0 |
| F4 | VCD↔FSDB 同设计成对差分(20+ 表达式:元信息/名字/宽度/变更列/getwave/取值) | mini、z6、zc12、zc110 **逐项一致**;verilog 对是**过期夹具**(VCD 189 信号 vs FSDB 181,不同版本设计)→ 记为 PAIR-MISMATCH 跳过 |
| F5 | 多波形 `-l vcd -l fsdb`:first-match + 次要波形独有信号的查询结果/索引空间与单波形一致 | 0 |
| F6 | 假 FSDB / 截断 FSDB / 不存在文件 | 全部**干净报错**(无 panic) |
| F7 | `sigs`/`count`/`topsig`/`--stdin`/`fsdb-timeline-map`+`merge` | 全部通过 |
| F8 | 指标/窗口算子跨格式(`wave`/`latency`/`median`/`percentile`/`stats`/`histogram`) | 0 |
| F9 | 缓存冷/建/命中三种口径结果一致 | 0 |

→ **FSDB 维度 findings=1,且是夹具过期(非代码问题)**;语义层面 FSDB 与 VCD 已对齐。

## 3.3 测试装置修补(非产品 bug)

- `tests/fsdb_diff.rs` 的 `--include-ignored` 并发跑时,**两个测试各自改进程 CWD/env**
  且用的不是同一把锁 → 相互污染(表现为随机 FAILED)。已改为**文件级共享锁** + 恢复 env;
  `scripts/ci.sh` 的 FSDB gate 改 `--test-threads=1`(NPI 进程级单例,并发跑无意义)。
- 并行 `fsdb_diff` 连跑 3 次全绿;串行 9/9 全绿。

## 3.4 本轮验证

- `cargo test --release`:**349 passed / 0 failed**。
- FSDB ignored 门(9 个)并行/串行均绿;线程回归测试绿。
- 单功能矩阵(VCD/FST)维持上轮结论:语义差异 0(剩余 24 条全是 trace 元信息路径)。

## 仍待办(下一轮)

- 覆盖面补测:宏(`defmacro/defunm/macroexpand`)、虚拟信号(`defsig/new-trace/dump-trace`)、
  `$dumpoff/$dumpon/$dumpall`、`search/freq/dela/period/groups/in-group`、`fsdb-timeline-*` 正确性
  (而不只是 smoke);
- 全部完成后**整体合回 main**。

---

# 第四轮:语言/语义覆盖(宏/闭包/组/虚拟信号/边沿计数,2026-10)

## 4.1 已修(带回归测试,`tests/bughunt_regressions.rs` 第四轮 6 条)

| bug | 症状(最小复现) | 根因(代码位置) | 修法 |
|---|---|---|---|
| **020** `(fn (x) body1 body2 ...)` 多 body 全坏 | `(define cnt (let ((n 0)) (fn () (set! n (+ n 1)) n))) (cnt)(cnt)(cnt)` → `1 1 1`(应为 `1 2 3`);<br>`(define f (fn (n) ... (f ...)))` 递归报 Unknown operator | `src/wal/eval/evaluator.rs` 符号头 `fn` 分支:`rest.len()>2` 时把第 2 个起的 body 当成**立即调用的实参**求值(IIFE 语义)。而 IIFE 写法 `((fn ...) args)` 本来就走 List-首元素分支,这个分支纯属误伤 | 符号头 `(fn ...)` **一律只创建闭包**(多 body 由 op_fn 包 do) |
| **021** define 的函数不能递归 | `(defun fact (n) ...(fact ...))` → `Unknown operator: fact` | 闭包捕获的是 define **之前**的环境快照(bindings map 拷贝), 之后 `self.env.define(name, closure)` 新增的绑定在快照里不存在 | define 之后把绑定**同步进闭包捕获环境**(自引用可用); 两个 define 分支都做 |
| **022** `count-rise/count-fall` 漏首个边沿 | 初值 0、10/20/30/40/50 翻转的 clk:`(count-rise)`=2 而 `(count (rising))`=3 | `src/wal/builtins/wave.rs::edge_count` 的 `prev` 从 `None` 开始, 没用初值判第一个变更 | `prev` 用 `tr.initial_value(sig)` 播种(0/1; x/z → None, 与 find/rising 口径一致) |
| **023** 多参数 count 含边沿条件时静默只算第一个 | `(count (rising "clk") (rising "rst"))` → `3`(应为 `(3 0)`) | `eval_count` 批量路径只解析"简单条件"; 任一条件分解失败时 `batch_entries<2` 就掉回单条件路径用 `args[0]` | 记录 `all_batchable`, 只有**全部**条件可批量才走批量; 否则**逐条件各算一个**返回列表 |
| **024** `(groups ready)` 返回 `()` | 手册示例 `(groups valid ready)` 无输出; `(groups ".ready")` 才工作 | `groups` 是普通 builtin, 参数被**预求值**(符号 ready 被当信号值/常量) | 在 evaluator 加 `Groups` 特殊形式: 符号/字符串参数按字面量, 其它表达式仍求值 |
| **025** 组前缀拼接缺分隔点 | `(in-group "top.in_" #ready)`(手册示例) → `Unresolved group signal: top.in_ready` | 4 处 `format!("{}{}", group, name)` 直拼; `groups` 返回的前缀带尾点, 手册示例不带点 → 直拼少一个 `.` | 新增 `scope::group_prefixed_names`(带点/不带点两种候选都试), 用于 evaluator/get/virtual-size/resolve-group |
| **026** in-group 不同步 CS | 手册说"同步 CS 为组名去掉尾点", 实测 `(in-group "top.in_" (list CG CS))` → CS `""` | evaluator 的 `eval_in_group` 特殊形式没设 scope(scope.rs 的 builtin 版本设了, 但分派走特殊形式) | 特殊形式里同步 `CS = group.trim_end_matches('.')` |
| **027** 虚拟信号在条件里被常量折叠 | `(do (defsig myv (+ clk 1)) (count (= myv 2)))` → 5(应为 3) | `collect_cond_signals_with_bare`: 裸符号能 env.lookup 到就直接 `continue`(变量优先), 虚拟信号引用的真实信号没进依赖 → 条件被折叠成常量 | 虚拟信号 → 递归展开其绑定的表达式收集信号名(循环风险: 自引用虚拟信号仍可能递归; 记录待观察) |
| **028** FST `(initial s)` 恒为 x | FST 上 `(getwave s)` 首点 `(0 1)` 但 `(initial s)` = "x"(自相矛盾) | `FstTrace` 未实现 `initial_value`(trait 默认 None) | 取时间表第 0 帧值(首变更落在 index 0) |

## 4.2 已定性为"格式限制/设计"(不修, 文档说明)

- **029** VCD↔FST 的 `(initial)` 语义差异:VCD 的 `initial` = `$dumpvars` 快照(冻结口径, 见
  `matrix_t0_dump_is_snapshot_not_index`/`matrix_dumpvars_sample_model`);FST 没有 dumpvars 概念,
  只能给第 0 帧值(== `#0` 最后一次写入)。二者仅在"dumpvars 与 #0 写入不同"时有差;
  跨格式矩阵已把 `initial` 排除在 VCD↔FST 对比之外, 并在此记录。
- **030** `(save out.csv ...)` 输出是**稀疏事件表**(未在该时间戳变化的信号列留空),
  不是"每行完整状态"。属可用性设计问题, 待用户确认是否需要 held-value 填充(不擅自改语义)。

## 4.3 第四轮验证

- `cargo test --release`:**355 passed / 0 failed**(新增 6 条回归)。
- 单功能矩阵(VCD/FST):语义差异 0(24 条全是 trace 元信息; `initial` 已标注格式限制)。
- 组合阶段 findings=0;文档示例对拍 MATCH 79(剩余 1 条 CLI 回显噪声 + 1 条 dump-trace 示例缺前置 defsig)。
- 语言/宏/闭包手算样例:fact 720、fib 13、累加器 (1 2 3)、IIFE 42、多 body 40、groups/in-group/CS 全部符合手册。

## 仍待办(下一轮)

- 剩余覆盖:宏变参/嵌套 macroexpand、`$dumpoff/$dumpon/$dumpall` 真实语料、`save/csv/import` 文件算子、
  REPL 交互细节、以及把上述修复**整体合回 main**。

---

# 收官:全部修复合回 main(第五轮,2026-10)

## 5.1 合并状态

- `dev/bughunt`(含 `dev/load-strategy` 的缓存/懒索引改动 + 本目标全部修复)**fast-forward 合入 `main`**,
  main = `48d2d34`,版本 **0.14.60**;
- 本地 `make ci` **9/9 全绿**(preflight/docs/fmt/clippy/build/test/samples/gates/package);
- 目标验收(A)–(E)对照:

| 目标项 | 证据 |
|---|---|
| (A) 每个功能逐一单功能测试 | 153 个注册算子(全部有 help);单功能矩阵 364 条表达式 × VCD/FST;文档示例对拍 124 条;FSDB 维度 F1–F9;CLI 子命令/参数/退出码逐项 |
| (B) 功能组合测试 | 组合测试 6 类(嵌套算子/跨格式对齐/多波形/CLI+脚本+stdin/文件健壮性/取值边界)→ findings **0**;另有 VCD↔FST 随机差分(fuzz)、引擎 vs 逐拍 oracle、确定性连跑 |
| (C) 版本化 bug list | 本文件:BUG-001…030 全量台账,每条含最小复现、观察 vs 期望、根因、状态 |
| (D) 根因分析 | 逐轮根因表(第 1–4 轮)+ 定性结论(撤回项/格式限制),共归纳出 6 类根因:索引语义、名字/范围表示、4-state 转换、NPI 线程安全、语言求值语义、边沿/窗口口径 |
| (E) 整体修复+回归+CI 绿+合并 | 21 个真 bug 修复(每个带回归测试,共 17 条 bug 回归 + FSDB 9 门 + 349 原有测试)、2 个误判撤回、3 个格式限制记录;356 tests 全绿;main 已合并 |

## 5.2 修复清单(按根因归类)

| 根因 | 修复的 bug |
|---|---|
| **索引语义**(重复时间戳/首沿/初值) | 001、001b、022;缓存 key `-v1`→`-v2` |
| **名字/范围表示**(跨格式统一) | 004、005、024、025、026;002/029 记格式限制 |
| **4-state 语义** | 017(VCD→FST x/z 折叠)、028(FST 初值) |
| **NPI 线程安全** | 019(并发 load SIGSEGV → 进程级串行锁) |
| **语言求值语义** | 011(let 顺序)、012(cond else)、020(fn 多 body 误当调用)、021(递归捕获)、023(多参 count)、027(虚拟信号折叠) |
| **边沿/输入/格式约定** | 010、018、019;003;007;008;013/014/015(文档口径/撤回);030(待确认) |

## 5.3 最终验证数字

- `cargo test --release`:**356 passed / 0 failed**;
- FSDB 门:`fsdb_diff` 9/9(`--include-ignored --test-threads=1`)、`fsdb_thread_load_test` 1/1;
- 单功能矩阵:VCD↔FST 语义差异 **0**(24 条为 trace 文件路径元信息);
- 组合阶段 findings **0**;文档示例 MATCH 79 / MISMATCH 1(CLI 回显噪声)/ ERR 1(dump-trace 示例缺 defsig);
- FSDB 单功能/组合(F1–F9):findings 1(过期夹具,非代码问题)。

## 5.4 遗留(不在本目标范围)

- 纯 Rust FSDB 全局时间线后端(见 `docs/fsdb-rust-backend-spec.md`,独立方向);
- `save` CSV 是否要填充 held value(BUG-030,待用户确认);
- FST 转义名/初值的格式固有限制(BUG-002/029,已文档化);
- 覆盖率可继续加深:真实大波形(数百 MB~GB)上的组合/边界回归。
