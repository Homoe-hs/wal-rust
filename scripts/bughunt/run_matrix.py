#!/usr/bin/env python3
"""bug-hunt 矩阵跑测器(单功能阶段)。

阶段:
  P1 语料筛选 + 语法/求值冒烟(每个表达式跑一次, 检测 panic/崩溃/非预期错误)
  P2 跨格式差分(VCD vs FST, 12 组边界夹具 × 成功用例)
  P3 引擎 vs 逐拍 oracle(WAL_NO_ENGINE=1)差分
  P4 非确定性(同一用例连跑两次, 输出必须一致)
  P5 错误路径(畸形输入必须"干净报错": rc!=0 且无 panic)

输出 .tools/bughunt/findings.tsv(阶段<TAB>用例<TAB>夹具<TAB>类别<TAB>细节)
汇总打印到 stdout。
"""
import os, re, subprocess, sys, json, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target/release/wal-rust")
BASE = os.path.join(ROOT, ".tools/bughunt")
FIX = os.path.join(BASE, "fixtures")
CASES = os.path.join(BASE, "cases/extracted.tsv")
OPS_FILE = os.path.join(ROOT, ".tools/bughunt_ops.txt")
OUT = os.path.join(BASE, "findings.tsv")
TIMEOUT = 20

OPS = set()
for line in open(OPS_FILE, encoding="utf-8"):
    s = line.strip()
    if s: OPS.add(s)
OPS |= {"list", "quote", "quasiquote", "unquote", "defmacro", "macroexpand", "lambda", "fn", "do", "define", "let", "set!"}
LEAD = re.compile(r"^\(\s*([A-Za-z0-9_+*/<>=!?%&|.\-]+)")

def run(args, env_extra=None, timeout=TIMEOUT):
    env = dict(os.environ)
    env.pop("WAL_PROF", None)
    if env_extra: env.update(env_extra)
    t0 = time.time()
    try:
        p = subprocess.run(args, capture_output=True, timeout=timeout, env=env, text=True, errors="replace")
        dt = time.time() - t0
        return p.returncode, p.stdout, p.stderr, dt
    except subprocess.TimeoutExpired:
        return "TIMEOUT", "", "", time.time() - t0

def is_panic(rc, err):
    return ("panicked at" in err or "RUST_BACKTRACE" in err or "index out of bounds" in err
            or (isinstance(rc, int) and rc in (134, 139, 101)))

def load_cases():
    cases, seen = [], set()
    for line in open(CASES, encoding="utf-8"):
        parts = line.rstrip("\n").split("\t")
        expr = parts[0]
        src = parts[1] if len(parts) > 1 else "?"
        if expr in seen: continue
        m = LEAD.match(expr)
        if not m: continue
        head = m.group(1)
        if head not in OPS: continue
        seen.add(expr)
        cases.append((expr, src))
    return cases

def sigs_of(expr):
    """取表达式里引用的信号名(FIXTURES 里可能不存在 → 用 basic 的 clk 兜底)。"""
    return re.findall(r'"([^"]+)"', expr)

def rewrite(expr, fixture, fmt="vcd"):
    """只替换**显式占位符** SIG/CLK/A/B(避免把普通字符串参数误改成信号名)。"""
    available = sorted(FIX_SIGS.get((fixture, fmt), []))   # 排序后对齐: 两格式同位置=同名, 避免顺序差异污染比较
    if not available: return expr
    pick = {
        "SIG": available[0],
        "A": available[0],
        "B": available[1] if len(available) > 1 else available[0],
        "CLK": next((x for x in available if "clk" in x.lower()), available[0]),
    }
    def repl(m):
        name = m.group(1)
        return '"' + pick[name] + '"' if name in pick else m.group(0)
    return re.sub(r'"([^"]+)"', repl, expr)

