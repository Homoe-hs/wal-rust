#!/usr/bin/env bash
# ============================================================================
# FSDB ↔ VCD 快速一致性诊断(1 分钟级)
#
#   ./scripts/fsdb_vcd_quickcheck.sh design.fsdb design.vcd [信号名...]
#
# 用途: 当"同一设计导出的 FSDB 与 VCD 结果对不上"时, 先回答三个问题:
#   ① 两边信号集是否一致(数量/名字交集/各自独有)
#   ② 索引空间(时间线长度)是否一致 —— 不一致说明 dump 范围/时间刻度不同
#   ③ 典型信号的关键计数是否一致(边沿/电平/变更点)
#
# 退出码: 0 = 三项都一致; 1 = 有不一致(打印具体差异, 便于贴给上游确认是不是格式转换问题)。
# 需要: 读 FSDB 需要 $VERDI_HOME 或 $WAL_NPI_LIB(+ 许可); 读 VCD 无依赖。
# ============================================================================
set -uo pipefail

FSDB="${1:-}"; VCD="${2:-}"; shift 2 2>/dev/null || true
SIGS=("$@")
BIN="${WAL_BIN:-target/release/wal-rust}"

if [ -z "$FSDB" ] || [ -z "$VCD" ]; then
    sed -n '2,12p' "$0"; exit 2
fi
for f in "$FSDB" "$VCD"; do
    [ -f "$f" ] || { echo "找不到文件: $f"; exit 2; }
done
[ -x "$BIN" ] || { echo "找不到 $BIN(先 cargo build --release 或 make dist)"; exit 2; }

export WAL_CACHE="${WAL_CACHE:-off}"      # 诊断要的是"读到的当下事实", 别用缓存
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
rc=0

q() { # q <wave> <expr> → 最后一个输出行(去掉 `=> ` 前缀)
    "$BIN" "$2" -l "$1" 2>&1 | tail -1 | sed 's/^=> //'
}

echo "=== ① 信号集 ==="
"$BIN" sigs "$FSDB" '.*' 1000000 2>/dev/null | tail -n +2 | sort > "$work/fsdb.names"
"$BIN" sigs "$VCD"  '.*' 1000000 2>/dev/null | tail -n +2 | sort > "$work/vcd.names"
nf=$(wc -l < "$work/fsdb.names"); nv=$(wc -l < "$work/vcd.names")
common=$(comm -12 "$work/fsdb.names" "$work/vcd.names" | wc -l)
printf '  FSDB: %s 个信号 / VCD: %s 个信号 / 同名交集: %s\n' "$nf" "$nv" "$common"
if [ "$common" -eq 0 ]; then
    echo "  ⚠️ 没有任何同名信号 —— 两份波形**不是同一命名体系**(先确认导出选项/层次前缀)"
    comm -23 "$work/fsdb.names" "$work/vcd.names" | head -5 | sed 's/^/    FSDB 独有: /'
    comm -13 "$work/fsdb.names" "$work/vcd.names" | head -5 | sed 's/^/    VCD  独有: /'
    rc=1
elif [ "$nf" != "$nv" ] || [ "$common" != "$nf" ]; then
    echo "  ⚠️ 信号集不完全一致(常见原因: VCD 把总线位炸开 / FSDB 合并;或导出层次不同)"
    comm -23 "$work/fsdb.names" "$work/vcd.names" | head -5 | sed 's/^/    FSDB 独有: /'
    comm -13 "$work/fsdb.names" "$work/vcd.names" | head -5 | sed 's/^/    VCD  独有: /'
    rc=1
else
    echo "  ✅ 信号集完全一致"
fi

echo "=== ② 索引空间 ==="
mxf=$(q "$FSDB" '(MAX-INDEX)'); mxv=$(q "$VCD" '(MAX-INDEX)')
printf '  FSDB MAX-INDEX: %s / VCD MAX-INDEX: %s\n' "$mxf" "$mxv"
if [ "$mxf" != "$mxv" ]; then
    echo "  ⚠️ 索引空间长度不同 —— 之后所有'计数'都会被这个差异污染(先对齐 dump 范围/时间刻度)"
    rc=1
else
    echo "  ✅ 索引空间一致"
fi

echo "=== ③ 典型信号计数 ==="
if [ ${#SIGS[@]} -eq 0 ]; then
    # 没给信号就取两边共有的前 3 个(名字已排序, 结果可复现)
    mapfile -t SIGS < <(comm -12 "$work/fsdb.names" "$work/vcd.names" | head -3)
fi
for s in "${SIGS[@]}"; do
    [ -n "$s" ] || continue
    printf '  -- %s\n' "$s"
    for expr in "(count (changes \"$s\"))" "(count (rising \"$s\"))" "(count (= (get \"$s\") 1))" "(count (is-x \"$s\"))"; do
        a=$(q "$FSDB" "$expr"); b=$(q "$VCD" "$expr")
        if [ "$a" = "$b" ]; then
            printf '     ✅ %-34s %s\n' "$expr" "$a"
        else
            printf '     ❌ %-34s fsdb=%s  vcd=%s\n' "$expr" "$a" "$b"
            rc=1
        fi
    done
done

echo
if [ $rc -eq 0 ]; then
    echo "结论: 三面一致 —— 两份波形可视为同一份仿真;若仍有结果差异, 请带最小复现提 issue。"
else
    echo "结论: 存在不一致(见上面 ⚠️/❌)。请把本输出贴给导出方确认是不是 fsdb2vcd 的匹配问题"
    echo "      (常见: 只导了部分层次、把 x/z 归零、时间刻度不同、dumpports/EVCD)。"
fi
exit $rc
