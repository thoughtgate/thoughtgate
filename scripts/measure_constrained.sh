#!/bin/bash
# measure_constrained.sh - Benchmark under sidecar-like resource constraints
#
# Implements: REQ-OBS-001 M-TP-002, M-MEM-004 (Constrained Resource Benchmarks)
#
# This script runs ThoughtGate under typical sidecar resource limits:
# - CPU: 0.5 cores (500m in K8s terms)
# - Memory: 128MB
#
# This validates marketing claims in production-like conditions.
#
# Usage:
#   ./scripts/measure_constrained.sh [--output FILE]
#
# Prerequisites:
# - Docker with resource limits support
# - k6 load testing tool
# - ThoughtGate Docker image (thoughtgate:test)

set -euo pipefail

# ─────────────────────────────────────────────────────────────────────────────
# Configuration
# ─────────────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

OUTPUT_FILE="${PROJECT_ROOT}/constrained_metrics.json"
DOCKER_IMAGE="${DOCKER_IMAGE:-thoughtgate:test}"

# Resource constraints (sidecar-realistic)
CPU_LIMIT="0.5"
MEMORY_LIMIT="128m"

# Container names
PROXY_CONTAINER="tg-constrained-proxy"
MOCK_CONTAINER="tg-constrained-mock"

# Ports
MOCK_LLM_PORT=8888

# ThoughtGate v0.2 uses 3-port Envoy-style model inside container:
# - Outbound port (7467): Main proxy for client requests
# - Admin port (7469): Health checks (/health, /ready)
# Map container ports to host ports for testing
CONTAINER_OUTBOUND_PORT=7467
CONTAINER_ADMIN_PORT=7469
HOST_PROXY_PORT=4141
HOST_ADMIN_PORT=4143

# k6 configuration
K6_VUS=10
K6_DURATION="30s"

