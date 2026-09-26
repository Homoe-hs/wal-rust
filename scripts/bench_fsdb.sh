#!/usr/bin/env bash
# ============================================================================
# FSDB 查询基准 —— 版本回归对比用(冷建时间线 / 暖查询 / 各查询路径各一条)。
#
#   ./scripts/bench_fsdb.sh <file.fsdb> [信号全名]
#   WAL_FSDB_BENCH=x.fsdb ./scripts/ci.sh --only perf   # CI 里可选跑(有 Verdi 才跑)
#
# 背景(现场事故): 265MB / 3700 万时间戳的 FSDB 上"慢到不可用"。FSDB 与 VCD 的
# 结构性差别是**变更列只能靠 NPI 重扫**:
#   * 冷: 建全局时间线 = 把整个文件的每条变更过一遍(唯一还在百秒量级的操作),
#         结果落 `.ftl`;VCD 侧同一件事是内存映射索引, 秒级。
#   * 暖: 每个新进程仍要为"这次查询用到的信号"再扫一遍它们的变更流,
#         于是同一查询每次都慢 —— 与查询本身多复杂无关。
# 这个脚本把这几条路径分开计时, 便于:
#   ① 改 FSDB 后端后做 A/B; ② 发版前留数字; ③ 复现"是不是 FSDB 后端的锅"。
#
# 判读要点:
#   * `warm-*` 应当接近 `load`(只加载): 说明旁挂列缓存(`.fcol`)命中了;
#     若 warm ≈ cold, 先查 WAL_CACHE / 缓存阈值(WAL_CACHE_MIN_MB)/ 目录权限。
#   * `load` 是每个进程的固定成本(NPI 初始化 + open + 名字树), 与文件大小弱相关。
# 数字追加到 bench/perf-history.csv(op=fsdb-*, fixture=文件名)。
# ============================================================================
set -uo pipefail
cd "$(dirname "$0")/.."

FSDB="${1:?用法: scripts/bench_fsdb.sh <file.fsdb> [信号全名]}"
SIG="${2:-}"
BIN="${WAL_BIN:-target/release/wal-rust}"
[ -x "$BIN" ] || { echo "找不到 $BIN(先 cargo build --release)"; exit 2; }
# 绝对路径: run() 会 cd 进临时目录, 相对路径会找不到二进制(踩过: 全 0.00s 却看不出错)
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
[ -f "$FSDB" ] || { echo "找不到波形: $FSDB"; exit 2; }
FSDB_ABS="$(cd "$(dirname "$FSDB")" && pwd)/$(basename "$FSDB")"
command -v /usr/bin/time >/dev/null || { echo "需要 GNU time(/usr/bin/time)"; exit 2; }

# 小样本夹具也要能验证缓存路径, 所以默认把写回阈值压到 0(与 CI 的 WAL_CACHE_MIN_MB 同义)
export WAL_CACHE_MIN_MB="${WAL_CACHE_MIN_MB:-0}"

if [ -z "$SIG" ]; then
    SIG=$("$BIN" sigs "$FSDB_ABS" "*" 1 2>/dev/null | sed -n '2p' | awk '{print $1}')
    [ -n "$SIG" ] || { echo "取不到信号名(用第二个参数显式给一个全名)"; exit 2; }
fi
echo "波形: $FSDB_ABS ($(du -h "$FSDB_ABS" | cut -f1))   信号: $SIG"

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
if [ -f "$ROOT/Cargo.toml" ]; then
    VERSION=$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
else
    VERSION="${WAL_BENCH_VERSION:-unknown}"   # 在仓库外跑(如客机里)也能用
fi
CSV="$ROOT/bench/perf-history.csv"
mkdir -p "$(dirname "$CSV")"
[ -f "$CSV" ] || echo "version,date,op,fixture,wall_s,rss_mb,note" > "$CSV"
DATE=$(date +%F)
FIXTURE=$(basename "$FSDB_ABS")

work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
mkdir -p "$work/cold" "$work/warm"

# run <op> <cache 模式> <工作目录> <查询>
run() {
    local op="$1" mode="$2" dir="$3" q="$4" out t m rc
    out=$( cd "$dir" && env WAL_CACHE="$mode" timeout 1800 /usr/bin/time -f "%e %M" \
        "$BIN" "$q" -l "$FSDB_ABS" 2>&1 )
    rc=$?
    t=$(echo "$out" | sed -n 's/^\([0-9][0-9.]*\) [0-9]*$/\1/p' | tail -1)
    m=$(echo "$out" | sed -n 's/^[0-9.]* \([0-9]*\)$/\1/p' | tail -1)
    local val; val=$(echo "$out" | grep -E '^(=>|\()' | tail -1)
    # 查询失败时不要把 NA 混进回归库: 打印原因并直接失败(免得"看起来跑过了")
    if [ -z "$t" ] || [ -z "$val" ]; then
        printf '%-22s 失败(rc=%s): %s\n' "$op" "$rc" "$(echo "$out" | head -2 | tr '\n' ' ')"
        FAILED=$((FAILED + 1))
        return
    fi
    printf "%s,%s,%s,%s,%s,%s,%s\n" "$VERSION" "$DATE" "$op" "$FIXTURE" "$t" "$m" "$(echo "$val" | cut -c1-40 | tr ',' ' ')" >> "$CSV"
    printf '%-22s %8ss  rss=%-9s %s\n' "$op" "$t" "${m}KB" "$val"
}
FAILED=0

echo
echo "== 冷(不读不写缓存) vs 暖(建一次缓存, 再命中) =="
for spec in "load|(length (SIGNALS))" \
            "edge|(count (rising \"$SIG\"))" \
            "level|(count (= (get \"$SIG\") 1))" \
            "at|(at \"$SIG\" 1)"; do
    op="${spec%%|*}"; q="${spec#*|}"
    run "fsdb-cold-$op" off "$work/cold" "$q"
    run "fsdb-build-$op" build "$work/warm" "$q"   # 第一次: 建时间线 + 列缓存
    run "fsdb-hit-$op" auto "$work/warm" "$q"      # 第二次: 命中缓存
done

echo
echo "缓存产物:"
find "$work/warm" -maxdepth 2 \( -name '*.fcol' -o -name '*.ftl' \) 2>/dev/null | sed "s|$work/warm||" | head -5
echo
echo "判读: warm-* 接近 load(只加载) 说明列表缓存生效; warm≈cold 说明没命中缓存。"
echo "      CSV 已追加: $CSV"
