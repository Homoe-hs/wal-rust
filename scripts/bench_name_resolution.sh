#!/usr/bin/env bash
# ============================================================================
# 名字解析/裸符号求值基准 —— 专门盯"O(N) 每次求值"这类回归。
#
#   ./scripts/bench_name_resolution.sh [信号数] [时间戳数]
#   # 默认 200000 2000(约 9MB VCD, 本机跑几十秒)
#
# 背景(真实事故): 裸符号求值走的是"自动解析信号名"的路径, 它曾经调用
# `traces.signals()` —— 返回整张名字表的副本。188 万信号的 FSDB 上, 统一引擎
# 在每个边界都重新求值条件 → 每次求值克隆 188 万个字符串, `get` 名字解析慢到
# 3 分钟不出结果。修复后改为后端 `resolve_name`(FSDB 叶子名走懒建排序索引)。
#
# 这个脚本把三条路径的耗时打出来, 便于:
#   * 改动求值/名字解析路径后做 A/B;
#   * 在 CI(--full)或发版前留一份数字;
#   * 复现"是不是名字解析的锅"。
#
# 用法提示: 想看**逐索引**最坏情况, 看下面的 step 行(每个索引求值一次)。
# ============================================================================
set -uo pipefail
cd "$(dirname "$0")/.."

NSIG="${1:-200000}"; NTS="${2:-2000}"
BIN="${WAL_BIN:-target/release/wal-rust}"
[ -x "$BIN" ] || { echo "找不到 $BIN(先 cargo build --release)"; exit 2; }
command -v python3 >/dev/null || { echo "需要 python3 生成样本"; exit 2; }

work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
wave="$work/bench.vcd"
echo "生成样本: $NSIG 信号 × $NTS 时间戳 …"
python3 scripts/gen_big_vcd.py "$wave" "$NSIG" "$NTS" 50 >/dev/null

# 前 5 个信号做裸符号条件(生成器用 big.sN 命名)
Q='(count (|| (= s0 1) (= s1 1) (= s2 1) (= s3 1) (= s4 1)))'
run() { # run <标签> [ENV=VAL…] -- <表达式>
    local label="$1"; shift
    local envs=(); while [ "${1:-}" != "--" ]; do envs+=("$1"); shift; done; shift
    local t0 t1 out
    t0=$(date +%s.%N)
    out=$(env "${envs[@]}" WAL_CACHE=off timeout 900 "$BIN" "$1" -l "$wave" 2>&1 | tail -1)
    t1=$(date +%s.%N)
    printf '  %-26s %8.2fs   %s\n' "$label" "$(echo "$t1 - $t0" | bc)" "$out"
}

echo
echo "查询: $Q"
run "引擎(区间扫描)"        -- "$Q"
run "逐拍(WAL_NO_ENGINE=1)" WAL_NO_ENGINE=1 -- "$Q"
run "字符串形式对照"        -- '(count (|| (= (get "big.s0") 1) (= (get "big.s1") 1) (= (get "big.s2") 1) (= (get "big.s3") 1) (= (get "big.s4") 1)))'
echo
echo "判读: step 行是「每个索引求值一次」的最坏情况;若它与字符串形式对照相差巨大,"
echo "      说明裸符号路径又退化成「每次求值扫/克隆整张名字表」了 —— 见上方背景说明。"
