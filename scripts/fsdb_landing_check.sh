#!/usr/bin/env bash
# ============================================================================
# FSDB「接收入库 + 基线体检」—— 别人的波形(现场大文件)到手时跑这一条。
#
#   scripts/fsdb_landing_check.sh <file.fsdb> [信号全名] [--quick]
#
# 为什么要有这一步: 波形一到手就改代码 = 没有基线, 也就没有"每次版本更新都要比上一次更好"。
# 这个脚本按固定顺序把"要不要动代码、该动哪一段"需要的事实一次收齐:
#   ① 身份与写者(大小 / sha256 / FSDB magic / VCS 版本串) —— 入库留档, A/B 有据;
#   ② 环境自检(许可通道 / 本机 reader 版本 / 真打开一次) —— 打不开就别谈性能;
#   ③ 磁盘余量(缓存会占地: .fnames 名字树 / .ftl 时间线 / .fcol 变更列);
#   ④ 基线电池(`bench_fsdb.sh`: load/edge/level/at × 冷/建/暖) → bench/perf-history.csv;
#   ⑤ 并行轴: 冷建时间线 `-j 1` vs `-j auto` vs `-j 8`(现场"单个波形多核加速"的关键一条);
#   ⑥ 并行正确性: `-j 1` 与 `-j auto` 产出的 `.ftl` 必须**逐字节相同**(不同 = 先修对, 再谈快),
#      外加索引空间取值逐字符相同。
#
# `--quick` 跳过 ④ 的完整电池(只做 ①③⑤⑥), 用于"先看能不能读、有没有并行收益"。
# 退出码: 0 = 全部通过; 2 = 打不开/环境问题; 3 = 并行产物不一致(正确性问题); 1 = 其它失败。
# ============================================================================
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

BIN="${WAL_BIN:-target/release/wal-rust}"
[ -x "$BIN" ] || { echo "找不到 $BIN(先 cargo build --release)"; exit 2; }
BIN="$ROOT/$BIN"
command -v /usr/bin/time >/dev/null || { echo "需要 GNU time(/usr/bin/time)"; exit 2; }

FSDB=""; SIG=""; QUICK=0
for a in "$@"; do
    case "$a" in
        --quick) QUICK=1 ;;
        *) if [ -z "$FSDB" ]; then FSDB="$a"; elif [ -z "$SIG" ]; then SIG="$a"; fi ;;
    esac
done
[ -n "$FSDB" ] || { echo "用法: scripts/fsdb_landing_check.sh <file.fsdb> [信号全名] [--quick]"; exit 2; }
[ -f "$FSDB" ] || { echo "找不到波形: $FSDB"; exit 2; }
FSDB_ABS="$(cd "$(dirname "$FSDB")" && pwd)/$(basename "$FSDB")"
export WAL_CACHE_MIN_MB="${WAL_CACHE_MIN_MB:-0}"   # 小夹具也要走缓存路径, 与 bench_fsdb.sh 一致

DATE=$(date +%F)
VERSION=$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
CSV="$ROOT/bench/perf-history.csv"
FIXTURE=$(basename "$FSDB_ABS")
[ -f "$CSV" ] || echo "version,date,op,fixture,wall_s,rss_mb,note" > "$CSV"
csv_add() { printf "%s,%s,%s,%s,%s,%s,%s\n" "$VERSION" "$DATE" "$1" "$FIXTURE" "$2" "$3" "$4" >> "$CSV"; }

hr() { printf '%s\n' "--------------------------------------------------------------------------"; }
say() { printf '%s\n' "$*"; }

