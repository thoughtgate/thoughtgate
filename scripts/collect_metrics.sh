#!/bin/bash
# collect_metrics.sh - Performance metrics collection for ThoughtGate
#
# Implements: REQ-OBS-001 (Performance Metrics & Benchmarking)
#
# This script collects various performance metrics and outputs them in
# Bencher.dev-compatible JSON format.
#
# Usage:
#   ./scripts/collect_metrics.sh [options]
#
# Options:
#   --binary-only       Only collect binary metrics
#   --startup-only      Only collect startup metrics
#   --all               Collect all metrics (default)
#   --output FILE       Output file (default: bench_metrics.json)

set -euo pipefail

# ─────────────────────────────────────────────────────────────────────────────
# Configuration
# ─────────────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY_PATH="${PROJECT_ROOT}/target/release/thoughtgate"
OUTPUT_FILE="${PROJECT_ROOT}/bench_metrics.json"
LISTEN_PORT="${THOUGHTGATE_LISTEN_PORT:-8080}"
MAX_STARTUP_WAIT_MS=10000
POLL_INTERVAL_MS=10

# Metrics array (will be converted to JSON)
declare -a METRICS=()

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

# Add a metric to the collection
# Usage: add_metric "name" "value" "unit"
add_metric() {
    local name="$1"
    local value="$2"
    local unit="$3"
    
    METRICS+=("{\"name\": \"$name\", \"value\": $value, \"unit\": \"$unit\"}")
    log_info "Collected: $name = $value $unit"
}

# Convert METRICS array to JSON
metrics_to_json() {
    local result="["
    local first=true
    
    for metric in "${METRICS[@]}"; do
        if [ "$first" = true ]; then
            first=false
        else
            result+=","
        fi
        result+=$'\n  '"$metric"
    done
    
    result+=$'\n]'
    echo "$result"
}

# Get current time in nanoseconds (Linux)
get_time_ns() {
    date +%s%N
}

# Get current time in milliseconds
get_time_ms() {
    echo $(($(get_time_ns) / 1000000))
}

# Wait for HTTP endpoint to return 200
# Returns: time in milliseconds, or -1 on timeout
wait_for_endpoint() {
    local url="$1"
    local start_ms=$(get_time_ms)
    local elapsed=0
    
    while [ $elapsed -lt $MAX_STARTUP_WAIT_MS ]; do
        if curl -sf "$url" > /dev/null 2>&1; then
            local end_ms=$(get_time_ms)
            echo $((end_ms - start_ms))
            return 0
        fi
        sleep 0.$(printf '%03d' $POLL_INTERVAL_MS)
        elapsed=$(($(get_time_ms) - start_ms))
    done
    
    echo "-1"
    return 1
}

# ─────────────────────────────────────────────────────────────────────────────
# M-BIN-001: Binary Size
# ─────────────────────────────────────────────────────────────────────────────

collect_binary_size() {
    log_info "Collecting binary size..."
    
    if [ ! -f "$BINARY_PATH" ]; then
        log_error "Binary not found at $BINARY_PATH"
        return 1
    fi
    
    local size
    # Use stat with appropriate flags for Linux vs macOS
    if [[ "$OSTYPE" == "darwin"* ]]; then
        size=$(stat -f%z "$BINARY_PATH")
    else
        size=$(stat -c%s "$BINARY_PATH")
    fi
    
    add_metric "binary/size" "$size" "bytes"
    
    # Also add size in MB for convenience
    local size_mb
    size_mb=$(echo "scale=2; $size / 1048576" | bc)
    log_info "Binary size: ${size_mb} MB"
}

# ─────────────────────────────────────────────────────────────────────────────
# M-START-001, M-START-003: Startup Timing
# ─────────────────────────────────────────────────────────────────────────────

