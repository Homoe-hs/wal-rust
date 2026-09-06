# wal-rust 内网反馈(0.10.10)修改清单 — 复评整理

> 状态: **A 批 + C 批已完成**; **B 批按用户决定关闭**:
> ① FST 名乱码不做诊断(读路径已切 wellen,旧手写解析器已退役,乱码应属旧解析器);
> ② FSDB 不做 convert 封装(已有逆向直读路线)。

> 复评基准: 本仓库 0.11.3; 关键前提: FST 读路径已切换 wellen 后端(自研 reader 退役),
> roundtrip(自研 FstWriter 写 → wellen 读)测试全绿。

## A. 优先修 —— 低风险小改批(可一次提交)

| # | 来源 | 内容 | 位置 | 校验 |
|---|---|---|---|---|
| A1 | P2 print 刷屏 | WList Display 对超长列表截断(如 `...(90662 items)`);列表打印不超过 ~2KB | `src/wal/ast/wlist.rs` (fmt::Display) | 打印 9 万信号 ≤ 几 KB |
| A2 | P2 bool/int 混现 | `&&`/`||` 输出统一为 `true/false`(与 `(> 5 3)` 一致;文档"1/0 ≡ #t/#f"与实际输出对齐) | core.rs 逻辑算符 | `(&& 1 1 0)` → `false`;`(count (&& ...))` 行为不变 |
| A3 | P2 未知操作符 | 报错补 top-3 候选(0.11.3 已有 "Did you mean: first?" 近邻 + "Try (help)",无近邻时给候选) | evaluator.rs 建议生成(≈:77) | `(signals "x")`/`(break)` 报错含候选 |
| A4 | P3 错误语义 | 默认行为不变(run 模式错误继续);新增 `--halt-on-error` 供脚本/CI | main.rs `run_wal_file` | 开关生效;默认不回归 |
| A5 | P1 list 诉求 | 加 `(take N lst)` 别名;文档写明 `(slice SIGNALS 0 1)`、`(slice (find-sig "pat") 0 5)` 已可用(SIGNALS 是一等 list) | list 内建 + doc | `(take 5 SIGNALS)` 正常 |
| A6 | 文档账 | AGENTS.md "FST endian auto-detect" 旧描述(退役 reader)→ 更新为 wellen 后端 | AGENTS.md | 无 |

## B. 按用户决定关闭(不再实施)

- ~~B1 · P0 FST 名称乱码~~ — **关闭**: 读路径已切 wellen(`wellen::simple::read`),旧手写解析器退役;
  乱码大概率属旧解析器问题,无需诊断(且该解析器已从波形查询路径移除)。
- ~~B2 · FSDB 通路~~ — **关闭**: 不做 fsdb2vcd convert 封装;FSDB 已有逆向直读路线。

## C. 可选 / 排期

- C1 一键查询 CLI: `wal count <wave> <sig>` / `wal sigs <wave> <pat>` / `wal topsig <wave>`(薄壳,shell/CI 人机两用;LLM 侧不受益)
- C2 VCD 场景"最近似信号" top-3 候选(与 FST 已有 "Available signals (first 5)" 对称)
- C3 `--version` 输出安装路径(帮助对账包装侧版本号)

## D. 不改(附原因)

- P1 "SIGNALS 不是一等 list" — 0.11.3 已解决(`(slice SIGNALS 0 1)` 实测正常;find-sig 已覆盖"匹配 xxx"诉求)
- P2 "未知操作符只报 Unknown" — 已给 "Try (help)" + "Did you mean"(实测)
- P2 版本/包装对齐 — 本仓库单一来源 Cargo.toml = 0.11.3;0.8.5/0.10.10 属打包侧/旧版环境
- 加分项 "FST Available signals (first 5)" — 已存在(VCD 侧对称化见 C2)