# ============================ ① 身份与写者 ==================================
say "== ① 身份 =="
SZ=$(stat -c %s "$FSDB_ABS")
say "  文件: $FSDB_ABS"
say "  大小: $(numfmt --to=iec --suffix=B "$SZ" 2>/dev/null || echo "$SZ B")  mtime: $(stat -c %y "$FSDB_ABS" | cut -d. -f1)"
say -n "  sha256: "; sha256sum "$FSDB_ABS" | cut -d' ' -f1
MAGIC=$(od -An -tx1 -j8 -N4 "$FSDB_ABS" 2>/dev/null | tr -d ' \n')
[ "$MAGIC" = "04030201" ] && say "  magic: 04030201 ✓ FSDB" || say "  magic: $MAGIC ✗(不是 FSDB / 传输损坏?)"
WRITER=$(grep -a -m1 -o 'VCS Release [A-Za-z0-9._ -]\{1,40\}' "$FSDB_ABS" 2>/dev/null | head -1)
say "  写者: ${WRITER:-<文件头里没找到 VCS Release 串>}"

# ============================ ③ 磁盘余量 ====================================
say
say "== ③ 磁盘与缓存目录 =="
AVAIL_KB=$(df -Pk "$ROOT" | awk 'NR==2{print $4}')
NEED_KB=$((SZ / 1024 * 3))
say "  仓库盘可用: $((AVAIL_KB / 1024 / 1024)) GB   建议 ≥ $((NEED_KB / 1024 / 1024)) GB(波形×3: 名字树/时间线/变更列)"
[ "$AVAIL_KB" -lt "$NEED_KB" ] && say "  ⚠️ 余量偏紧 —— 缓存写失败只提示一次、不影响结果, 但暖路径会失效"
say "  缓存落点: $ROOT/.wal-rust-cache(执行命令的路径下)"

# ============================ ② 环境自检 ====================================
say
say "== ② 环境自检(许可 / reader / 真打开) =="
./scripts/fsdb_env_check.sh "$FSDB_ABS" 2>&1 | sed 's/^/  /'
RC=${PIPESTATUS[0]}
if [ "$RC" != 0 ]; then
    say
    say "  环境没过 —— 先把许可通道拉起来再回来:  sh .tools/vm/license_up.sh"
    exit 2
fi

if [ -z "$SIG" ]; then
    SIG=$("$BIN" sigs "$FSDB_ABS" '*' 1 2>/dev/null | sed -n '2p' | awk '{print $1}')
fi
[ -n "$SIG" ] || { say "取不到信号名(第二个参数显式给一个全名)"; exit 2; }
say "  探针信号: $SIG"

# ============================ ④ 基线电池 ====================================
if [ "$QUICK" = 0 ]; then
    say
    say "== ④ 基线电池(冷/建/暖, 追加 $CSV) =="
    ./scripts/bench_fsdb.sh "$FSDB_ABS" "$SIG" 2>&1 | sed 's/^/  /'
fi

# ============================ ⑤⑥ 并行轴 + 一致性 ============================
say
say "== ⑤⑥ 并行轴(冷建时间线) + 产物一致性 =="
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/j1" "$WORK/jauto" "$WORK/j8"

