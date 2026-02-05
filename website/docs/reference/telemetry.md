---
sidebar_position: 6
---

# Telemetry Reference

Complete reference for ThoughtGate telemetry: OTLP tracing, Prometheus metrics, and configuration.

## YAML Configuration

Telemetry is configured in the `telemetry` section of `thoughtgate.yaml`:

```yaml
telemetry:
  # Master enable/disable for OTLP trace export (default: false)
  enabled: true

  otlp:
    # OTLP HTTP endpoint (only http/protobuf supported, no gRPC)
    endpoint: "http://otel-collector:4318"
    protocol: http/protobuf  # Only supported protocol

  prometheus:
    enabled: true  # Enable /metrics endpoint (default: true)

  sampling:
    strategy: head  # Only "head" sampling supported (no tail sampling)
    success_sample_rate: 0.10  # Sample 10% of successful requests (0.0-1.0)

  batch:
    max_queue_size: 2048       # Max spans in export queue (default: 2048)
    max_export_batch_size: 512 # Max spans per export batch (default: 512)
    scheduled_delay_ms: 5000   # Delay between exports in ms (default: 5000)
    export_timeout_ms: 30000   # Export timeout in ms (default: 30000)

  resource:
    service.name: my-thoughtgate        # OTLP service name
    deployment.environment: staging     # Custom resource attribute

  stdio:
    # Preserve trace context when forwarding to upstream MCP servers.
    # Default: false (strip _meta.traceparent/tracestate to avoid breaking strict servers)
    propagate_upstream: false
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTGATE_TELEMETRY_ENABLED` | `false` | Master switch for OTLP trace export. Overrides `telemetry.enabled` in YAML. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | — | OTLP HTTP endpoint URL. Overrides `telemetry.otlp.endpoint`. |
| `OTEL_SERVICE_NAME` | `thoughtgate` | Service name for OTLP resource. Overrides `telemetry.resource.service.name`. |
| `K8S_NAMESPACE` | — | Kubernetes namespace (added as resource attribute). |
| `K8S_POD_NAME` | — | Kubernetes pod name (added as resource attribute). |
| `K8S_NODE_NAME` | — | Kubernetes node name (added as resource attribute). |
| `DEPLOY_ENV` | — | Deployment environment (added as resource attribute). |

## W3C Trace Context Propagation

ThoughtGate propagates W3C Trace Context across both transport modes.

### HTTP Mode (Sidecar)

Standard HTTP header propagation:

| Header | Description |
|--------|-------------|
| `traceparent` | W3C Trace Context trace-parent header |
| `tracestate` | W3C Trace Context trace-state header |
| `baggage` | W3C Baggage header |

Incoming `traceparent` headers are parsed and used as the parent span. If absent, ThoughtGate generates a new trace ID.

### Stdio Mode (CLI Wrapper)

