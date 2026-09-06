#!/usr/bin/env bash
# wal-rust 大文件基准: load / 首查 / 重复查 / RSS
# 用法: scripts/run_bench.sh <file.vcd> [signal]
set -u
F="$1"
SIG="${2:-s0}"
BIN=./target/release/wal-rust
[ -f "$F" ] || { echo "missing $F"; exit 1; }

say() { printf "%-42s %s\n" "$1" "$2"; }

run() { # label, wal args...
    local label="$1"; shift
    local out
    out=$( { /usr/bin/time -v "$BIN" "$@" 2>&1 & } ; \
           wait $! )  # crude: keep below
    echo "$out" | tail -n +1 | head -1
}

# --- cold start: load + first query (single expression, one process) ---
echo "=== $F (signal $SIG) ==="
/usr/bin/time -v "$BIN" "(count (= (get \"$SIG\") 0))" -l "$F" > bench/data/out_cold.txt 2> bench/data/time_cold.txt
grep -E "^=>" bench/data/out_cold.txt | head -1 | sed 's/^=> /cold:  /'
grep -E "wall clock|Maximum resident" bench/data/time_cold.txt | sed 's/^/time:   /'

# --- load only (index build; separates load from query cost) ---
/usr/bin/time -v "$BIN" '(MAX-INDEX)' -l "$F" > bench/data/out_load.txt 2> bench/data/time_load.txt
grep -E "^=>" bench/data/out_load.txt | head -1 | sed 's/^/load:   /'
grep -E "wall clock|Maximum resident" bench/data/time_load.txt | sed 's/^/time:   /'

# --- warm repeat + second signal in ONE process (signal_cache hit) ---
cat > bench/data/run_warm.wal <<EOF
(print "warm1" (count (= (get "$SIG") 0)))
(print "warm2" (count (= (get "$SIG") 1)))
EOF
/usr/bin/time -v "$BIN" run bench/data/run_warm.wal -l "$F" > bench/data/out_warm.txt 2> bench/data/time_warm.txt
grep -E "^warm" bench/data/out_warm.txt | head -2 | sed 's/^/warm:   /'
grep -E "wall clock|Maximum resident" bench/data/time_warm.txt | sed 's/^/time:   /'

echo
