#!/usr/bin/env python3
"""bug-hunt: FSDB 维度(需 Verdi 许可;许可通道见 .tools/vm/license_up.sh)。

阶段:
  F1 冒烟: SCOPES/SIGNALS/MAX-INDEX/宽度/名字可解析(不 panic、干净报错)
  F2 自洽: count(changes/rising/falling) == length(find(...)); getwave 与 at 互证
  F3 引擎 vs 逐拍(WAL_NO_ENGINE=1)差分
  F4 跨格式差分: VCD ↔ FSDB 同设计成对夹具(fsdb-parser/samples/*)
  F5 多波形: -l vcd -l fsdb 的 first-match / 索引空间
  F6 健壮性: 假 FSDB / 截断 FSDB / 不存在文件 → 干净报错
  F7 CLI: sigs/count/topsig/run/--stdin + fsdb-timeline-map/merge
输出 .tools/bughunt/fsdb_findings.tsv
"""
import os, re, subprocess, sys, time, glob

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target/release/wal-rust")
BASE = os.path.join(ROOT, ".tools/bughunt")
OUT = os.path.join(BASE, "fsdb_findings.tsv")
WORK = os.path.join(BASE, "work")
SAMPLES = "/home/hesheng/Projects/fsdb-parser/samples"
TIMEOUT = 180

ENV = dict(os.environ)
ENV.pop("WAL_PROF", None)
ENV.pop("WAL_NO_ENGINE", None)
ENV["VERDI_HOME"] = "/home/hesheng/eda_tools/synopsys/verdi/X-2025.06-SP1"
ENV["SNPSLMD_LICENSE_FILE"] = "27080@192.168.122.50"
ENV["LM_LICENSE_FILE"] = "27080@192.168.122.50"

findings = []

def add(cat, name, cls, detail):
    findings.append((cat, name, cls, detail.replace("\n", " ⏎ ")[:400]))

def run(args, env_extra=None, timeout=TIMEOUT, stdin_data=None):
    env = dict(ENV)
    if env_extra: env.update(env_extra)
    t0 = time.time()
    try:
        p = subprocess.run(args, capture_output=True, timeout=timeout, env=env,
                           text=True, errors="replace", input=stdin_data)
        return p.returncode, p.stdout, p.stderr, time.time() - t0
    except subprocess.TimeoutExpired:
        return "TIMEOUT", "", "", time.time() - t0

BANNER = re.compile(r"(NPI - Native Programming Interface|Copyright|Synopsys|Licensed Products|"
                    r"Inclusivity|SolvNetPlus|Version X-|proprietary|infringers|written license|"
                    r"updates, detecting|Released|Confidential|^\s*$)")
def val(out):
    """取 `=> ...` 结果行(NPI banner 可能混在 stdout)。"""
    for line in out.splitlines():
        s = line.strip()
        if s.startswith("=>"):
            return s[2:].strip()
    return None

def is_panic(rc, err):
    return ("panicked at" in err or "RUST_BACKTRACE" in err
            or (isinstance(rc, int) and rc in (134, 139)))

def clean_err(cat, name, rc, out, err):
    if is_panic(rc, err):
        add(cat, name, "PANIC", f"rc={rc} err={err.strip()[:200]}")
        return
    if rc == "TIMEOUT":
        add(cat, name, "TIMEOUT", "超过 %ds" % TIMEOUT)
        return
    if rc == 0:
        add(cat, name, "SILENT-OK", f"out={val(out)}")
        return
    add(cat, name, "CLEAN-ERR", (err.strip().splitlines() or [""])[-1][:200])

# ---------- F1 冒烟 ----------
FIXTURES = [os.path.join(SAMPLES, f) for f in ("mini.fsdb", "verilog.fsdb", "z6.fsdb", "zc12.fsdb")]
FIXTURES += [os.path.join(ROOT, ".tools/vm/share/fsdb/cal1m.fsdb")]

