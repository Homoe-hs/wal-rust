# wal-rust Agent-first CLI 协议（草案 v0，占位版）

> 目标：让**从未见过 WAL 的 LLM** 以最少 token 完成波形 debug。
> 状态：**占位版**——`why` 输出形态与 `prep` 预检内容以破坏性实验结果为准（见 `exp/`），
> 本文件先锁定协议骨架（工具面 / 契约 / schema），实验后只修订正文部分字段。
> 参照：wave-mcp（Tencent 蓬莱，MCP+CLI 同名同参）、wavepeek（CLI+skill）、
> Anthropic Writing Tools for Agents、orchestratectl AGENTS-AI-FIRST-CLI、pedroanisio AI-focused CLI。

## 0. 定位与边界（产品主张）

- **wal-rust = 运行态呈现器**：波形是仿真器**运行中的状态**轨迹——我们只呈现/查询这份"运行态事实"，不做任何结构推导。
- **非目标：读取/理解 RTL 与网表**（驱动链、扇入扇出、声明位置等属 wave-mcp+pyslang 领域）——wal-rust 不解析源码，不建网表，不需要 RTL 文件；
- **推论一（独立性）**：输入只有波形文件 → 无 license、无源码依赖、无网表同步/一致性校验负担；
- **推论二（A 口径的依据）**：因为不做结构理解，软件侧**零启发式**（候选信号 = 窗口内活跃 Top-N，不做 scope/命名过滤）；RTL 结构知识由 LLM 自带（它会读信号名、可以看源码），软件只保证"运行态事实"绝对正确、可复现、不幻觉。

## 1. 设计原则（拍板过的 7 条）1. **严格校验，不静默修正**：错误消息包含实际非法值 + 期望格式 + 可用选项；
2. **结构化输出**：默认 JSON（AI 面紧凑），`--pretty` 为人面 opt-in；stdout=数据，stderr=错误；
3. **零交互**：无 Y/N / pager / $EDITOR；破坏性动作显式 flag；
4. **自描述**：`wal doc` 一次输出机器可读完整规范（schema + 语法 + 模板 + 错误格式）；
5. **可组合**：只读查询幂等；`--json-lines` 流式；stdin `-` 支持；统一 flag 命名；
6. **会话态**：`prep` 产出会话（文件即 ID），`--resume` 可重放、可审计；
7. **有界输出**：结果默认截断（轨迹 ≤ N 变化/信号），带 `truncated: true`。

## 2. 工具面（第一版）

| 命令 | 说明 | 备注 |
|------|------|------|
| `wal prep -l <waveform>...` | 唯一第一入口：load + 会话文件 + 摘要 | 摘要见 §4 |
| `wal q '<expr>'` | WAL 表达式查询（完整语义） | 探针档=WAL 直通 + doc 模板 |
| `wal why <signal> at <t>` | 归因链（信号级，纯波形） | 形态见 §5，实验定稿 |
| `wal session <id> --info / --resume` | 会话查看/恢复 | |
| `wal doc [--section]` | 自描述（见 §3） | 语法+模板+schema 一次给全 |

命名约定：动词-名词（`prep`/`q`/`why` 为高频探针，保持短）；`--json` 默认开启，`--pretty` 切换人面；退出码 `0` 成功 / `1` 用户错误（LLM 可修复）/ `2` 系统错误。

## 3. `wal doc` 自描述（LLM 唯一的常驻替代品）

输出一块 Markdown + 一份内嵌 JSON schema：
- 3 个高价值模板（`count`、`find`、`why`）各带一个真实示例；
- 全部探针动词表（名称、一句话语义、参数、示例、常见错误）；
- 错误消息格式说明（LLM 读了错误能自修复）；
- 输出 schema（结果 JSON 的字段表）。

目标：**一次 `wal doc` ≤ 800 token**，之后 LLM 的查询只需 `q '...'` ≈ 30-80 token。

## 4. `prep` 摘要 schema（占位）

