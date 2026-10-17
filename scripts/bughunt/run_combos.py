#!/usr/bin/env python3
"""bug-hunt 组合阶段:嵌套算子 / 多波形 / CLI / 脚本层 / 文件健壮性 / 取值类型边界。

每个用例标注期望:ok(应成功) / err(应干净报错) / skip。判定:
  - panic(SIGSEGV/SIGABRT/panicked at) → PANIC(高)
  - 期望 ok 却 rc!=0 → UNEXPECTED-ERR(高/中)
  - 期望 err 却 rc==0 → SILENT-OK(中)
  - 同一用例两种入口/格式结果不同 → MISMATCH(高)
输出 .tools/bughunt/combo_findings.tsv + 摘要。
"""
import os, re, subprocess, json, sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target/release/wal-rust")
FIX = os.path.join(ROOT, ".tools/bughunt/fixtures")
WORK = os.path.join(ROOT, ".tools/bughunt/work")
OUT = os.path.join(ROOT, ".tools/bughunt/combo_findings.tsv")
os.makedirs(WORK, exist_ok=True)
TIMEOUT = 30

def run(args, env_extra=None, stdin=None, timeout=TIMEOUT):
    env = dict(os.environ); env.pop("WAL_PROF", None)
    if env_extra: env.update(env_extra)
    try:
        p = subprocess.run(args, capture_output=True, timeout=timeout, env=env, text=True, errors="replace",
                           input=stdin)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return "TIMEOUT", "", ""

def panic(rc, err):
    return ("panicked at" in err or "RUST_BACKTRACE" in err or (isinstance(rc, int) and rc in (101, 132, 134, 139)))

findings = []
def add(cat, name, kind, detail, cmd):
    findings.append((cat, name, kind, detail.replace("\t"," ").replace("\n"," ")[:300], cmd.replace("\t"," ")[:300]))
    print(f"  [{kind}] {name}: {detail[:150]}", flush=True)

def check(cat, name, args, expect="ok", stdin=None, env_extra=None):
    rc, out, err = run(args, env_extra, stdin)
    cmd = " ".join(args[1:])
    if panic(rc, err):
        add(cat, name, "PANIC", f"rc={rc} {err.strip()[:240]}", cmd); return rc, out, err
    if expect == "ok" and rc != 0:
        add(cat, name, "UNEXPECTED-ERR", f"rc={rc} err={err.strip()[:240]}", cmd)
    elif expect == "err" and rc == 0:
        add(cat, name, "SILENT-OK", f"out={out.strip()[:200]}", cmd)
    return rc, out, err

SIGS = {}
for _fx in sorted(os.listdir(FIX)):
    if not (_fx.endswith(".vcd") or _fx.endswith(".fst")): continue
    _n, _, _e = _fx.rpartition(".")
    _rc, _o, _er = run([BIN, "sigs", os.path.join(FIX, _fx), ".", "400"])
    SIGS[(_n, _e)] = [l.strip() for l in _o.splitlines()
                      if l.strip() and not re.match(r"^\d+ signal\(s\) matching", l.strip())]

V = "basic.vcd"; B = os.path.join(FIX, V)
XZ = os.path.join(FIX, "xz.vcd"); GL = os.path.join(FIX, "glitch.vcd")
STS = os.path.join(FIX, "scopes.vcd"); SAME = os.path.join(FIX, "same_time.vcd")
EMPTY = os.path.join(FIX, "empty.vcd"); ONE = os.path.join(FIX, "one_ts.vcd")
NOD = os.path.join(FIX, "no_dump.vcd"); REAL = os.path.join(FIX, "real.vcd")
WIDE = os.path.join(FIX, "wide.vcd"); ALIAS = os.path.join(FIX, "alias.vcd")
BFST = os.path.join(FIX, "basic.fst")