def f1():
    for f in FIXTURES:
        name = os.path.basename(f)
        rc, out, err, _ = run([BIN, "(list (length (SCOPES)) (length (SIGNALS)) MAX-INDEX)", "-l", f])
        if rc != 0:
            clean_err("F1-smoke", name, rc, out, err); continue
        v = val(out)
        if not v:
            add("F1-smoke", name, "NO-RESULT", f"out={out[:160]}"); continue
        try:
            nums = re.findall(r"-?\d+", v)
            ns, nsig, mx = int(nums[0]), int(nums[1]), int(nums[2])
            assert ns >= 0 and nsig > 0 and mx >= 0
        except Exception as e:
            add("F1-smoke", name, "BAD-META", f"{v}: {e}"); continue
        # SCOPES 不含空串 / SIGNALS 可解析
        rc2, out2, err2, _ = run([BIN, "(SCOPES)", "-l", f])
        if rc2 == 0 and '""' in (val(out2) or ""):
            add("F1-smoke", name, "EMPTY-SCOPE", val(out2))
        rc3, out3, err3, _ = run([BIN, "(signal-width (first (SIGNALS)))", "-l", f])
        if rc3 != 0:
            clean_err("F1-smoke-width", name, rc3, out3, err3)

# ---------- helpers: 取信号名 ----------
def fsdb_signals(f, limit=8):
    rc, out, err, _ = run([BIN, "(SIGNALS)", "-l", f])
    if rc != 0: return []
    v = val(out) or ""
    return re.findall(r'"((?:[^"\\]|\\.)*)"', v)[:limit]

# ---------- F2 自洽 ----------
def f2():
    for f in FIXTURES:
        name = os.path.basename(f)
        sigs = fsdb_signals(f, 6)
        for s in sigs:
            q = s.replace('"', '\\"')
            for cond in ("changes", "rising", "falling"):
                e_count = f"(count ({cond} \"{q}\"))"
                e_find = f"(length (find ({cond} \"{q}\")))"
                rc1, o1, er1, _ = run([BIN, e_count, "-l", f])
                rc2, o2, er2, _ = run([BIN, e_find, "-l", f])
                if rc1 != 0 or rc2 != 0:
                    if rc1 != 0: clean_err("F2", f"{name}:{s}:{cond}", rc1, o1, er1)
                    if rc2 != 0: clean_err("F2", f"{name}:{s}:{cond}", rc2, o2, er2)
                    continue
                v1, v2 = val(o1), val(o2)
                if v1 != v2:
                    add("F2", f"{name}:{s}:{cond}", "COUNT!=FIND", f"{v1} vs {v2}")
            # getwave 与 at: 每个点的时间↔值一致
            rc, out, err, _ = run([BIN, f"(getwave \"{q}\")", "-l", f])
            if rc != 0:
                clean_err("F2-getwave", f"{name}:{s}", rc, out, err); continue
            v = val(out) or ""
            pts = re.findall(r"\((-?\d+)\s+(\"[^\"]*\"|\d+)\)", v)
            if not pts:
                # 永不变化的信号(getwave 为空)是合法的; 但若 changes 非空则矛盾
                rc3, o3, _, _ = run([BIN, f"(count (changes \"{q}\"))", "-l", f])
                if rc3 == 0 and val(o3) not in ("0", "()", None):
                    add("F2-getwave", f"{name}:{s}", "EMPTY-BUT-CHANGES", f"getwave={v} count={val(o3)}")
                continue
            t0 = pts[0][0]
            rc2, o2, er2, _ = run([BIN, f"(at \"{q}\" {t0})", "-l", f])
            if rc2 != 0:
                clean_err("F2-at", f"{name}:{s}@t0", rc2, o2, er2)
            elif val(o2) != f"(0 {pts[0][1]})" and val(o2) != f"({t0} {pts[0][1]})":
                add("F2-at", f"{name}:{s}@t0", "MISMATCH", f"getwave首点={pts[0]} at={val(o2)}")

# ---------- F3 引擎 vs 逐拍 ----------
def f3():
    for f in FIXTURES[:3]:
        name = os.path.basename(f)
        sigs = fsdb_signals(f, 3)
        if len(sigs) < 2: continue
        a, b = (s.replace('"', '\\"') for s in sigs[:2])
        exprs = [
            f"(count (&& (rising \"{a}\") (falling \"{b}\")))",
            f"(count (|| (rising \"{a}\") (falling \"{b}\")))",
            f"(count (&& (rising \"{a}\") (not (rising \"{b}\"))))",
            f"(find (&& (rising \"{a}\") (falling \"{b}\")))",
        ]
        for e in exprs:
            rc1, o1, er1, _ = run([BIN, e, "-l", f])
            rc2, o2, er2, _ = run([BIN, e, "-l", f], {"WAL_NO_ENGINE": "1"})
            if rc1 != 0 or rc2 != 0:
                add("F3", f"{name}:{e}"[:120], "ERR", f"engine rc={rc1}/{val(o1)} err={er1.strip()[-120:]}; step rc={rc2} err={er2.strip()[-120:]}")
                continue
            if val(o1) != val(o2):
                add("F3", f"{name}:{e}"[:120], "ENGINE!=STEP", f"{val(o1)} vs {val(o2)}")

