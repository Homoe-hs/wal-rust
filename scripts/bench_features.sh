#!/usr/bin/env bash
# ============================================================================
# bench_features.sh — 给 wal-rust 的**每个功能**在真波形上建可回归的耗时基线。
#
# 两类口径(按任务约定):
#   mode A `single`  —— 每条功能一个**独立进程**: 量"加载 + 该功能"的端到端成本。
#   mode B `session` —— 一个 `--stdin` 会话里跑完全部 probe: 量"查询侧边际成本"
#                       (加载只付一次; 数据源是 `WAL_PROF=1` 的阶段账)。
#
# 用法:
#   scripts/bench_features.sh single          # 逐功能起进程, 每功能 3 次取中位
#   scripts/bench_features.sh session         # 单次 --stdin 会话, 解析 [prof] 阶段账
#   scripts/bench_features.sh all             # 两类顺序跑(内部保证不并发 FSDB)
#   scripts/bench_features.sh append-csv      # 按现有 schema 追加进 bench/perf-history.csv
#   scripts/bench_features.sh check           # 离线回归闸(不需要 FSDB; 委托 python 脚本)
#   scripts/bench_features.sh emit-probes     # 由 features.tsv 重新生成 probes.wal
#   scripts/bench_features.sh --help
#
# 清单落位(权威 = 入库的那份):
#   bench/features.tsv + bench/probes.wal   ← **权威**, 与版本一起入库, 基线靠它复现
#   .tools/bench/baseline/*.tsv|wal         ← 本地副本(emit-probes 自动同步), 只当便利
#
# 产物(默认目录 .tools/bench/baseline/):
#   single.csv          列 feature,mode,run,wall_s,rss_mb,cmd   (每次 single 重写)
#   single.median.csv   列 feature,mode,median_wall_s,median_rss_mb,runs,cmd
#   session.csv         列 feature,mode,run,wall_s,rss_mb,cmd
#                       mode=session      → 阶段账条目(wall-total/session/load/probe/probe-mean/fsdb:*)
#                       mode=session-probe→ **每个功能的会话内边际耗时**(WAL_PROF_TRACE 的 trace probe #k)
#   session.<n>.out/.err  mode B 的**分流**原始输出(.out=值行, .err=[prof] 阶段账; 逐探针归因只认 .err)
#   session.<n>.log     mode B 的合流副本(仅兼容/人读; 管道刷新可能让两路行交错, 不可用于归因)
#   single.failures.tsv 失败/超时的功能与首行错误
#
# 硬约束(踩过才写):
#   * **同一时刻只允许一个 FSDB 进程**(许可席位 + 计时噪声) —— 本脚本全程串行,
#     并用 .bench.lock 串行化自身。
#   * 许可通道不稳: 每次测量前在同一进程内 source 环境 + `license_up.sh`, 失败自动
#     重跑配方重试(共 3 次机会)。测 FSDB 时**必须**让它走这条路径(`--license off` 只给小夹具/VCD)。
#   * 一律 `timeout -k 5 N`; 产物只落 `--outdir`(默认 .tools/bench/baseline/), 不落 /tmp。
#   * 每条记录都带完整命令行(`cmd` 列), 数字可复现。
#
# 环境变量覆盖: WAL_BENCH_BIN / WAL_BENCH_WAVE / WAL_BENCH_RUNS / WAL_BENCH_TIMEOUT /
#               WAL_BENCH_SESSION_TIMEOUT / WAL_BENCH_OUTDIR / WAL_BENCH_LICENSE(each)
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 2

# ---- 默认值 ----------------------------------------------------------------
BIN="${WAL_BENCH_BIN:-target/release/wal-rust}"
WAVE="${WAL_BENCH_WAVE:-/home/hesheng/Downloads/core 级波形/aicore_smoke_test_000.fsdb}"
OUTDIR="${WAL_BENCH_OUTDIR:-.tools/bench/baseline}"
FEATURES=""
PROBES=""
RUNS="${WAL_BENCH_RUNS:-5}"
SESSION_RUNS="${WAL_BENCH_SESSION_RUNS:-1}"
# single --append: 不清空已有数据(用于单点补测, 如 edges-list 归位), 之后会重算中位表
APPEND_SINGLE="${WAL_BENCH_APPEND:-0}"
# 非空时 run_measured 把 stdout/stderr 分流到 <prefix>.out / <prefix>.err
SPLIT_PREFIX=""
TIMEOUT="${WAL_BENCH_TIMEOUT:-900}"
SESSION_TIMEOUT="${WAL_BENCH_SESSION_TIMEOUT:-1800}"
ONLY=""
INCLUDE_HEAVY=0
LICENSE_MODE="${WAL_BENCH_LICENSE:-auto}"     # auto | on | off
LICENSE_EACH="${WAL_BENCH_LICENSE_EACH:-0}"   # 1 = 每次测量前都重跑配方(更稳更慢)
APPEND_AFTER_ALL=0
EXTRA_NOTE="${WAL_BENCH_NOTE:-}"
CSV_FILE="bench/perf-history.csv"
CHECK_ARGS=()

