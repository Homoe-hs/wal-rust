#!/usr/bin/env bash
# =============================================================================
# bench_lazy_index.sh — #3(惰性/不物化)最小原型 A/B:名字索引**惰性构建**
#
# 现状: 加载时预建全名索引(1796 万信号 ≈ 0.82s / ~400MB), 即使查询根本不解析名字。
# 原型: 索引改为 OnceCell, 首次解析名字时才建(元信息类查询完全不付这笔钱)。
# 开关: 同一二进制, `WAL_FSDB_EAGER_INDEX=1` 强制旧行为(加载时建) → 同二进制 A/B。
#
# 输出 .tools/bench/co/lazy_index.csv(wave,query,variant,wall_s,rss_mb,load_s,correct)
# =============================================================================
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
ROOT="$PWD"; BIN="${BIN:-$ROOT/target/release/wal-rust}"
OUTDIR="${OUTDIR:-$ROOT/.tools/bench/co}"; CSV="$OUTDIR/lazy_index.csv"
BIG="${WAVE_BIG:-/home/hesheng/Downloads/core 级波形/aicore_smoke_test_000.fsdb}"
SMALL="${WAVE_SMALL:-$ROOT/.tools/bench/v2f/many4m.fsdb}"
MULTI_N="${MULTI_N:-20}"
mkdir -p "$OUTDIR"; [ -f "$CSV" ] || echo "wave,query,variant,wall_s,rss_mb,load_s,correct" > "$CSV"
say() { printf '[lazy %s] %s\n' "$(date +%T)" "$*"; }
license() {
    [ -f "$ROOT/.tools/vm/host_fsdb_env.sh" ] && . "$ROOT/.tools/vm/host_fsdb_env.sh" >/dev/null 2>&1
    [ -f "$ROOT/.tools/vm/license_up.sh" ] && sh "$ROOT/.tools/vm/license_up.sh" >/dev/null 2>&1
    return 0
}
measure() { # $1=variant(eager|lazy) $2=wave $3=probes $4=tag
    local variant="$1" wave="$2" probes="$3" tag="$4" log="$OUTDIR/.lazy.log"
    local -a envs=(); [ "$variant" = "eager" ] && envs=(WAL_FSDB_EAGER_INDEX=1)
    env "${envs[@]}" WAL_PROF=1 timeout -k 5 600 /usr/bin/time -f "__TIME__%e %M" \
        "$BIN" --stdin -l "$wave" < "$probes" > "$OUTDIR/.lazy.$variant.out" 2> "$log"
    local tl; tl=$(grep -a '__TIME__' "$log" | tail -1)
    local W="-" R="0"; if [ -n "$tl" ]; then W=$(printf '%s' "$tl" | sed 's/.*__TIME__//; s/ .*//'); R=$(( $(printf '%s' "$tl" | awk '{print $NF}') / 1024 )); fi
    local L; L=$(awk '/^\[prof\]   load /{print $3; exit}' "$log" | sed 's/s$//'); L="${L:--}"
    printf '%s,%s,%s,%s,%s,%s,%s\n' "$(basename "$wave")" "$tag" "$variant" "$W" "$R" "$L" "-" >> "$CSV"
    say "$tag $variant: wall=${W}s rss=${R}MB load=${L}s"
}
run_pair() { # $1=wave $2=tag $3=probes
    license
    measure eager "$1" "$3" "$2"; cp "$OUTDIR/.lazy.eager.out" "$OUTDIR/.lazy.eager.keep"
    measure lazy  "$1" "$3" "$2"
    local c; if diff -q "$OUTDIR/.lazy.eager.keep" "$OUTDIR/.lazy.lazy.out" >/dev/null 2>&1; then c=yes; else c=DIFF; fi
    say "$2 正确性: $c"; echo "$c" > "$OUTDIR/.lazy.correct.$2"
}
mkdir -p "$OUTDIR"
license
META="$OUTDIR/.meta.wal"; printf '(length (SIGNALS))\n' > "$META"
META20="$OUTDIR/.meta20.wal"; : > "$META20"; for i in $(seq 1 "$MULTI_N"); do printf '(length (SIGNALS))\n' >> "$META20"; done
NAME="$OUTDIR/.name.wal"; SIG=$(timeout -k 5 300 "$BIN" sigs "$BIG" . 1 2>/dev/null | grep -aE '^[A-Za-z_][A-Za-z0-9_.]*$' | head -1)
[ -n "$SIG" ] && printf '(count (changes "%s"))\n' "$SIG" > "$NAME"
say "大波形: $BIG(信号 ${SIG:-<无>})"
[ -f "$BIG" ] && { run_pair "$BIG" big-single-meta "$META"; run_pair "$BIG" big-multi-meta "$META20"; [ -n "$SIG" ] && run_pair "$BIG" big-single-name "$NAME"; }
if [ -f "$SMALL" ]; then
    license
    S2=$(timeout -k 5 300 "$BIN" sigs "$SMALL" . 1 2>/dev/null | grep -aE '^[A-Za-z_][A-Za-z0-9_.]*$' | head -1)
    printf '(length (SIGNALS))\n' > "$META"
    say "小波形: $SMALL"
    run_pair "$SMALL" small-single-meta "$META"
    [ -n "$S2" ] && { printf '(count (changes "%s"))\n' "$S2" > "$NAME"; run_pair "$SMALL" small-single-name "$NAME"; }
fi
say "完成 → $CSV"; column -t -s, "$CSV" 2>/dev/null || cat "$CSV"
