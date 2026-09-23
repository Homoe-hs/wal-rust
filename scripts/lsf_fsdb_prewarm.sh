#!/usr/bin/env bash
# ============================================================================
# LSF 并行预热: 把 FSDB 全局时间线(索引空间)的全文件扫描摊到多台机器上。
#
#   ./scripts/lsf_fsdb_prewarm.sh <file.fsdb> [分片数] [选项]
#     选项: --queue <Q>       bsub 队列
#           --wall <HH:MM>   bsub 墙钟上限(默认 02:00)
#           --cache-dir <D>  .ftl 落盘位置(默认 $WAL_CACHE_DIR 或 ./.wal-rust-cache)
#           --work <D>       分片文件目录(默认 ./.wal-rust-lsf; 必须在共享文件系统上)
#           --local          没有 LSF 时本机并发跑(等价 WAL_FSDB_TL_JOBS=N)
#           --dry-run        只打印将要提交的 bsub 命令
#
# 为什么值得: 索引空间 = "所有信号变更时间的并集" —— 第一次查询必须把整个 FSDB 的每条
# 变更过一遍, 这是唯一还在百秒量级的操作(174MB 样本单进程 400s, 4 进程 175s)。
# 它天然可并行: 每个 job 只扫 `idx % shards == k` 那批信号, reduce 归并成同一份 `.ftl`。
#
# ⚠️ 许可: 每个 worker 是一次独立 NPI 会话, **各占一个 Verdi 许可**。shards 别超过
#    许可池的余量; 不确定就先跑 4。
# ⚠️ 共享文件系统: FSDB、$BIN、--work、--cache-dir 都必须对所有计算节点可见,
#    否则 job 会在别的节点上找不到文件。
#
# 完成后: 在**你平时查询的目录**里把 --cache-dir 指过来(或直接把 .ftl 放到
# <查询目录>/.wal-rust-cache/), 之后的查询就只付"读缓存"的钱。
# ============================================================================
set -uo pipefail
# 注意: 不 cd —— 工作目录/缓存目录的默认值跟随**调用者的当前目录**(与 wal-rust
# "缓存在执行命令的路径下"的约定一致), 只把二进制默认值解析到仓库里。
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
START_PWD="$PWD"

FSDB="${1:-}"
[ -n "$FSDB" ] || { sed -n '2,12p' "$0"; exit 2; }
shift || true
SHARDS="${1:-8}"
case "$SHARDS" in [0-9]*) shift || true ;; *) SHARDS=8 ;; esac

QUEUE=""; WALL="02:00"; CACHE_DIR="${WAL_CACHE_DIR:-}"; WORK=""; LOCAL=0; DRY=0
while [ $# -gt 0 ]; do
    case "$1" in
        --queue) QUEUE="${2:-}"; shift 2 ;;
        --wall) WALL="${2:-02:00}"; shift 2 ;;
        --cache-dir) CACHE_DIR="${2:-}"; shift 2 ;;
        --work) WORK="${2:-}"; shift 2 ;;
        --local) LOCAL=1; shift ;;
        --dry-run) DRY=1; shift ;;
        *) echo "未知参数: $1"; exit 2 ;;
    esac
done

BIN="${WAL_BIN:-$REPO_ROOT/target/release/wal-rust}"
[ -x "$BIN" ] || { echo "找不到 $BIN(先 cargo build --release, 或设 WAL_BIN)"; exit 2; }
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"   # 集群 job 里必须是绝对路径
[ -f "$FSDB" ] || { echo "找不到波形: $FSDB"; exit 2; }
FSDB="$(cd "$(dirname "$FSDB")" && pwd)/$(basename "$FSDB")"
WORK="${WORK:-$START_PWD/.wal-rust-lsf}"
mkdir -p "$WORK" || { echo "建不了工作目录 $WORK"; exit 2; }
WORK="$(cd "$WORK" && pwd)"

echo "波形  : $FSDB ($(du -h "$FSDB" | cut -f1))"
echo "分片  : $SHARDS   工作目录: $WORK"
echo "二进制: $BIN"
echo "缓存  : ${CACHE_DIR:-<查询目录>/.wal-rust-cache}"
echo

T0=$(date +%s)
if [ "$LOCAL" = 1 ]; then
    echo "== 本机并发(等价 WAL_FSDB_TL_JOBS=$SHARDS) =="
    pids=()
    for k in $(seq 0 $((SHARDS - 1))); do
        "$BIN" fsdb-timeline-map "$FSDB" "$k" "$SHARDS" "$WORK/tl.$k.part" \
            >"$WORK/tl.$k.log" 2>&1 &
        pids+=($!)
    done
    rc=0
    for p in "${pids[@]}"; do wait "$p" || rc=1; done
    [ "$rc" = 0 ] || { echo "有分片失败:"; tail -3 "$WORK"/tl.*.log; exit 1; }
else
    if [ "$DRY" != 1 ]; then
        command -v bsub >/dev/null || {
            echo "没有 bsub(不在 LSF 环境)。用 --local 在本机并发跑, 或先 module load lsf。"; exit 2; }
    fi
    JOBS=()
    for k in $(seq 0 $((SHARDS - 1))); do
        CMD="$BIN fsdb-timeline-map $FSDB $k $SHARDS $WORK/tl.$k.part"
        ARGS=(-J "wal-tl-$k" -n 1 -W "$WALL" -o "$WORK/tl.$k.log" -e "$WORK/tl.$k.err")
        [ -n "$QUEUE" ] && ARGS+=(-q "$QUEUE")
        if [ "$DRY" = 1 ]; then
            echo "bsub ${ARGS[*]} \"$CMD\""
            continue
        fi
        jid=$(bsub "${ARGS[@]}" "$CMD" 2>&1 | sed -n 's/.*Job <\([0-9]*\)>.*/\1/p')
        [ -n "$jid" ] || { echo "bsub 提交失败(第 $k 片)"; exit 1; }
        JOBS+=("$jid")
        echo "  提交 shard $k → job $jid"
    done
    [ "$DRY" = 1 ] && exit 0
    echo "== 等待 ${#JOBS[@]} 个 job =="
    while :; do
        running=0
        for j in "${JOBS[@]}"; do
            st=$(bjobs -noheader -o stat "$j" 2>/dev/null | tr -d ' ')
            case "$st" in RUN|PEND|WAIT|PSUSP|USUSP|SSUSP|PROV) running=$((running + 1)) ;; esac
        done
        [ "$running" = 0 ] && break
        sleep 15
    done
    # 收尾检查: 每片都必须产出非空分片文件
    for k in $(seq 0 $((SHARDS - 1))); do
        [ -s "$WORK/tl.$k.part" ] || {
            echo "shard $k 没有产出(看 $WORK/tl.$k.log / .err)"; exit 1; }
    done
fi

echo
echo "== reduce(归并 → .ftl) =="
if [ -n "$CACHE_DIR" ]; then
    WAL_CACHE_DIR="$CACHE_DIR" "$BIN" fsdb-timeline-merge "$FSDB" "$WORK"/tl.*.part --cache-dir "$CACHE_DIR"
else
    "$BIN" fsdb-timeline-merge "$FSDB" "$WORK"/tl.*.part
fi
RC=$?
T1=$(date +%s)
echo "总墙钟: $((T1 - T0))s"
exit $RC
