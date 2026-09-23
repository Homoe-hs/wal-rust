#!/usr/bin/env bash
# ============================================================================
# wal-rust 本地 CI —— 单一入口, 本机与 GitHub Actions 跑的是**同一份**脚本。
#
#   ./scripts/ci.sh              # 默认: 跑全部必过阶段(fast 档)
#   ./scripts/ci.sh --full       # 追加: 大规模 fuzz 对拍 + glibc2.17 打包冒烟
#   ./scripts/ci.sh --list       # 列出阶段
#   ./scripts/ci.sh --only fmt,clippy
#   ./scripts/ci.sh --fast       # 只跑最快的一圈(提交前用)
#
# 退出码: 0 = 全部通过(跳过的不算失败); 非 0 = 有阶段失败。
# 日志:   每个阶段单独一份 .tools/ci-logs/<stage>.log, 失败时打印尾部。
#
# 设计约束(本项目特有, 别改成通用模板):
#   * 依赖装在仓库内(.cargo-home / .tools/cache), 所以要显式传 CARGO_HOME/XDG_CACHE_HOME;
#   * 需要 Verdi/NPI 的 FSDB 门在没有许可证的环境自动跳过, 不能因此判失败;
#   * 长任务(build/test)不要用管道接 head —— 早退的 SIGPIPE 会杀掉 cargo;
#   * 本脚本只读仓库, 不写任何被跟踪的文件。
# ============================================================================
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# --- 环境: 仓库内工具链/缓存(与 CI runner 一致) ------------------------------
export CARGO_HOME="${CARGO_HOME:-$REPO_ROOT/.cargo-home}"
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-$REPO_ROOT/.tools/cache}"
ZIG_DIR="$REPO_ROOT/.tools/zig-linux-x86_64-0.13.0"
[ -x "$ZIG_DIR/zig" ] && export PATH="$ZIG_DIR:$CARGO_HOME/bin:$PATH"
export PATH="$CARGO_HOME/bin:$PATH"
# 让测试也走仓库内缓存, 避免污染 $HOME(沙箱/CI 上可能只读)
export WAL_CACHE_DIR="${WAL_CACHE_DIR:-$REPO_ROOT/.tools/ci-cache}"

LOG_DIR="$REPO_ROOT/.tools/ci-logs"
mkdir -p "$LOG_DIR" "$WAL_CACHE_DIR"

# --- 颜色(非 tty 自动关闭) ----------------------------------------------------
if [ -t 1 ]; then B=$'\033[1m'; G=$'\033[32m'; R=$'\033[31m'; Y=$'\033[33m'; N=$'\033[0m'; else B=; G=; R=; Y=; N=; fi

# --- 阶段表: 名字|是否默认跑|是否 full 才跑|说明 ---------------------------------
STAGES_ALL=(
    "preflight|1|0|环境自检(rustc/cargo/必需文件/仓库卫生)"
    "docs|1|0|文档一致性: 本地路径引用、版本号、AGENTS.md 声称的测试数"
    "fmt|1|0|rustfmt 检查(默认提示级, WAL_CI_FMT_STRICT=1 才失败)"
    "clippy|1|0|cargo clippy(默认提示级, WAL_CI_CLIPPY_STRICT=1 才失败)"
    "build|1|0|cargo build --release"
    "test|1|0|cargo test --release(全部单元/集成测试)"
    "samples|1|0|脚本层冒烟: test_samples/ 自包含 + 依赖波形的自检脚本(断言式)"
    "gates|1|0|语义冻结闸: 矩阵 / VCD↔FST 差分 / 引擎↔逐拍 oracle"
    "perf|0|0|性能冒烟: 名字解析基准(防 O(N) 求值回归) + 有 bench/data 时的冷加载"
    "package|1|0|打包冒烟: glibc2.17 交叉构建 + --version/基本查询"
    "fuzz|0|1|大规模随机差分(WAL_CI_FUZZ_N 控制规模)"
)

