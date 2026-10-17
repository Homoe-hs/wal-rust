#!/usr/bin/env bash
# =============================================================================
# bench_cache_only.sh — #2(cache-only)× #3(惰性)四象限实验装置
#
# 四象限:(小波形 / 大波形)×(单次查询 / 多次查询),每格两种实现:
#   normal     = 现状(open NPI + 物化名字表)
#   cache_only = 只用侧车(.fnames/.ftl/.fcol),完全不碰 NPI(WAL_FSDB_CACHE_ONLY=1)
#
# 输出 CSV(wave,mode,variant,wall_s,rss_mb,load_s,probe_s,probes_ok,correct)
#   wall_s   /usr/bin/time %e(整进程)     rss_mb /usr/bin/time %M
#   load_s   WAL_PROF 阶段账 load          probe_s  阶段账 probe 合计
#   probes_ok 实际拿到 => 的行数(应为 1 / MULTI_N)
#   correct  与 normal 输出逐行一致 → yes;不一致 → DIFF;自身失败 → FAIL
#
# 纪律:同一时刻一个 FSDB 进程;许可不稳 → 每次测量前 license_up 并在同一 bash 调用里执行。
# =============================================================================
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
ROOT="$PWD"
BIN="${BIN:-$ROOT/target/release/wal-rust}"
OUTDIR="${OUTDIR:-$ROOT/.tools/bench/co}"
CSV="${CSV:-$OUTDIR/cache_only.csv}"
BIG="${WAVE_BIG:-/home/hesheng/Downloads/core 级波形/aicore_smoke_test_000.fsdb}"
SMALL="${WAVE_SMALL:-$ROOT/.tools/bench/v2f/many4m.fsdb}"
MULTI_N="${MULTI_N:-20}"
mkdir -p "$OUTDIR"
[ -f "$CSV" ] || echo "wave,mode,variant,wall_s,rss_mb,load_s,probe_s,probes_ok,correct" > "$CSV"

