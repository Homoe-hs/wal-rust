#!/bin/bash
# WAL Spec Compliance Test Runner
# Compares: original WAL (Python) vs wal-rust (Rust)
#
# Usage: ./test_samples/run_spec_test.sh

set -e

WAL_RUST="${CARGO_TARGET_DIR:-target/debug}/wal-rust"
WAL_ORIG="/tmp/wal-env/bin/wal"
SPEC_FILE="test_samples/spec_test.wal"
VCD_FILE="test_data/counter.vcd"

cd "$(dirname "$0")/.."

echo "========================================"
echo "  WAL Spec Compliance Test v0.8.4"
echo "========================================"
echo ""

# Build Rust version
echo "Building..."
cargo build -q 2>/dev/null

# Run original WAL
echo ">>> Running original WAL (Python) ..."
$WAL_ORIG $SPEC_FILE > /tmp/wal_orig.out 2>&1 || true
ORIG_PASS=$(grep -c "PASS" /tmp/wal_orig.out || echo 0)
ORIG_FAIL=$(grep -c "FAIL" /tmp/wal_orig.out || echo 0)
echo "    PASS: $ORIG_PASS, FAIL: $ORIG_FAIL"

# Run Rust WAL
echo ">>> Running wal-rust (Rust) ..."
$WAL_RUST $SPEC_FILE > /tmp/wal_rust.out 2>&1 || true
RUST_PASS=$(grep -c "PASS" /tmp/wal_rust.out || echo 0)
RUST_FAIL=$(grep -c "FAIL" /tmp/wal_rust.out || echo 0)
echo "    PASS: $RUST_PASS, FAIL: $RUST_FAIL"

# Compare
echo ""
echo ">>> Comparison:"
if [ "$ORIG_PASS" -eq "$RUST_PASS" ] && [ "$ORIG_FAIL" -eq "$RUST_FAIL" ]; then
    echo "  ✅ PASS/FAIL counts match!"
else
    echo "  ⚠️  Mismatch: original $ORIG_PASS/$ORIG_FAIL vs rust $RUST_PASS/$RUST_FAIL"
fi

# Run Rust unit tests
echo ""
echo ">>> Rust unit tests:"
cargo test -q 2>&1 | tail -1

echo ""
echo "========================================"
echo "  Done"
echo "========================================"