run_stage() {
    local name="$1" desc="$2" fn="$3"
    local log="$LOG_DIR/$name.log"
    local t0 t1 rc
    printf '%s==> %-9s%s %s\n' "$B" "$name" "$N" "$desc"
    t0=$(date +%s)
    ( set -o pipefail; "$fn" ) >"$log" 2>&1
    rc=$?
    t1=$(date +%s)
    local dur=$((t1 - t0))
    if [ $rc -eq 0 ]; then
        printf '    %sPASS%s (%ss)  日志: %s\n' "$G" "$N" "$dur" "${log#$REPO_ROOT/}"
    elif [ $rc -eq 77 ]; then
        printf '    %sSKIP%s (%ss)  %s\n' "$Y" "$N" "$dur" "$(tail -1 "$log" 2>/dev/null)"
    else
        printf '    %sFAIL%s (%ss, rc=%d)  日志: %s\n' "$R" "$N" "$dur" "$rc" "${log#$REPO_ROOT/}"
        printf -- '---- 最后 25 行 ----\n'
        tail -25 "$log"
        printf -- '--------------------\n'
    fi
    RESULTS+=("$(printf '%-9s %-5s %4ss' "$name" "$([ $rc -eq 0 ] && echo PASS || { [ $rc -eq 77 ] && echo SKIP || echo FAIL; })" "$dur")")
    [ $rc -eq 0 ] || [ $rc -eq 77 ] || FAILED=$((FAILED + 1))
}

# --- 各阶段实现 ---------------------------------------------------------------

stage_preflight() {
    echo "rustc: $(rustc --version)"
    echo "cargo: $(cargo --version)"
    command -v rustc >/dev/null || { echo "缺 rustc"; return 1; }
    command -v cargo >/dev/null || { echo "缺 cargo"; return 1; }
    # 工程外壳必需文件(缺了说明仓库结构被破坏)
    local missing=0
    for f in Cargo.toml Cargo.lock LICENSE-MIT LICENSE-APACHE README.md CONTRIBUTING.md CHANGELOG.md; do
        [ -e "$f" ] || { echo "缺文件: $f"; missing=1; }
    done
    [ $missing -eq 0 ] || return 1
    # 仓库卫生: 不能有被跟踪的编译产物/大文件(>5MB)
    local big
    big=$(git ls-files -z | xargs -0 -I{} sh -c 'f="{}"; [ -f "$f" ] && s=$(stat -c%s "$f") && [ "$s" -gt 5242880 ] && echo "$s $f"' 2>/dev/null | sort -rn | head -5)
    if [ -n "$big" ]; then echo "被 git 跟踪的大文件(>5MB):"; echo "$big"; return 1; fi
    if git ls-files | grep -qE '\.(o|so|a|rlib)$'; then echo "被跟踪的编译产物:"; git ls-files | grep -E '\.(o|so|a|rlib)$'; return 1; fi
    local dirty
    dirty=$(git status --porcelain | wc -l)
    echo "工作区改动: $dirty 处(CI 只读, 不清理)"
    echo "OK"
}

stage_docs() {
    [ -f scripts/check_docs.py ] || { echo "缺 scripts/check_docs.py"; return 1; }
    python3 scripts/check_docs.py
}

stage_fmt() {
    if ! cargo fmt --check >"$LOG_DIR/fmt.diff" 2>&1; then
        local n
        n=$(grep -c '^Diff in' "$LOG_DIR/fmt.diff" 2>/dev/null || echo '?')
        echo "有 $n 处格式差异(明细: .tools/ci-logs/fmt.diff)"
        echo "只改你要提交的文件:  cargo fmt -- <file>..."
        if [ "${WAL_CI_FMT_STRICT:-0}" = "1" ]; then return 1; fi
        echo "(提示级: 设 WAL_CI_FMT_STRICT=1 让此阶段失败)"
        return 0
    fi
    echo "rustfmt: 全部已格式化"
}

