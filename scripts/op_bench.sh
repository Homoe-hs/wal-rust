#!/usr/bin/env bash
# op_bench.sh — wal-rust 操作级冷启动基准:每操作一个新进程,记录 wall+峰值 RSS。
# usage: bash scripts/op_bench.sh [wave] [signal]
set -u
WAVE=${1:-bench/data/bench_2g.vcd}
SIG=${2:-s0}
BIN=${BIN:-target/release/wal-rust}

printf "%-22s %10s %12s\n" "op" "wall(s)" "rss(MB)"
measure() {
  local name="$1"; shift
  local t m out
  out=$(/usr/bin/time -f "%e %M" "$BIN" "$@" 2>&1)
  t=$(echo "$out" | sed -n 's/^\([0-9][0-9.]*\) .*/\1/p' | tail -1)
  m=$(echo "$out" | sed -n 's/^[0-9.]* \([0-9]*\)$/\1/p' | tail -1)
  m=$(( ${m:-0} / 1024 ))
  printf "%-22s %10s %12s\n" "$name" "${t:-err}" "${m:-?}"
}

measure load           '(length (SIGNALS))' -l "$WAVE"
measure get-idx0       '(get "'$SIG'")' -l "$WAVE"
measure count-lit      '(count (= (get "'$SIG'") 1))' -l "$WAVE"
measure count-var      '(define v (get "'$SIG'")) (count (= (get "'$SIG'") v))' -l "$WAVE"
measure count-rise     '(count (rising "'$SIG'"))' -l "$WAVE"
measure count-is-x     '(count (is-x "'$SIG'"))' -l "$WAVE"
measure find-rise      '(length (find (rising "'$SIG'")))' -l "$WAVE"
measure at-time        '(at "'$SIG'" 12345678)' -l "$WAVE"
measure getwave-len    '(length (getwave "'$SIG'"))' -l "$WAVE"
measure changes-len    '(length (getwave "'$SIG'"))' -l "$WAVE"

# warm same-process: two queries, second answers from the column cache
out=$(/usr/bin/time -f "%e %M" "$BIN" -c '(define n (count (= (get "'$SIG'") 1))) n' -l "$WAVE" 2>&1)
t=$(echo "$out" | sed -n 's/^\([0-9][0-9.]*\) .*/\1/p' | tail -1)
printf "%-22s %10s %12s\n" "count-lit-warm-2nd" "${t:-err}" "?"
