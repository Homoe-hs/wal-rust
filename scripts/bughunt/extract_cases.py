#!/usr/bin/env python3
"""从仓库里已有测试/样例中提取 WAL 表达式语料(只读, 不执行)。

输出 .tools/bughunt/cases/extracted.tsv: expr<TAB>来源
过滤: 以 '(' 开头、括号平衡、长度<600、去重。另附手写边界用例(见 CURATED)。
"""
import os, re, sys, glob

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, ".tools/bughunt/cases")
os.makedirs(OUT, exist_ok=True)

def balanced(s):
    d = 0; instr = False; esc = False
    for ch in s:
        if instr:
            if esc: esc = False
            elif ch == '\\': esc = True
            elif ch == '"': instr = False
            continue
        if ch == '"': instr = True
        elif ch == '(': d += 1
        elif ch == ')':
            d -= 1
            if d < 0: return False
    return d == 0 and not instr

cases = {}   # expr -> source
# ── Rust 测试里的字符串字面量 ───────────────────────────────────────────────
for f in glob.glob(os.path.join(ROOT, "tests/**/*.rs"), recursive=True) + glob.glob(os.path.join(ROOT, "src/**/*.rs"), recursive=True):
    try: txt = open(f, encoding="utf-8", errors="replace").read()
    except OSError: continue
    for m in re.finditer(r'r#*"((?:[^"\\]|\\.|"(?!#))*)""?#*', txt):
        s = m.group(1)
        if s.startswith("(") and balanced(s) and len(s) < 600:
            cases.setdefault(s, os.path.relpath(f, ROOT))
    for m in re.finditer(r'"((?:[^"\\]|\\.)*)"', txt):
        s = m.group(1)
        if s.startswith("(") and balanced(s) and len(s) < 600:
            cases.setdefault(s, os.path.relpath(f, ROOT))
# ── .wal 脚本里的表达式行 ────────────────────────────────────────────────────
for f in glob.glob(os.path.join(ROOT, "test_samples/*.wal")) + glob.glob(os.path.join(ROOT, "scripts/**/*.wal"), recursive=True):
    try: txt = open(f, encoding="utf-8", errors="replace").read()
    except OSError: continue
    for line in txt.splitlines():
        s = line.strip()
        if s.startswith("(") and balanced(s) and len(s) < 600:
            cases.setdefault(s, os.path.relpath(f, ROOT))

# ── 手写边界用例(每个算子至少一条; 格式占位符 SIG/IDX 由跑测器替换) ────────
CURATED = [
    # 取值 / 4-state
    '(get "SIG")', '(at "SIG" 0)', '(at "SIG" 10)', '(at "SIG" 5)', '(sample-at "SIG" 0)',
    '(sample-at "SIG" 2)', '(initial "SIG")', '(getwave "SIG")', '(wave "SIG" 0 20)',
    '(signal-width "SIG")', '(is-x (get "SIG"))', '(is-z (get "SIG"))',
    # 变化 / 边沿
    '(count (changes "SIG"))', '(count (rising "SIG"))', '(count (falling "SIG"))',
    '(count-edges "SIG" 0 100)', '(count-rise "SIG" 0 100)', '(count-fall "SIG" 0 100)',
    '(edges "SIG")', '(edges "SIG" 0 20)', '(find (rising "SIG"))', '(find (rising "SIG") 2)',
    # 条件 / 引擎
    '(count (= (get "SIG") 1))', '(count (= (get "SIG") 0))', '(count (&& (= (get "SIG") 1) (= (get "CLK") 1)))',
    '(count (|| (is-x (get "SIG")) (is-z (get "SIG"))))',
    '(whenever (rising "SIG") INDEX)', '(whenever (rising "SIG") (print INDEX))',
    '(count (not (= (get "SIG") 1)))',
    # 索引 / 时间
    '(list INDEX TS MAX-INDEX)', '(do (step 1) (list INDEX TS))', '(do (step 2) INDEX (step -2) INDEX)',
    '(timescale)', '(beat 0)', '(beat 1000)', '(beat 123456789)',
    # 元信息
    '(length (SIGNALS))', '(length SCOPES)', '(list CG)', '(list LOCAL-SIGNALS)',
    '(all-scopes)', '(length (all-scopes))', '(in-scope "top.u1")',
    # 信号名解析
    '(length (find-sig "clk"))', '(length (find-sig "zzz_no_such"))',
    # 指标
    '(stats (latency (rising "A") (rising "B")))', '(median (latency (rising "A") (rising "B")))',
    '(percentile (latency (rising "A") (rising "B")) 50)', '(stddev (latency (rising "A") (rising "B")))',
    '(histogram (latency (rising "A") (rising "B")) 4)', '(ipc (rising "A") (rising "B"))',
    '(latency (rising "A") (rising "B"))',
    # 断言 / 错误路径
    '(count (= (get "SIG") 0))', '(search "SIG" "1")',
    # 语言/集合
    '(+ 1 2)', '(- 1 2)', '(* 3 4)', '(/ 1 0)', '(div 1 0)', '(mod 1 0)', '(/ 1 0.0)',
    '(< 1 2)', '(= 1 1)', '(&& 0 1)', '(|| 0 1)', '(not 0)',
    '(list 1 2 3)', '(length (list 1 2 3))', '(first (list 1 2))', '(rest (list 1 2))',
    '(last (list 1 2))', '(map (lambda (x) (* x 2)) (list 1 2 3))',
    '(fold + 0 (list 1 2 3))', '(filter (lambda (x) (> x 1)) (list 1 2 3))',
    '(range 0 5)', '(zip (list 1 2) (list 3 4))',
    '(if 1 2 3)', '(cond (1 10) (1 20))', '(case 2 (1 10) (2 20) (else 30))',
    '(do 1 2 3)', '(let ((x 1)) (+ x 1))',
    '(string-append "a" "b")', '(string->int "42")', '(int->string 42)',
    '(symbol->string (quote x))', '(type 1)', '(atom? 1)', '(null? (list))',
    # 位运算/宽值
    '(band 6 3)', '(bor 6 3)', '(bxor 6 3)', '(ceil 1.2)', '(floor 1.8)', '(round 1.5)', '(abs -3)',
    '(max 1 2)', '(min 1 2)', '(sum (list 1 2 3))', '(average (list 1 2 3))',
    # 多波形(第二条 -l 由跑测器按需加)
    '(get "SIG")',
]
for c in CURATED:
    cases.setdefault(c, "curated")

with open(os.path.join(OUT, "extracted.tsv"), "w", encoding="utf-8") as f:
    for expr, src in cases.items():
        f.write(expr.replace("\t", " ") + "\t" + src + "\n")
print(f"{len(cases)} cases -> {os.path.join(OUT, 'extracted.tsv')}")