stage_clippy() {
    cargo clippy --release --all-targets -- -D warnings >"$LOG_DIR/clippy.raw" 2>&1
    local rc=$?
    local warns
    warns=$(grep -cE '^(warning|error)' "$LOG_DIR/clippy.raw" || true)
    if [ $rc -eq 0 ]; then echo "clippy: 零告警"; return 0; fi
    echo "clippy: $warns 条告警/错误(明细: .tools/ci-logs/clippy.raw)"
    grep -E '^(warning|error)' "$LOG_DIR/clippy.raw" | sort | uniq -c | sort -rn | head -8
    if [ "${WAL_CI_CLIPPY_STRICT:-0}" = "1" ]; then return 1; fi
    echo "(提示级: 设 WAL_CI_CLIPPY_STRICT=1 让此阶段失败; 收敛计划见 rustfmt.toml 注释)"
    return 0
}

stage_build() {
    cargo build --release 2>&1 | tail -3
    local bin=target/release/wal-rust
    [ -x "$bin" ] || return 1
    "$bin" --version
}

stage_test() {
    cargo test --release 2>&1 | tee "$LOG_DIR/test.raw" | grep -E '^(running|test result|error|FAILED)' | tail -40
    ! grep -qE 'FAILED|test result: FAILED' "$LOG_DIR/test.raw"
}

stage_samples() {
    # 生成一条最小波形, 让"需要波形"的自检脚本真的跑起来(否则它们永远是跳过)
    local smoke="$LOG_DIR/smoke.vcd"
    {
        printf '$timescale 1ns $end\n$scope module t $end\n'
        printf '$var wire 1 ! clk $end\n$var wire 8 " data [7:0] $end\n'
        printf '$enddefinitions $end\n$dumpvars\n0!\nb00000000 "\n$end\n'
        for i in $(seq 0 20); do
            printf '#%d\n%d!\nb0000000%d "\n' "$((i * 5))" "$((i % 2))" "$((i % 8))"
        done
    } > "$smoke"
    WAL_BIN=target/release/wal-rust WAVE="$smoke" bash test_samples/run_tests.sh
}

stage_gates() {
    local rc=0
    # 1) 语义冻结矩阵 + 引擎↔逐拍对拍(146 项里最关键的 51 项都在这)
    cargo test --release --test regression_matrix 2>&1 | tail -3 || rc=1
    # 2) VCD↔FST 随机差分(引擎 ↔ 纯逐拍 oracle 的独立对拍)
    WAL_FUZZ_N="${WAL_FUZZ_N:-120}" cargo test --release --test fuzz_vcd_fst_diff 2>&1 | tail -3 || rc=1
    # 3) 旧版本二进制语义门(可选: 有 .tools/wal-rust.old 才跑)
    if [ -x .tools/wal-rust.old ]; then
        bash scripts/diff_find.sh .tools/wal-rust.old target/release/wal-rust 2>&1 | tail -2 || rc=1
    else
        echo "跳过旧版语义门(无 .tools/wal-rust.old)"
    fi
    # 4) FSDB↔VCD 同源差分门: 需要 Verdi/NPI + 一对同源波形。
    #    这两个测试标了 #[ignore], 所以"没 Verdi"时在测试统计里如实显示为 ignored
    #    (以前是静默 return → 报 5 passed, 造成"门禁跑过"的假象)。
    cargo test --release --test fsdb_diff 2>&1 | tail -2 || rc=1
    if [ -n "${WAL_FSDB_TEST_FILE:-}" ] && [ -n "${WAL_FSDB_TEST_VCD:-}" ]; then
        echo "Verdi 样本已提供 → 跑被 ignore 的 FSDB↔VCD 差分门"
        cargo test --release --test fsdb_diff -- --include-ignored --nocapture 2>&1 | tail -6 || rc=1
    else
        echo "FSDB↔VCD 差分门未跑(未设 WAL_FSDB_TEST_FILE/WAL_FSDB_TEST_VCD): 上面应显示 ignored, 不是 passed"
        echo "  跑法: export VERDI_HOME=... SNPSLMD_LICENSE_FILE=... ;"
        echo "        WAL_FSDB_TEST_FILE=design.fsdb WAL_FSDB_TEST_VCD=design.vcd make gates"
    fi
    return $rc
}