print("[combos] C1 嵌套算子", flush=True)
C1 = [
    (f'(count (&& (rising clk) (= (get d) 1)))', "ok"),
    (f'(find (&& (rising clk) (is-x (get data))))', "ok"),
    (f'(count (|| (rising clk) (falling rst)))', "ok"),
    (f'(count (not (rising "clk")))', "ok"),
    (f'(length (find (changes "data") 3))', "ok"),
    (f'(sum (map (lambda (x) (signal-width x)) (SIGNALS)))', "ok"),
    (f'(fold + 0 (map (lambda (x) (count (changes x))) (SIGNALS)))', "ok"),
    (f'(do (step 2) (list INDEX (count (rising clk))) (step -2) INDEX)', "ok"),
    (f'(whenever (rising "clk") (print (list INDEX TS)))', "ok"),
    (f'(let ((v (get "clk"))) (if (= v 1) (count (rising "clk")) INDEX))', "ok"),
    (f'(stats (latency (rising clk) (falling clk)))', "ok"),
    (f'(median (latency (rising clk) (falling clk)))', "ok"),
    (f'(percentile (ipc (rising clk) (rising clk)) 50)', "ok"),
    (f'(histogram (latency (rising clk) (falling clk)) 4)', "ok"),
    (f'(stddev (latency (rising clk) (falling clk)))', "ok"),
    (f'(map (lambda (s) (count (rising s))) (SIGNALS))', "ok"),
    (f'(map (lambda (s) (= (count (changes s)) 0)) (SIGNALS))', "ok"),
    (f'(do (define a (count (rising clk))) (define b (count (falling clk))) (list a b))', "ok"),
    (f'(case (count (rising clk)) (2 "two") (else "other"))', "ok"),
    (f'(cond ((>= (count (rising "clk")) 1) "has") (1 "none"))', "ok"),
]
for e, exp in C1: check("C1-nested", e, [BIN, e, "-l", B], exp)

print("[combos] C2 跨格式一致性(名字对齐的夹具)", flush=True)
ALIGNED = ["xz", "glitch", "one_ts", "same_time", "no_dump", "alias", "scopes", "dup_ts"]
C2 = [
    '(count (changes SIG))', '(count (rising SIG))', '(list INDEX TS MAX-INDEX)',
    '(getwave SIG)', '(length (SIGNALS))', '(length SCOPES)', '(stats (latency (rising A) (rising B)))',
    '(do (step 1) (list INDEX TS) (step -1))', '(count (&& (rising A) (= (get B) 1)))',
]
for fx in ALIGNED:
    vcd = os.path.join(FIX, fx + ".vcd"); fst = os.path.join(FIX, fx + ".fst")
    if not os.path.exists(fst): continue
    import re as _re
    def _rw(e, fx, fmt):
        names = sorted(SIGS.get((fx, fmt), []))
        if not names: return e
        p = {"SIG": names[0], "A": names[0], "B": names[1] if len(names) > 1 else names[0],
             "CLK": next((x for x in names if "clk" in x.lower()), names[0])}
        return _re.sub(r'"([^"]+)"', lambda m: '"' + p[m.group(1)] + '"' if m.group(1) in p else m.group(0), e)
    for expr in C2:
        expr = _rw(expr, fx, "vcd")
        rc1, o1, e1 = run([BIN, expr, "-l", vcd])
        rc2, o2, e2 = run([BIN, expr, "-l", fst])
        if panic(rc1, e1) or panic(rc2, e2):
            add("C2-format", f"{fx}:{expr}", "PANIC", f"vcd={rc1} fst={rc2} {e1[:100]} {e2[:100]}", expr); continue
        if rc1 != rc2:
            add("C2-format", f"{fx}:{expr}", "成败不同", f"vcd rc={rc1} fst rc={rc2} {e1[:120]} {e2[:120]}", expr)
        elif rc1 == 0 and o1 != o2:
            add("C2-format", f"{fx}:{expr}", "输出不同", f"vcd={o1.strip()[:110]} fst={o2.strip()[:110]}", expr)

print("[combos] C3 多波形", flush=True)
MULTI = [
    ([BIN, '(get "clk")', "-l", STS, "-l", B], "ok"),
    ([BIN, '(count (changes "clk"))', "-l", STS, "-l", B], "ok"),
    ([BIN, '(list (TRACE-NAME))', "-l", B, "-l", BFST], "ok"),
    ([BIN, '(list (loaded-traces))', "-l", B, "-l", EMPTY], "ok"),
    ([BIN, '(length (SIGNALS))', "-l", EMPTY, "-l", B], "ok"),
    ([BIN, '(get "top.en")', "-l", EMPTY, "-l", B], "ok"),
    ([BIN, '(count (rising "clk"))', "-l", B, "-l", STS], "ok"),
    ([BIN, '(MAX-INDEX)', "-l", ONE, "-l", B], "ok"),
    ([BIN, '(get "top_clk")', "-l", STS, "-l", B], "ok"),
]
for args, exp in MULTI:
    check("C3-multi", " ".join(args[1:]), args, exp)