TIME_MARK="@@bench_time@@"
TIME_FMT="${TIME_MARK}%e %M"
TIME_RE="^${TIME_MARK}[0-9.]+ [0-9]+$"

say()  { printf '[bench] %s\n' "$*"; }
info() { printf '[bench] %s\n' "$*" >&2; }
warn() { printf '[bench] !! %s\n' "$*" >&2; }
die()  { printf '[bench] 错误: %s\n' "$*" >&2; exit 2; }

usage() {
    sed -n '1,45p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    cat <<'EOF'

选项:
  --wave PATH        被测波形 (默认真波形 aicore_smoke_test_000.fsdb)
  --bin PATH         被测二进制 (默认 target/release/wal-rust)
  --features FILE    功能清单 TSV (默认 bench/features.tsv = 权威; 回退 .tools 本地副本)
  --probes FILE      mode B 的 probe 清单 (默认 bench/probes.wal = 权威; 回退 .tools 本地副本)
  --outdir DIR       产物目录 (默认 .tools/bench/baseline)
  --runs N           mode A 每功能重复次数 (默认 5, 取中位)
  --append           配合 single: 不清空既有 CSV, 只补测(之后重算中位表)
  --session-runs N   mode B 重复次数 (默认 1)
  --timeout SEC      mode A 单次超时 (默认 900)
  --session-timeout SEC mode B 单次超时 (默认 1800)
  --only REGEX       只跑名字匹配的功能 (grep -E, 可重复)
  --include-heavy    包含标了 heavy 的功能(如 topsig: 真波形上是数分钟~小时级)
  --license MODE     auto|on|off (默认 auto: .fsdb → 走许可配方)
  --license-each     每次测量前都重跑一次许可配方(默认只在开头与失败时)
  --append           `all` 跑完顺手 append-csv
  --note TEXT        追加到写进 perf-history.csv 的 note
  --csv PATH         perf-history.csv 路径 (默认 bench/perf-history.csv)
  --baseline VER     (check) 基线版本号; 默认取每个 op 最早出现的版本
  --new VER          (check) 新版本号; 默认取 CSV 里最后出现的版本
  --threshold PCT    (check) 回归阈值百分比 (默认 20)
  -h, --help         本帮助
EOF
}

# ---- 参数 ------------------------------------------------------------------
if [ $# -eq 0 ]; then usage; exit 2; fi
CMD="$1"; shift
case "$CMD" in
    -h|--help|help) usage; exit 0 ;;
    single|session|all|append-csv|append|check|--check|emit-probes|verify) ;;
    *) die "未知子命令 '$CMD'(用 --help)" ;;
esac
[ "$CMD" = "--check" ] && CMD="check"
[ "$CMD" = "append" ] && CMD="append-csv"

while [ $# -gt 0 ]; do
    case "$1" in
        --wave) WAVE="$2"; shift 2 ;;
        --bin) BIN="$2"; shift 2 ;;
        --features) FEATURES="$2"; shift 2 ;;
        --probes) PROBES="$2"; shift 2 ;;
        --outdir) OUTDIR="$2"; shift 2 ;;
        --runs) RUNS="$2"; shift 2 ;;
        --append) APPEND_SINGLE=1; shift ;;
        --session-runs) SESSION_RUNS="$2"; shift 2 ;;
        --timeout) TIMEOUT="$2"; shift 2 ;;
        --session-timeout) SESSION_TIMEOUT="$2"; shift 2 ;;
        --only) ONLY="${ONLY:+$ONLY|}${2//\//\\/}"; shift 2 ;;
        --include-heavy) INCLUDE_HEAVY=1; shift ;;
        --license) LICENSE_MODE="$2"; shift 2 ;;
        --license-each) LICENSE_EACH=1; shift ;;
        --append) APPEND_AFTER_ALL=1; shift ;;
        --note) EXTRA_NOTE="$2"; shift 2 ;;
        --csv) CSV_FILE="$2"; shift 2 ;;
        --baseline|--new|--threshold|--min-wall|--noise-floor|--op)
            CHECK_ARGS+=("$1" "$2"); shift 2 ;;
        --gate-rss|--quiet|--json)
            CHECK_ARGS+=("$1"); shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "未知选项 '$1'(用 --help)" ;;
    esac
