#!/usr/bin/env bash
# ============================================================================
# FSDB 环境自检: "这份波形在**本机**能不能读、是谁写的、本机 reader 是哪个版本"。
#
#   ./scripts/fsdb_env_check.sh [file.fsdb]
#
# 为什么需要: FSDB 是**写者产物**。波形能不能读, 取决于
#   ① 文件是谁写的(写者版本串就写在文件头里, **不需要许可**就能看出来);
#   ② 本机 Verdi/NPI reader 的版本 —— 比写者老的 reader 会报 NSIS/打不开;
#   ③ 许可能不能 checkout(NPI 在 open 时才 check out)。
# 拿到别人的波形先跑这一条, 就知道"是波形的问题、reader 版本的问题、还是许可的问题",
# 不用去猜报错。
#
# 输出四段: 写者版本 / 本机 reader 与库 / 许可 / 真实打开一次(带耗时)。
# 退出码: 0 = 打开成功; 2 = 打不开(原因见输出)。
# ============================================================================
set -uo pipefail
cd "$(dirname "$0")/.."

BIN="${WAL_BIN:-target/release/wal-rust}"
FILE="${1:-}"

echo "== 本机工具链 =="
for d in "${VERDI_HOME:-}" /home/hesheng/eda_tools/synopsys/verdi/*/ ; do
    [ -n "$d" ] && [ -x "$d/bin/fsdbdebug" ] && echo "  verdi: $d"
done 2>/dev/null | sort -u
[ -n "${WAL_NPI_LIB:-}" ] && echo "  WAL_NPI_LIB=$WAL_NPI_LIB"
[ -x "$BIN" ] && echo "  wal-rust: $BIN ($("$BIN" --version 2>/dev/null))" \
              || echo "  wal-rust: 找不到 $BIN(先 cargo build --release)"

if [ -z "$FILE" ]; then
    echo
    echo "用法: $0 <file.fsdb>   (只看本机工具链也可以不带参数)"
    exit 0
fi
[ -f "$FILE" ] || { echo "找不到文件: $FILE"; exit 2; }

echo
echo "== 波形 =="
echo "  文件: $FILE ($(du -h "$FILE" | cut -f1))"
# 写者版本串在文件头里(FSDB 的 magic 是 04 03 02 01); grep -a 直接扫二进制
WRITER=$(grep -a -m1 -o 'VCS Release [A-Za-z0-9._ -]\{1,40\}' "$FILE" 2>/dev/null | head -1)
echo "  写者: ${WRITER:-<没找到 VCS Release 串: 可能不是 FSDB, 或写者不是 VCS>}"
MAGIC=$(od -An -tx1 -j8 -N4 "$FILE" 2>/dev/null | tr -d ' \n')
[ "$MAGIC" = "04030201" ] && echo "  magic: 04030201 ✓(是 FSDB)" \
                          || echo "  magic: ${MAGIC:-?}(不是 04030201 —— 可能不是 FSDB)"

echo
echo "== 许可 =="
for v in SNPSLMD_LICENSE_FILE LM_LICENSE_FILE; do
    [ -n "${!v:-}" ] && echo "  $v=${!v}"
done
[ -z "${SNPSLMD_LICENSE_FILE:-}${LM_LICENSE_FILE:-}" ] && echo "  (两个许可变量都没设 —— NPI 多半会在 open 时失败)"

echo
echo "== 真实打开一次 =="
if [ ! -x "$BIN" ]; then
    echo "  跳过: 没有 wal-rust 二进制"
    exit 2
fi
T0=$(date +%s)
OUT=$(WAL_DEBUG_FSDB=1 timeout 600 "$BIN" '(length (SIGNALS))' -l "$FILE" 2>&1)
RC=$?
T1=$(date +%s)
# NPI 库自己会往 stdout 打一段版权 banner —— 只取结果行, 其余丢掉
RESULT=$(printf '%s\n' "$OUT" | grep -E '^(=>|\()' | tail -1)
if [ $RC -eq 0 ]; then
    echo "  结果: ${RESULT:-$(printf '%s\n' "$OUT" | tail -1)}"
    echo "  耗时: $((T1 - T0))s(含 NPI 初始化)"
    exit 0
else
    echo "$OUT" | grep -E 'NSIS|License|许可|npi_init|FSDB 需要|error' | head -6 | sed 's/^/  /'
    echo "  打开失败(rc=$RC)。判读:"
    echo "   - 出现 NSIS / 'Verdi reader 新' → 本机 reader 比写者老: 要换 >= 波形写者版本的 Verdi/NPI"
    echo "   - 出现许可相关 → 先解决 license(SNPSLMD_LICENSE_FILE 指向可用的 daemon)"
    echo "   - '不是 FSDB 文件' → magic 对不上, 文件本身有问题"
    exit 2
fi
