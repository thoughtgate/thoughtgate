---
sidebar_position: 6
---

# Monitor ThoughtGate

This guide explains how to monitor ThoughtGate in production environments.

## Health Endpoints

ThoughtGate exposes health endpoints on the admin port (default: 7469):

| Endpoint | Purpose | Kubernetes Use |
|----------|---------|----------------|
| `GET /health` | Liveness probe | Restart if unhealthy |
| `GET /ready` | Readiness probe | Remove from service if not ready |
| `GET /metrics` | Prometheus metrics | Scrape for monitoring |

### Kubernetes Probe Configuration

```yaml
apiVersion: v1
kind: Pod
spec:
  containers:
    - name: thoughtgate
      livenessProbe:
        httpGet:
          path: /health
          port: 7469
        initialDelaySeconds: 5
        periodSeconds: 10
        failureThreshold: 3
      readinessProbe:
        httpGet:
          path: /ready
          port: 7469
        initialDelaySeconds: 5
        periodSeconds: 5
        failureThreshold: 2
```

### Health vs Ready

- **`/health`** — Returns 200 if the process is alive. Use for liveness probes.
- **`/ready`** — Returns 200 only when:
  - Configuration is loaded
  - Upstream is reachable
  - Task store is initialized

If `/ready` returns 503, ThoughtGate won't receive traffic until it becomes ready.

## Prometheus Metrics

ThoughtGate exposes Prometheus-format metrics on `GET /metrics` (port 7469).

### Request Metrics

```
# Request counts by method, tool, and status
thoughtgate_requests_total{method="tools/call", tool_name="delete_user", status="forward"}
thoughtgate_requests_total{method="tools/call", tool_name="admin_reset", status="deny"}

# Request latency histogram (milliseconds)
thoughtgate_request_duration_ms{method="tools/call", tool_name="read_file"}
```

### Gate Decision Metrics

```
# Gate evaluation decisions
thoughtgate_gate_decisions_total{gate="visibility", outcome="allow"}
thoughtgate_gate_decisions_total{gate="governance", outcome="deny"}
thoughtgate_gate_decisions_total{gate="policy", outcome="forward"}
```

### Approval Metrics

```
# Approval workflow outcomes
thoughtgate_approval_requests_total{channel="#approvals", outcome="approved"}
thoughtgate_approval_requests_total{channel="#approvals", outcome="rejected"}
thoughtgate_approval_requests_total{channel="#approvals", outcome="timeout"}

# Tasks currently pending
thoughtgate_tasks_pending{task_type="approval"}
```

### Upstream Metrics

```
# Upstream request outcomes
thoughtgate_upstream_requests_total{target="mcp-server:3000", status_code="200"}
thoughtgate_upstream_requests_total{target="mcp-server:3000", status_code="500"}
```

### Stdio Transport Metrics (CLI Wrapper)

```
# Stdio messages by server, direction, and method
thoughtgate_stdio_messages_total{server_id="filesystem", direction="agent_to_server", method="tools/call"}
thoughtgate_stdio_messages_total{server_id="filesystem", direction="server_to_agent", method="tools/call"}

# Stdio governance decisions
thoughtgate_stdio_governance_decisions_total{server_id="filesystem", decision="forward", profile="production"}
thoughtgate_stdio_governance_decisions_total{server_id="filesystem", decision="deny", profile="production"}

# Stdio framing/parse errors
thoughtgate_stdio_framing_errors_total{server_id="filesystem", error_type="smuggling_detected"}
thoughtgate_stdio_framing_errors_total{server_id="filesystem", error_type="invalid_json"}

# Active stdio server connections
thoughtgate_stdio_active_servers
```

### Traffic Path Metrics

```
# Green path bytes transferred
thoughtgate_green_bytes_total{direction="upload"}
thoughtgate_green_bytes_total{direction="download"}

# Active green path streams
thoughtgate_green_streams_active

# Green path time to first byte
thoughtgate_green_ttfb_ms
```

See [Telemetry Reference](/docs/reference/telemetry) for the complete metrics inventory (37 counters, histograms, and gauges).

## Prometheus Scrape Configuration

Add ThoughtGate to your Prometheus scrape config:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'thoughtgate'
    kubernetes_sd_configs:
      - role: pod
    relabel_configs:
      - source_labels: [__meta_kubernetes_pod_container_name]
        regex: thoughtgate
        action: keep
      - source_labels: [__meta_kubernetes_pod_container_port_name]
        regex: admin
        action: keep
```

Or for static targets:

```yaml
scrape_configs:
  - job_name: 'thoughtgate'
    static_configs:
      - targets: ['localhost:7469']