print("[combos] C4 CLI / 脚本 / stdin", flush=True)
# count / sigs / topsig 边界
for name, args, exp in [
    ("count 默认值", [BIN, "count", B, "clk"], "ok"),
    ("count 值 0", [BIN, "count", B, "clk", "0"], "ok"),
    ("count 非数字值", [BIN, "count", B, "clk", "xx"], "err"),
    ("count 不存在信号", [BIN, "count", B, "nope_zz"], "err"),
    ("count 空夹具", [BIN, "count", EMPTY, "a"], "err"),
    ("sigs limit 0", [BIN, "sigs", B, ".", "0"], "ok"),
    ("sigs limit 负数", [BIN, "sigs", B, ".", "-1"], "ok"),
    ("sigs 无匹配", [BIN, "sigs", B, "zzz_zzz"], "ok"),
    ("sigs 空夹具", [BIN, "sigs", EMPTY, "."], "ok"),
    ("topsig 空夹具", [BIN, "topsig", EMPTY], "ok"),
    ("topsig limit 0", [BIN, "topsig", B, "0"], "ok"),
    ("topsig 1", [BIN, "topsig", B, "1"], "ok"),
]:
    check("C4-cli", name, args, exp)

# 脚本层
def wscript(name, body):
    p = os.path.join(WORK, name); open(p, "w").write(body); return p
scripts = [
    ("s1.wal", '(print "hi")\n(assert-eq "a" 1 1)\n', "ok", "普通脚本"),
    ("s2.wal", '(assert-eq "a" 1 2)\n', "err", "断言失败应非 0"),
    ("s3.wal", '(exit 0)\n', "ok", "exit 0"),
    ("s4.wal", '(exit 7)\n', "ok", "exit 7(退出码应为 7)"),
    ("s5.wal", '(define f (lambda (x) (* x 2)))\n(print (f 21))\n', "ok", "lambda"),
    ("s6.wal", '(defun add (a b) (+ a b))\n(print (add 1 2))\n', "ok", "defun"),
    ("s7.wal", '(print (count (rising "clk")))\n', "ok", "带波形脚本"),
    ("s8.wal", '(print (get "nope_x"))\n', "err", "脚本内错误应非 0"),
]
for fn, body, exp, nm in scripts:
    p = wscript(fn, body)
    check("C4-script", f"{nm}({fn})", [BIN, "run", p, "-l", B], exp)
check("C4-script", "--halt-on-error 中途错", [BIN, "run", wscript("s9.wal", '(print "a")\n(print (get "nope_x"))\n(print "b")\n'), "-l", B, "--halt-on-error"], "err")

# stdin 会话
probes = '(length (SIGNALS))\n(count (rising "clk"))\n(text-no-such-op)\n(count (changes "clk"))\n'
rc, out, err = run([BIN, "--stdin", "-l", B], stdin=probes)
lines = [l for l in out.splitlines() if l.startswith("=>")]
if rc == 0:
    add("C4-stdin", "会话内错误后仍 rc=0", "SILENT-OK", f"rc={rc} out={out[:200]} err={err[:160]}", "--stdin")
elif len(lines) < 3:
    add("C4-stdin", "会话内错误导致后续探针丢失", "PROBE-LOST", f"rc={rc} 结果行={lines}", "--stdin")
# INDEX 变更是否影响后续探针
rc, out, err = run([BIN, "--stdin", "-l", B], stdin='(step 3)\nINDEX\n(list INDEX TS)\n')
if rc == 0 and "(3 " not in out:
    add("C4-stdin", "会话内 INDEX 变更未延续", "STATE", f"out={out.strip()[:160]}", "--stdin step 3; INDEX")