done

[ -x "$BIN" ] || die "找不到可执行文件 $BIN(先 cargo build --release)"
[ -f "$WAVE" ] || die "找不到波形 $WAVE"

# 清单落位(权威 = 入库的那份):
#   显式 --features/--probes  >  bench/features.tsv | bench/probes.wal(**权威**, 跟版本走)
#   >  .tools/bench/baseline/ 下的本地副本
# 判据: 清单决定"量哪些功能/哪些命令", 必须跟被测版本一起入库才可复现; .tools 只是本地副本
# (踩过: .tools 副本过期 → 装置静默量了旧 probe 集, 看着跑通了其实测错对象)。
pick_file() {
    local explicit="$1" repo="$2" mirror="$3"
    if [ -n "$explicit" ]; then printf '%s' "$explicit"; return; fi
    if [ -f "$repo" ]; then printf '%s' "$repo"; return; fi
    printf '%s' "$mirror"
}
AUTH_FEATURES="bench/features.tsv"
AUTH_PROBES="bench/probes.wal"
LOCAL_FEATURES=".tools/bench/baseline/features.tsv"
LOCAL_PROBES=".tools/bench/baseline/probes.wal"
FEATURES="$(pick_file "$FEATURES" "$AUTH_FEATURES" "$LOCAL_FEATURES")"
PROBES="$(pick_file "$PROBES" "$AUTH_PROBES" "$LOCAL_PROBES")"

SINGLE_CSV="$OUTDIR/single.csv"
SINGLE_MED="$OUTDIR/single.median.csv"
SESSION_CSV="$OUTDIR/session.csv"
FAIL_LOG="$OUTDIR/single.failures.tsv"

# ---- 许可配方 --------------------------------------------------------------
is_fsdb() { case "$(basename "$WAVE" | tr 'A-Z' 'a-z')" in *.fsdb) return 0 ;; *) return 1 ;; esac; }

need_license() {
    case "$LICENSE_MODE" in
        off) return 1 ;;
        on)  return 0 ;;
        auto) is_fsdb ;;
    esac
}

license_ensure() {
    [ -f "$ROOT/.tools/vm/license_up.sh" ] || { warn "缺 .tools/vm/license_up.sh"; return 1; }
    . "$ROOT/.tools/vm/host_fsdb_env.sh" >/dev/null 2>&1 || true
    sh "$ROOT/.tools/vm/license_up.sh" >/dev/null 2>&1
    local rc=$?
    [ $rc -ne 0 ] && warn "license_up.sh 退出码 $rc(通道可能不可用)"
    return $rc
}

looks_like_license_err() {
    printf '%s' "$1" | grep -qaE 'licen[cs]e|License|LICENSE|npi_init|npi_fsdb_open|SCL-602|16,287|NSIS|libNPI|NPI 许可|check out'
}

# ---- 通用工具 --------------------------------------------------------------
csv_field() {
    local v="$1"
    case "$v" in *[,]*) v="\"${v//\"/\"\"}\"" ;; esac
    printf '%s' "$v"
}

csv_reset() { # $1=file $2=header
    mkdir -p "$(dirname "$1")"
    printf '%s\n' "$2" > "$1"
}

median_list() { # 已排序的值列表 → 中位
    local n=$#
    [ "$n" -eq 0 ] && { printf '%s' "-"; return; }
    local arr=("$@")
    if [ $((n % 2)) -eq 1 ]; then
        printf '%s' "${arr[$((n / 2))]}"
    else
        awk -v a="${arr[$((n / 2 - 1))]}" -v b="${arr[$((n / 2))]}" 'BEGIN{printf "%.3f", (a+b)/2}'
    fi
}

median_by_feature() { # $1=csv $2=value col(1-based) → "feature<TAB>median<TAB>n"
    local csv="$1" col="$2" f vals
    while IFS= read -r f; do
        [ -z "$f" ] && continue
        mapfile -t vals < <(awk -F, -v f="$f" -v c="$col" \
            'NR>1 && $1==f && $c ~ /^[0-9]+(\.[0-9]+)?$/ {print $c}' "$csv" | sort -n)
        printf '%s\t%s\t%s\n' "$f" "$(median_list "${vals[@]}")" "${#vals[@]}"
    done < <(awk -F, 'NR>1{print $1}' "$csv" | sort -u)
}

