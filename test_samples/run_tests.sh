#!/usr/bin/env bash
# ============================================================================
# WAL 脚本层冒烟测试 —— 跑 test_samples/ 下"自包含"的脚本, 检查解释器可用性。
#
#   ./test_samples/run_tests.sh                    # 用 target/release/wal-rust
#   WAVE=design.vcd ./test_samples/run_tests.sh    # 额外跑需要波形的脚本
#
# 与 cargo 测试的分工: cargo 测的是**内部实现**(引擎/解析/缓存), 这里测的是
# "使用者视角": 脚本能跑通、没有 error、退出码为 0。
# 历史用例(0.8 时代的 spec_test/spec_audit)默认**不跑** —— 它们针对上游 WAL 语法,
# 在本实现上本来就会报错, 只作参考(见 docs/README.md 的文档状态约定)。
# ============================================================================
set -uo pipefail
cd "$(dirname "$0")/.."

BIN="${WAL_BIN:-target/release/wal-rust}"
[ -x "$BIN" ] || { echo "找不到 $BIN(先 cargo build --release)"; exit 2; }

# 自包含脚本: 不依赖外部波形
PLAIN=(test_load.wal)
# 需要波形: 用 -l 提供(WAVE=... 时跑)
NEEDS_WAVE=(verify_vcd.wal fst_check.wal test_fst_analysis.wal)

pass=0; fail=0
run_one() {
    local f="$1"; shift
    local out rc
    out=$(WAL_CACHE=off "$BIN" run "$@" "test_samples/$f" 2>&1); rc=$?
    # 只看真正的错误形态(脚本里出现 "error" 这个词不算), 并信任退出码
    if [ $rc -eq 0 ] && ! printf '%s' "$out" | grep -qE '^Error on line|^error:|error\(s\),'; then
        printf '  \033[32mPASS\033[0m %-24s %s\n' "$f" "$(printf '%s' "$out" | tail -1)"
        pass=$((pass + 1))
    else
        printf '  \033[31mFAIL\033[0m %-24s rc=%d\n' "$f" "$rc"
        printf '%s\n' "$out" | grep -iE "error" | head -3 | sed 's/^/       /'
        fail=$((fail + 1))
    fi
}

echo "== 自包含脚本 =="
for f in "${PLAIN[@]}"; do run_one "$f"; done

if [ -n "${WAVE:-}" ] && [ -f "${WAVE:-}" ]; then
    echo "== 需要波形的脚本(WAVE=$WAVE) =="
    for f in "${NEEDS_WAVE[@]}"; do run_one "$f" -l "$WAVE"; done
else
    echo "== 需要波形的脚本: 跳过(设 WAVE=<波形文件> 才跑) =="
fi

echo
echo "通过 $pass / 失败 $fail"
[ $fail -eq 0 ]