FIX_SIGS = {}       # (fixture, fmt) -> [sig names]
for fx in sorted(os.listdir(FIX)):
    if not (fx.endswith(".vcd") or fx.endswith(".fst")): continue
    name, _, ext = fx.rpartition(".")
    fmt = ext
    rc, out, err, _ = run([BIN, "sigs", os.path.join(FIX, fx), ".", "400"])
    sigs = [l.strip() for l in out.splitlines() if l.strip().endswith(("]", "clk","d","s","a","b","top_clk",")")) or re.match(r"^[A-Za-z_\\]", l)]
    # sigs 输出首行是 "N signal(s) matching ..."
    sigs = [l.strip() for l in out.splitlines()
            if l.strip() and not re.match(r"^\d+ signal\(s\) matching", l.strip())]
    FIX_SIGS[(name, fmt)] = sigs

findings = []
def add(phase, expr, fixture, kind, detail):
    findings.append((phase, expr, fixture, kind, detail.replace("\t", " ").replace("\n", " ")[:300]))

cases = load_cases()
print(f"[matrix] 语料 {len(cases)} 条;夹具 {len({k[0] for k in FIX_SIGS})} 组", flush=True)

# ── P1: 语法/求值冒烟(在 basic 上) ─────────────────────────────────────────
basic = os.path.join(FIX, "basic.vcd")
survivors = []
for expr, src in cases:
    e = rewrite(expr, "basic", "vcd")
    rc, out, err, dt = run([BIN, e, "-l", basic])
    if is_panic(rc, err):
        add("P1-panic", expr, "basic.vcd", "PANIC", f"rc={rc} err={err.strip()[:240]}")
    elif rc != 0:
        # 非 0: 只有"干净错误"才可接受; 记录文案供人工判断
        add("P1-error", expr, "basic.vcd", "clean-error?", f"rc={rc} err={err.strip()[:240]}")
    else:
        survivors.append((expr, src, out))
    if dt > 5:
        add("P1-slow", expr, "basic.vcd", "SLOW", f"{dt:.1f}s")
print(f"[matrix] P1 完成: 存活 {len(survivors)} 条", flush=True)

# ── P2: 跨格式差分(VCD vs FST) ─────────────────────────────────────────────
for (name, fmt) in list(FIX_SIGS):
    if fmt != "vcd": continue
    vcd = os.path.join(FIX, name + ".vcd")
    fst = os.path.join(FIX, name + ".fst")
    if not os.path.exists(fst): continue
    vn = FIX_SIGS.get((name, "vcd"), [])
    fn = FIX_SIGS.get((name, "fst"), [])
    if sorted(set(vn)) != sorted(set(fn)):
        add("P2-names", "-", name, "VCD/FST 信号集合不同", f"vcd={vn[:6]} fst={fn[:6]}")
        continue
    for expr, src, _ in survivors[:250]:
        # `initial` 在 FST 上是**格式固有限制**: FST 没有 $dumpvars 快照概念, 初值只能取
        # 时间表第 0 帧的值(== #0 最后一次写入); VCD 的 (initial) 是 dumpvars 快照。
        # 两者仅当"dumpvars 与 #0 写入不同"时才有差异 → 跨格式差分跳过(BUG-029)。
        if "(initial" in expr:
            continue
        e1 = rewrite(expr, name, "vcd")
        e2 = rewrite(expr, name, "fst")
        rc1, o1, er1, _ = run([BIN, e1, "-l", vcd])
        rc2, o2, er2, _ = run([BIN, e2, "-l", fst])
        if is_panic(rc1, er1) or is_panic(rc2, er2):
            add("P2-panic", expr, name, "PANIC", f"vcd rc={rc1}/{er1.strip()[:120]} fst rc={rc2}/{er2.strip()[:120]}")
        elif rc1 == 0 and rc2 == 0 and o1 != o2:
            add("P2-format", expr, name, "VCD!=FST", f"vcd={o1.strip()[:120]} fst={o2.strip()[:120]}")
        elif (rc1 == 0) != (rc2 == 0):
            add("P2-format", expr, name, "VCD/FST 成败不同", f"vcd rc={rc1} fst rc={rc2} err1={er1.strip()[:100]} err2={er2.strip()[:100]}")