# 运行一次并测量: sets RC OUT WALL RSS_MB LAST_ERR
RUN_STDIN=""
run_measured() {
    local T="$1"; shift
    local attempt=0 tl kb
    RC=0; WALL="-"; RSS_MB="-"; OUT=""; LAST_ERR=""
    [ "$LICENSE_EACH" = 1 ] && need_license && license_ensure
    while : ; do
        attempt=$((attempt + 1))
        if [ -n "$SPLIT_PREFIX" ]; then
            # stdout 与 stderr **分流**: 值行(=>)与 [prof] trace 行各自保序, 才能可靠地逐探针归因
            if [ -n "$RUN_STDIN" ]; then
                timeout -k 5 "$T" /usr/bin/time -f "$TIME_FMT" "$@" < "$RUN_STDIN" \
                    > "$SPLIT_PREFIX.out" 2> "$SPLIT_PREFIX.err"
            else
                timeout -k 5 "$T" /usr/bin/time -f "$TIME_FMT" "$@" \
                    > "$SPLIT_PREFIX.out" 2> "$SPLIT_PREFIX.err"
            fi
            RC=$?
            OUT="$(cat "$SPLIT_PREFIX.out" "$SPLIT_PREFIX.err" 2>/dev/null)"
        elif [ -n "$RUN_STDIN" ]; then
            OUT=$(timeout -k 5 "$T" /usr/bin/time -f "$TIME_FMT" "$@" < "$RUN_STDIN" 2>&1)
        else
            OUT=$(timeout -k 5 "$T" /usr/bin/time -f "$TIME_FMT" "$@" 2>&1)
        fi
        RC=$?
        tl=$(printf '%s\n' "$OUT" | grep -aE "$TIME_RE" | tail -1)
        WALL="-"; RSS_MB="-"
        if [ -n "$tl" ]; then
            tl="${tl#"$TIME_MARK"}"
            WALL="${tl%% *}"
            kb="${tl##* }"
            case "$kb" in ''|*[!0-9]*) RSS_MB="-" ;; *) RSS_MB=$((kb / 1024)) ;; esac
        fi
        if [ "$RC" -eq 124 ] || [ "$RC" -eq 137 ]; then
            LAST_ERR="timeout(>${T}s)"
            return 0
        fi
        if [ "$RC" -ne 0 ] && need_license && looks_like_license_err "$OUT"; then
            if [ "$attempt" -lt 3 ]; then
                warn "许可失败(第 $attempt 次) → 重跑配方后重试"
                license_ensure || true
                continue
            fi
            LAST_ERR="license"
            return 1
        fi
        return 0
    done
}

first_error_line() {
    printf '%s\n' "$1" | grep -avE "$TIME_RE" | grep -aE 'error|Error|错误|失败|panic|FAILED' | head -1
}

# ---- mode A: single --------------------------------------------------------
build_argv() { # $1=kind $2=a_cmd → ARGV
    ARGV=()
    if [ "$1" = "cli" ]; then
        read -r -a ARGV <<< "$2"
        local i
        for i in "${!ARGV[@]}"; do
            [ "${ARGV[$i]}" = "@WAVE@" ] && ARGV[$i]="$WAVE"
        done
        ARGV=("$BIN" "${ARGV[@]}")
    else
        # CLI 自动识别要求表达式以 '(' 开头; 裸特殊变量(MAX-INDEX/INDEX/TS)会被当成脚本文件路径
        # → 统一包一层 (list ...), 求值语义不变(多形式顶层 = list)。
        local code="$2"
        case "$code" in
            '('*) ;;
            *) code="(list $code)" ;;
        esac
        ARGV=("$BIN" "$code" -l "$WAVE")
    fi
}