Since stdio transport has no HTTP headers, trace context is embedded in JSON-RPC `params._meta`:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "read_file",
    "arguments": { "path": "/tmp/test.txt" },
    "_meta": {
      "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
      "tracestate": "congo=t61rcWkgMzE",
      "baggage": "userId=alice"
    }
  }
}
```

### `propagate_upstream` Behavior

| Setting | Behavior |
|---------|----------|
| `false` (default) | Strip `_meta.traceparent`, `_meta.tracestate`, and `_meta.baggage` before forwarding to upstream MCP servers. Prevents breaking strict schema validators. |
| `true` | Preserve trace context fields when forwarding. Use when upstream servers are trace-aware. |

When `propagate_upstream: false`, ThoughtGate still records the extracted trace context in its own spans — it only strips the fields from the forwarded message.

## Prometheus Metrics

All metrics are exported on `GET /metrics` (admin port, default 7469) in OpenMetrics text format.

### Counters

| Metric | Labels | Description |
|--------|--------|-------------|
| `thoughtgate_requests_total` | `method`, `tool_name`, `status` | Total requests processed |
| `thoughtgate_gate_decisions_total` | `gate`, `outcome` | Gate evaluation decisions |
| `thoughtgate_errors_total` | `error_type`, `method` | Errors by type and method |
| `thoughtgate_cedar_evaluations_total` | `decision`, `policy_id` | Cedar policy evaluations |
| `thoughtgate_approval_requests_total` | `channel`, `outcome` | Approval workflow outcomes |
| `thoughtgate_upstream_requests_total` | `target`, `status_code` | Upstream HTTP requests |
| `thoughtgate_tasks_created_total` | `task_type` | Tasks created |
| `thoughtgate_tasks_completed_total` | `task_type`, `outcome` | Tasks completed |
| `thoughtgate_telemetry_dropped_total` | `signal` | Telemetry signals dropped (queue full) |
| `thoughtgate_stdio_messages_total` | `server_id`, `direction`, `method` | Stdio transport messages |
| `thoughtgate_stdio_governance_decisions_total` | `server_id`, `decision`, `profile` | Stdio governance decisions |
| `thoughtgate_stdio_framing_errors_total` | `server_id`, `error_type` | Stdio framing/parse errors |
| `thoughtgate_green_bytes_total` | `direction` | Green path bytes transferred |
| `thoughtgate_green_streams_total` | `outcome` | Green path stream outcomes |
| `thoughtgate_amber_decisions_total` | `decision` | Amber path inspection decisions |
| `thoughtgate_amber_errors_total` | `error_type` | Amber path errors |
| `thoughtgate_pipeline_failures_total` | `stage` | Governance pipeline stage failures |

### Histograms

| Metric | Labels | Buckets | Description |
|--------|--------|---------|-------------|
| `thoughtgate_request_duration_ms` | `method`, `tool_name` | 0.5, 1, 2, 5, 10, 25, 50, 100, 250, 500, 1000 | Request duration in milliseconds |
| `thoughtgate_cedar_eval_duration_ms` | `decision` | 0.01, 0.05, 0.1, 0.25, 0.5, 1, 5, 10 | Cedar evaluation duration in ms |
| `thoughtgate_upstream_duration_ms` | `target` | 1, 5, 10, 25, 50, 100, 250, 500, 1000, 5000, 10000 | Upstream round-trip duration in ms |
| `thoughtgate_approval_duration_seconds` | `channel` | 1, 5, 10, 30, 60, 120, 300, 600 | Time from approval request to decision |
| `thoughtgate_task_duration_seconds` | `task_type` | 0.1, 1, 5, 10, 30, 60, 120, 300 | Task lifetime duration |
| `thoughtgate_payload_size_bytes` | `direction`, `method` | 64, 256, 1024, 4096, 16384, 65536, 262144, 1048576 | Request/response payload size |
| `thoughtgate_green_ttfb_ms` | — | 0.5, 1, 2, 5, 10, 25, 50 | Green path time to first byte |
| `thoughtgate_amber_inspector_duration_ms` | `inspector_name` | 0.1, 0.5, 1, 5, 10, 50, 100 | Per-inspector execution time |
| `thoughtgate_amber_buffer_bytes` | — | 1024, 4096, 16384, 65536, 262144, 1048576 | Amber path buffer size |
| `thoughtgate_stdio_governance_duration_ms` | `server_id` | 0.1, 0.5, 1, 5, 10, 50, 100 | Stdio governance evaluation duration |
| `thoughtgate_stdio_e2e_latency_ms` | `server_id` | 0.5, 1, 2, 5, 10, 25, 50, 100, 250 | Stdio end-to-end message latency |

### Gauges

| Metric | Labels | Description |
|--------|--------|-------------|
| `thoughtgate_connections_active` | `transport` | Active connections by transport type |
| `thoughtgate_tasks_pending` | `task_type` | Currently pending tasks |
| `thoughtgate_cedar_policies_loaded` | — | Number of Cedar policies loaded |
| `thoughtgate_uptime_seconds` | — | Process uptime |
| `thoughtgate_green_streams_active` | — | Active green path streams |
| `thoughtgate_amber_buffer_active` | — | Active amber path buffers |
| `thoughtgate_amber_semaphore_available` | — | Available amber path semaphore permits |
| `thoughtgate_stdio_active_servers` | — | Active stdio server connections |
| `thoughtgate_config_loaded` | — | Whether config is loaded (0 or 1) |

## Protocol Support Notes

- **OTLP protocol:** Only `http/protobuf` is supported. gRPC (`grpc`) is **not** supported.
- **Sampling:** Only head sampling is supported. Tail sampling is not available.
- **Prometheus format:** OpenMetrics text format (compatible with Prometheus 2.x+).
- **Cardinality management:** Tool names and server IDs are passed through a cardinality limiter to prevent metric explosion from dynamic values.
