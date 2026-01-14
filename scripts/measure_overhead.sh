#!/bin/bash
# measure_overhead.sh - Measure proxy overhead vs direct connection
#
# Implements: REQ-OBS-001 M-OH-001, M-OH-002 (Proxy Overhead Measurement)
#
# This script:
# 1. Starts mock LLM server
# 2. Runs k6 load test directly against mock LLM
# 3. Starts ThoughtGate proxy
# 4. Runs k6 load test through proxy
# 5. Calculates overhead difference
#
# Usage:
#   ./scripts/measure_overhead.sh [--output FILE]

set -euo pipefail

# ─────────────────────────────────────────────────────────────────────────────
# Configuration
# ─────────────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

THOUGHTGATE_BIN="${PROJECT_ROOT}/target/release/thoughtgate"
MOCK_LLM_BIN="${PROJECT_ROOT}/target/release/mock_llm"
K6_SCRIPT="${PROJECT_ROOT}/tests/benchmark-fast.js"
OUTPUT_FILE="${PROJECT_ROOT}/overhead_metrics.json"

MOCK_LLM_PORT=8888
PROXY_PORT=4141
K6_VUS=10
K6_DURATION="10s"

# Initialize PIDs to avoid unset variable errors under set -u
MOCK_LLM_PID=""
PROXY_PID=""

# Cleanup function
cleanup() {
    echo "🧹 Cleaning up..."
    [ -n "$MOCK_LLM_PID" ] && kill "$MOCK_LLM_PID" 2>/dev/null || true
    [ -n "$PROXY_PID" ] && kill "$PROXY_PID" 2>/dev/null || true
    rm -f direct_results.json proxy_results.json 2>/dev/null || true
}

trap cleanup EXIT

# ─────────────────────────────────────────────────────────────────────────────
# Helper Functions
# ─────────────────────────────────────────────────────────────────────────────

log_info() {
    echo "ℹ️  $*" >&2
}

log_error() {
    echo "❌ $*" >&2
}

log_success() {
    echo "✅ $*" >&2
}

wait_for_port() {
    local port=$1
    local max_wait=30
    local waited=0
    
    while ! nc -z localhost "$port" 2>/dev/null; do
        sleep 0.5
        waited=$((waited + 1))
        if [ $waited -gt $((max_wait * 2)) ]; then
            log_error "Timeout waiting for port $port"
            return 1
        fi
    done
    log_info "Port $port is ready"
}

# ─────────────────────────────────────────────────────────────────────────────
# Check Prerequisites
# ─────────────────────────────────────────────────────────────────────────────

check_prerequisites() {
    log_info "Checking prerequisites..."
    
    if ! command -v k6 &> /dev/null; then
        log_error "k6 is not installed. Install with: brew install k6"
        exit 1
    fi
    
    if [ ! -f "$THOUGHTGATE_BIN" ]; then
        log_error "ThoughtGate binary not found at $THOUGHTGATE_BIN"
        log_info "Build with: cargo build --release --bin thoughtgate"
        exit 1
    fi
    
    if [ ! -f "$MOCK_LLM_BIN" ]; then
        log_error "Mock LLM binary not found at $MOCK_LLM_BIN"
        log_info "Build with: cargo build --release --bin mock_llm --features mock"
        exit 1
    fi
    
    if [ ! -f "$K6_SCRIPT" ]; then
        log_error "k6 benchmark script not found at $K6_SCRIPT"
        exit 1
    fi
    
    log_success "All prerequisites satisfied"
}

# ─────────────────────────────────────────────────────────────────────────────
# Start Mock LLM
# ─────────────────────────────────────────────────────────────────────────────

start_mock_llm() {
    log_info "Starting mock LLM server on port $MOCK_LLM_PORT..."
    
    MOCK_LLM_PORT=$MOCK_LLM_PORT "$MOCK_LLM_BIN" &
    MOCK_LLM_PID=$!
    
    wait_for_port $MOCK_LLM_PORT
    log_success "Mock LLM started (PID: $MOCK_LLM_PID)"
}

# ─────────────────────────────────────────────────────────────────────────────
# Start ThoughtGate Proxy
# ─────────────────────────────────────────────────────────────────────────────

start_proxy() {
    log_info "Starting ThoughtGate proxy on port $PROXY_PORT..."
    
    THOUGHTGATE_UPSTREAM_URL="http://127.0.0.1:${MOCK_LLM_PORT}" \
    THOUGHTGATE_LISTEN_ADDR="127.0.0.1:${PROXY_PORT}" \
    "$THOUGHTGATE_BIN" &
    PROXY_PID=$!
    
    wait_for_port $PROXY_PORT
    log_success "ThoughtGate proxy started (PID: $PROXY_PID)"
}

# ─────────────────────────────────────────────────────────────────────────────
# Run k6 Load Test
# ─────────────────────────────────────────────────────────────────────────────