do_single() {
    [ -f "$FEATURES" ] || die "缺功能清单 $FEATURES"
    if [ "$APPEND_SINGLE" -eq 1 ] && [ -s "$SINGLE_CSV" ]; then
        say "single --append: 保留既有 $(($(wc -l < "$SINGLE_CSV") - 1)) 行, 只补测 ${ONLY:-<全部>}"
        [ -z "$ONLY" ] && warn "--append 但没给 --only: 会与既有行叠加, 建议配 --only <正则>"
    else
        csv_reset "$SINGLE_CSV" 'feature,mode,run,wall_s,rss_mb,cmd'
        : > "$FAIL_LOG"
    fi
    mkdir -p "$OUTDIR"

    if need_license; then
        say "许可配方: 先拉一次通道(${BIN} 读 $(basename "$WAVE"))…"
        license_ensure || warn "首次 license_up.sh 未成功 —— 逐条测量时还会自动重试"
    fi

    local rows=0 skipped=0 feature kind a_cmd b_expr signals idx heavy note
    while IFS=$'\t' read -r feature kind a_cmd b_expr signals idx heavy note; do
        case "$feature" in ''|'#'*) continue ;; esac
        [ "$feature" = "feature" ] && continue      # 跳过 TSV 表头
        [ "$b_expr" = "-" ] && b_expr=""           # TSV 里空字段写 '-'(tab 是 IFS 空白, 空字段会被吞)
        if [ -n "$ONLY" ] && ! printf '%s' "$feature" | grep -qE "$ONLY"; then continue; fi
        [ "$heavy" = "yes" ] && [ "$INCLUDE_HEAVY" -eq 0 ] && { info "跳过 heavy: $feature($note)"; skipped=$((skipped+1)); continue; }
        build_argv "$kind" "$a_cmd"
        local cmdstr; cmdstr="$(printf '%s ' "${ARGV[@]}")"; cmdstr="${cmdstr% }"
        local run=1
        while [ "$run" -le "$RUNS" ]; do
            run_measured "$TIMEOUT" "${ARGV[@]}"
            printf '%s,%s,%s,%s,%s,%s\n' "$(csv_field "$feature")" "A" "$run" "$WALL" "$RSS_MB" "$(csv_field "$cmdstr")" >> "$SINGLE_CSV"
            rows=$((rows + 1))
            if [ "$WALL" = "-" ] || [ "$RC" -ne 0 ]; then
                printf '%s\t%s\t%s\t%s\t%s\n' "$feature" "$run" "$RC" "${LAST_ERR:-rc}" "$(first_error_line "$OUT" | cut -c1-200)" >> "$FAIL_LOG"
                info "  ✗ $feature run$run rc=$RC ${LAST_ERR:+($LAST_ERR)}"
            else
                say "  $feature run$run: ${WALL}s rss=${RSS_MB}MB"
            fi
            run=$((run + 1))
        done
    done < "$FEATURES"

    # 中位数表(append-csv 与 --check 的输入)
    {
        printf 'feature,mode,median_wall_s,median_rss_mb,runs,cmd\n'
        local f w n mr cmd rows_v
        while IFS=$'\t' read -r f w n; do
            [ -z "$f" ] && continue
            cmd=$(awk -F, -v f="$f" 'NR>1 && $1==f {print $6; exit}' "$SINGLE_CSV")
            mapfile -t rows_v < <(awk -F, -v f="$f" 'NR>1 && $1==f && $5 ~ /^[0-9]+$/ {print $5}' "$SINGLE_CSV" | sort -n)
            mr="$(median_list "${rows_v[@]}")"
            printf '%s,%s,%s,%s,%s,%s\n' "$f" "A" "$w" "$mr" "$n" "$cmd"
        done < <(median_by_feature "$SINGLE_CSV" 4)
    } > "$SINGLE_MED"

    say "mode A 完成: $rows 次测量 → $SINGLE_CSV(中位表 $SINGLE_MED)$([ $skipped -gt 0 ] && printf ', 跳过 %d 个 heavy' "$skipped")"
    [ -s "$FAIL_LOG" ] && warn "有失败/超时: 见 $FAIL_LOG"
    return 0
}

# ---- mode B: session -------------------------------------------------------
# WAL_PROF_TRACE=1 的逐次打点: "[prof] trace probe #k 17.035ms"
# ⚠️ 只认 phase 名 = probe 的行; load/session 也会各打一行, 不能算进探针序号。
parse_traces() { # $1=log → "k<TAB>ms"
    grep -aoE '^\[prof\] trace probe #[0-9]+ [0-9.]+ms' "$1" |
        sed -E 's/^\[prof\] trace probe #([0-9]+) ([0-9.]+)ms/\1\t\2/'
}

# probes.wal 里每个 probe 前面有一行 "; <feature> — <note>"; 按**实际执行序**取 feature 名
load_probe_features() { # 填全局数组 PROBE_FEAT
    PROBE_FEAT=()
    local line f=""
    while IFS= read -r line; do
        case "$line" in
            ';'*) f="${line#; }"; f="${f%% *}" ;;
            ''|' '*|'\t'*|'   '*) continue ;;
            *) PROBE_FEAT+=("${f:-probe}") ;;
        esac
    done < "$PROBES"
}

