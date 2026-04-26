#!/bin/bash
# WAL performance benchmark script
# Tests VCD loading + query performance across file sizes
#
# Usage:
#   ./scripts/bench_wal.sh                    # Run all benchmarks
#   ./scripts/bench_wal.sh --quick            # Quick mode (1GB only)
#   ./scripts/bench_wal.sh --size 10          # Test specific size
#   ./scripts/bench_wal.sh --generate 150     # Generate 150GB VCD first

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TEST_DATA_DIR="$PROJECT_DIR/test_data"
RESULTS_FILE="$PROJECT_DIR/bench_results.csv"
WAL_BIN="$PROJECT_DIR/target/release/wal-rust"

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

log_info()  { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Parse args
QUICK=false
GENERATE=""
SPECIFIC_SIZE=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --quick)    QUICK=true; shift ;;
        --generate) GENERATE="$2"; shift 2 ;;
        --size)     SPECIFIC_SIZE="$2"; shift 2 ;;
        *)          echo "Unknown: $1"; exit 1 ;;
    esac
done

# Generate VCD if requested
if [ -n "$GENERATE" ]; then
    log_info "Generating ${GENERATE}GB VCD file..."
    python3 "$PROJECT_DIR/gen_vcd_pyvcd.py" \
        --size "$GENERATE" \
        --output "$TEST_DATA_DIR/test_pyvcd_${GENERATE}G.vcd" \
        --signals 100
fi

# Build
log_info "Building WAL in release mode..."
cd "$PROJECT_DIR"
cargo build --release 2>&1 | tail -1

# Initialize results
echo "timestamp,file,size_bytes,load_time_s,max_index,signals" > "$RESULTS_FILE"

# Benchmark a single VCD file
bench_file() {
    local vcd_file="$1"
    local fname
    fname=$(basename "$vcd_file")

    if [ ! -f "$vcd_file" ]; then
        log_warn "Skipping: $vcd_file not found"
        return
    fi

    local size_bytes
    size_bytes=$(stat -c%s "$vcd_file" 2>/dev/null || stat -f%z "$vcd_file" 2>/dev/null)
    local size_gb
    size_gb=$(echo "scale=2; $size_bytes / 1073741824" | bc)

    log_info "Benchmarking: $fname (${size_gb} GB)"

    # Measure load time
    local start_ts end_ts load_time
    start_ts=$(date +%s%N)
    local output
    output=$("$WAL_BIN" run /dev/stdin -l "$vcd_file" -c '(+ (length (signals "t0")) (max-index))' 2>&1)
    end_ts=$(date +%s%N)
    load_time=$(echo "scale=3; ($end_ts - $start_ts) / 1000000000" | bc)

    # Parse result
    local result_line
    result_line=$(echo "$output" | grep "^=>" | head -1)
    if [ -z "$result_line" ]; then
        log_error "  Failed to load $vcd_file"
        echo "$output" | tail -3
        return
    fi

    # Get signal count separately
    local signals_output signals_count
    signals_output=$("$WAL_BIN" run /dev/stdin -l "$vcd_file" -c '(length (signals "t0"))' 2>&1)
    signals_count=$(echo "$signals_output" | grep "^=>" | sed 's/=> //')

    local max_index_output max_index
    max_index_output=$("$WAL_BIN" run /dev/stdin -l "$vcd_file" -c '(max-index)' 2>&1)
    max_index=$(echo "$max_index_output" | grep "^=>" | sed 's/=> //')

    local timestamp
    timestamp=$(date -Iseconds)

    echo "$timestamp,$fname,$size_bytes,$load_time,$max_index,$signals_count" >> "$RESULTS_FILE"

    log_info "  Load time: ${load_time}s"
    log_info "  Signals: $signals_count, Max index: $max_index"
    log_info "  Throughput: $(echo "scale=1; $size_gb / $load_time" | bc) GB/s"

    # Memory check
    log_info "  Memory target: <=2GB RSS for 150GB file"
}

# Main
echo "=========================================="
echo "  WAL Performance Benchmark"
echo "=========================================="
echo ""

if [ "$QUICK" = true ]; then
    bench_file "$TEST_DATA_DIR/test_pyvcd_1G.vcd"
else
    for size in 100M 1G; do
        bench_file "$TEST_DATA_DIR/test_pyvcd_${size}.vcd"
        echo ""
    done

    if [ -n "$SPECIFIC_SIZE" ]; then
        bench_file "$TEST_DATA_DIR/test_pyvcd_${SPECIFIC_SIZE}G.vcd"
    fi
fi

echo ""
log_info "Results saved to: $RESULTS_FILE"
log_info "Done!"