```json
{
  "session": "hvm2sm-20260902-a",
  "files": ["tb_hvm2sm_single.fst"],
  "timescale": "10ps",
  "time_range": [0, 24500],
  "signals": [
    {"name": "top.sm_wr_en", "width": 1, "changes": 312, "xz_frac": 0.0, "note": "active"}
  ],
  "flags": {
    "constant_signals": ["...", 3],
    "xz_signals": ["...", 2],
    "min_interval_ns": {"sig": "clk", "value": 10},
    "suspicious": ["..."]
  },
  "truncated": false
}
```

`--fast`（默认）只做信号级统计（已有索引，成本低）；`--deep` 预留（需要毛刺检测等新引擎能力）。

## 5. `why` 输出形态（占位：B 主 A 辅）

候选（实验对比，见 `exp/README.md` 裁判协议）：
- **A 结论型**：首行断言 + 证据摘要（"data[3] 自 t=128 起为 x，携带源"）；
- **B 证据链型**（默认）：时间线表——每步 `t → 信号 → 值 → 变化间隔 → 备注`，逐步到 T；
- **C 假设枚举型**：N 个候选上游 + 每个的验证查询模板。

占位：`why` 输出 = 首行 A 级结论 + B 级证据链主体；C 以 `--hypotheses` 可选开启。
**确定性声明**：输出标注 `level: "signal-level"`，不声称 RTL/网表根因（网表分析明确为非目标）。

## 6. 错误消息格式（契约）

```
{"error": "invalid value 'data3[0]' for <signal>", "expected": "信号必须已在会话中",
 "available": ["top.clk", "top.data[3]", "top.sm_wr_en"], "hint": "用 quotes: why \"top.data[3]\" at 200"}
```

规则：`error`=描述+实际值；`expected`=期望格式；`available`=可选项；`hint`=可执行纠错模板。错误一律单行 JSON 到 stderr，exit 1。

## 7. 会话文件

`~/.wal/sessions/<id>.json`：trace 路径 + 摘要缓存 + 查询历史（重放/审计）。
Q 命令用会话 id 或"最近会话默认"；`-l` 仅在 `prep` 使用。

## 8. Skill 模板（~/.dsh/skills/wal/SKILL.md，草案）

```markdown
# WAL 波形分析
1. 用 `wal prep -l <波形>` 加载并看摘要 JSON（信号、x/z 名单、常量信号）。
2. 查询用 `wal q '<WAL 表达式>'`；常用模板见 `wal doc`（先跑一次拿模板）。
3. 定位 x/z 用 `wal why <信号> at <时刻>`（证据链输出，含确定性声明）。
4. 每次调用输出均含 token 精简的 JSON；出错时先读 error/expected/hint 再重试。
```

## 9. 定位对照（与 wave-mcp / wavepeek）

| | wave-mcp | wavepeek | wal-rust 目标 |
|---|---|---|---|
| 波形 | FST/VCD | VCD/FST | **VCD/FST/FSDB（零专有库）** |
| 查询 | 27 个 MCP 工具（原语） | CLI 子命令 | **WAL 表达式（可编程）+ 模板** |
| 归因 | trace_x（网表驱动） | 无 | **why（纯波形信号级）** |
| 规模 | 百万 scope | — | **155GB VCD 场景** |

## 10. 里程碑

1. ✅ 决策：CLI-only（MCP 可后置，桥接承诺=命令↔工具一一映射）；
2. ⏳ 破坏性实验：定稿 `why` 形态（A/B/C）+ `prep` 预检内容（`exp/`）；
3. ⏳ 修订本文（实验数据入档）；
4. ⏳ 实现 alpha：`prep`/`q`/`doc` + 会话 + JSON 契约 → `why`；
5. ⏳ 验收：注入 bug 矩阵上"定位轮次/总 token"基准（验证文化，对标 wave-mcp 310 万调用）。