```

## Grafana Dashboard

### Key Panels

1. **Request Rate by Status**
   ```promql
   sum(rate(thoughtgate_requests_total[5m])) by (status)
   ```

2. **Request Latency (p95)**
   ```promql
   histogram_quantile(0.95, rate(thoughtgate_request_duration_ms_bucket[5m]))
   ```

3. **Approval Wait Time**
   ```promql
   histogram_quantile(0.95, rate(thoughtgate_approval_duration_seconds_bucket[5m]))
   ```

4. **Pending Tasks**
   ```promql
   thoughtgate_tasks_pending
   ```

5. **Upstream Error Rate**
   ```promql
   sum(rate(thoughtgate_upstream_requests_total{status_code=~"5.."}[5m])) /
   sum(rate(thoughtgate_upstream_requests_total[5m]))
   ```

6. **Stdio Governance Decisions (CLI Wrapper)**
   ```promql
   sum(rate(thoughtgate_stdio_governance_decisions_total[5m])) by (decision, server_id)
   ```

### Recommended Alerts

```yaml
# alerting_rules.yml
groups:
  - name: thoughtgate
    rules:
      # High denial rate
      - alert: ThoughtGateHighDenialRate
        expr: |
          sum(rate(thoughtgate_requests_total{status="deny"}[5m])) /
          sum(rate(thoughtgate_requests_total[5m])) > 0.1
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High denial rate ({{ $value | humanizePercentage }})"

      # Upstream errors
      - alert: ThoughtGateUpstreamErrors
        expr: |
          sum(rate(thoughtgate_upstream_requests_total{status_code=~"5.."}[5m])) > 0
        for: 2m
        labels:
          severity: critical
        annotations:
          summary: "Upstream connection errors"

      # Approval timeouts
      - alert: ThoughtGateApprovalTimeouts
        expr: |
          sum(rate(thoughtgate_approval_requests_total{outcome="timeout"}[5m])) > 0
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Approvals timing out"

      # Not ready
      - alert: ThoughtGateNotReady
        expr: up{job="thoughtgate"} == 1 and thoughtgate_config_loaded == 0
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "ThoughtGate not ready to serve traffic"
```

## Logging

ThoughtGate uses structured JSON logging via `tracing`:

```json
{
  "timestamp": "2025-01-25T10:30:00.000Z",
  "level": "INFO",
  "target": "thoughtgate::governance",
  "message": "Request evaluated",
  "fields": {
    "method": "tools/call",
    "tool_name": "delete_user",
    "action": "approve",
    "task_id": "tg_abc123"
  }
}
```

### Log Levels

| Level | Use |
|-------|-----|
| `ERROR` | Failures requiring attention |
| `WARN` | Degraded operation |
| `INFO` | Normal operation events |
| `DEBUG` | Detailed flow for troubleshooting |
| `TRACE` | Very verbose (development only) |

Configure via `RUST_LOG`:

```bash
# Production
export RUST_LOG=thoughtgate=info

# Troubleshooting
export RUST_LOG=thoughtgate=debug

# Full trace (verbose)
export RUST_LOG=thoughtgate=trace
```

### Sensitive Data

ThoughtGate **never logs**:
- Tool arguments (may contain secrets)
- Authorization headers
- Slack bot tokens
- API keys

## Distributed Tracing

ThoughtGate supports OpenTelemetry tracing. Enable it and configure the OTLP endpoint:

```bash
export THOUGHTGATE_TELEMETRY_ENABLED=true
export OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4318
```

:::note
Only `http/protobuf` protocol is supported. gRPC endpoints (port 4317) are **not** supported — use the HTTP endpoint (port 4318).
:::

Each request gets a trace ID that flows through:
1. Agent → ThoughtGate
2. ThoughtGate → Upstream
3. ThoughtGate → Slack (for approvals)

In stdio mode, trace context is propagated via `params._meta.traceparent` fields in JSON-RPC messages. See [Telemetry Reference](/docs/reference/telemetry) for details.

## CLI Wrapper Monitoring

When using `thoughtgate wrap`, the governance service runs on an ephemeral port on localhost. To monitor:

- **Governance logs** — All decisions are logged to stderr with structured JSON
- **Verbose mode** — Use `--verbose` for debug-level logging
- **Stdio metrics** — Monitor `thoughtgate_stdio_*` metrics for per-server activity

## Troubleshooting with Metrics

### "Requests are slow"

Check latency breakdown:

```promql
# Overall latency
histogram_quantile(0.95, rate(thoughtgate_request_duration_ms_bucket[5m]))

# Is it upstream?
histogram_quantile(0.95, rate(thoughtgate_upstream_duration_ms_bucket[5m]))

# Is it policy evaluation?
histogram_quantile(0.95, rate(thoughtgate_cedar_eval_duration_ms_bucket[5m]))
```

### "Approvals are getting stuck"

Check task metrics:

```promql
# Pending tasks (should not grow unbounded)
thoughtgate_tasks_pending

# Timeout rate
rate(thoughtgate_approval_requests_total{outcome="timeout"}[5m])
```

### "High memory usage"

Check active connections:

```promql
# Active streams
thoughtgate_green_streams_active

# Active stdio servers
thoughtgate_stdio_active_servers
```

## Resource Recommendations

For sidecar deployment:

| Resource | Request | Limit |
|----------|---------|-------|
| CPU | 50m | 200m |
| Memory | 50Mi | 100Mi |

ThoughtGate is designed to be lightweight:
- Idle memory: < 20 MB
- Per-request overhead: < 2 ms
- Cold start: < 100 ms

## Next Steps

- [Deploy to Kubernetes](/docs/how-to/deploy-kubernetes) — Full deployment guide
- [Troubleshoot Common Issues](/docs/how-to/troubleshoot) — Problem-solving guide
- [Telemetry Reference](/docs/reference/telemetry) — Complete metrics inventory
- [Architecture](/docs/explanation/architecture) — Understand the 4-Gate model
