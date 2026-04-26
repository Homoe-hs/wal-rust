#!/usr/bin/env bash
# Verification script for wal-rust project

set -e

echo "=== WAL-Rust Project Verification ==="
echo ""

echo "1. Checking project structure..."
echo "   - FST module exists: $([ -d src/fst ] && echo 'YES' || echo 'NO')"
echo "   - VCD module exists: $([ -d src/vcd ] && echo 'YES' || echo 'NO')"
echo "   - WAL module exists: $([ -d src/wal ] && echo 'YES' || echo 'NO')"
echo "   - Trace module exists: $([ -d src/trace ] && echo 'YES' || echo 'NO')"
echo "   - Convert module removed: $([ -d src/convert ] && echo 'NO (still exists!)' || echo 'YES')"
echo ""

echo "2. Checking dependencies..."
grep -q "lz4_flex" Cargo.toml && echo "   - LZ4 support: YES" || echo "   - LZ4 support: NO"
grep -q "flate2" Cargo.toml && echo "   - Zlib support: YES" || echo "   - Zlib support: NO"
grep -q "bzip2" Cargo.toml && echo "   - Bzip2 support: YES" || echo "   - Bzip2 support: NO"
echo ""

echo "3. Checking CLI commands..."
grep -q "Convert" src/cli.rs && echo "   - Convert command: STILL EXISTS (should be removed!)" || echo "   - Convert command: REMOVED (correct)"
grep -q "Run\|Repl" src/cli.rs && echo "   - Run/Repl commands: YES" || echo "   - Run/Repl commands: NO"
echo ""

echo "4. Checking trace implementations..."
grep -q "FstTrace" src/trace/mod.rs && echo "   - FstTrace: YES" || echo "   - FstTrace: NO"
grep -q "VcdTrace" src/trace/mod.rs && echo "   - VcdTrace: YES" || echo "   - VcdTrace: NO"
echo ""

echo "5. Running tests..."
cargo test --lib 2>&1 | tail -5
echo ""

echo "6. Building release..."
cargo build --release 2>&1 | tail -3
echo ""

echo "=== Verification Complete ==="