#!/usr/bin/env bash
# ============================================================================
# 把 wal-rust 的 **stdin 会话模式** 提交到 LSF: 一次加载波形, 多探针顺序复用缓存。
#
#   ./scripts/lsf_wal_run.sh <wave> <probes.wal> [slots] [选项]
#     slots             bsub -n(默认 8); wal-rust 会按 $LSB_DJOB_NUMPROC 自动开
#                       等量时间线 worker(封顶 8), 不需要再设 WAL_FSDB_TL_JOBS
#     --queue <Q>       bsub -q
#     --wall <HH:MM>    bsub -W(默认 02:00)
#     --job-name <N>    bsub -J(默认 wal-probes)
#     --work <DIR>      job 的工作目录(= 缓存落地处, 必须共享; 默认探针文件所在目录)
#     --cache-dir <D>   WAL_CACHE_DIR(默认 <work>/.wal-rust-cache)
#     --no-span         不加 -R "span[hosts=1]"(默认加: 多个 worker 进程要在同一台机器上)
#     --wait            提交后轮询 bjobs, 结束后打印日志尾部(探针结果)
#     --dry-run         只打印将要提交的 bsub 命令
#
# ⚠️ LSF 批处理 job 的 stdin **不是**提交机的 stdin(bsub 从 stdin 读的是 job 脚本本身),
#    所以这里把探针文件写进 job 命令的 `<` 重定向里 —— 探针文件与波形都必须在
#    所有计算节点可见的共享文件系统上。
#
# 为什么用 stdin 会话而不是一条命令一个 job:
#   * 一次加载(NPI 初始化 + open + 名字树)只付一次;
#   * 第一个查询会把全局时间线冷建出来并落 `.ftl`, 之后的探针只付"读缓存";
#   * 冷建会自动按 bsub -n 的 slot 数并行(每个 worker 各占一个 NPI 许可)。
# ============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
START_PWD="$PWD"

WAVE="${1:-}"; PROBES="${2:-}"
[ -n "$WAVE" ] && [ -n "$PROBES" ] || { sed -n '2,20p' "$0"; exit 2; }
shift 2
SLOTS=8
case "${1:-}" in [0-9]*) SLOTS="$1"; shift ;; esac

QUEUE=""; WALL="02:00"; JOB="wal-probes"; WORK=""; CACHE_DIR=""; SPAN=1; WAIT=0; DRY=0
while [ $# -gt 0 ]; do
    case "$1" in
        --queue) QUEUE="${2:-}"; shift 2 ;;
        --wall) WALL="${2:-02:00}"; shift 2 ;;
        --job-name) JOB="${2:-wal-probes}"; shift 2 ;;
        --work) WORK="${2:-}"; shift 2 ;;
        --cache-dir) CACHE_DIR="${2:-}"; shift 2 ;;
        --no-span) SPAN=0; shift ;;
        --wait) WAIT=1; shift ;;
        --dry-run) DRY=1; shift ;;
        *) echo "未知参数: $1"; exit 2 ;;
    esac
done

BIN="${WAL_BIN:-$REPO_ROOT/target/release/wal-rust}"
[ -x "$BIN" ] || { echo "找不到 $BIN(先 cargo build --release, 或设 WAL_BIN)"; exit 2; }
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
for f in "$WAVE" "$PROBES"; do
    [ -f "$f" ] || { echo "找不到文件: $f"; exit 2; }
done
WAVE="$(cd "$(dirname "$WAVE")" && pwd)/$(basename "$WAVE")"
PROBES="$(cd "$(dirname "$PROBES")" && pwd)/$(basename "$PROBES")"
WORK="${WORK:-$(dirname "$PROBES")}"
[ -d "$WORK" ] || { echo "工作目录不存在: $WORK"; exit 2; }
CACHE_DIR="${CACHE_DIR:-$WORK/.wal-rust-cache}"
LOGDIR="$WORK/.wal-rust-lsf"
LOG="$LOGDIR/$JOB.out"

CMD="cd '$WORK' && WAL_CACHE_DIR='$CACHE_DIR' '$BIN' --stdin -l '$WAVE' < '$PROBES'"
ARGS=(-J "$JOB" -n "$SLOTS" -W "$WALL" -o "$LOG" -e "$LOGDIR/$JOB.err")
[ -n "$QUEUE" ] && ARGS+=(-q "$QUEUE")
[ "$SPAN" = 1 ] && ARGS+=(-R "span[hosts=1]")

echo "波形  : $WAVE"
echo "探针  : $PROBES ($(grep -cve '^\s*$' -e '^\s*;' "$PROBES" 2>/dev/null || echo '?') 行)"
echo "二进制: $BIN"
echo "工作目录: $WORK   缓存: $CACHE_DIR"
echo "slot  : $SLOTS(时间线冷建会按这个数并行 worker, 每个占一个 NPI 许可)"
echo

if [ "$DRY" = 1 ]; then
    echo "bsub ${ARGS[*]} \"$CMD\""
    exit 0
fi

if [ "$SPAN" = 0 ]; then
    echo "提示: 没加 span[hosts=1], worker 进程仍会在 job 的首个执行节点上跑。"
fi

if ! command -v bsub >/dev/null; then
    echo "没有 bsub(不在 LSF 环境)。本机直接跑就是同一件事:"
    echo "  $CMD"
    exit 0
fi

mkdir -p "$LOGDIR"
RC=0
mkdir -p "$CACHE_DIR" 2>/dev/null
jid=$(bsub "${ARGS[@]}" "$CMD" 2>&1 | sed -n 's/.*Job <\([0-9]*\)>.*/\1/p')
[ -n "$jid" ] || { echo "bsub 提交失败"; exit 1; }
echo "已提交: job $jid  日志: $LOG"

if [ "$WAIT" = 1 ]; then
    while :; do
        st=$(bjobs -noheader -o stat "$jid" 2>/dev/null | tr -d ' ')
        case "$st" in RUN|PEND|WAIT|PSUSP|USUSP|SSUSP|PROV) sleep 10 ;; *) break ;; esac
    done
    echo "== job $jid 结束(${st:-未知}) =="
    [ -f "$LOG" ] && tail -30 "$LOG"
    [ "$st" = DONE ] || RC=1
else
    echo "（未加 --wait; 用 bjobs $jid / bpeek $jid 看进度, 结果在 $LOG）"
fi
exit $RC