print("[combos] C5 文件健壮性", flush=True)
# 截断/破坏的 VCD
src = open(B, "rb").read()
for cut in (10, 80, 200, 400):
    p = os.path.join(WORK, f"trunc{cut}.vcd"); open(p, "wb").write(src[:cut])
    check("C5-file", f"截断 VCD({cut}B)", [BIN, '(length (SIGNALS))', "-l", p], "err")
open(os.path.join(WORK, "garbage.vcd"), "wb").write(b"\x00\x01\x02garbage not vcd\n")
check("C5-file", "二进制垃圾当 VCD", [BIN, '(length (SIGNALS))', "-l", os.path.join(WORK, "garbage.vcd")], "err")
open(os.path.join(WORK, "empty.vcd"), "wb").write(b"")
check("C5-file", "空文件", [BIN, '(length (SIGNALS))', "-l", os.path.join(WORK, "empty.vcd")], "err")
# CRLF / $comment 含 # 与 (
vcd_crlf = ("$timescale 1ns $end\r\n$scope module t $end\r\n$var wire 1 ! a $end\r\n$upscope $end\r\n$enddefinitions $end\r\n"
            "$comment this has ( and #10 inside $end\r\n#0\r\n0!\r\n#10\r\n1!\r\n").replace("\n", "\r\n")
p = os.path.join(WORK, "crlf_comment.vcd"); open(p, "w").write(vcd_crlf)
rc, out, err = run([BIN, '(count (changes "a"))', "-l", p])
if rc != 0: add("C5-file", "CRLF+comment 夹具", "UNEXPECTED-ERR", f"rc={rc} {err.strip()[:200]}", "count changes a")
# 无 $enddefinitions
p = os.path.join(WORK, "no_enddef.vcd")
open(p, "w").write("$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! a $end\n$upscope $end\n#0\n0!\n#10\n1!\n")
check("C5-file", "缺 $enddefinitions", [BIN, '(count (changes "a"))', "-l", p], "err")
# 截断的 FST
fstsrc = open(BFST, "rb").read()
for cut in (16, 64, 200):
    p = os.path.join(WORK, f"trunc{cut}.fst"); open(p, "wb").write(fstsrc[:cut])
    check("C5-file", f"截断 FST({cut}B)", [BIN, '(length (SIGNALS))', "-l", p], "err")

print("[combos] C6 取值/类型边界", flush=True)
C6 = [
    ((f'(get "d")',), "ok"),
    ((f'(sample-at "d" -1)',), "err"),
    ((f'(sample-at "d" 999)',), "err"),
    ((f'(at "d" 0)',), "ok"),
    ((f'(at "d" 9999)',), "ok"),
    ((f'(+ (get "d") 1)',), "ok"),
    ((f'(= (get "q") "00xz")',), "ok"),
    ((f'(bits->sint (get "d"))',), "ok"),
    ((f'(band (get "d") 3)',), "ok"),
    ((f'(string->int "999999999999999999999999")',), "err"),
    ((f'(/ 1 0)',), "err"),
    ((f'(/ 1.0 0.0)',), "ok"),
    ((f'(string->int "abc")',), "err"),
    ((f'(+ 1 "x")',), "err"),
    ((f'(sample-at "d" 1.5)',), "err"),
    ((f'(step 99999999999999999999)',), "err"),
    ((f'(at "d" -5)',), "err"),
    ((f'(getwave "d")',), "ok"),
    ((f'(edges "d" 10 5)',), "ok"),
]
for (expr,), exp in C6:
    check("C6-value", expr, [BIN, expr, "-l", XZ if "q" in expr else B], exp)
# real 信号
for expr in ['(get "r")', '(at "r" 20)', '(getwave "r")', '(+ (get "r") 1.0)', '(= (get "r") 1.5)']:
    check("C6-real", expr, [BIN, expr, "-l", REAL], "ok")
# 宽值信号(100 位)
for expr in ['(signal-width "w100")', '(get "w100")', '(bits->sint (get "w100"))', '(= (get "w100") (get "w100"))']:
    check("C6-wide", expr, [BIN, expr, "-l", WIDE], "ok")

with open(OUT, "w", encoding="utf-8") as f:
    for r in findings: f.write("\t".join(r) + "\n")
print(f"[combos] findings={len(findings)} -> {OUT}")
from collections import Counter
for k, v in Counter(r[2] for r in findings).most_common(): print(f"  {k}: {v}")
