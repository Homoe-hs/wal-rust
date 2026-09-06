#!/usr/bin/env bash
# 差分门禁: 新旧 find_indices 实现必须逐值一致。
# 用法: scripts/diff_find.sh <old-bin> <new-bin> [fixture...]
# 每个 fixture 一个进程批量跑全部查询(快); 逐查询 diff。
set -u
OLD="$1"; NEW="$2"; shift 2
FIXTURES=("$@")
[ ${#FIXTURES[@]} -eq 0 ] && FIXTURES=(.tools/xv.vcd .tools/zv2.vcd bench/data/small.vcd)
fail=0
for f in "${FIXTURES[@]}"; do
  [ -f "$f" ] || { echo "skip $f"; continue; }
  SIGS=$("$OLD" '(find-sig "clk")' -l "$f" 2>/dev/null | grep -o '"[^"]*"' | tr -d '"' | head -1)
  [ -z "$SIGS" ] && SIGS=$("$OLD" '(find-sig "v")' -l "$f" 2>/dev/null | grep -o '"[^"]*"' | tr -d '"' | head -1)
  [ -z "$SIGS" ] && { echo "skip $f (no signals)"; continue; }
  {
    echo "(print \"q1\" (count (= (get \"$SIGS\") 0)))"
    echo "(print \"q2\" (count (!= (get \"$SIGS\") 0)))"
    echo "(print \"q3\" (count (= (get \"$SIGS\") 1)))"
    echo "(print \"q4\" (length (find (rising \"$SIGS\"))))"
    echo "(print \"q5\" (length (find (falling \"$SIGS\"))))"
    echo "(print \"q6\" (length (find (changes \"$SIGS\"))))"
    echo "(print \"q7\" (count (is-x \"$SIGS\")))"
    echo "(print \"q8\" (count (is-z \"$SIGS\")))"
    echo "(print \"q9\" (length (getwave \"$SIGS\")))"
  } > .tools/diff_wal.wal
  o=$("$OLD" run .tools/diff_wal.wal -l "$f" 2>&1)
  n=$("$NEW" run .tools/diff_wal.wal -l "$f" 2>&1)
  if [ "$o" != "$n" ]; then
    echo "FAIL [$f]"
    diff <(echo "$o") <(echo "$n") | head -12
    fail=1
  else
    echo "ok   $f  ($(echo "$o" | tr '\n' ' ' | head -c 100)...)"
  fi
done
[ $fail -eq 0 ] && echo "DIFF-GATE: ALL MATCH" || echo "DIFF-GATE: FAILURES"
exit $fail