say() { printf '[co] %s\n' "$*"; }
license() {
    [ -f "$ROOT/.tools/vm/host_fsdb_env.sh" ] && . "$ROOT/.tools/vm/host_fsdb_env.sh" >/dev/null 2>&1
    [ -f "$ROOT/.tools/vm/license_up.sh" ] && sh "$ROOT/.tools/vm/license_up.sh" >/dev/null 2>&1
    return 0
}
pick_sig() {
    local w="$1" s
    s=$(timeout -k 5 300 "$BIN" sigs "$w" . 5 2>/dev/null | grep -aE '^[A-Za-z_][A-Za-z0-9_.]*$' | head -1)
    printf '%s' "$s"
}
measure() {
    local t="$1" variant="$2" wave="$3" probes="$4" log="$OUTDIR/.run.log"
    local -a envs=()
    [ "$variant" = "cache_only" ] && envs=(WAL_FSDB_CACHE_ONLY=1)
    license
    env "${envs[@]}" WAL_PROF=1 timeout -k 5 "$t" /usr/bin/time -f "__TIME__%e %M" \
        "$BIN" --stdin -l "$wave" < "$probes" > "$OUTDIR/.out.$variant" 2> "$log"
    local tl; tl=$(grep -a '__TIME__' "$log" | tail -1)
    N_WALL="-"; N_RSS="0"
    if [ -n "$tl" ]; then
        N_WALL=$(printf '%s' "$tl" | sed 's/.*__TIME__//; s/ .*//')
        N_RSS=$(( $(printf '%s' "$tl" | awk '{print $NF}') / 1024 ))
    fi
    N_OK=$(grep -ac '^=>' "$OUTDIR/.out.$variant" 2>/dev/null || true)
    N_LOAD=$(awk '/^\[prof\]   load /{print $3; exit}' "$log" | sed 's/s$//');   N_LOAD="${N_LOAD:--}"
    N_PROBE=$(awk '/^\[prof\]   probe /{print $3; exit}' "$log" | sed 's/s$//'); N_PROBE="${N_PROBE:--}"
}
emit() { # label mode variant wall rss load probe ok correct
    printf '%s,%s,%s,%s,%s,%s,%s,%s,%s\n' "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$8" "$9" >> "$CSV"
    say "$1 $2 $3: wall=${4}s rss=${5}MB load=${6}s probe=${7}s ok=${8} correct=${9}"
}
run_cell() {
    local label="$1" wave="$2" sig="$3"
    local one="$OUTDIR/.one.wal" many="$OUTDIR/.many.wal"
    printf '(count (changes "%s"))\n' "$sig" > "$one"
    : > "$many"; local i; for i in $(seq 1 "$MULTI_N"); do printf '(count (changes "%s"))\n' "$sig" >> "$many"; done
    # 预热**被测信号本身**的 .fcol:cache-only 只能读已缓存的列(缺列会明确报错, 这是设计)
    license
    timeout -k 5 600 "$BIN" --stdin -l "$wave" < "$one" >/dev/null 2>&1

    measure 600 normal "$wave" "$one";     local AW="$N_WALL" AR="$N_RSS" AL="$N_LOAD" AP="$N_PROBE" AOK="$N_OK"; cp "$OUTDIR/.out.normal" "$OUTDIR/.out.normal.1"
    measure 600 cache_only "$wave" "$one"; local BW="$N_WALL" BR="$N_RSS" BL="$N_LOAD" BP="$N_PROBE" BOK="$N_OK"
    local c1; if [ "$AOK" != "1" ] || [ "$BOK" != "1" ]; then c1="FAIL"; elif diff -q "$OUTDIR/.out.normal.1" "$OUTDIR/.out.cache_only" >/dev/null 2>&1; then c1="yes"; else c1="DIFF"; fi
    emit "$label" single normal     "$AW" "$AR" "$AL" "$AP" "$AOK" "$c1"
    emit "$label" single cache_only "$BW" "$BR" "$BL" "$BP" "$BOK" "$c1"

    measure 900 normal "$wave" "$many";     local AW2="$N_WALL" AR2="$N_RSS" AL2="$N_LOAD" AP2="$N_PROBE" AOK2="$N_OK"; cp "$OUTDIR/.out.normal" "$OUTDIR/.out.normal.2"
    measure 900 cache_only "$wave" "$many"; local BW2="$N_WALL" BR2="$N_RSS" BL2="$N_LOAD" BP2="$N_PROBE" BOK2="$N_OK"
    local c2; if [ "$AOK2" != "$MULTI_N" ] || [ "$BOK2" != "$MULTI_N" ]; then c2="FAIL"; elif diff -q "$OUTDIR/.out.normal.2" "$OUTDIR/.out.cache_only" >/dev/null 2>&1; then c2="yes"; else c2="DIFF"; fi
    emit "$label" multi normal      "$AW2" "$AR2" "$AL2" "$AP2" "$AOK2" "$c2"
    emit "$label" multi cache_only  "$BW2" "$BR2" "$BL2" "$BP2" "$BOK2" "$c2"
}

say "大波形: $BIG"
license
BSIG=$(pick_sig "$BIG")
say "大波形信号: ${BSIG:-<没取到>}"
if [ -n "$BSIG" ]; then
    printf '(length (SIGNALS))\n' > "$OUTDIR/.warm.wal"
    license; timeout -k 5 600 "$BIN" --stdin -l "$BIG" < "$OUTDIR/.warm.wal" >/dev/null 2>&1
    run_cell big "$BIG" "$BSIG"
fi
if [ -f "$SMALL" ]; then
    say "小波形: $SMALL"
    license; timeout -k 5 300 "$BIN" '(length (SIGNALS))' -l "$SMALL" >/dev/null 2>&1
    SSIG=$(pick_sig "$SMALL")
    say "小波形信号: ${SSIG:-<没取到>}"
    [ -n "$SSIG" ] && run_cell small "$SMALL" "$SSIG"
else
    say "小波形缺失, 跳过: $SMALL"
fi
say "完成 → $CSV"
column -t -s, "$CSV" 2>/dev/null || cat "$CSV"