run_k6_test() {
    local target_url=$1
    local output_file=$2
    local label=$3
    
    log_info "Running k6 test against $target_url ($label)..."
    
    # Create temporary k6 script with dynamic URL
    local temp_script
    temp_script=$(mktemp)
    
    cat > "$temp_script" << EOF
import http from 'k6/http';

export const options = {
  vus: ${K6_VUS},
  duration: '${K6_DURATION}',
};

const PAYLOAD = JSON.stringify({ prompt: "A".repeat(1000) });

export default function () {
  http.post('${target_url}/v1/chat/completions', PAYLOAD, {
    headers: { 'Content-Type': 'application/json' },
    timeout: '5s',
  });
}
EOF
    
    k6 run --out json="$output_file" "$temp_script" 2>&1 | grep -E '(http_req_duration|http_reqs|iterations)' || true
    
    rm -f "$temp_script"
    log_success "k6 test completed for $label"
}

# ─────────────────────────────────────────────────────────────────────────────
# Calculate Overhead
# ─────────────────────────────────────────────────────────────────────────────

calculate_overhead() {
    local direct_file=$1
    local proxy_file=$2
    
    log_info "Calculating overhead metrics..."
    
    python3 << PYEOF
import json
import sys

def parse_k6_results(filename):
    """Parse k6 JSON output and return latency metrics."""
    durations = []
    
    with open(filename, 'r') as f:
        for line in f:
            try:
                data = json.loads(line.strip())
                if data.get('type') == 'Point' and data.get('metric') == 'http_req_duration':
                    value = data.get('data', {}).get('value')
                    if value is not None:
                        durations.append(value)
            except json.JSONDecodeError:
                continue
    
    if not durations:
        return None
    
    durations.sort()
    n = len(durations)
    
    return {
        'p50': durations[n // 2],
        'p95': durations[int(n * 0.95)],
        'p99': durations[int(n * 0.99)] if n >= 100 else durations[-1],
        'mean': sum(durations) / n,
        'count': n
    }

# Parse results
direct = parse_k6_results('$direct_file')
proxy = parse_k6_results('$proxy_file')

if not direct or not proxy:
    print("Error: Could not parse k6 results", file=sys.stderr)
    sys.exit(1)

print(f"Direct latency: p50={direct['p50']:.2f}ms, p95={direct['p95']:.2f}ms")
print(f"Proxy latency:  p50={proxy['p50']:.2f}ms, p95={proxy['p95']:.2f}ms")

# Calculate overhead
overhead_p50_ms = proxy['p50'] - direct['p50']
overhead_p95_ms = proxy['p95'] - direct['p95']
overhead_percent_p50 = ((proxy['p50'] - direct['p50']) / direct['p50'] * 100) if direct['p50'] > 0 else 0
overhead_percent_p95 = ((proxy['p95'] - direct['p95']) / direct['p95'] * 100) if direct['p95'] > 0 else 0

print(f"\nOverhead: p50={overhead_p50_ms:.2f}ms ({overhead_percent_p50:.1f}%), p95={overhead_p95_ms:.2f}ms ({overhead_percent_p95:.1f}%)")

# Generate metrics output
metrics = [
    {"name": "overhead/latency_p50", "value": round(overhead_p50_ms, 3), "unit": "ms"},
    {"name": "overhead/latency_p95", "value": round(overhead_p95_ms, 3), "unit": "ms"},
    {"name": "overhead/percent_p50", "value": round(overhead_percent_p50, 2), "unit": "%"},
    {"name": "overhead/percent_p95", "value": round(overhead_percent_p95, 2), "unit": "%"},
    {"name": "direct/latency_p50", "value": round(direct['p50'], 3), "unit": "ms"},
    {"name": "direct/latency_p95", "value": round(direct['p95'], 3), "unit": "ms"},
    {"name": "proxy/latency_p50", "value": round(proxy['p50'], 3), "unit": "ms"},
    {"name": "proxy/latency_p95", "value": round(proxy['p95'], 3), "unit": "ms"},
]

# Check targets
targets_met = True
if overhead_p50_ms > 2.0:
    print(f"⚠️  WARNING: Overhead p50 ({overhead_p50_ms:.2f}ms) exceeds target (< 2ms)")
    targets_met = False
if overhead_percent_p50 > 10.0:
    print(f"⚠️  WARNING: Overhead percent p50 ({overhead_percent_p50:.1f}%) exceeds target (< 10%)")
    targets_met = False

if targets_met:
    print("✅ All overhead targets met!")

with open('$OUTPUT_FILE', 'w') as f:
    json.dump(metrics, f, indent=2)
print(f"\nMetrics written to $OUTPUT_FILE")
PYEOF
}

# ─────────────────────────────────────────────────────────────────────────────
# Main
# ─────────────────────────────────────────────────────────────────────────────

main() {
    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            --output)
                OUTPUT_FILE="$2"
                shift 2
                ;;
            *)
                log_error "Unknown option: $1"
                exit 1
                ;;
        esac
    done
    
    log_info "ThoughtGate Overhead Measurement"
    log_info "================================="
    
    check_prerequisites
    
    # Start mock LLM
    start_mock_llm
    
    # Run direct test
    run_k6_test "http://127.0.0.1:${MOCK_LLM_PORT}" "direct_results.json" "direct"
    
    # Start proxy
    start_proxy
    
    # Run proxy test
    run_k6_test "http://127.0.0.1:${PROXY_PORT}" "proxy_results.json" "proxy"
    
    # Calculate overhead
    calculate_overhead "direct_results.json" "proxy_results.json"
    
    log_success "Overhead measurement complete!"
}

main "$@"