# runpar <tag> <jobs> <cache模式> <查询> <目录>
runpar() {
    local tag="$1" jobs="$2" mode="$3" q="$4" dir="$5" out t m val
    out=$( cd "$dir" && env WAL_CACHE="$mode" timeout 3600 /usr/bin/time -f "%e %M" \
        "$BIN" -j "$jobs" "$q" -l "$FSDB_ABS" 2>&1 )
    t=$(printf '%s\n' "$out" | sed -n 's/^\([0-9][0-9.]*\) [0-9]*$/\1/p' | tail -1)
    m=$(printf '%s\n' "$out" | sed -n 's/^[0-9.]* \([0-9]*\)$/\1/p' | tail -1)
    [ -n "$m" ] && m=$((m / 1024))   # %M 是 KB → 表头写的是 rss_mb
    val=$(printf '%s\n' "$out" | grep -E '^(=>|\()' | tail -1)
    if [ -z "$t" ]; then
        printf '  %-14s 失败: %s\n' "$tag" "$(printf '%s\n' "$out" | head -2 | tr '\n' ' ')"
        printf '%s\n' "$out" > "$WORK/$tag.err"
        return 1
    fi
    csv_add "fsdb-par-$tag" "$t" "$m" "$(printf '%s' "$val" | cut -c1-40 | tr ',' ' ')"
    printf '  %-14s %8ss  rss=%-9s %s\n' "$tag" "$t" "${m}KB" "$val"
    printf '%s\n' "$val" > "$WORK/$tag.val"
}
# 探针查询必须**逼出全局时间线**才算量到点上: 电平计数 `(count (= (get s) 1))` 按索引空间
# 求和 → `timeline()` → 冷建全文件一遍(FSDB 上就是"逐个信号走 NPI 变更流"), 这正是现场
# 大波形最贵的一段。只取几个值 `(get s i)` 的查询**不需要**时间线, 量出来会假快(踩过:
# 探针用 (get s 1000) 时 -j 1 与 -j auto 都是 6.5s, 且根本看不到 .ftl)。
Q="(list (count (= (get \"$SIG\") 1)) (get \"$SIG\" 1000) (at \"$SIG\" 1) (at \"$SIG\" 1000000000))"
FAILED=0
runpar j1 1 build "$Q" "$WORK/j1" || FAILED=1
runpar jauto auto build "$Q" "$WORK/jauto" || FAILED=1
runpar j8 8 build "$Q" "$WORK/j8" || FAILED=1
NPROC=$(nproc)
say "  本机核数: $NPROC (auto = min(额度, 8))"

# 取值一致性
if [ -f "$WORK/j1.val" ] && [ -f "$WORK/jauto.val" ]; then
    if cmp -s "$WORK/j1.val" "$WORK/jauto.val"; then
        say "  取值一致 ✓  -j 1 与 -j auto 同结果"
    else
        say "  ✗ 取值不一致: j1=$(cat "$WORK/j1.val")  jauto=$(cat "$WORK/jauto.val")"
        FAILED=1
    fi
fi
# .ftl 逐字节一致(并行 map/reduce 的核心闸)
ftl1=$(find "$WORK/j1" -name '*.ftl' | head -1)
ftla=$(find "$WORK/jauto" -name '*.ftl' | head -1)
if [ -n "$ftl1" ] && [ -n "$ftla" ]; then
    h1=$(sha256sum "$ftl1" | cut -c1-16); ha=$(sha256sum "$ftla" | cut -c1-16)
    s1=$(stat -c %s "$ftl1"); sa=$(stat -c %s "$ftla")
    if [ "$h1" = "$ha" ]; then
        say "  .ftl 逐字节相同 ✓  ($(numfmt --to=iec "$s1" 2>/dev/null || echo $s1)  sha=$h1)"
    else
        say "  ✗ .ftl 不一致!  j1=$h1/$s1  jauto=$ha/$sa  —— 这是正确性问题, 先修再谈快"
        FAILED=1
    fi
else
    say "  ⚠️ 没看到 .ftl(j1=$ftl1 jauto=$ftla)—— 可能没走时间线路径, 检查查询是否需要索引空间"
fi

# 暖命中(同一目录第二次)
if [ -f "$WORK/jauto.val" ]; then
    runpar jauto-hit auto auto "$Q" "$WORK/jauto" || true
fi

# ============================ 汇总 ==========================================
hr
say "== 汇总 =="
if [ "$QUICK" = 0 ]; then
    say "  基线电池已入 $CSV(op=fsdb-cold-*/build-*/hit-*)"
fi
say "  并行轴已入 $CSV(op=fsdb-par-*)"
if [ "$FAILED" = 1 ]; then
    say "  ❌ 有失败项 —— 先解决正确性/环境, 再动性能。"
    exit 3
fi
say "  ✅ 接收入库完成。下一步: 把上面数字与 bench/perf-history.csv 里上一个版本对比(同一 fixture),"
say "     再决定优化哪一段: load(名字树) / 冷建时间线(全文件一遍) / 暖查询(变更列缓存)。"
exit 0
