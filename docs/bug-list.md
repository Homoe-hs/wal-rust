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

## BUG-002 FST 读侧名字丢失向量范围,并把**转义标识符**里的方括号误当范围截掉

- 严重度:**高**(转义名整条查询失效;向量名跨格式不一致)
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

## BUG-004 VCD 信号名带范围且用**空格**分隔(`top.data [3:0]`),自然写法查不到

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

## BUG-005 `(SIGNALS)` 顺序跨格式不同(VCD 声明序 vs FST 字典序)

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

## BUG-008 损坏/截断波形只 warning 不报错(rc=0,空/部分结果)

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

## BUG-009 `(sample-at s 越界索引)` 静默返回 nil,负索引却报错

- 严重度:**中**(越界取值应报错,不能给 nil 让上层继续算)
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

## BUG-015(待确认)`(1 2)` 数字当函数调用静默返回列表

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