collect_startup_metrics() {
    log_info "Collecting startup metrics..."
    
    if [ ! -f "$BINARY_PATH" ]; then
        log_error "Binary not found at $BINARY_PATH"
        return 1
    fi
    
    # Start ThoughtGate in background
    local start_ns=$(get_time_ns)
    
    # Use a temp config to avoid needing upstream
    THOUGHTGATE_UPSTREAM_URL="http://127.0.0.1:19999" \
    THOUGHTGATE_LISTEN_ADDR="127.0.0.1:${LISTEN_PORT}" \
    "$BINARY_PATH" &
    local pid=$!
    
    # Ensure cleanup on exit
    trap "kill $pid 2>/dev/null || true" EXIT
    
    # M-START-001: Wait for /health (liveness)
    local health_url="http://127.0.0.1:${LISTEN_PORT}/health"
    local healthy_ms
    healthy_ms=$(wait_for_endpoint "$health_url") || {
        log_error "Health check timed out after ${MAX_STARTUP_WAIT_MS}ms"
        kill $pid 2>/dev/null || true
        return 1
    }
    add_metric "startup/to_healthy" "$healthy_ms" "ms"
    
    # M-START-003: Wait for /ready (readiness - policies loaded)
    local ready_url="http://127.0.0.1:${LISTEN_PORT}/ready"
    local ready_ms
    ready_ms=$(wait_for_endpoint "$ready_url") || {
        log_error "Readiness check timed out after ${MAX_STARTUP_WAIT_MS}ms"
        kill $pid 2>/dev/null || true
        return 1
    }
    add_metric "startup/to_ready" "$ready_ms" "ms"
    
    # M-MEM-001: Idle RSS
    if [[ "$OSTYPE" != "darwin"* ]]; then
        local idle_rss_kb
        idle_rss_kb=$(grep VmRSS /proc/$pid/status 2>/dev/null | awk '{print $2}' || echo "0")
        local idle_rss=$((idle_rss_kb * 1024))
        add_metric "memory/idle_rss" "$idle_rss" "bytes"
    else
        # macOS fallback using ps
        local idle_rss_kb
        idle_rss_kb=$(ps -o rss= -p $pid 2>/dev/null || echo "0")
        local idle_rss=$((idle_rss_kb * 1024))
        add_metric "memory/idle_rss" "$idle_rss" "bytes"
    fi
    
    # M-CPU-001: Idle CPU (sample over 1 second)
    if [[ "$OSTYPE" != "darwin"* ]]; then
        local cpu_before
        cpu_before=$(cat /proc/$pid/stat 2>/dev/null | awk '{print $14 + $15}' || echo "0")
        sleep 1
        local cpu_after
        cpu_after=$(cat /proc/$pid/stat 2>/dev/null | awk '{print $14 + $15}' || echo "0")
        local idle_cpu_ticks=$((cpu_after - cpu_before))
        local idle_cpu_percent
        idle_cpu_percent=$(echo "scale=2; $idle_cpu_ticks / 100" | bc)
        add_metric "cpu/idle_percent" "$idle_cpu_percent" "%"
    fi
    
    # Cleanup
    kill $pid 2>/dev/null || true
    wait $pid 2>/dev/null || true
    trap - EXIT
    
    log_success "Startup metrics collected"
}

# ─────────────────────────────────────────────────────────────────────────────
# Parse Criterion JSON output
# ─────────────────────────────────────────────────────────────────────────────

parse_criterion_results() {
    local criterion_dir="$PROJECT_ROOT/target/criterion"
    
    if [ ! -d "$criterion_dir" ]; then
        log_info "No Criterion results found at $criterion_dir"
        return 0
    fi
    
    log_info "Parsing Criterion benchmark results..."
    
    # Find and parse estimates.json files
    # Use process substitution to avoid subshell scope issue with METRICS array
    while read -r file; do
        # Extract benchmark name from path
        local rel_path="${file#$criterion_dir/}"
        local bench_name
        bench_name=$(echo "$rel_path" | cut -d'/' -f1-2 | tr '/' '_')
        
        # Parse mean value (in nanoseconds)
        local mean_ns
        mean_ns=$(python3 -c "import json; f=open('$file'); d=json.load(f); print(d['mean']['point_estimate'])" 2>/dev/null || echo "")
        
        if [ -n "$mean_ns" ]; then
            # Convert to microseconds for policy benchmarks
            if [[ "$bench_name" == *"policy"* ]]; then
                local mean_us
                mean_us=$(echo "scale=3; $mean_ns / 1000" | bc)
                add_metric "policy/eval_mean" "$mean_us" "µs"
            else
                # Keep TTFB in milliseconds
                local mean_ms
                mean_ms=$(echo "scale=3; $mean_ns / 1000000" | bc)
                add_metric "ttfb/${bench_name}" "$mean_ms" "ms"
            fi
        fi
    done < <(find "$criterion_dir" -name "estimates.json" -type f)
}

