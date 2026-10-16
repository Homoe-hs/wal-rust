#!/usr/bin/env python3
"""check_bench_regression.py — 离线性能回归闸(纯标准库, 不需要波形/许可)。

读 `bench/perf-history.csv`(schema: version,date,op,fixture,wall_s,rss_mb,note),
对**每个 op** 取"基线版本"与"新版本"的**中位数**(同版本同 op 的多行就是重复测量),
算变化率; 超过阈值(默认 20%)判回归并以**非 0** 退出。

仓库约定: "回归 >20% 必须写明原因" —— 所以这个闸只做一件事: 让 >20% 的地方红出来,
并把可复现的命令(CSV 的 note 里带的 cmd)一起打出来。

用法:
  scripts/check_bench_regression.py                          # 自动选基线/新版本
  scripts/check_bench_regression.py --baseline 0.14.46 --new 0.14.47
  scripts/check_bench_regression.py --op 'fsdb-feat-(session|session-load)'
  scripts/check_bench_regression.py --threshold 15 --gate-rss --json

判读口径:
  * 基线版本(baseline): 显式 `--baseline V`; 否则 = 每个 op **最早出现**的版本
    (第一行就是那条 op 的历史基线)。
  * 新版本(new): 显式 `--new V`; 否则 = CSV 里**最后出现**的版本(按文件顺序, 追加即时间序)。
  * 不可测的值(`-`/空/非数)与只在一侧出现的 op 会被跳过并计入 "skipped"。
  * wall_s 一律是闸的对象; rss_mb 默认只报, 加 `--gate-rss` 才参与判定。
  * 噪声地板: 新版本 wall_s < `--min-wall`(默认 0.05s)的 op 只报不判 —— 毫秒级数字
    的 20% 是抖动, 不是回归。
"""
from __future__ import annotations

import argparse
import csv
import json
import re
import statistics
import sys
from collections import defaultdict

NA = ("-", "", "na", "n/a", "NA", "none", "-1")


def parse_num(s: str):
    """返回 float 或 None(不可测)。"""
    if s is None:
        return None
    t = s.strip()
    if t.lower() in NA:
        return None
    try:
        return float(t)
    except ValueError:
        return None


def read_history(path: str):
    """宽松读: 老行里偶有未加引号的逗号(字段数 >7), 把多出来的并回 note。"""
    rows = []
    with open(path, newline="", encoding="utf-8", errors="replace") as fh:
        rd = csv.reader(fh)
        for i, raw in enumerate(rd):
            if not raw or all(not c.strip() for c in raw):
                continue
            if i == 0 and raw[0].strip().lower() == "version":
                continue
            if len(raw) < 6:
                continue
            rows.append(
                {
                    "version": raw[0].strip(),
                    "date": raw[1].strip(),
                    "op": raw[2].strip(),
                    "fixture": raw[3].strip(),
                    "wall": parse_num(raw[4]),
                    "rss": parse_num(raw[5]),
                    "note": ",".join(raw[6:]).strip(),
                }
            )
    return rows


def version_order(rows):
    """按文件出现顺序去重(追加写 = 时间序)。"""
    seen, order = set(), []
    for r in rows:
        if r["version"] not in seen:
            seen.add(r["version"])
            order.append(r["version"])
    return order


def med(values):
    vals = [v for v in values if v is not None]
    if not vals:
        return None
    return statistics.median(vals)


def fmt(x, unit=""):
    return "-" if x is None else f"{x:.3f}{unit}"


