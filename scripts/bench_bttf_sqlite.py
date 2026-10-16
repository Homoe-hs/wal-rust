#!/usr/bin/env python3
"""BTTF(arXiv:2610.06790)思路对照实验: 把我们的 FSDB 变更列装进 SQLite。

数据源: wal-rust 自己的旁挂列缓存 `.fcol`(`WALFCOL2` 格式, 与 CLI 无关, 直接读文件)——
这样"两份存储、同一批行"是严格可比的: 左边是我们的 .col, 右边是论文的 SQLite 两表方案。

用法:
  python3 scripts/bench_bttf_sqlite.py <fcol_dir> <names_file> [db_dir]

输出: JSON(各变体的 体积/建库耗时/覆盖率/6 条 agent 式查询中位延迟)
"""
import json
import os
import re
import sqlite3
import statistics
import sys
import time


# ---------- wal-rust 侧: 解析 WALFCOL2 ----------
def fnv1a(s: str) -> int:
    h = 0xCBF29CE484222325
    for b in s.encode():
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def varint(buf, pos):
    v, shift = 0, 0
    while True:
        b = buf[pos]
        pos += 1
        v |= (b & 0x7F) << shift
        if not (b & 0x80):
            return v, pos
        shift += 7


def parse_col(path):
    buf = open(path, "rb").read()
    if buf[:8] != b"WALFCOL2":
        return None
    pos = 20                                   # magic(8) + fp(8) + width(4)
    width = int.from_bytes(buf[16:20], "little")
    kind = buf[pos]; pos += 1
    init = None
    if kind == 1:
        init = chr(buf[pos]); pos += 1
    elif kind == 2:
        n, pos = varint(buf, pos); init = buf[pos:pos + n].decode("latin1"); pos += n
    elif kind == 3:
        init = str(__import__("struct").unpack("<d", buf[pos:pos + 8])[0]); pos += 8
    n = int.from_bytes(buf[pos:pos + 4], "little"); pos += 4   # 点数是定长 u32(不是 varint)
    rows, t = [], 0
    for _ in range(n):
        d, pos = varint(buf, pos)
        t += d
        tag = buf[pos]; pos += 1
        if tag == 0:
            v = chr(buf[pos]); pos += 1
        elif tag == 1:
            ln, pos = varint(buf, pos); v = buf[pos:pos + ln].decode("latin1"); pos += ln
        else:
            v = str(__import__("struct").unpack("<d", buf[pos:pos + 8])[0]); pos += 8
        rows.append((t, v))
    if pos != len(buf):
        raise SystemExit(f"{path}: 解析到 {pos}B, 文件 {len(buf)}B —— 列格式理解有误")
    return {"width": width, "init": init, "rows": rows, "bytes": len(buf), "declared": n}


def load_store(fcol_dir, names_file):
    names = [l.strip() for l in open(names_file, encoding="utf-8") if l.strip()]
    by_hash = {f"{fnv1a(n):016x}": n for n in names}
    out, store_bytes, missing = [], 0, []
    for fn in sorted(os.listdir(fcol_dir)):
        if not fn.endswith("-v1.col"):
            continue
        h = fn[:-len("-v1.col")]
        name = by_hash.get(h)
        if name is None:
            missing.append(fn)
            continue
        c = parse_col(os.path.join(fcol_dir, fn))
        if c is None:
            continue
        if c["declared"] != len(c["rows"]):
            raise SystemExit(f"{fn}: declared {c['declared']} != parsed {len(c['rows'])}")
        out.append((name, c))
        store_bytes += c["bytes"]
    return out, store_bytes, missing


# ---------- BTTF 侧: 两表 + 剪枝 + 批量插入 ----------
def is_clock_rows(rows):
    if len(rows) < 1000:
        return False
    if not {v for _, v in rows[:2000]} <= {"0", "1"}:
        return False
    deltas = [b[0] - a[0] for a, b in zip(rows, rows[1:]) if b[0] > a[0]]
    if not deltas:
        return False
    mode = statistics.mode(deltas)
    return sum(1 for d in deltas if d == mode) / len(deltas) > 0.8


def build(db_path, store, keep, batch=200_000):
    if os.path.exists(db_path):
        os.remove(db_path)
    con = sqlite3.connect(db_path)
    con.execute("PRAGMA journal_mode=OFF")
    con.execute("PRAGMA synchronous=OFF")
    con.execute("CREATE TABLE signal_metadata(id INTEGER PRIMARY KEY, name TEXT, width INT, n_changes INT, first_time INT, last_time INT)")
    con.execute("CREATE TABLE signal_changes(sig_id INTEGER, time INTEGER, value TEXT)")
    meta, changes, rows_kept = [], [], 0
    t0 = time.perf_counter()
    for sid, (name, c) in enumerate(store, start=1):
        if not keep(name, c):
            continue
        r = c["rows"]
        meta.append((sid, name, c["width"], len(r), r[0][0] if r else -1, r[-1][0] if r else -1))
        for t, v in c["rows"]:
            changes.append((sid, t, v))
            if len(changes) >= batch:            # 论文的批量插入(EXECUTEMANY)
                con.executemany("INSERT INTO signal_changes VALUES (?,?,?)", changes)
                changes.clear()
        rows_kept += len(c["rows"])
    if changes:
        con.executemany("INSERT INTO signal_changes VALUES (?,?,?)", changes)
    con.executemany("INSERT INTO signal_metadata VALUES (?,?,?,?,?,?)", meta)
    con.execute("CREATE INDEX idx_sig_time ON signal_changes(sig_id, time)")
    con.execute("CREATE INDEX idx_time ON signal_changes(time)")
    con.commit()
    build_s = time.perf_counter() - t0
    con.close()
    return {"build_s": round(build_s, 3), "rows": rows_kept, "signals": len(meta),
            "mb": round(os.path.getsize(db_path) / 1e6, 2)}