# ─────────────────────────────────────────────────────────────────────────────
# Parse k6 NDJSON output for latency metrics
# ─────────────────────────────────────────────────────────────────────────────

parse_k6_results() {
    local k6_results="$1"
    
    if [ ! -f "$k6_results" ]; then
        log_info "No k6 results found at $k6_results"
        return 0
    fi
    
    log_info "Parsing k6 results from $k6_results..."
    
    python3 << PYEOF
import json
import sys

def calculate_percentile(values, p):
    """Calculate percentile from sorted values."""
    if not values:
        return 0
    values = sorted(v for v in values if v == v)  # Filter NaN
    idx = int(len(values) * p / 100)
    idx = min(idx, len(values) - 1)
    return values[idx]

waiting_values = []
duration_values = []
request_count = 0
total_time_s = 0

with open("$k6_results", "r") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            data = json.loads(line)
            if data.get("type") == "Point":
                metric = data.get("metric", "")
                value = data.get("data", {}).get("value")
                if value is not None:
                    if metric == "http_req_waiting":
                        waiting_values.append(value)
                    elif metric == "http_req_duration":
                        duration_values.append(value)
                    elif metric == "http_reqs":
                        request_count += 1
        except json.JSONDecodeError:
            continue

# Calculate percentiles
if duration_values:
    p50 = calculate_percentile(duration_values, 50)
    p95 = calculate_percentile(duration_values, 95)
    p99 = calculate_percentile(duration_values, 99)
    print(f"latency/p50:{p50:.3f}")
    print(f"latency/p95:{p95:.3f}")
    print(f"latency/p99:{p99:.3f}")

if waiting_values:
    ttfb_p95 = calculate_percentile(waiting_values, 95)
    print(f"ttfb/p95:{ttfb_p95:.3f}")

# Throughput (requests per second)
if request_count > 0 and len(duration_values) > 0:
    # Approximate: total duration of all requests / count
    # This is a rough estimate; k6 summary has better numbers
    print(f"http_reqs:{request_count}")
PYEOF
}

# ─────────────────────────────────────────────────────────────────────────────
# Docker Image Metrics (M-BIN-002 to M-BIN-005)
# ─────────────────────────────────────────────────────────────────────────────

collect_docker_metrics() {
    local image_name="${1:-thoughtgate:test}"
    
    log_info "Collecting Docker image metrics for $image_name..."
    
    # Check if image exists
    if ! docker image inspect "$image_name" > /dev/null 2>&1; then
        log_info "Docker image $image_name not found, skipping Docker metrics"
        return 0
    fi
    
    # M-BIN-002: Compressed image size (from registry manifest)
    # Note: This requires the image to be pushed to a registry
    # For local images, we use the uncompressed size
    local image_size
    image_size=$(docker image inspect "$image_name" --format='{{.Size}}' 2>/dev/null || echo "0")
    if [ "$image_size" != "0" ]; then
        add_metric "binary/docker_image_size" "$image_size" "bytes"
    fi
    
    # M-BIN-003: Layer count
    local layer_count
    layer_count=$(docker image inspect "$image_name" --format='{{len .RootFS.Layers}}' 2>/dev/null || echo "0")
    if [ "$layer_count" != "0" ]; then
        add_metric "binary/docker_layers" "$layer_count" "count"
    fi
    
    # M-BIN-004: Check base image (simplified - check if FROM scratch)
    # We check the history for minimal layers
    local history_count
    history_count=$(docker history "$image_name" --no-trunc -q 2>/dev/null | wc -l || echo "0")
    log_info "Docker image has $history_count history entries"
    
    log_success "Docker metrics collected"
}