# Cleanup function
cleanup() {
    echo "🧹 Cleaning up containers..."
    docker stop "$PROXY_CONTAINER" 2>/dev/null || true
    docker rm "$PROXY_CONTAINER" 2>/dev/null || true
    docker stop "$MOCK_CONTAINER" 2>/dev/null || true
    docker rm "$MOCK_CONTAINER" 2>/dev/null || true
    rm -f constrained_results.json 2>/dev/null || true
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

wait_for_container_port() {
    local container=$1
    local port=$2
    local max_wait=60
    local waited=0
    
    while ! docker exec "$container" sh -c "nc -z localhost $port" 2>/dev/null; do
        sleep 1
        waited=$((waited + 1))
        if [ $waited -gt $max_wait ]; then
            log_error "Timeout waiting for container $container port $port"
            docker logs "$container" 2>&1 | tail -20
            return 1
        fi
    done
    log_info "Container $container port $port is ready"
}

wait_for_host_port() {
    local port=$1
    local max_wait=60
    local waited=0
    
    while ! nc -z localhost "$port" 2>/dev/null; do
        sleep 1
        waited=$((waited + 1))
        if [ $waited -gt $max_wait ]; then
            log_error "Timeout waiting for port $port on host"
            return 1
        fi
    done
    log_info "Host port $port is ready"
}

# ─────────────────────────────────────────────────────────────────────────────
# Check Prerequisites
# ─────────────────────────────────────────────────────────────────────────────

check_prerequisites() {
    log_info "Checking prerequisites..."
    
    if ! command -v docker &> /dev/null; then
        log_error "Docker is not installed"
        exit 1
    fi
    
    if ! command -v k6 &> /dev/null; then
        log_error "k6 is not installed. Install with: brew install k6"
        exit 1
    fi
    
    # Check if Docker image exists
    if ! docker image inspect "$DOCKER_IMAGE" &> /dev/null; then
        log_error "Docker image '$DOCKER_IMAGE' not found"
        log_info "Build with: docker build -t $DOCKER_IMAGE ."
        exit 1
    fi
    
    log_success "All prerequisites satisfied"
}

# ─────────────────────────────────────────────────────────────────────────────
# Start Mock LLM Container
# ─────────────────────────────────────────────────────────────────────────────

start_mock_llm() {
    log_info "Starting mock LLM container..."
    
    docker run -d \
        --name "$MOCK_CONTAINER" \
        -p "${MOCK_LLM_PORT}:8080" \
        -e MOCK_LLM_PORT=8080 \
        --entrypoint /entrypoint.sh \
        "$DOCKER_IMAGE" \
        /mock_llm
    
    wait_for_host_port $MOCK_LLM_PORT
    log_success "Mock LLM container started"
}

# ─────────────────────────────────────────────────────────────────────────────
# Start Constrained Proxy Container
# ─────────────────────────────────────────────────────────────────────────────

start_constrained_proxy() {
    log_info "Starting ThoughtGate proxy with constraints (CPU: ${CPU_LIMIT}, Memory: ${MEMORY_LIMIT})..."
    
    # Get the host IP that containers can reach
    # On Docker Desktop, host.docker.internal works
    # On Linux, we need the bridge gateway
    local host_ip
    if [[ "$OSTYPE" == "darwin"* ]]; then
        host_ip="host.docker.internal"
    else
        host_ip=$(docker network inspect bridge --format='{{range .IPAM.Config}}{{.Gateway}}{{end}}')
    fi
    
    # ThoughtGate v0.2 uses 3-port model - map outbound and admin ports
    docker run -d \
        --name "$PROXY_CONTAINER" \
        --cpus "$CPU_LIMIT" \
        --memory "$MEMORY_LIMIT" \
        -p "${HOST_PROXY_PORT}:${CONTAINER_OUTBOUND_PORT}" \
        -p "${HOST_ADMIN_PORT}:${CONTAINER_ADMIN_PORT}" \
        -e THOUGHTGATE_UPSTREAM_URL="http://${host_ip}:${MOCK_LLM_PORT}" \
        "$DOCKER_IMAGE"

    # Wait for admin port (health endpoint) to be ready
    wait_for_host_port $HOST_ADMIN_PORT
    log_success "Constrained proxy container started"
    
    # Show container resource limits
    docker inspect "$PROXY_CONTAINER" --format='{{.HostConfig.NanoCpus}} nano-cpus, {{.HostConfig.Memory}} bytes memory' | \
        awk '{printf "  Limits: %.2f CPUs, %.0f MB memory\n", $1/1e9, $3/1048576}'
}

# ─────────────────────────────────────────────────────────────────────────────
# Collect Memory Metrics
# ─────────────────────────────────────────────────────────────────────────────

collect_memory_metrics() {
    local phase=$1
    
    # Get container stats
    local stats
    stats=$(docker stats "$PROXY_CONTAINER" --no-stream --format '{{.MemUsage}}' 2>/dev/null || echo "0MiB / 0MiB")
    
    # Parse memory usage
    local mem_used
    mem_used=$(echo "$stats" | sed 's/MiB.*//' | sed 's/GiB.*//' | tr -d ' ')
    
    # Handle GiB vs MiB
    if [[ "$stats" == *"GiB"* ]]; then
        mem_used=$(echo "$mem_used * 1024" | bc)
    fi
    
    log_info "Memory ($phase): ${mem_used}MiB"
    echo "$mem_used"
}

# ─────────────────────────────────────────────────────────────────────────────
# Run k6 Load Test
# ─────────────────────────────────────────────────────────────────────────────

run_k6_test() {
    log_info "Running k6 load test through constrained proxy..."
    
    # Create temporary k6 script
    local temp_script
    temp_script=$(mktemp)
    
    cat > "$temp_script" << EOF
import http from 'k6/http';
import { check, sleep } from 'k6';

export const options = {
  vus: ${K6_VUS},
  duration: '${K6_DURATION}',
  thresholds: {
    http_req_duration: ['p(95)<1000'],  // 1 second under load
  },
};

const PAYLOAD = JSON.stringify({ prompt: "A".repeat(1000) });

export default function () {
  const res = http.post('http://127.0.0.1:${HOST_PROXY_PORT}/v1/chat/completions', PAYLOAD, {
    headers: { 'Content-Type': 'application/json' },
    timeout: '10s',
  });
  
  check(res, {
    'status is 200': (r) => r.status === 200,
  });
}
EOF
    
    k6 run --out json=constrained_results.json "$temp_script" 2>&1 | grep -E '(http_req|iterations|vus)' || true
    
    rm -f "$temp_script"
    log_success "k6 load test completed"
}

# ─────────────────────────────────────────────────────────────────────────────
# Calculate Metrics
# ─────────────────────────────────────────────────────────────────────────────

calculate_metrics() {
    local idle_mem=$1
    local mem_under_load=$2
    
    log_info "Calculating constrained performance metrics..."
    
    python3 << PYEOF
import json
import sys

def parse_k6_results(filename):
    """Parse k6 JSON output."""
    durations = []
    request_count = 0
    
    with open(filename, 'r') as f:
        for line in f:
            try:
                data = json.loads(line.strip())
                if data.get('type') == 'Point':
                    metric = data.get('metric')
                    value = data.get('data', {}).get('value')
                    if metric == 'http_req_duration' and value is not None:
                        durations.append(value)
                    elif metric == 'http_reqs':
                        request_count += 1
            except json.JSONDecodeError:
                continue
    
    if not durations:
        return None
    
    durations.sort()
    n = len(durations)
    
    # Calculate test duration (approximate from data points)
    # k6 logs points every 100ms, so count / 10 ≈ seconds
    test_duration_s = max(1, len(durations) / 10)
    rps = request_count / test_duration_s if test_duration_s > 0 else 0
    
    return {
        'p50': durations[n // 2],
        'p95': durations[int(n * 0.95)],
        'p99': durations[int(n * 0.99)] if n >= 100 else durations[-1],
        'count': request_count,
        'rps': rps,
    }

# Parse results
results = parse_k6_results('constrained_results.json')

if not results:
    print("Error: Could not parse k6 results", file=sys.stderr)
    sys.exit(1)

print(f"Constrained Performance:")
print(f"  Latency: p50={results['p50']:.2f}ms, p95={results['p95']:.2f}ms, p99={results['p99']:.2f}ms")
print(f"  Requests: {results['count']} total, ~{results['rps']:.0f} RPS")
print(f"  Memory: ${idle_mem}MB idle, ${mem_under_load}MB under load")

# Generate metrics
idle_mem_bytes = int(float('$idle_mem') * 1024 * 1024)
load_mem_bytes = int(float('$mem_under_load') * 1024 * 1024)
metrics = [
    {"name": "throughput/rps_constrained", "value": round(results['rps'], 0), "unit": "req/s"},
    {"name": "memory/idle_rss", "value": idle_mem_bytes, "unit": "bytes"},
    {"name": "memory/constrained_rss", "value": load_mem_bytes, "unit": "bytes"},
    {"name": "latency/constrained_p50", "value": round(results['p50'], 3), "unit": "ms"},
    {"name": "latency/constrained_p95", "value": round(results['p95'], 3), "unit": "ms"},
    {"name": "latency/constrained_p99", "value": round(results['p99'], 3), "unit": "ms"},
]

# Check targets
targets_met = True

# M-TP-002: > 5,000 RPS under constraints
if results['rps'] < 5000:
    print(f"⚠️  WARNING: Constrained RPS ({results['rps']:.0f}) below target (> 5000)")
    targets_met = False

# M-MEM-004: < 100 MB under constraints
if float('$mem_under_load') > 100:
    print(f"⚠️  WARNING: Constrained memory ({float('$mem_under_load'):.0f}MB) exceeds target (< 100MB)")
    targets_met = False

if targets_met:
    print("✅ All constrained resource targets met!")

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
            --image)
                DOCKER_IMAGE="$2"
                shift 2
                ;;
            *)
                log_error "Unknown option: $1"
                exit 1
                ;;
        esac
    done
    
    log_info "ThoughtGate Constrained Resource Benchmark"
    log_info "==========================================="
    log_info "CPU limit: ${CPU_LIMIT} cores"
    log_info "Memory limit: ${MEMORY_LIMIT}"
    log_info ""
    
    check_prerequisites
    
    # Start containers
    start_mock_llm
    start_constrained_proxy
    
    # Collect idle memory
    log_info "Collecting idle memory metrics..."
    sleep 2
    idle_mem=$(collect_memory_metrics "idle")
    
    # Run load test
    run_k6_test
    
    # Collect memory under load
    load_mem=$(collect_memory_metrics "under_load")
    
    # Calculate metrics (pass both idle and load memory)
    calculate_metrics "$idle_mem" "$load_mem"
    
    log_success "Constrained resource benchmark complete!"
}

main "$@"