QUERIES = {
    "1_value_at_t": ("SELECT value FROM signal_changes WHERE sig_id=? AND time<=? ORDER BY time DESC LIMIT 1", "sigt"),
    "2_toggle_count": ("SELECT COUNT(*) FROM signal_changes WHERE sig_id=?", "sig"),
    "3_top_nets": ("SELECT sig_id, COUNT(*) c FROM signal_changes GROUP BY sig_id ORDER BY c DESC LIMIT 10", "none"),
    "4_active_in_window": ("SELECT COUNT(DISTINCT sig_id) FROM signal_changes WHERE time BETWEEN ? AND ?", "win"),
    "5_first_after_t": ("SELECT time,value FROM signal_changes WHERE sig_id=? AND time>=? ORDER BY time LIMIT 1", "sigt"),
    "6_rising_count": ("""SELECT COUNT(*) FROM (SELECT value, LAG(value) OVER (PARTITION BY sig_id ORDER BY time) prev
                          FROM signal_changes WHERE sig_id=?) WHERE prev='0' AND value='1'""", "sig"),
    # 同样的问题, 改吃 signal_metadata(论文自己的 schema 就有 n_changes / 首末时间)
    "3b_top_nets_via_meta": ("SELECT id,n_changes FROM signal_metadata ORDER BY n_changes DESC LIMIT 10", "none"),
    "4b_active_via_meta": ("SELECT COUNT(*) FROM signal_metadata WHERE first_time<=? AND last_time>=?", "win"),
}


def bench(db_path, sig_id, t_lo, t_mid):
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    out = {}
    for name, (sql, kind) in QUERIES.items():
        params = {"sig": (sig_id,), "sigt": (sig_id, t_mid), "win": (t_lo, t_mid), "none": ()}[kind]
        ts = []
        for _ in range(7):
            t0 = time.perf_counter()
            con.execute(sql, params).fetchall()
            ts.append((time.perf_counter() - t0) * 1000)
        out[name] = round(statistics.median(ts), 3)
    con.close()
    return out


def main():
    fcol_dir, names_file = sys.argv[1], sys.argv[2]
    dbdir = sys.argv[3] if len(sys.argv) > 3 else ".tools/bttf/db"
    os.makedirs(dbdir, exist_ok=True)
    store, store_bytes, missing = load_store(fcol_dir, names_file)
    total_rows = sum(len(c["rows"]) for _, c in store)
    clocks = {n for n, c in store if is_clock_rows(c["rows"])}
    active = [(n, c) for n, c in store if c["rows"]]
    avg = total_rows / len(active) if active else 0
    print(f"// 解析: {len(store)} 列 / {total_rows} 行 / 我们的 .col 合计 {store_bytes/1e6:.1f} MB; "
          f"时钟 {len(clocks)} 个; 非静默列均值 {avg:.0f} 行; 未匹配 {len(missing)}", file=sys.stderr)

    variants = {
        "full": lambda n, c: bool(c["rows"]),
        "no_clock": lambda n, c: bool(c["rows"]) and n not in clocks,
        "active_above_avg": lambda n, c: bool(c["rows"]) and len(c["rows"]) >= avg,
        "drop_heavy": lambda n, c: bool(c["rows"]) and len(c["rows"]) < 100_000,
    }
    all_t = sorted(t for _, c in store for t, _ in c["rows"])
    heavy = sorted(((n, len(c["rows"])) for n, c in store if len(c["rows"]) >= 100_000), key=lambda x: -x[1])
    res = {"columns": len(store), "total_rows": total_rows, "our_store_mb": round(store_bytes / 1e6, 2),
           "bytes_per_row_ours": round(store_bytes / max(1, total_rows), 2),
           "clocks": sorted(clocks), "heavy_togglers": len(heavy),
           "heavy_rows": sum(r for _, r in heavy), "variants": {}}
    for vname, keep in variants.items():
        st = build(os.path.join(dbdir, f"{vname}.db"), store, keep)
        st["coverage_rows_pct"] = round(100.0 * st["rows"] / total_rows, 2)
        st["coverage_active_signals_pct"] = round(100.0 * st["signals"] / max(1, len(active)), 2)
        st["query_ms"] = bench(os.path.join(dbdir, f"{vname}.db"), 1, all_t[0], all_t[len(all_t) // 2])
        res["variants"][vname] = st
    print(json.dumps(res, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