## 11. 破坏性实验结论（终稿，2026-09-03，实验数据：exp/results.csv）

数据：exp/results.csv；案例 B1（响应生成侧缺失）/B2（选取侧饿死）/B3x（未初始化 x）/B4（wstrb 截断）。

1. **最小轮数模式 = 纯统计差分（`why --baseline`）**：B1 与 B3x 两个独立案例中，
   LLM 仅凭差分注入（额外 0-1 次验证）即命中根因——`rounds=1`；无基线时的
   "呈现+验证"（M1）与裸查（M0）在伪案例中表现为「机制脑补 + 忽略 x」。
2. **x 必须是一等公民**：B3x 的根因信号恒 x（is-x=20001）在差分中独占首位；
   三处独立 LLM 会话（M0/M1 早期案例）均倾向"把早期 x 当正常复位态排除"——
   **探针档必须提供 `is-x` 且差分必须含 x 维度**（x_win）。
3. **M2 不是银弹（排序局限）**：B2（计数类根因）根因簇不在 diff top20——
   被下游蔓延信号的"事务计数差"淹没。差分排序对"恒值/x 类"强、对"计数类"弱；
   需 LLM 语义挑选或未来引入“结构分组”排序。
4. **呈现+验证（M1）是兜底主线**：真实场景大多无基线；M1 给 target 事实 +
   窗口上下文 + verify 模板，LLM 的 RTL 知识负责假设、软件负责验证。
5. **实验纪律**：破坏性实验必须先验证同环境基线 PASS（B3 反例：现象来自
   TB/仿真器兼容性而非注入，golden 作废）。

## 12. 最终结论（取代 §11；2026-09-03，真实场景协议）

**协议修正**（用户两轮校正）：真实 bug 调试 = **错误 RTL 在手 + 该 RTL 的波形**，"正确版"不存在；
工具 = 纯原语（q：count/edges/is-x/get）或 +xwin。

**两异质案例实测定论**（判决 = 10 分钟内收敛 + 命中 golden）：

| 案例（bug 类） | 工具形态 | 结果 |
|---------------|---------|------|
| B3x（未初始化 x） | 错误RTL + 纯原语 | ✅ 10 次调用命中（源码定位 r0_sel_rd_p 无赋值 + is-x=20001 验证） |
| B4（wstrb 截断/数据损坏） | 错误RTL + 纯原语 | ✅ 10 次调用命中（:154 标量声明 + VCD{w0,1}/全1=0/数据路径正常 三重隔离） |

**结论（对"呈现/候选/why 设计"的否定）**：
1. **最小闭环足够**：读代码提假设 + 精确原语验证（is-x/count/edges）——两个异质案例均 ~10 次收敛；
   早期"批量呈现/候选列表/统计差分"设计：B2 案例根因不在 top20（噪声淹没）、B3 伪现象诱导
   "机制脑补"——**对 agent 是误导性噪声，已全部退役**；
2. **xwin 降级为可选辅助**：其"区间切片"能力在"无源码"场景（旧协议 P2）有价值
   （17 次 vs 纯原语在同样禁 RTL 协议下未收敛），但在"源码+RTL"主场景被
   "定位代码+is-x"替代——保留为 `--x-window` 可选原语，不进主工具面；
3. **工具面定稿**：`wal q`（原语）+ `wal doc`（模板/自描述）+ `prep` 仅做
   "设计信号筛选 + 统计数量"（不做任何候选/相关性呈现）；错误消息契约照 §6。

## 13. 工具面消融归因（2026-09-03，实验：exp/ablation/SUMMARY.md）

**结论**：主工具面定稿 = **RTL 白名单（决定性）+ wal 原语 + prep 摘要 + doc 模板**；**xwin 降级为可选辅助**（−tool 45 查询优于 −xwin 141，xwin 边际被策略噪声淹没；仅无源码场景保留价值）。
表格与失效模式（无源码→表现当根因；TB 自检假失败）见 exp/ablation/SUMMARY.md。
