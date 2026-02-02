# ThoughtGate

[![CI](https://github.com/thoughtgate/thoughtgate/actions/workflows/ci.yml/badge.svg)](https://github.com/thoughtgate/thoughtgate/actions/workflows/ci.yml)

**Human-in-the-loop approval workflows for AI agents.**

ThoughtGate is a high-performance Rust sidecar that acts as a governance layer for AI agents. It intercepts MCP (Model Context Protocol) tool calls and enforces human approval workflows without modifying your agent's code.

Unlike framework-specific solutions like LangChain's `interrupt()`, ThoughtGate can govern closed-source vendor agents and doesn't require application code changes.

```
┌─────────────┐     ┌─────────────────────────────────────┐     ┌─────────────┐
│  AI Agent   │────▶│           ThoughtGate               │────▶│  MCP Server │
│  (Claude,   │◀────│  • YAML governance rules            │◀────│  (Tools)    │
│   GPT, etc) │     │  • Cedar policy engine              │     │             │
└─────────────┘     │  • Slack approval workflows         │     └─────────────┘
                    │  • SEP-1686 async tasks             │
                    └─────────────────────────────────────┘
```

## Why ThoughtGate?

AI agents are increasingly capable of taking real-world actions—deleting files, sending emails, making purchases, modifying databases. ThoughtGate provides a governance layer that:

- **Enforces policies** — Define which tools require approval using YAML rules or Cedar policies
- **Enables human oversight** — Route sensitive operations to Slack for approval
- **Works with any agent** — No SDK required; deploy as a sidecar proxy
- **Maintains audit trails** — Log all tool calls and approval decisions

## Features (v0.2)

| Feature | Status | Description |
|---------|--------|-------------|
| MCP Proxy | ✅ | JSON-RPC 2.0 compliant proxy for MCP traffic |
| YAML Rules | ✅ | Simple glob-based routing for quick setup |
| Cedar Policies | ✅ | AWS Cedar engine for complex access control |
| Async Approvals | ✅ | Native SEP-1686 task support for long-running workflows |
| Slack Integration | ✅ | Post approval requests, detect 👍/👎 reactions |
| K8s Native | ✅ | Designed as a sidecar with zero-config identity from Pod labels |

### Roadmap

| Feature | Version | Description |
|---------|---------|-------------|
| Response Inspection | v0.3 | Buffer and inspect responses for PII/schemas |
| Persistent State | v0.3 | Redis-backed task storage |
| Multi-Upstream | v0.3 | Route to multiple MCP servers |
| A2A Protocol | v0.4 | Agent-to-agent approval workflows |

## Performance

ThoughtGate is built for minimal overhead using `hyper`, `mimalloc`, and `socket2` TCP optimizations:

| Metric | Target | Description |
|--------|--------|-------------|
| **Binary size** | < 15 MB | Single static binary |
| **Memory (idle)** | < 20 MB | Low footprint sidecar |
| **Latency overhead** | < 2 ms p50 | Minimal proxy overhead |
| **Throughput** | > 10,000 RPS | High capacity under load |
| **Policy evaluation** | < 100 µs | Fast Cedar evaluation |
| **Startup time** | < 100 ms | Fast cold start |

## Quick Start

### Prerequisites

- Rust 1.87+ (edition 2024, for building from source)
- An MCP server to proxy
- Slack workspace with bot token (for approvals)

### Installation

```bash
# From source
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo build --release -p thoughtgate-proxy

# Binary at target/release/thoughtgate-proxy
```

Or use the Docker image:

```bash
docker pull ghcr.io/thoughtgate/thoughtgate:v0.2.2
```

### Basic Usage

Create a configuration file `thoughtgate.yaml`:

```yaml
# The simplest config: proxy and log everything (zero-config mode)
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://localhost:3000

governance:
  defaults:
    action: forward
```

This passthrough mode is useful to validate your setup and gain observability before adding governance rules.

To add governance, extend with rules:

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://localhost:3000

governance:
  defaults:
    action: forward
  rules:
    # Require approval for destructive operations
    - match: "delete_*"
      action: approve
    - match: "drop_*"
      action: approve
    # Block admin tools
    - match: "admin_*"
      action: deny
```

Start ThoughtGate:

```bash
export THOUGHTGATE_CONFIG=./thoughtgate.yaml
./thoughtgate-proxy
```

Point your MCP client at `http://localhost:7467` (the default outbound port).

### With Slack Approvals

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://localhost:3000

governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: approve
      approval: slack-ops

approval:
  slack-ops:
    adapter: slack
    channel: "#approvals"
    timeout: 5m
```

```bash
export THOUGHTGATE_CONFIG=./thoughtgate.yaml
export THOUGHTGATE_SLACK_BOT_TOKEN=xoxb-your-token
./thoughtgate-proxy
```

## Architecture

ThoughtGate uses a 4-Gate decision model:

```
                    ┌─────────────────────────────────────────────────────────┐
                    │                    THOUGHTGATE                          │
                    │                                                         │
  MCP Request ─────▶│  ┌──────────┐    ┌──────────┐    ┌──────────────────┐  │
                    │  │  Gate 1  │───▶│  Gate 2  │───▶│     Gate 3/4     │  │
                    │  │Visibility│    │  Rules   │    │                  │  │
                    │  └──────────┘    └──────────┘    │  forward ──▶ ✓   │  │
                    │                                  │  deny ────▶ ✗   │  │
                    │                                  │  approve ─▶ Slack│  │
                    │                                  │  policy ──▶ Cedar│  │
                    │                                  └──────────────────┘  │
                    │                                                         │
                    └─────────────────────────────────────────────────────────┘
```

| Gate | Purpose | Configuration |
|------|---------|---------------|
| **Gate 1** | Tool visibility filtering | `sources[].expose` allowlist/blocklist |
| **Gate 2** | YAML rule matching | `governance.rules[]` with glob patterns |
| **Gate 3** | Cedar policy evaluation | When `action: policy` |
| **Gate 4** | Human approval workflow | When `action: approve` |

### Actions

| Action | Behavior |
|--------|----------|
| `forward` | Send to upstream immediately |
| `deny` | Return error immediately |
| `approve` | Post to Slack, wait for 👍/👎, then forward or reject |
| `policy` | Evaluate Cedar policy, then forward/approve/deny based on result |

## Configuration

### Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `THOUGHTGATE_CONFIG` | ✅ | — | Path to YAML configuration file |
| `THOUGHTGATE_OUTBOUND_PORT` | | `7467` | Main proxy port for client requests |
| `THOUGHTGATE_ADMIN_PORT` | | `7469` | Admin port for health/metrics |
| `THOUGHTGATE_SLACK_BOT_TOKEN` | For approvals | — | Slack bot OAuth token |
| `THOUGHTGATE_SLACK_CHANNEL` | For approvals | `#approvals` | Default channel for approval messages |
| `THOUGHTGATE_REQUEST_TIMEOUT_SECS` | | `300` | Per-request timeout for proxy connections |
| `THOUGHTGATE_MAX_BATCH_SIZE` | | `100` | Maximum JSON-RPC batch array size |
| `THOUGHTGATE_ENVIRONMENT` | | `production` | Environment name (`development` for dev mode) |

### Port Model

ThoughtGate uses an Envoy-inspired 3-port architecture:

| Port | Name | Purpose |
|------|------|---------|
| 7467 | Outbound | Client requests → upstream (main proxy) |
| 7468 | Inbound | Reserved for webhooks (v0.3+) |
| 7469 | Admin | Health checks (`/health`, `/ready`), metrics |

### Cedar Policy Example

For complex access control, use Cedar policies:

```cedar
// Allow tools/list without restrictions
permit(
    principal,
    action == Action::"Forward",
    resource
) when {
    resource.method == "tools/list"
};

// Require approval for destructive operations
permit(
    principal,
    action == Action::"Approve",
    resource
) when {
    resource.tool_name like "delete_*"
};

// Deny access to admin tools
forbid(
    principal,
    action,
    resource
) when {
    resource.tool_name like "admin_*"
};
```

## Deployment

### Kubernetes Sidecar

ThoughtGate is designed to run as a sidecar container. Identity is automatically inferred from Pod labels—no API keys required.

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: my-agent
  labels:
    app: my-agent  # Used for principal identity
spec:
  containers:
    - name: agent
      image: my-agent:latest
      env:
        - name: MCP_SERVER_URL
          value: "http://localhost:7467"  # Points to ThoughtGate

    - name: thoughtgate
      image: ghcr.io/thoughtgate/thoughtgate:v0.2.2
      ports:
        - containerPort: 7467  # Outbound (proxy)
        - containerPort: 7469  # Admin (health)
      env:
        - name: THOUGHTGATE_CONFIG
          value: "/etc/thoughtgate/config.yaml"
        - name: THOUGHTGATE_SLACK_BOT_TOKEN
          valueFrom:
            secretKeyRef:
              name: thoughtgate-secrets
              key: slack-token
      volumeMounts:
        - name: config
          mountPath: /etc/thoughtgate
      livenessProbe:
        httpGet:
          path: /health
          port: 7469
      readinessProbe:
        httpGet:
          path: /ready
          port: 7469
  volumes:
    - name: config
      configMap:
        name: thoughtgate-config
```

### Docker

```bash
docker run -d \
  -p 7467:7467 \
  -p 7469:7469 \
  -v $(pwd)/thoughtgate.yaml:/etc/thoughtgate/config.yaml \
  -e THOUGHTGATE_CONFIG=/etc/thoughtgate/config.yaml \
  -e THOUGHTGATE_SLACK_BOT_TOKEN=xoxb-... \
  ghcr.io/thoughtgate/thoughtgate:v0.2.2
```

## Slack Setup

1. Create a Slack app at [api.slack.com/apps](https://api.slack.com/apps)

2. Add Bot Token Scopes:
   - `chat:write` — Post approval messages
   - `reactions:read` — Detect approval reactions
   - `channels:history` — Poll channel for reaction updates
   - `users:read` — Resolve user display names

3. Install to workspace and copy the Bot OAuth Token

4. Invite the bot to your approvals channel: `/invite @ThoughtGate`

### Approval Flow

When a tool call requires approval:

1. ThoughtGate creates an SEP-1686 task and posts a message to Slack:
   ```
   🔒 Approval Required: delete_user

   Tool: delete_user
   Principal: my-agent
   Arguments:
   {
     "user_id": "12345"
   }

   React with 👍 to approve or 👎 to reject

   Task ID: tg_abc123 • Expires: 2024-01-15 10:30 UTC
   ```

2. A human reacts with 👍 or 👎

3. ThoughtGate detects the reaction and either:
   - **👍** Executes the tool, returns result via `tasks/result`
   - **👎** Returns `ApprovalRejected` error

4. The agent polls `tasks/get` to retrieve the result

## v0.2 Limitations

| Limitation | Impact | Future |
|------------|--------|--------|
| **In-memory state** | Pending tasks lost on pod restart | v0.3: Redis persistence |
| **Single upstream** | One MCP server per ThoughtGate instance | v0.3: Multi-upstream routing |
| **Polling-based** | ~5s delay to detect Slack reactions | v0.3: Slack Events API |
| **No response inspection** | Cannot inspect/redact response content | v0.3: Amber path buffering |

## Observability

### Health Endpoints

| Endpoint | Port | Purpose |
|----------|------|---------|
| `GET /health` | 7469 | Liveness probe |
| `GET /ready` | 7469 | Readiness probe |
| `GET /metrics` | 7469 | Prometheus metrics |

### Prometheus Metrics

ThoughtGate exposes standard Prometheus counters on the admin port:

```
mcp_requests_total{method, outcome}
mcp_request_duration_seconds{method}
mcp_policy_eval_duration_seconds
governance_tasks_created_total
governance_tasks_terminal_total{status}
governance_approval_latency_seconds
governance_pipeline_failures_total{stage}
governance_scheduler_polls_total
```

## Documentation

- [Architecture Spec](specs/architecture.md) — System design and 4-Gate model
- [Configuration Spec](specs/REQ-CFG-001_Configuration.md) — YAML schema reference
- [Policy Engine Spec](specs/REQ-POL-001_Cedar_Policy_Engine.md) — Cedar integration
- [Task Lifecycle Spec](specs/REQ-GOV-001_Task_Lifecycle.md) — SEP-1686 implementation
- [Approval Integration Spec](specs/REQ-GOV-003_Approval_Integration.md) — Slack adapter

## License

Apache 2.0
