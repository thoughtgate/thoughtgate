---
sidebar_position: 1
---

# Configuration Reference

ThoughtGate is configured via a YAML configuration file and environment variables.

## Configuration File

Set the path via `THOUGHTGATE_CONFIG` environment variable:

```bash
export THOUGHTGATE_CONFIG=/etc/thoughtgate/config.yaml
```

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

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `THOUGHTGATE_CONFIG` | Yes | — | Path to YAML configuration file |
| `THOUGHTGATE_OUTBOUND_PORT` | No | `7467` | Port for proxy traffic |
| `THOUGHTGATE_ADMIN_PORT` | No | `7469` | Port for health/metrics endpoints |
| `THOUGHTGATE_SLACK_BOT_TOKEN` | For approvals | — | Slack Bot OAuth token (`xoxb-...`) |
| `SLACK_CHANNEL` | No | `#approvals` | Default channel for approval messages |

## Port Model

ThoughtGate uses an Envoy-inspired 3-port architecture:

| Port | Name | Purpose |
|------|------|---------|
| 7467 | Outbound | Client requests → upstream (main proxy) |
| 7468 | Inbound | Reserved for webhooks (v0.3+) |
| 7469 | Admin | Health checks, metrics |

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

```
thoughtgate_requests_total{action="forward|approve|deny"}
thoughtgate_request_duration_seconds{quantile="0.5|0.95|0.99"}
thoughtgate_approval_total{result="approved|rejected|timeout"}
thoughtgate_tasks_active
thoughtgate_upstream_requests_total{status="success|error|timeout"}
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
