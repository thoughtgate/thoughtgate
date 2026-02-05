---
sidebar_position: 2
---

# Configuration Reference

ThoughtGate is configured via a YAML configuration file and environment variables.

## Configuration File

**HTTP sidecar (Kubernetes):** Set the path via `THOUGHTGATE_CONFIG` environment variable:

```bash
export THOUGHTGATE_CONFIG=/etc/thoughtgate/config.yaml
```

**CLI wrapper (local development):** Pass the path via `--thoughtgate-config` flag (defaults to `thoughtgate.yaml` in the working directory):

```bash
thoughtgate wrap --thoughtgate-config /path/to/config.yaml -- claude-code
```

:::note
`THOUGHTGATE_CONFIG` is only used in HTTP sidecar mode. The CLI wrapper uses `--thoughtgate-config` instead.
:::

### Minimal Configuration

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:3000

governance:
  defaults:
    action: forward
```

### Full Configuration

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:3000
    prefix: null  # Optional prefix for tool names
    expose:
      mode: all   # all | allowlist | blocklist

governance:
  defaults:
    action: forward  # forward | approve | deny | policy
  rules:
    - match: "delete_*"
      action: approve
      approval: slack-ops  # References approval section
    - match: "admin_*"
      action: deny
    - match: "sensitive_*"
      action: policy
      policy_id: sensitive-tools  # References Cedar policy

approval:
  slack-ops:
    adapter: slack
    channel: "#approvals"
    timeout: 5m

cedar:
  policy_path: /etc/thoughtgate/policies.cedar
```

## Environment Variables

### Core

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `THOUGHTGATE_CONFIG` | Sidecar only | — | Path to YAML configuration file (sidecar mode) |
| `THOUGHTGATE_OUTBOUND_PORT` | No | `7467` | Port for proxy traffic (sidecar mode) |
| `THOUGHTGATE_ADMIN_PORT` | No | `7469` | Port for health/metrics endpoints (sidecar mode) |
| `THOUGHTGATE_ENVIRONMENT` | No | `production` | Deployment environment label (for telemetry) |

### Slack & Approvals

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `THOUGHTGATE_SLACK_BOT_TOKEN` | For approvals | — | Slack Bot OAuth token (`xoxb-...`) |
| `THOUGHTGATE_SLACK_CHANNEL` | No | `#approvals` | Default channel for approval messages |
| `THOUGHTGATE_APPROVAL_TIMEOUT_SECS` | No | `300` | Default approval timeout in seconds |
| `THOUGHTGATE_REQUEST_TIMEOUT_SECS` | No | `300` | Default upstream request timeout in seconds |
| `THOUGHTGATE_MAX_BATCH_SIZE` | No | `100` | Maximum Slack batch polling size |

### Telemetry

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `THOUGHTGATE_TELEMETRY_ENABLED` | No | `false` | Enable OTLP trace export |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | No | — | OTLP HTTP endpoint (only `http/protobuf` supported) |
| `OTEL_SERVICE_NAME` | No | `thoughtgate` | Service name for OTLP resource |

See [Telemetry Reference](/docs/reference/telemetry) for the full telemetry configuration.

## Port Model

### HTTP Sidecar

ThoughtGate uses an Envoy-inspired 3-port architecture:

| Port | Name | Purpose |
|------|------|---------|
| 7467 | Outbound | Client requests → upstream (main proxy) |
| 7468 | Inbound | Reserved for webhooks |
| 7469 | Admin | Health checks, metrics |

### CLI Wrapper

In CLI wrapper mode, the governance service binds to an **OS-assigned ephemeral port** on `127.0.0.1`. You can override this with `--governance-port`:

```bash
thoughtgate wrap --governance-port 9090 -- claude-code
```

## Governance Actions

| Action | Behavior |
|--------|----------|
| `forward` | Send to upstream immediately |
| `deny` | Return error immediately |
| `approve` | Create SEP-1686 task, post to Slack, wait for approval |
| `policy` | Evaluate Cedar policy, then forward/approve/deny |

## Rule Matching

Rules are evaluated in order. First match wins.

```yaml
governance:
  rules:
    # Most specific rules first
    - match: "admin_delete_*"
      action: deny
    # Then broader patterns
    - match: "admin_*"
      action: approve
    # Catch-all last (or use defaults)
    - match: "*"
      action: forward
```

Patterns use glob syntax:
- `*` matches any characters
- `?` matches single character
- `[abc]` matches character class

## Exposure Configuration

Control which tools are visible to agents:

```yaml
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:3000
    expose:
      mode: allowlist
      tools:
        - "get_*"
        - "list_*"
        - "search"
```

| Mode | Behavior |
|------|----------|
| `all` | All tools visible (default) |
| `allowlist` | Only listed patterns visible |
| `blocklist` | All except listed patterns visible |

## Admin Endpoints

Available on admin port (default 7469):

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Liveness check (returns 200 if running) |
| `/ready` | GET | Readiness check (returns 200 if ready for traffic) |
| `/metrics` | GET | Prometheus metrics |

## Prometheus Metrics

Key metrics (see [Telemetry Reference](/docs/reference/telemetry) for the complete inventory):

```
thoughtgate_requests_total{method, tool_name, status}
thoughtgate_request_duration_ms{method, tool_name}
thoughtgate_approval_requests_total{channel, outcome}
thoughtgate_tasks_pending{task_type}
thoughtgate_upstream_requests_total{target, status_code}
thoughtgate_gate_decisions_total{gate, outcome}
```

## Example Configurations

### Passthrough Mode (Logging Only)

```yaml
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:3000
governance:
  defaults:
    action: forward
```

### Strict Mode (Approve Everything)

```yaml
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:3000
governance:
  defaults:
    action: approve
approval:
  default:
    adapter: slack
    channel: "#all-approvals"
    timeout: 10m
```

### Production Mode (Tiered)

```yaml
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:3000
    expose:
      mode: blocklist
      tools:
        - "internal_*"
        - "debug_*"
governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: approve
      approval: destructive
    - match: "drop_*"
      action: approve
      approval: destructive
    - match: "admin_*"
      action: deny
    - match: "transfer_*"
      action: approve
      approval: financial
approval:
  destructive:
    adapter: slack
    channel: "#ops-approvals"
    timeout: 5m
  financial:
    adapter: slack
    channel: "#finance-approvals"
    timeout: 10m
```