print("[matrix] P2 完成", flush=True)

# ── P3: 引擎 vs 逐拍 oracle ─────────────────────────────────────────────────
QUERY = re.compile(r"^\(\s*(count|find|whenever|count-rise|count-fall|count-edges)\b")
for expr, src, _ in survivors:
    if not QUERY.match(expr): continue
    e = rewrite(expr, "basic", "vcd")
    rc1, o1, er1, _ = run([BIN, e, "-l", basic])
    rc2, o2, er2, _ = run([BIN, e, "-l", basic], {"WAL_NO_ENGINE": "1"})
    if rc1 == 0 and rc2 == 0 and o1 != o2:
        add("P3-oracle", expr, "basic.vcd", "引擎!=逐拍", f"engine={o1.strip()[:120]} per-tick={o2.strip()[:120]}")
    elif rc1 == 0 and rc2 != 0:
        add("P3-oracle", expr, "basic.vcd", "逐拍失败", f"engine ok, per-tick rc={rc2} err={er2.strip()[:160]}")
print("[matrix] P3 完成", flush=True)

# ── P4: 非确定性(连跑两次) ─────────────────────────────────────────────────
for expr, src, _ in survivors[:120]:
    e = rewrite(expr, "scopes", "vcd")
    rc1, o1, _, _ = run([BIN, e, "-l", os.path.join(FIX, "scopes.vcd")])
    rc2, o2, _, _ = run([BIN, e, "-l", os.path.join(FIX, "scopes.vcd")])
    if rc1 == 0 and rc2 == 0 and o1 != o2:
        add("P4-nondet", expr, "scopes.vcd", "两次输出不同", f"run1={o1.strip()[:100]} run2={o2.strip()[:100]}")
print("[matrix] P4 完成", flush=True)

# ── P5: 错误路径(畸形输入必须干净报错) ─────────────────────────────────────
BAD = [
    "(", ")", "()", "((", "(foo", "(foo)", "(count)", "(count (get))", "(get)",
    "(get 1 2 3 4)", "(at)", "(at \"nope\")", "(at \"nope\" \"x\")",
    "(count (rising))", "(if)", "(if 1)", "(let)", "(+ )", "(/ )",
    "(unknown-op 1)", "(print", "\"unterminated", "#)", "(1 2)", "(nil)",
    "(steps 1)",
]
for expr in BAD:
    rc, out, err, dt = run([BIN, expr, "-l", basic])
    if is_panic(rc, err):
        add("P5-badinput", expr, "basic.vcd", "PANIC", f"rc={rc} err={err.strip()[:240]}")
    elif rc == 0 and expr not in ("()", "(nil)"):
        add("P5-badinput", expr, "basic.vcd", "畸形输入却成功", f"out={out.strip()[:120]}")
    elif rc != 0 and not err.strip():
        add("P5-badinput", expr, "basic.vcd", "错误无文案", f"rc={rc}")

# 文件级错误
for args, why in [([BIN, "(length (SIGNALS))", "-l", "/nonexistent/x.vcd"], "不存在的文件"),
                  ([BIN, "(length (SIGNALS))", "-l", "/tmp"], "目录当波形"),
                  ([BIN, "count", "/nonexistent.vcd", "a"], "count 文件不存在")]:
    rc, out, err, dt = run(args)
    if is_panic(rc, err):
        add("P5-file", " ".join(args[1:]), "-", "PANIC", f"rc={rc} err={err.strip()[:240]}")
    elif rc == 0:
        add("P5-file", " ".join(args[1:]), "-", f"{why} 却成功", f"out={out.strip()[:120]}")

with open(OUT, "w", encoding="utf-8") as f:
    for row in findings:
        f.write("\t".join(row) + "\n")
print(f"[matrix] findings={len(findings)} -> {OUT}")
from collections import Counter
for k, v in Counter(r[3] for r in findings).most_common():
    print(f"  {k}: {v}")