stage_perf() {
    # ① 名字解析/裸符号求值基准(自带合成样本, 不需要 bench/data):
    #    这条专门防"每次求值克隆整张名字表"回归 —— 188 万信号 FSDB 上它就是分钟级卡死。
    #    预算放得很宽(60k 信号 × 800 时间戳的逐拍路径, 正常 <5s), 只抓数量级退化。
    local n="${WAL_CI_NAMES_N:-60000}" t="${WAL_CI_NAMES_T:-800}"
    local log="$LOG_DIR/bench_names.log"
    local t0 t1 step_s
    t0=$(date +%s)
    ./scripts/bench_name_resolution.sh "$n" "$t" >"$log" 2>&1 || { echo "基准脚本失败(见 $log)"; return 1; }
    t1=$(date +%s)
    step_s=$((t1 - t0))
    grep -E "引擎|逐拍|字符串" "$log" | sed 's/^/  /'
    echo "  基准总耗时: ${step_s}s"
    if [ "$step_s" -gt 240 ]; then
        echo "名字解析疑似退化(超过 240s 预算) —— 检查热路径是否又在调用 traces.signals()"
        return 1
    fi

    # ①b FSDB 查询基准(有 Verdi + 大 FSDB 才跑: 设 WAL_FSDB_BENCH=<file.fsdb>)。
    #     判据是"暖查询应当接近只加载": 若 fsdb-hit-* 退化到 cold 量级, 说明旁挂列
    #     缓存(`.fcol`)没生效 —— 那正是现场"每次查询都慢"的根因。
    if [ -n "${WAL_FSDB_BENCH:-}" ]; then
        local flog="$LOG_DIR/bench_fsdb.log"
        echo "  FSDB 基准: $WAL_FSDB_BENCH"
        if ./scripts/bench_fsdb.sh "$WAL_FSDB_BENCH" ${WAL_FSDB_BENCH_SIG:-} >"$flog" 2>&1; then
            grep -E "load|edge|level|at" "$flog" | sed 's/^/  /'
        else
            echo "  FSDB 基准失败(见 $flog; 记录数字但不拦 CI)"; tail -3 "$flog" | sed 's/^/  /'
        fi
    else
        echo "  跳过 FSDB 基准(设 WAL_FSDB_BENCH=<file.fsdb> 开启)"
    fi

    # ② 大样本冒烟(有 bench/data 才跑)
    if [ ! -d bench/data ] || [ -z "$(ls -A bench/data 2>/dev/null | grep -E '\.(vcd|fst|fsdb)$')" ]; then
        echo "跳过: bench/data 里没有大样本(见 bench/README.md 的生成方法)"; return 0
    fi
    local w
    w=$(ls -S bench/data/*.vcd 2>/dev/null | head -1)
    echo "样本: $w ($(du -h "$w" | cut -f1))"
    local t0 t1 out
    t0=$(date +%s%N)
    # `-l` 已是加载入口;这里读一次信号表(冷加载 = 头解析 + $dumpvars 快照)
    out=$(target/release/wal-rust '(length (SIGNALS))' -l "$w" 2>&1 | tail -1) || true
    t1=$(date +%s%N)
    case "$out" in
        "=> "*|"("*) echo "冷加载(信号表): $(( (t1 - t0) / 1000000 ))ms  $out" ;;
        *) echo "加载/查询失败: $out"; return 1 ;;
    esac
    echo "对照历史: bench/RESULTS.md(超过 1.5 倍即视为回归, 需要人工确认)"
}

stage_package() {
    # 注意: `--target x86_64-unknown-linux-gnu.2.17` 里的 `.2.17` 是 zigbuild 的
    # glibc 版本标记, **产物目录名会去掉它**(target/x86_64-unknown-linux-gnu/release)。
    local zig_target=x86_64-unknown-linux-gnu.2.17
    local out_target=x86_64-unknown-linux-gnu
    if ! command -v cargo-zigbuild >/dev/null || ! command -v zig >/dev/null; then
        echo "跳过: 没装 cargo-zigbuild/zig(见 scripts/install.sh)"; return 77
    fi
    cargo zigbuild --release --target "$zig_target" 2>&1 | tail -2 || return 1
    local bin="target/$out_target/release/wal-rust"
    [ -x "$bin" ] || return 1
    "$bin" --version
    # 冒烟: 用最小波形跑一次真实查询
    local tmp
    tmp=$(mktemp -d)
    printf '$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n#0\n0!\n#5\n1!\n#10\n0!\n' >"$tmp/smoke.vcd"
    local out
    out=$(WAL_CACHE=off "$bin" '(count (rising "t.c"))' -l "$tmp/smoke.vcd" 2>&1 | tail -1)
    rm -rf "$tmp"
    echo "冒烟查询: $out"
    [ "$out" = "=> 1" ] || { echo "期望 => 1"; return 1; }
    echo "glibc 需求: $(objdump -T "$bin" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)"
}

stage_fuzz() {
    local n="${WAL_CI_FUZZ_N:-1500}"
    echo "随机差分规模 WAL_FUZZ_N=$n(可用 WAL_CI_FUZZ_N 调整)"
    WAL_FUZZ_N="$n" cargo test --release --test fuzz_vcd_fst_diff 2>&1 | tail -3
}

# --- 参数解析 -----------------------------------------------------------------
ONLY=""
MODE="default"
while [ $# -gt 0 ]; do
    case "$1" in
        --list)
            printf '%-10s %-6s %s\n' "阶段" "默认" "说明"
            for row in "${STAGES_ALL[@]}"; do
                IFS='|' read -r n d f desc <<<"$row"
                printf '%-10s %-6s %s\n' "$n" "$([ "$d" = 1 ] && echo 是 || { [ "$f" = 1 ] && echo full || echo 否; })" "$desc"
            done
            exit 0 ;;
        --only) ONLY="$2"; shift 2 ;;
        --full) MODE="full"; shift ;;
        --fast) MODE="fast"; shift ;;
        -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
        *) echo "未知参数: $1(用 --help)"; exit 2 ;;
    esac
done

FAILED=0
RESULTS=()
T0=$(date +%s)
printf '%s== wal-rust CI%s  %s  模式=%s\n' "$B" "$N" "$(date '+%F %T')" "$MODE"

if [ -n "$ONLY" ]; then
    IFS=',' read -ra wanted <<<"$ONLY"
    for w in "${wanted[@]}"; do
        found=0
        for row in "${STAGES_ALL[@]}"; do
            IFS='|' read -r n d f desc <<<"$row"
            if [ "$n" = "$w" ]; then found=1; run_stage "$n" "$desc" "stage_$n"; fi
        done
        [ $found -eq 1 ] || { echo "未知阶段: $w(--list 看清单)"; exit 2; }
    done
else
    for row in "${STAGES_ALL[@]}"; do
        IFS='|' read -r n d f desc <<<"$row"
        if [ "$d" = 1 ] || { [ "$f" = 1 ] && [ "$MODE" = "full" ]; }; then
            run_stage "$n" "$desc" "stage_$n"
        fi
        if [ "$MODE" = "fast" ] && [ "$n" = "build" ]; then break; fi
    done
fi

TOTAL=$(( $(date +%s) - T0 ))
printf '\n%s== 汇总%s (用时 %ss)\n' "$B" "$N" "$TOTAL"
for r in "${RESULTS[@]}"; do echo "  $r"; done
if [ $FAILED -eq 0 ]; then
    printf '%sCI 通过%s —— 可以提交/发布\n' "$G" "$N"
    exit 0
fi
printf '%sCI 失败: %d 个阶段%s\n' "$R" "$FAILED" "$N"
echo "复跑单个阶段: ./scripts/ci.sh --only <阶段名>"
exit 1
