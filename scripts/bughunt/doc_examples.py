#!/usr/bin/env python3
"""从 docs/wal-编程手册.md 抽取每个算子的示例与期望输出, 直接对拍实现。

手册结构约定: `#### `op`` 之后有 ```wal 代码块 ... ``` 紧跟 ``` 期望输出 ``` 块。
对每个示例: 跑 `wal-rust <代码>`, 与期望输出比对(去空白/去前缀差异)。
分类: MATCH / MISMATCH / ERR(跑失败) / SKIP(需要波形或不可执行)。
输出 .tools/bughunt/doc_examples.tsv。
"""
import os, re, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DOC = os.path.join(ROOT, "docs/wal-编程手册.md")
BIN = os.path.join(ROOT, "target/release/wal-rust")
OUT = os.path.join(ROOT, ".tools/bughunt/doc_examples.tsv")
txt = open(DOC, encoding="utf-8").read()

# 按 "#### `op`" 切段
secs = re.split(r"^#### `([^`]+)`\s*$", txt, flags=re.M)
cases = []
for i in range(1, len(secs), 2):
    op = secs[i].strip(); body = secs[i+1]
    blocks = re.findall(r"```(\w*)\n(.*?)```", body, flags=re.S)
    wal, expected = None, []
    for j, (lang, content) in enumerate(blocks):
        if lang == "wal" and wal is None:
            wal = content.strip()
        elif wal is not None and lang == "":
            expected.append(content.strip())
    if wal:
        cases.append((op, wal, expected[0] if expected else ""))

def run(code):
    try:
        p = subprocess.run([BIN, code], capture_output=True, text=True, errors="replace", timeout=20)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return "TIMEOUT", "", ""

def norm(s):
    s = s.replace("\r", "").strip()
    # 去掉回显前缀 "=> "
    s = re.sub(r"^=> ?", "", s)
    return re.sub(r"\s+", " ", s).strip()

rows = []
stat = {"MATCH":0,"MISMATCH":0,"ERR":0,"SKIP":0}
for op, code, exp in cases:
    if not exp:
        stat["SKIP"] += 1; rows.append((op, code, exp, "SKIP", "无期望输出块")); continue
    rc, out, err = run(code)
    if ("-l" in code) or ("load" in code and ".vcd" in code) or (rc != 0 and ("not found" in err or "Failed to mmap" in err)):
        stat["SKIP"] += 1; rows.append((op, code, exp, "SKIP", f"需要外部波形/环境: {err.strip()[:120]}")); continue
    if rc != 0:
        stat["ERR"] += 1; rows.append((op, code, exp, "ERR", f"rc={rc} {err.strip()[:160]}")); continue
    got = norm(out)
    want = norm(exp)
    if got == want or got.startswith(want) or want in got:
        stat["MATCH"] += 1; rows.append((op, code, exp, "MATCH", got[:120]))
    else:
        stat["MISMATCH"] += 1; rows.append((op, code, exp, "MISMATCH", f"want={want[:120]} got={got[:160]}"))
    if stat["MISMATCH"] == 40 or stat["ERR"] == 40: pass

with open(OUT, "w", encoding="utf-8") as f:
    for r in rows: f.write("\t".join(x.replace("\t"," ").replace("\n"," ") for x in r) + "\n")
print(f"[doc-examples] 共 {len(cases)} 个算子示例; 判定: {stat} -> {OUT}")
for r in rows:
    if r[3] in ("MISMATCH", "ERR"):
        print(f"  [{r[3]}] {r[0]}: {r[1][:80]} || {r[4][:180]}")