# ─────────────────────────────────────────────────────────────────────────────
# Overhead Comparison (M-OH-001, M-OH-002)
# ─────────────────────────────────────────────────────────────────────────────

collect_overhead_metrics() {
    local direct_results="$1"
    local proxy_results="$2"
    
    if [ ! -f "$direct_results" ] || [ ! -f "$proxy_results" ]; then
        log_info "Overhead comparison requires both direct and proxy results"
        return 0
    fi
    
    log_info "Calculating proxy overhead..."
    
    python3 << PYEOF
import json

def parse_k6_p50(filename):
    """Parse k6 results and return p50 latency."""
    values = []
    with open(filename, "r") as f:
        for line in f:
            try:
                data = json.loads(line.strip())
                if data.get("type") == "Point" and data.get("metric") == "http_req_duration":
                    value = data.get("data", {}).get("value")
                    if value is not None:
                        values.append(value)
            except:
                continue
    if not values:
        return None
    values.sort()
    idx = len(values) // 2
    return values[idx]

direct_p50 = parse_k6_p50("$direct_results")
proxy_p50 = parse_k6_p50("$proxy_results")

if direct_p50 and proxy_p50:
    overhead_ms = proxy_p50 - direct_p50
    overhead_percent = ((proxy_p50 - direct_p50) / direct_p50) * 100 if direct_p50 > 0 else 0
    print(f"overhead/latency_p50:{overhead_ms:.3f}")
    print(f"overhead/percent_p50:{overhead_percent:.2f}")
else:
    print("Could not calculate overhead", file=__import__('sys').stderr)
PYEOF
}

# ─────────────────────────────────────────────────────────────────────────────
# Main
# ─────────────────────────────────────────────────────────────────────────────

main() {
    local mode="all"
    local k6_results=""
    local docker_image=""
    local direct_results=""
    local proxy_results=""
    
    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            --binary-only)
                mode="binary"
                shift
                ;;
            --startup-only)
                mode="startup"
                shift
                ;;
            --all)
                mode="all"
                shift
                ;;
            --output)
                OUTPUT_FILE="$2"
                shift 2
                ;;
            --k6-results)
                k6_results="$2"
                shift 2
                ;;
            --docker-image)
                docker_image="$2"
                shift 2
                ;;
            --direct-results)
                direct_results="$2"
                shift 2
                ;;
            --proxy-results)
                proxy_results="$2"
                shift 2
                ;;
            *)
                log_error "Unknown option: $1"
                exit 1
                ;;
        esac
    done
    
    log_info "ThoughtGate Performance Metrics Collection"
    log_info "==========================================="
    log_info "Mode: $mode"
    log_info "Output: $OUTPUT_FILE"
    
    case $mode in
        binary)
            collect_binary_size
            ;;
        startup)
            collect_startup_metrics
            ;;
        all)
            collect_binary_size || true
            collect_startup_metrics || true
            parse_criterion_results || true
            
            if [ -n "$k6_results" ]; then
                parse_k6_results "$k6_results" || true
            fi
            
            if [ -n "$docker_image" ]; then
                collect_docker_metrics "$docker_image" || true
            fi
            
            if [ -n "$direct_results" ] && [ -n "$proxy_results" ]; then
                collect_overhead_metrics "$direct_results" "$proxy_results" || true
            fi
            ;;
    esac
    
    # Output metrics
    log_info ""
    log_info "Writing metrics to $OUTPUT_FILE"
    metrics_to_json > "$OUTPUT_FILE"
    
    log_success "Metrics collection complete!"
    log_info "Collected ${#METRICS[@]} metrics"
    
    # Print summary
    cat "$OUTPUT_FILE"
}

main "$@"