parse_prof() { # $1=log → "name<TAB>secs<TAB>calls"
    awk '
        /^\[prof\] 阶段账/ { next }
        /^\[prof\]   / {
            name=$2; s=$3; sub(/s$/, "", s); c=$4; sub(/^×/, "", c);
            printf "%s\t%s\t%s\n", name, s, c
        }' "$1"
}

do_session() {
    [ -f "$PROBES" ] || die "缺 probe 清单 $PROBES"
    csv_reset "$SESSION_CSV" 'feature,mode,run,wall_s,rss_mb,cmd'
    mkdir -p "$OUTDIR"
    local nprobes; nprobes=$(grep -cvE '^\s*(;|$)' "$PROBES")
    say "mode B: $nprobes 个 probe, 单进程会话, 重复 $SESSION_RUNS 次"

    if need_license; then
        say "许可配方: 先拉一次通道…"
        license_ensure || warn "首次 license_up.sh 未成功 —— 会按失败重试"
    fi

    load_probe_features
    [ "${#PROBE_FEAT[@]}" -gt 0 ] || warn "probes.wal 里没找到 '; feature' 注释行 → 逐探针行会退化成 probe#k"
    say "逐探针归因: WAL_PROF_TRACE=1(${#PROBE_FEAT[@]} 个探针名)"
    local cmdstr="WAL_PROF=1 WAL_PROF_TRACE=1 $BIN --stdin -l $WAVE < $PROBES"
    local run=1
    while [ "$run" -le "$SESSION_RUNS" ]; do
        RUN_STDIN="$PROBES"
        SPLIT_PREFIX="$OUTDIR/session.$run"
        run_measured "$SESSION_TIMEOUT" env WAL_PROF=1 WAL_PROF_TRACE=1 "$BIN" --stdin -l "$WAVE"
        SPLIT_PREFIX=""
        RUN_STDIN=""
        local log="$OUTDIR/session.$run.log"
        printf '%s\n' "$OUT" > "$log"
        local phases="$OUTDIR/.session.$run.phases"
        parse_prof "$log" > "$phases"

        # 整进程墙钟(time 的 %e)+ 各阶段账
        printf '%s,%s,%s,%s,%s,%s\n' "wall-total" "session" "$run" "$WALL" "$RSS_MB" "$(csv_field "$cmdstr")" >> "$SESSION_CSV"

        # 逐探针**会话内边际耗时**(WAL_PROF_TRACE 的 trace probe #k, 与 probes.wal 非空行序一一对应)
        local pk pms pfeat psecs nprobe=0
        while IFS=$'\t' read -r pk pms; do
            [ -z "$pk" ] && continue
            pfeat="${PROBE_FEAT[$((pk - 1))]:-probe#$pk}"
            psecs=$(awk -v m="$pms" 'BEGIN{printf "%.6f", m/1000}')
            printf '%s,%s,%s,%s,%s,%s\n' "$(csv_field "$pfeat")" "session-probe" "$run" "$psecs" "$RSS_MB" \
                "$(csv_field "会话内边际(probe #$pk): $cmdstr")" >> "$SESSION_CSV"
            nprobe=$((nprobe + 1))
        done < <(parse_traces "$OUTDIR/session.$run.err")   # trace 行在 stderr(与值行分流后保序)
        [ "$nprobe" -eq 0 ] && warn "没解析到 trace probe 行(二进制是否支持 WAL_PROF_TRACE?)"

        local name secs calls
        while IFS=$'\t' read -r name secs calls; do
            [ -z "$name" ] && continue
            printf '%s,%s,%s,%s,%s,%s\n' "$(csv_field "$name")" "session" "$run" "$secs" "$RSS_MB" "$(csv_field "$cmdstr")" >> "$SESSION_CSV"
            if [ "$name" = "probe" ]; then
                local mean
                mean=$(awk -v s="$secs" -v c="$calls" 'BEGIN{ printf (c>0? "%.6f" : "-"), (c>0? s/c : 0) }')
                printf '%s,%s,%s,%s,%s,%s\n' "probe-mean" "session" "$run" "$mean" "$RSS_MB" "$(csv_field "$cmdstr")" >> "$SESSION_CSV"
            fi
        done < "$phases"
        rm -f "$phases"

        if [ "$RC" -ne 0 ]; then
            warn "会话退出码 $RC ${LAST_ERR:+($LAST_ERR)}: $(first_error_line "$OUT" | cut -c1-160)"
        fi
        say "run$run: 进程墙钟 ${WALL}s rss=${RSS_MB}MB"
        parse_prof "$log" | while IFS=$'\t' read -r name secs calls; do
            printf '        阶段账 %-14s %9ss ×%s\n' "$name" "$secs" "$calls"
        done
        run=$((run + 1))
    done
    say "mode B 完成 → $SESSION_CSV(原始 [prof] 输出: $OUTDIR/session.<run>.log)"
    say "会话内边际最重的 5 个功能(中位):"
    median_by_feature "$SESSION_CSV" 4 | awk -F'\t' '$1!="wall-total" && $1!="session" && $1!="load" && $1!="probe" && $1!="probe-mean" && $1!~/^fsdb:/ && $2!="-"' |
        sort -t$'\t' -k2 -gr | head -5 |
        while IFS=$'\t' read -r f w n; do printf '        %-24s %8ss (n=%s)\n' "$f" "$w" "$n"; done
    return 0
}

# ---- append-csv ------------------------------------------------------------
op_prefix() {
    case "$(basename "$WAVE" | tr 'A-Z' 'a-z')" in
        *.fsdb) printf 'fsdb' ;;
        *.vcd)  printf 'vcd' ;;
        *.fst)  printf 'fst' ;;
        *)      printf 'wave' ;;
    esac
}

