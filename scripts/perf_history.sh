#!/usr/bin/env bash
# perf_history.sh — 版本级回归基准: 每操作独立进程 wall+RSS, 追加到 bench/perf-history.csv
# usage: bash scripts/perf_history.sh [wave] [signal] [tag-or-current]
set -u
WAVE=${1:-bench/data/bench_2g.vcd}
SIG=${2:-s0}
TAG=${3:-current}
BIN=${BIN:-target/release/wal-rust}
ROOT="$(git rev-parse --show-toplevel)"
VERSION=$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
CSV="$ROOT/bench/perf-history.csv"
mkdir -p "$(dirname "$CSV")"
[ -f "$CSV" ] || echo "version,date,op,fixture,wall_s,rss_mb,note" > "$CSV"
DATE=$(date +%F)
if [[ "$TAG" == "current" ]]; then
  BIN="$ROOT/target/release/wal-rust"
else
  BIN="$ROOT/.tools/wt-$TAG/target/release/wal-rust"
fi
run() {
  local name="$1"; shift
  local out t m
  out=$(timeout 900 /usr/bin/time -f "%e %M" "$BIN" "$@" 2>&1)
  t=$(echo "$out" | sed -n 's/^\([0-9][0-9.]*\) .*/\1/p' | tail -1)
  m=$(echo "$out" | sed -n 's/^[0-9.]* \([0-9]*\)$/\1/p' | tail -1)
  printf "%s,%s,%s,%s,%s,%s,\n" "$VERSION" "$DATE" "$name" "$(basename "$WAVE")" "${t:-NA}" "${m:-NA}" >> "$CSV"
  echo "$name ${t:-NA}s ${m:-NA}KB"
}
echo "version=$VERSION tag=$TAG"
run load '(length (SIGNALS))' -l "$WAVE"
run count-lit '(count (= (get "'$SIG'") 1))' -l "$WAVE"
run count-var '(define v (get "'$SIG'")) (count (= (get "'$SIG'") v))' -l "$WAVE"
run count-rise '(count (rising "'$SIG'"))' -l "$WAVE"
run at '(at "'$SIG'" 12345678)' -l "$WAVE"