# ---------- F4 跨格式差分 ----------
PAIR_NAMES = ["mini", "verilog", "z6", "zc12", "zc110"]

def pair_exprs(fsdb_sigs, mx):
    out = ["(list (length (SCOPES)) (length (SIGNALS)) MAX-INDEX)", "(SCOPES)", "(SIGNALS)"]
    mid = mx // 2
    for s in fsdb_sigs[:3]:
        q = s.replace('"', '\\"')
        out += [
            f"(signal-width \"{q}\")",
            f"(list (count (changes \"{q}\")) (count (rising \"{q}\")) (count (falling \"{q}\")))",
            f"(getwave \"{q}\")",
            f"(list (sample-at \"{q}\" 0) (sample-at \"{q}\" {mid}) (sample-at \"{q}\" {mx}))",
            f"(at \"{q}\" 0)",
        ]
    return out

def f4():
    for nm in PAIR_NAMES:
        vcd = os.path.join(SAMPLES, nm + ".vcd")
        fsdb = os.path.join(SAMPLES, nm + ".fsdb")
        if not (os.path.exists(vcd) and os.path.exists(fsdb)): continue
        sigs = fsdb_signals(fsdb, 3)
        rv = run([BIN, "(list (length (SCOPES)) (length (SIGNALS)) MAX-INDEX)", "-l", vcd])
        rf = run([BIN, "(list (length (SCOPES)) (length (SIGNALS)) MAX-INDEX)", "-l", fsdb])
        if rv[0] != 0 or rf[0] != 0:
            add("F4-meta", nm, "ERR", f"vcd rc={rv[0]} fsdb rc={rf[0]}"); continue
        nv, nf = (val(rv[1]) or ""), (val(rf[1]) or "")
        # 先验: 成对夹具必须同设计(信号数一致), 否则不是合法差分对 → 记为 SKIP
        if nv != nf:
            add("F4-skip", nm, "PAIR-MISMATCH", f"vcd={nv} fsdb={nf}")
            continue
        nums = re.findall(r"-?\d+", nf)
        mx = int(nums[2]) if len(nums) >= 3 else 0
        for e in pair_exprs(sigs, mx):
            r1 = run([BIN, e, "-l", vcd]); r2 = run([BIN, e, "-l", fsdb])
            if r1[0] != 0 or r2[0] != 0:
                add("F4", f"{nm}:{e}"[:140], "ERR",
                    f"vcd rc={r1[0]} err={r1[2].strip()[-100:]}; fsdb rc={r2[0]} err={r2[2].strip()[-100:]}")
                continue
            v1, v2 = val(r1[1]), val(r2[1])
            if v1 != v2:
                add("F4", f"{nm}:{e}"[:140], "VCD!=FSDB", f"vcd={v1} fsdb={v2}")

# ---------- F5 多波形 ----------
def f5():
    mini_v = os.path.join(SAMPLES, "mini.vcd")
    mini_f = os.path.join(SAMPLES, "mini.fsdb")
    # 1) 同名信号: -l vcd -l fsdb 先算第一条(first-match)
    e = '(list (get "tb.clk") (getwave "tb.clk"))'
    single = run([BIN, e, "-l", mini_v])
    multi = run([BIN, e, "-l", mini_v, "-l", mini_f])
    if single[0] == 0 and multi[0] == 0 and val(single[1]) != val(multi[1]):
        add("F5", "first-match 同名信号", "NO-FIRST-MATCH", f"single={val(single[1])} multi={val(multi[1])}")
    # 2) 只在第二条里的信号: 查询的索引空间/结果必须与"单 FSDB"完全一致
    cal = os.path.join(ROOT, ".tools/vm/share/fsdb/cal1m.fsdb")
    sigs = fsdb_signals(cal, 2)
    if sigs:
        q = sigs[0].replace('"', '\\"')
        for e2 in (f"(length (find (= (get \"{q}\") 1)))",
                   f"(count (&& (rising \"{q}\") (= (get \"{q}\") 1)))"):
            single = run([BIN, e2, "-l", cal], timeout=600)
            multi = run([BIN, e2, "-l", mini_v, "-l", cal], timeout=600)
            if single[0] != 0 or multi[0] != 0:
                add("F5", f"第二条独有信号:{e2[:60]}", "ERR",
                    f"single rc={single[0]} multi rc={multi[0]} err={multi[2].strip()[-120:]}")
            elif val(single[1]) != val(multi[1]):
                add("F5", f"第二条独有信号:{e2[:60]}", "MULTI!=SINGLE",
                    f"single={val(single[1])} multi={val(multi[1])}")