do_append() {
    [ -f "$SINGLE_MED" ] || [ -f "$SESSION_CSV" ] || die "没有 $SINGLE_MED / $SESSION_CSV: 先跑 single / session"
    local version date fixture pf
    version=$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
    date=$(date +%F)
    fixture=$(basename "$WAVE")
    pf=$(op_prefix)
    mkdir -p "$(dirname "$CSV_FILE")"
    [ -f "$CSV_FILE" ] || printf 'version,date,op,fixture,wall_s,rss_mb,note\n' > "$CSV_FILE"
    local appended=0

    if [ -f "$SINGLE_MED" ]; then
        local f mw mr n cmd fail
        while IFS=, read -r f _ mw mr n cmd; do
            [ "$f" = "feature" ] && continue
            [ -z "$f" ] && continue
            fail=""
            if grep -q "^${f}	" "$FAIL_LOG" 2>/dev/null; then
                fail="; 部分 run 失败($(grep -c "^${f}	" "$FAIL_LOG" 2>/dev/null) 次, 见 $FAIL_LOG)"
            fi
            local note="median of ${n} runs; cmd: ${cmd}${fail}"
            [ -n "$EXTRA_NOTE" ] && note="$note; $EXTRA_NOTE"
            printf '%s,%s,%s,%s,%s,%s,%s\n' "$version" "$date" "${pf}-feat-${f}" "$fixture" \
                "$mw" "$mr" "$(csv_field "$note")" >> "$CSV_FILE"
            appended=$((appended + 1))
        done < "$SINGLE_MED"
    fi

    if [ -f "$SESSION_CSV" ]; then
        local wall="-" load="-" pmean="-" nruns="-" rss="-" cmd="" pcalls="-"
        while IFS=$'\t' read -r f med n; do
            case "$f" in
                wall-total) wall="$med" ;;
                load) load="$med" ;;
                probe-mean) pmean="$med"; nruns="$n" ;;
            esac
        done < <(median_by_feature "$SESSION_CSV" 4)
        local plog="$OUTDIR/session.${SESSION_RUNS}.log"
        [ -f "$plog" ] && pcalls=$(parse_prof "$plog" | awk -F'\t' '$1=="probe"{print $3}')
        rss=$(awk -F, '$1=="wall-total" && $5 ~ /^[0-9]+$/ {print $5}' "$SESSION_CSV" | sort -n | tail -1)
        cmd=$(awk -F, '$1=="wall-total" {print $6; exit}' "$SESSION_CSV")
        [ -z "$rss" ] && rss="-"
        local heaviest
        heaviest=$(median_by_feature "$SESSION_CSV" 4 | awk -F'\t' '$1!="wall-total" && $1!="session" && $1!="load" && $1!="probe" && $1!="probe-mean" && $1!~/^fsdb:/ && $2!="-"' | sort -t$'\t' -k2 -gr | head -1 | awk -F'\t' '{printf "%s=%ss", $1, $2}')
        local note="--stdin 单次加载; load=${load}s; probe 均值 ${pmean}s/次 × ${pcalls} 个探针(${nruns} 次会话取中位); 最重边际探针 ${heaviest}; 逐探针边际见 $SESSION_CSV; cmd: ${cmd}"
        [ -n "$EXTRA_NOTE" ] && note="$note; $EXTRA_NOTE"
        printf '%s,%s,%s,%s,%s,%s,%s\n' "$version" "$date" "${pf}-feat-session" "$fixture" \
            "$wall" "$rss" "$(csv_field "$note")" >> "$CSV_FILE"
        printf '%s,%s,%s,%s,%s,%s,%s\n' "$version" "$date" "${pf}-feat-session-load" "$fixture" \
            "$load" "$rss" "$(csv_field "加载阶段墙钟(阶段账 load); cmd: ${cmd}")" >> "$CSV_FILE"
        printf '%s,%s,%s,%s,%s,%s,%s\n' "$version" "$date" "${pf}-feat-session-probe-mean" "$fixture" \
            "$pmean" "$rss" "$(csv_field "查询侧边际成本(阶段账 probe 合计 / 次数); cmd: ${cmd}")" >> "$CSV_FILE"
        appended=$((appended + 3))
    fi

    say "追加 $appended 行(version=$version fixture=$fixture)→ $CSV_FILE"
    return 0
}

