#!/bin/bash
# WAL Test Runner
# Usage: ./test_samples/run_tests.sh

set -e

WAL_RUST="${CARGO_TARGET_DIR:-target/debug}/wal-rust"
cd "$(dirname "$0")/.."

echo "========================================"
echo "  WAL Test Suite v0.8.4"
echo "========================================"
echo ""

# Build if needed
if [ ! -f "$WAL_RUST" ]; then
  echo "Building..."
  cargo build -q
fi

# Core language tests
echo ">>> Running test_core.wal..."
$WAL_RUST test_samples/test_core.wal 2>&1 | grep -E "PASS|FAIL|Error"
echo ""

# Waveform tests (requires counter.vcd)
echo ">>> Running test_waveform.wal..."
$WAL_RUST run -l test_data/counter.vcd test_samples/test_waveform.wal 2>&1 | grep -E "PASS|FAIL|Error"
echo ""

echo "========================================"
echo "  Tests complete"
echo "========================================"
