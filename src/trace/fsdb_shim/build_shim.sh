#!/bin/sh
# 编 FSDB 垫片(libwal_fsdb.so)+ 冒烟测试。
# 需要宿主的 Verdi FFR 头/库(免 license): VERDI_HOME/share/FsdbReader。
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
VERDI_HOME=${VERDI_HOME:-/home/hesheng/eda_tools/synopsys/verdi/X-2025.06-SP1}
F=$VERDI_HOME/share/FsdbReader
SRC=${SRC:-/home/hesheng/Projects/wal-rust/src/trace/fsdb_shim/ffr_shim.cpp}
OUT=${OUT:-$HERE/libwal_fsdb.so}

# ABI 相关的宏必须与厂商 COMPILER_DEF 一致(类布局/符号名依赖它们)
DEFS="-m64 -msse2 -DVCS64_FLAG -fPIC -DSTATIC_LIBRARY -DVCSCPU_X86_64 -mstackrealign \
-DFGP_ENABLE_TLS -DPEBLK_THREAD -DINST64_ENABLE -fcommon -DLINUX -DSynopsys_linux \
-DPPORT_LINUX -DPORT_LINUXAMD64 -DSVR4 -DNOVAS_OEM -D_REENTRANT -DFSDB_LIBRARY \
-DFSDBCOPYRIGHT -DVERDISIMDBLICKEY -w"

g++ $DEFS -shared -fPIC -I$F "$SRC" -o "$OUT" \
    -L$F/linux64 -lnffr -lnsys -lz -lpthread -ldl -lm
echo "built: $OUT"

# ABI 冒烟测试(按 Rust 侧将来同样的调用序列)
SMOKE=$(dirname "$SRC")/shim_smoke.c
if [ -f "$SMOKE" ]; then
    gcc -o "$HERE/shim_smoke" "$SMOKE" -L"$HERE" -lwal_fsdb \
        -L$F/linux64 -lnffr -lnsys -lz -Wl,-rpath,'$ORIGIN' -Wl,-rpath,"$F/linux64" 2>&1 | grep -v "^/usr/bin/ld: warning" || true
    echo "smoke: $HERE/shim_smoke  (用法: LD_LIBRARY_PATH=$F/linux64 ./shim_smoke <file.fsdb> [sig])"
fi