# ---------- F8 指标/窗口算子(跨格式一致) ----------
def f8():
    for nm in ("mini", "z6", "zc12"):
        vcd = os.path.join(SAMPLES, nm + ".vcd")
        fsdb = os.path.join(SAMPLES, nm + ".fsdb")
        if not (os.path.exists(vcd) and os.path.exists(fsdb)): continue
        sigs = fsdb_signals(fsdb, 3)
        if len(sigs) < 2:
            continue
        a, b = (x.replace('"', '\\"') for x in sigs[:2])
        exprs = [
            f"(wave \"{a}\" 0 100)",
            f"(wave \"{a}\" 50 200)",
            f"(latency (rising \"{a}\") (rising \"{b}\"))",
            f"(latency (rising \"{a}\") (falling \"{b}\"))",
            f"(list (median '(3 1 2)) (percentile '(1 2 3 4 5) 50) (stddev '(2 4 4 4 5 5 7 9)))",
            f"(stats '(1 2 3 4))",
            f"(histogram '(1 2 2 3 3 3) 2)",
            f"(list (getwave \"{a}\") (find (rising \"{a}\")))",
        ]
        for e in exprs:
            r1 = run([BIN, e, "-l", vcd], timeout=180)
            r2 = run([BIN, e, "-l", fsdb], timeout=180)
            if r1[0] != 0 or r2[0] != 0:
                add("F8", f"{nm}:{e}"[:130], "ERR",
                    f"vcd rc={r1[0]} err={r1[2].strip()[-90:]}; fsdb rc={r2[0]} err={r2[2].strip()[-90:]}")
                continue
            if val(r1[1]) != val(r2[1]):
                add("F8", f"{nm}:{e}"[:130], "VCD!=FSDB", f"vcd={val(r1[1])} fsdb={val(r2[1])}")

# ---------- F9 缓存冷/暖结果一致 ----------
def f9():
    mini = os.path.join(SAMPLES, "mini.fsdb")
    base = os.path.join(WORK, "fsdb_cache")
    cold, warm = os.path.join(base, "cold"), os.path.join(base, "warm")
    for d in (cold, warm):
        os.makedirs(d, exist_ok=True)
    exprs = ["(list (getwave \"tb.clk\") (count (rising \"tb.clk\")))",
             "(at \"tb.data\" 15)", "(sample-at \"tb.reset\" 5)"]
    for e in exprs:
        def run_in(d, cache):
            env = {"WAL_CACHE": cache, "WAL_CACHE_DIR": os.path.join(d, ".wal-rust-cache"), "WAL_CACHE_MIN_MB": "0"}
            r = run([BIN, e, "-l", mini], env_extra=env, timeout=300)
            # current_dir 不在这里切: 用 WAL_CACHE_DIR 显式指定缓存目录
            return r
        a = run_in(cold, "off")
        b = run_in(warm, "build")
        c = run_in(warm, "build")
        if a[0] != 0 or b[0] != 0 or c[0] != 0:
            add("F9", e[:80], "ERR", f"rc off/build/hit={a[0]}/{b[0]}/{c[0]} err={c[2].strip()[-100:]}")
        elif val(a[1]) != val(b[1]) or val(a[1]) != val(c[1]):
            add("F9", e[:80], "CACHE-DIFF", f"off={val(a[1])} cold={val(b[1])} warm={val(c[1])}")