# ---- check -----------------------------------------------------------------
do_check() {
    local py="$ROOT/scripts/check_bench_regression.py"
    [ -f "$py" ] || die "缺 $py"
    exec python3 "$py" --csv "$CSV_FILE" ${CHECK_ARGS[@]+"${CHECK_ARGS[@]}"}
}

# ---- emit-probes -----------------------------------------------------------
do_emit_probes() {
    [ -f "$FEATURES" ] || die "缺功能清单 $FEATURES"
    local out="$AUTH_PROBES" n=0
    mkdir -p "$(dirname "$out")"
    : > "$out"
    local feature kind a_cmd b_expr signals idx heavy note
    while IFS=$'\t' read -r feature kind a_cmd b_expr signals idx heavy note; do
        case "$feature" in ''|'#'*) continue ;; esac
        [ "$feature" = "feature" ] && continue      # 跳过 TSV 表头
        [ "$b_expr" = "-" ] && b_expr=""           # TSV 里空字段写 '-'(tab 是 IFS 空白, 空字段会被吞)
        [ -z "$b_expr" ] && continue
        [ "$heavy" = "yes" ] && continue
        printf '; %s — %s\n%s\n' "$feature" "$note" "$b_expr" >> "$out"
        n=$((n + 1))
    done < "$FEATURES"
    # 同步一份到 .tools/bench/baseline(本地可读副本); 权威永远是 bench/ 下那份
    mkdir -p "$(dirname "$LOCAL_PROBES")"
    cp -f "$AUTH_FEATURES" "$LOCAL_FEATURES"
    cp -f "$AUTH_PROBES" "$LOCAL_PROBES"
    say "写出 $n 个 probe → $out(权威); 已同步副本 → $LOCAL_PROBES"
    return 0
}

# ---- verify(离线自检) ------------------------------------------------------
do_verify() {
    local bad=0
    [ -f "$FEATURES" ] || die "缺 $FEATURES"
    local nfeat; nfeat=$(awk -F'\t' 'NR>1 && $1!="" && $1!~/^#/ {c++} END{print c+0}' "$FEATURES")
    local nb; nb=$(awk -F'\t' 'NR>1 && $4!="" && $4!="-" && $7!="yes" {c++} END{print c+0}' "$FEATURES")
    local np; np=$(grep -cvE '^\s*(;|$)' "$PROBES" 2>/dev/null || echo 0)
    say "功能条数: $nfeat(mode B 探针 $nb);  probes.wal 非注释行: $np"
    [ "$nb" = "$np" ] || { warn "features.tsv 的 b_expr 条数($nb)与 probes.wal 行数($np)不一致 → 跑 emit-probes"; bad=1; }
    local ncli; ncli=$(awk -F'\t' 'NR>1 && $2=="cli" {c++} END{print c+0}' "$FEATURES")
    say "其中 CLI 子命令: $ncli"
    return $bad
}

# ---- 串行锁 + 分发 ---------------------------------------------------------
mkdir -p "$OUTDIR"
if command -v flock >/dev/null 2>&1; then
    exec 9>"$OUTDIR/.bench.lock"
    flock -w 3600 9 || die "拿不到 $OUTDIR/.bench.lock(另一个基准进程在跑?)"
fi

case "$CMD" in
    single)      do_single ;;
    session)     do_session ;;
    all)
        do_single
        do_session
        [ "$APPEND_AFTER_ALL" = 1 ] && do_append
        ;;
    append-csv)  do_append ;;
    check)       do_check ;;
    emit-probes) do_emit_probes ;;
    verify)      do_verify ;;
esac
