# ThoughtGate

[![CI](https://github.com/thoughtgate/thoughtgate/actions/workflows/ci.yml/badge.svg)](https://github.com/thoughtgate/thoughtgate/actions/workflows/ci.yml)
[![Binary Size](https://bencher.dev/perf/thoughtgate/badge/binary-size)](https://bencher.dev/perf/thoughtgate)
[![Latency p95](https://bencher.dev/perf/thoughtgate/badge/latency-p95)](https://bencher.dev/perf/thoughtgate)
[![Throughput](https://bencher.dev/perf/thoughtgate/badge/throughput)](https://bencher.dev/perf/thoughtgate)

**Human-in-the-loop approval workflows for MCP (Model Context Protocol) agents.**

ThoughtGate is a sidecar proxy that intercepts MCP tool calls and routes them through policy-based approval workflows before execution. It ensures AI agents can't perform sensitive operations without human oversight.

```
┌─────────────┐     ┌─────────────────────────────────────┐     ┌─────────────┐
│  AI Agent   │────▶│           ThoughtGate               │────▶│  MCP Server │
│  (Claude,   │◀────│  • Policy evaluation (Cedar)        │◀────│  (Tools)    │
│   GPT, etc) │     │  • Approval workflows (Slack)       │     │             │
└─────────────┘     │  • Request inspection               │     └─────────────┘
                    └─────────────────────────────────────┘
```

## Why ThoughtGate?

AI agents are increasingly capable of taking real-world actions—deleting files, sending emails, making purchases, modifying databases. ThoughtGate provides a governance layer that:

- **Enforces policies** — Define which tools require approval using Cedar policies
- **Enables human oversight** — Route sensitive operations to Slack for approval
- **Inspects requests** — Detect PII, validate schemas, apply transformations
- **Maintains audit trails** — Log all tool calls and approval decisions

## Features (v0.1 MVP)

| Feature | Status | Description |
|---------|--------|-------------|
| MCP Proxy | ✅ | JSON-RPC 2.0 compliant proxy for MCP traffic |
| Cedar Policies | ✅ | Flexible policy engine for routing decisions |
| Four-Path Routing | ✅ | Green (stream), Amber (inspect), Red (deny), Approval (human) |
| Slack Approvals | ✅ | Post approval requests, detect 👍/👎 reactions |
| Request Inspection | ✅ | PII detection, schema validation |
| Blocking Mode | ✅ | Hold connection until approval (v0.1) |

### Roadmap

| Feature | Version | Description |
|---------|---------|-------------|
| SEP-1686 Tasks | v0.2 | Async task-based approval flow |
| Persistent State | v0.2 | Redis-backed task storage |
| Multi-Upstream | v0.2 | Route to multiple MCP servers |
| A2A Approval | v1.0 | Agent-to-agent approval workflows |
| Prompt Guard | v1.0 | ML-based prompt injection detection |

## Performance

ThoughtGate is designed as a lightweight sidecar with minimal overhead:

| Metric | Target | Description |
|--------|--------|-------------|
| **Binary size** | < 15 MB | Single static binary |
| **Memory (idle)** | < 20 MB | Low footprint sidecar |
| **Latency overhead** | < 2 ms p50 | Minimal proxy overhead |
| **Throughput** | > 10,000 RPS | High capacity under load |
| **Policy evaluation** | < 100 µs | Fast Cedar evaluation |
| **Startup time** | < 100 ms | Fast cold start |

See the [Bencher.dev dashboard](https://bencher.dev/perf/thoughtgate) for live performance tracking.

## Quick Start

### Prerequisites

- Rust 1.75+
- An MCP server to proxy
- Slack workspace with bot token (for approvals)

### Installation

```bash
# From source
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo build --release

# Binary will be at target/release/thoughtgate
```

### Basic Usage

```bash
# Start ThoughtGate proxying to an MCP server
export THOUGHTGATE_UPSTREAM=http://localhost:3000
export THOUGHTGATE_LISTEN=0.0.0.0:8080

./thoughtgate
```

Point your MCP client at `http://localhost:8080` instead of the upstream server.

### With Slack Approvals

```bash
export THOUGHTGATE_UPSTREAM=http://localhost:3000
export THOUGHTGATE_SLACK_BOT_TOKEN=xoxb-your-token
export THOUGHTGATE_SLACK_CHANNEL="#approvals"
export THOUGHTGATE_CEDAR_POLICY_PATH=./policy.cedar

./thoughtgate
```

## Architecture

ThoughtGate classifies all traffic into one of four paths:

```
                    ┌─────────────────────────────────────────────────────────┐
                    │                    THOUGHTGATE                          │
                    │                                                         │
  MCP Request ─────▶│  ┌──────────┐    ┌─────────────────────────────────┐   │
                    │  │  Cedar   │───▶│         PATH ROUTER             │   │
                    │  │  Policy  │    │                                 │   │
                    │  └──────────┘    │  Green ──▶ Stream directly      │   │
                    │                  │  Amber ──▶ Buffer & inspect     │   │
                    │                  │  Red ────▶ Reject immediately   │   │
                    │                  │  Approval ▶ Slack workflow      │   │
                    │                  └─────────────────────────────────┘   │
                    │                                                         │
                    └─────────────────────────────────────────────────────────┘
```

| Path | When | Behavior |
|------|------|----------|
| **Green** | Trusted traffic (e.g., `tools/list`) | Zero-copy streaming, minimal latency |
| **Amber** | Needs inspection (e.g., responses with PII) | Buffer, run inspectors, then forward |
| **Red** | Policy denied | Return error immediately |
| **Approval** | Sensitive operations (e.g., `delete_user`) | Post to Slack, wait for 👍/👎 |

## Configuration

### Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `THOUGHTGATE_UPSTREAM` | ✅ | — | Upstream MCP server URL |
| `THOUGHTGATE_LISTEN` | | `0.0.0.0:8080` | Listen address |
| `THOUGHTGATE_CEDAR_POLICY_PATH` | | (embedded) | Path to Cedar policy file |
| `THOUGHTGATE_SLACK_BOT_TOKEN` | For approvals | — | Slack bot OAuth token |
| `THOUGHTGATE_SLACK_CHANNEL` | For approvals | — | Channel for approval messages |
| `THOUGHTGATE_APPROVAL_TIMEOUT_SECS` | | `300` | Max wait time for approval |
| `THOUGHTGATE_REQUEST_TIMEOUT_SECS` | | `30` | Upstream request timeout |

See [docs/configuration.md](docs/configuration.md) for the complete list.

### Cedar Policy Example

```cedar
// Allow tools/list without restrictions
permit(
    principal,
    action == Action::"tools/list",
    resource
);

// Require approval for destructive operations
permit(
    principal,
    action == Action::"tools/call",
    resource
) when {
    resource.tool_name in ["delete_user", "drop_table", "send_email"]
} advice {
    "require_approval": true
};

// Deny access to admin tools for non-admin roles
forbid(
    principal,
    action == Action::"tools/call",
    resource
) when {
    resource.tool_name == "admin_console" &&
    !principal in Role::"admin"
};
```

## Deployment

### Kubernetes Sidecar

ThoughtGate is designed to run as a sidecar container:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: my-agent
spec:
  containers:
    - name: agent
      image: my-agent:latest
      env:
        - name: MCP_SERVER_URL
          value: "http://localhost:8080"  # Points to ThoughtGate
    
    - name: thoughtgate
      image: thoughtgate:v0.1
      ports:
        - containerPort: 8080
      env:
        - name: THOUGHTGATE_UPSTREAM
          value: "http://mcp-server:3000"
        - name: THOUGHTGATE_SLACK_BOT_TOKEN
          valueFrom:
            secretKeyRef:
              name: thoughtgate-secrets
              key: slack-token
```

### Docker

```bash
docker run -d \
  -p 8080:8080 \
  -e THOUGHTGATE_UPSTREAM=http://host.docker.internal:3000 \
  -e THOUGHTGATE_SLACK_BOT_TOKEN=xoxb-... \
  -e THOUGHTGATE_SLACK_CHANNEL="#approvals" \
  thoughtgate:v0.1
```

## Slack Setup

1. Create a Slack app at [api.slack.com/apps](https://api.slack.com/apps)

2. Add Bot Token Scopes:
   - `chat:write` — Post approval messages
   - `reactions:read` — Detect approval reactions
   - `channels:history` — Read channel messages for polling

3. Install to workspace and copy the Bot OAuth Token

4. Invite the bot to your approvals channel: `/invite @ThoughtGate`

### Approval Flow

When a tool call requires approval:

1. ThoughtGate posts a message to Slack:
   ```
   🔔 Approval Required
   
   Tool: delete_user
   Arguments: {"user_id": "12345"}
   Requested by: agent-pod-abc
   
   React 👍 to approve or 👎 to reject
   ```

2. A human reacts with 👍 or 👎

3. ThoughtGate detects the reaction and either:
   - **👍** Executes the tool, returns result to agent
   - **👎** Returns `ApprovalRejected` error to agent

## v0.1 Limitations

| Limitation | Impact | Future |
|------------|--------|--------|
| **Blocking mode** | HTTP connection held during approval (may timeout) | v0.2: SEP-1686 async tasks |
| **In-memory state** | Pending approvals lost on restart | v0.2: Redis persistence |
| **Single upstream** | One MCP server per ThoughtGate instance | v0.2: Multi-upstream routing |
| **Polling-based** | 5s delay to detect Slack reactions | v0.2: Slack Events API |

## Observability

### Prometheus Metrics

```
thoughtgate_requests_total{path="green|amber|red|approval"}
thoughtgate_request_duration_seconds{path="..."}
thoughtgate_approval_total{result="approved|rejected|timeout"}
thoughtgate_upstream_requests_total{status="success|error|timeout"}
thoughtgate_policy_evaluations_total{decision="green|amber|red|approval"}
```

### Health Endpoints

| Endpoint | Purpose |
|----------|---------|
| `GET /health` | Liveness probe |
| `GET /ready` | Readiness probe |
| `GET /metrics` | Prometheus metrics |

## Documentation

| Document | Description |
|----------|-------------|
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture overview |
| [RFC-001](docs/rfcs/RFC-001_Traffic_Model_Architecture.md) | Traffic model design |
| [REQ-CORE-*](specs/) | Core requirements (transport, streaming, errors) |
| [REQ-POL-*](specs/) | Policy engine requirements |
| [REQ-GOV-*](specs/) | Governance/approval requirements |
| [IMPLEMENTATION_PLAYBOOK.md](docs/IMPLEMENTATION_PLAYBOOK.md) | Developer guide |