# ---------- F6 健壮性 ----------
def f6():
    fake = os.path.join(ROOT, ".tools/fake.fsdb")
    if os.path.exists(fake):
        rc, out, err, _ = run([BIN, "(length (SIGNALS))", "-l", fake], timeout=60)
        if rc == 0:
            add("F6", "fake.fsdb", "SILENT-OK", f"out={val(out)}")
        elif is_panic(rc, err):
            add("F6", "fake.fsdb", "PANIC", err.strip()[-200:])
        else:
            pass  # 干净报错 = 期望行为(不记 finding)
    # 截断 FSDB
    src = os.path.join(SAMPLES, "mini.fsdb")
    data = open(src, "rb").read()
    trunc = os.path.join(WORK, "trunc_mini.fsdb")
    open(trunc, "wb").write(data[: len(data) // 3])
    rc, out, err, _ = run([BIN, "(length (SIGNALS))", "-l", trunc], timeout=60)
    if rc == 0:
        add("F6", "截断 FSDB(1/3)", "SILENT-OK", f"out={val(out)}")
    elif is_panic(rc, err):
        add("F6", "截断 FSDB(1/3)", "PANIC", err.strip()[-200:])
    else:
        pass  # 干净报错 = 期望行为
    # 不存在
    rc, out, err, _ = run([BIN, "(length (SIGNALS))", "-l", "/nonexistent/x.fsdb"], timeout=60)
    if rc == 0: add("F6", "不存在的 FSDB", "SILENT-OK", val(out))
    elif is_panic(rc, err): add("F6", "不存在的 FSDB", "PANIC", err.strip()[-200:])

# ---------- F7 CLI ----------
def f7():
    mini = os.path.join(SAMPLES, "mini.fsdb")
    checks = [
        ("sigs", [BIN, "sigs", mini, "clk", "5"]),
        ("count", [BIN, "count", mini, "tb.clk", "1"]),
        ("topsig", [BIN, "topsig", mini, "3"]),
        ("timeline-map-help", [BIN, "fsdb-timeline-map", "--help"]),
        ("timeline-merge-help", [BIN, "fsdb-timeline-merge", "--help"]),
    ]
    for name, args in checks:
        rc, out, err, _ = run(args, timeout=120)
        if rc != 0:
            clean_err("F7", name, rc, out, err)
        elif is_panic(rc, err):
            add("F7", name, "PANIC", err.strip()[-200:])
    # --stdin 会话: 一次加载, 多探针
    probes = '(get "tb.clk")\n(count (rising "tb.clk"))\nMAX-INDEX\n'
    rc, out, err, _ = run([BIN, "--stdin", "-l", mini], stdin_data=probes, timeout=120)
    if rc != 0:
        clean_err("F7", "--stdin 会话", rc, out, err)
    else:
        outs = [l.strip() for l in out.splitlines() if l.strip().startswith("=>")]
        if len(outs) < 3:
            add("F7", "--stdin 会话", "MISSING-RESULTS", f"{outs}")
    # fsdb-timeline-map + merge 小夹具(2 分片)
    part0 = os.path.join(WORK, "tl.mini.0.part")
    part1 = os.path.join(WORK, "tl.mini.1.part")
    r1 = run([BIN, "fsdb-timeline-map", mini, "0", "2", part0], timeout=300)
    r2 = run([BIN, "fsdb-timeline-map", mini, "1", "2", part1], timeout=300)
    for i, (rc, out, err, _) in enumerate((r1, r2)):
        if rc != 0:
            clean_err("F7", f"timeline-map shard{i}", rc, out, err)
    if r1[0] == 0 and r2[0] == 0:
        rc, out, err, _ = run([BIN, "fsdb-timeline-merge", mini, part0, part1], timeout=300)
        if rc != 0:
            clean_err("F7", "timeline-merge", rc, out, err)

def main():
    os.makedirs(WORK, exist_ok=True)
    t0 = time.time()
    for ph in (f1, f2, f3, f4, f5, f6, f7, f8, f9):
        try:
            ph()
        except Exception as e:
            add(ph.__name__, "harness", "EXCEPTION", f"{type(e).__name__}: {e}")
        print(f"[fsdb] {ph.__name__} 完成 ({time.time()-t0:.1f}s, findings={len(findings)})")
    with open(OUT, "w", encoding="utf-8") as fh:
        for row in findings:
            fh.write("\t".join(row) + "\n")
    print(f"\n[fsdb] findings={len(findings)} -> {OUT}")
    from collections import Counter
    for k, v in Counter(r[2] for r in findings).most_common():
        print(f"  {k}: {v}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