def pct(new, base):
    if new is None or base is None or base == 0:
        return None
    return (new - base) / base * 100.0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description="离线性能回归闸: 比对 perf-history.csv 的基线版本行与新版本行(默认 >20% 判红)",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    ap.add_argument("--csv", default="bench/perf-history.csv", help="perf-history.csv 路径")
    ap.add_argument("--baseline", default=None, help="基线版本号(默认: 每个 op 最早的版本)")
    ap.add_argument("--new", default=None, help="新版本号(默认: CSV 里最后出现的版本)")
    ap.add_argument("--threshold", type=float, default=20.0, help="回归阈值百分比(默认 20)")
    ap.add_argument("--op", default=None, help="只比对匹配该正则的 op")
    ap.add_argument("--min-wall", type=float, default=0.05,
                    help="新版本 wall_s 低于此值的 op 只报不判(默认 0.05s, 噪声地板)")
    ap.add_argument("--gate-rss", action="store_true", help="RSS 也参与判定(默认只报)")
    ap.add_argument("--json", action="store_true", help="输出 JSON")
    ap.add_argument("--quiet", action="store_true", help="只输出结论行")
    args = ap.parse_args(argv)

    try:
        rows = read_history(args.csv)
    except OSError as e:
        print(f"error: 读不了 {args.csv}: {e}", file=sys.stderr)
        return 2
    if not rows:
        print(f"error: {args.csv} 里没有数据行", file=sys.stderr)
        return 2

    order = version_order(rows)
    new_ver = args.new or order[-1]
    if new_ver not in order:
        print(f"error: CSV 里没有版本 {new_ver}; 现有: {', '.join(order[-12:])}", file=sys.stderr)
        return 2
    if args.baseline and args.baseline not in order:
        print(f"error: CSV 里没有版本 {args.baseline}; 现有: {', '.join(order[-12:])}", file=sys.stderr)
        return 2

    by_op = defaultdict(list)
    for r in rows:
        by_op[r["op"]].append(r)

    op_filter = re.compile(args.op) if args.op else None
    results, regressed, skipped = [], [], []
    for op in sorted(by_op):
        if op_filter and not op_filter.search(op):
            continue
        orows = by_op[op]
        new_rows = [r for r in orows if r["version"] == new_ver]
        if not new_rows:
            skipped.append((op, "新版本没有这一行"))
            continue
        if args.baseline:
            base_rows = [r for r in orows if r["version"] == args.baseline]
            base_ver = args.baseline
        else:
            base_ver = next(r["version"] for r in orows)          # 最早出现
            base_rows = [r for r in orows if r["version"] == base_ver]
        if not base_rows:
            skipped.append((op, f"基线 {base_ver} 没有这一行"))
            continue

        nw, bw = med([r["wall"] for r in new_rows]), med([r["wall"] for r in base_rows])
        nr, br = med([r["rss"] for r in new_rows]), med([r["rss"] for r in base_rows])
        dw, dr = pct(nw, bw), pct(nr, br)

        verdict, why = "ok", ""
        if nw is None or bw is None:
            verdict, why = "n/a", "wall 不可测"
        elif nw < args.min_wall and bw < args.min_wall:
            verdict, why = "n/a", f"低于噪声地板 {args.min_wall}s"
        elif dw is not None and dw > args.threshold:
            verdict, why = "REGRESS", f"wall +{dw:.1f}% > {args.threshold:g}%"
        if args.gate_rss and dr is not None and dr > args.threshold:
            verdict = "REGRESS"
            why = (why + "; " if why else "") + f"rss +{dr:.1f}% > {args.threshold:g}%"

        rec = {
            "op": op,
            "fixture": new_rows[-1]["fixture"],
            "baseline_version": base_ver,
            "new_version": new_ver,
            "baseline_wall_s": bw,
            "new_wall_s": nw,
            "wall_delta_pct": dw,
            "baseline_rss_mb": br,
            "new_rss_mb": nr,
            "rss_delta_pct": dr,
            "n_baseline": len([r for r in base_rows if r["wall"] is not None]),
            "n_new": len([r for r in new_rows if r["wall"] is not None]),
            "verdict": verdict,
            "reason": why,
            "cmd": new_rows[-1]["note"],
        }
        results.append(rec)
        if verdict == "REGRESS":
            regressed.append(rec)

    if args.json:
        print(json.dumps({"baseline": args.baseline, "new": new_ver,
                          "threshold": args.threshold, "results": results,
                          "regressed": [r["op"] for r in regressed],
                          "skipped": [{"op": o, "why": w} for o, w in skipped]},
                         ensure_ascii=False, indent=2))
        return 1 if regressed else 0

    if not args.quiet:
        print(f"perf-history: {args.csv}")
        print(f"基线版本 = {args.baseline or '(每个 op 最早出现的版本)'}   新版本 = {new_ver}   阈值 = >{args.threshold:g}%")
        print()
        hdr = f"{'verdict':<9} {'op':<38} {'base_wall':>10} {'new_wall':>10} {'Δwall':>9} {'base_rss':>9} {'new_rss':>9} {'Δrss':>8}  n"
        print(hdr)
        print("-" * len(hdr))
        for r in results:
            dw = "-" if r["wall_delta_pct"] is None else f"{r['wall_delta_pct']:+.1f}%"
            dr = "-" if r["rss_delta_pct"] is None else f"{r['rss_delta_pct']:+.1f}%"
            print(f"{r['verdict']:<9} {r['op']:<38} {fmt(r['baseline_wall_s']):>10} "
                  f"{fmt(r['new_wall_s']):>10} {dw:>9} {fmt(r['baseline_rss_mb']):>9} "
                  f"{fmt(r['new_rss_mb']):>9} {dr:>8}  {r['n_baseline']}/{r['n_new']}")
        print()

    if skipped and not args.quiet:
        print("跳过:")
        for op, why in skipped:
            print(f"  - {op}: {why}")
        print()

    if regressed:
        print(f"❌ 回归(>{args.threshold:g}%): {len(regressed)} 个 op")
        for r in regressed:
            print(f"  * {r['op']} ({r['fixture']}): {fmt(r['baseline_wall_s'])}s → "
                  f"{fmt(r['new_wall_s'])}s ({r['wall_delta_pct']:+.1f}%) [{r['reason']}]")
            if r["cmd"]:
                print(f"      复现: {r['cmd'][:220]}")
        print("  按仓库约定: 回归 >20% 必须在提交/CHANGELOG 里写明原因。")
        return 1

    print(f"✅ 无回归(阈值 >{args.threshold:g}%, 比对 {len(results)} 个 op; "
          f"跳过 {len(skipped)} 个)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
