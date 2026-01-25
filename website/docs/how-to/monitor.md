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
# Request counts by action
thoughtgate_requests_total{action="forward"}
thoughtgate_requests_total{action="deny"}
thoughtgate_requests_total{action="approve"}

# Request latency histogram
thoughtgate_request_duration_seconds{quantile="0.5"}
thoughtgate_request_duration_seconds{quantile="0.95"}
thoughtgate_request_duration_seconds{quantile="0.99"}
```

### Approval Metrics

```
# Approval outcomes
thoughtgate_approval_total{result="approved"}
thoughtgate_approval_total{result="rejected"}
thoughtgate_approval_total{result="timeout"}

# Active tasks waiting for approval
thoughtgate_tasks_active
```

### Upstream Metrics

```
# Upstream request outcomes
thoughtgate_upstream_requests_total{status="success"}
thoughtgate_upstream_requests_total{status="error"}
thoughtgate_upstream_requests_total{status="timeout"}
```

### Traffic Path Metrics

```
# Bytes transferred
green_path_bytes_total{direction="upload"}
green_path_bytes_total{direction="download"}

# Active streams
green_path_streams_active

# Time to first byte
green_path_ttfb_seconds
```

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

1. **Request Rate by Action**
   ```promql
   sum(rate(thoughtgate_requests_total[5m])) by (action)
   ```

2. **Request Latency (p95)**
   ```promql
   histogram_quantile(0.95, rate(thoughtgate_request_duration_seconds_bucket[5m]))
   ```

3. **Approval Wait Time**
   ```promql
   histogram_quantile(0.95, rate(thoughtgate_approval_duration_seconds_bucket[5m]))
   ```

4. **Active Tasks**
   ```promql
   thoughtgate_tasks_active
   ```

5. **Upstream Error Rate**
   ```promql
   sum(rate(thoughtgate_upstream_requests_total{status="error"}[5m])) /
   sum(rate(thoughtgate_upstream_requests_total[5m]))
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
          sum(rate(thoughtgate_requests_total{action="deny"}[5m])) /
          sum(rate(thoughtgate_requests_total[5m])) > 0.1
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High denial rate ({{ $value | humanizePercentage }})"

      # Upstream errors
      - alert: ThoughtGateUpstreamErrors
        expr: |
          sum(rate(thoughtgate_upstream_requests_total{status="error"}[5m])) > 0
        for: 2m
        labels:
          severity: critical
        annotations:
          summary: "Upstream connection errors"

      # Approval timeouts
      - alert: ThoughtGateApprovalTimeouts
        expr: |
          sum(rate(thoughtgate_approval_total{result="timeout"}[5m])) > 0
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Approvals timing out"

      # Not ready
      - alert: ThoughtGateNotReady
        expr: up{job="thoughtgate"} == 1 and thoughtgate_ready == 0
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

ThoughtGate supports OpenTelemetry tracing (v0.3+). Configure the OTLP endpoint:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=http://jaeger:4317
```

Each request gets a trace ID that flows through:
1. Agent → ThoughtGate
2. ThoughtGate → Upstream
3. ThoughtGate → Slack (for approvals)

## Troubleshooting with Metrics

### "Requests are slow"

Check latency breakdown:

```promql
# Overall latency
histogram_quantile(0.95, rate(thoughtgate_request_duration_seconds_bucket[5m]))

# Is it upstream?
histogram_quantile(0.95, rate(thoughtgate_upstream_duration_seconds_bucket[5m]))

# Is it policy evaluation?
histogram_quantile(0.95, rate(thoughtgate_policy_eval_seconds_bucket[5m]))
```

### "Approvals are getting stuck"

Check task metrics:

```promql
# Active tasks (should not grow unbounded)
thoughtgate_tasks_active

# Timeout rate
rate(thoughtgate_approval_total{result="timeout"}[5m])
```

### "High memory usage"

Check active connections:

```promql
# Active streams
green_path_streams_active

# Buffer usage (for approve path)
thoughtgate_buffer_bytes_total
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
- [Architecture](/docs/explanation/architecture) — Understand the 4-Gate model
