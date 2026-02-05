# ThoughtGate

[![CI](https://github.com/thoughtgate/thoughtgate/actions/workflows/ci.yml/badge.svg)](https://github.com/thoughtgate/thoughtgate/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![MSRV: 1.87+](https://img.shields.io/badge/MSRV-1.87+-orange.svg)](https://blog.rust-lang.org/)

**Human-in-the-loop approval workflows for AI agents.**

ThoughtGate governs AI agent tool calls so humans stay in control. Run `thoughtgate wrap -- claude-code` and every MCP tool invocation flows through your policy rules — forwarded, denied, or held for Slack approval — without touching agent code.

Unlike framework-specific solutions like LangChain's `interrupt()`, ThoughtGate works with closed-source vendor agents and requires zero application code changes. It ships as a CLI wrapper for local development and as an HTTP sidecar for Kubernetes.

## Table of Contents

- [Quick Start](#quick-start)
- [How It Works](#how-it-works)
- [Supported Agents](#supported-agents)
- [Features](#features)
- [Configuration](#configuration)
- [Profiles](#profiles)
- [Deployment](#deployment)
- [Slack Setup](#slack-setup)
- [Observability](#observability)
- [Performance](#performance)
- [Project Structure](#project-structure)
- [Known Limitations](#known-limitations)
- [Documentation](#documentation)
- [Getting Help](#getting-help)
- [Contributing](#contributing)
- [License](#license)

## Quick Start

### Prerequisites

- Rust 1.87+ (edition 2024, for building from source)
- An MCP-enabled agent (Claude Code, Cursor, VS Code, etc.)
- Slack workspace with bot token (optional, for approval workflows)

### Install

```bash
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo install --path thoughtgate
```

### 1. Create a policy file

```yaml
# thoughtgate.yaml — minimal config, forward everything
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://localhost:3000

governance:
  defaults:
    action: forward
```

### 2. Wrap your agent

```bash
thoughtgate wrap -- claude-code
```

That's it. ThoughtGate discovers your agent's MCP config, rewrites it to route through governance, launches the agent, and restores the original config on exit.

### 3. Add governance rules

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
    - match: "drop_*"
      action: approve
    - match: "admin_*"
      action: deny
```

### 4. Try development mode first

```bash
# Dry-run: see what would change without writing anything
thoughtgate wrap --dry-run -- claude-code

# Development profile: log decisions without blocking
thoughtgate wrap --profile development -- claude-code
```

## How It Works

ThoughtGate operates in two modes depending on your deployment:

### CLI Wrapper (stdio)

For local development. ThoughtGate injects itself between your agent and its MCP servers:

```
┌──────────────┐     ┌─────────────────────────┐     ┌─────────────┐
│  AI Agent    │────▶│  ThoughtGate Shim (×N)   │────▶│ MCP Server  │
│  (Claude     │◀────│  per-server stdio proxy   │◀────│ (stdio)     │
│   Code, etc) │     │  ┌─────────────────────┐ │     └─────────────┘
└──────────────┘     │  │ Governance Service  │ │
                     │  │ (ephemeral port)    │ │
                     │  └─────────────────────┘ │
                     └─────────────────────────┘
```

### HTTP Sidecar (Kubernetes)

For production. ThoughtGate runs as a sidecar container proxying HTTP/SSE traffic:

```
┌──────────────┐     ┌──────────────────────┐     ┌─────────────┐
│  AI Agent    │────▶│     ThoughtGate      │────▶│  MCP Server │
│  (Pod)       │◀────│  :7467 (proxy)       │◀────│  (HTTP)     │
└──────────────┘     │  :7469 (admin)       │     └─────────────┘
                     └──────────────────────┘
```

### 4-Gate Decision Model

Every tool call passes through up to four gates:

```
                    ┌─────────────────────────────────────────────────────────┐
                    │                    THOUGHTGATE                          │
                    │                                                         │
  MCP Request ─────▶  ┌──────────┐    ┌──────────┐    ┌──────────────────┐  │
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

## Supported Agents

ThoughtGate auto-detects your agent from the command name and rewrites its MCP configuration:

| Agent | Command | Config Path (macOS) |
|-------|---------|---------------------|
| **Claude Code** | `claude`, `claude-code` | `~/.claude.json` + `.mcp.json` |
| **Claude Desktop** | `claude-desktop` | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| **Cursor** | `cursor` | `~/.cursor/mcp.json` + `.cursor/mcp.json` |
| **VS Code** | `code`, `code-insiders` | `.vscode/mcp.json` |
| **Windsurf** | `windsurf` | `~/.codeium/windsurf/mcp_config.json` |
| **Zed** | `zed` | `~/.config/zed/settings.json` |
| **Custom** | `--agent-type custom` | Falls back to Claude Desktop format |

> **Note:** Linux config paths follow XDG conventions (e.g., `~/.config/Claude/` instead of `~/Library/Application Support/Claude/`). Use `--config-path` to override auto-detection.

## Features

| Feature | Version | Description |
|---------|---------|-------------|
| MCP Proxy | v0.2 | JSON-RPC 2.0 compliant proxy for MCP traffic |
| YAML Rules | v0.2 | Simple glob-based routing for quick setup |
| Cedar Policies | v0.2 | AWS Cedar engine for complex access control |
| Async Approvals | v0.2 | Native SEP-1686 task support for long-running workflows |
| Slack Integration | v0.2 | Post approval requests, detect 👍/👎 reactions |
| K8s Sidecar | v0.2 | HTTP/SSE proxy with zero-config Pod identity |
| **CLI Wrapper** | **v0.3** | `thoughtgate wrap` — auto-rewrite agent config, launch, restore |
| **stdio Transport** | **v0.3** | Per-server shim proxies for stdio-based MCP servers |
| **Profiles** | **v0.3** | Production (enforcing) and development (log-only) modes |
| **NDJSON Detection** | **v0.3** | Framing error detection for smuggling/corruption |
| **Config Backup** | **v0.3** | Automatic backup and restore of agent config files |
| **OpenTelemetry** | **v0.3** | Distributed tracing via OTLP (HTTP/protobuf) |
| **Prometheus Metrics** | **v0.3** | `thoughtgate_*` counters, gauges, and histograms |
| **Env Var Expansion** | **v0.3** | `${VAR}` and `${VAR:-default}` in configs |

### Roadmap

| Feature | Version | Description |
|---------|---------|-------------|
| Response Inspection | v0.4 | Buffer and inspect responses for PII/schemas |
| Persistent State | v0.4 | Redis-backed task storage |
| Multi-Upstream | v0.4 | Route to multiple MCP servers |
| A2A Protocol | v0.5 | Agent-to-agent approval workflows |

## Configuration

### CLI Reference

```
thoughtgate wrap [OPTIONS] -- <COMMAND>...
```

| Flag | Default | Description |
|------|---------|-------------|
| `--agent-type <TYPE>` | auto-detected | Override agent type (`claude-code`, `claude-desktop`, `cursor`, `vscode`, `windsurf`, `zed`, `custom`) |
| `--config-path <PATH>` | auto-discovered | Override agent config file path |
| `--profile <PROFILE>` | `production` | Runtime profile: `production` or `development` |
| `--thoughtgate-config <PATH>` | `thoughtgate.yaml` | ThoughtGate governance config file |
| `--governance-port <PORT>` | `0` (ephemeral) | Port for the governance HTTP service |
| `--no-restore` | `false` | Don't restore original agent config on exit |
| `--dry-run` | `false` | Print config diff without writing |
| `--verbose` | `false` | Enable debug logging |

### YAML Configuration

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
    - match: "admin_*"
      action: deny

approval:
  slack-ops:
    adapter: slack
    channel: "#approvals"
    timeout: 5m

telemetry:
  enabled: true
  otlp:
    endpoint: "http://otel-collector:4318"
    protocol: http/protobuf
  sampling:
    strategy: head
    success_sample_rate: 0.10
```

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

### Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `THOUGHTGATE_CONFIG` | Sidecar only | — | Path to YAML configuration file |
| `THOUGHTGATE_OUTBOUND_PORT` | | `7467` | Main proxy port (sidecar mode) |
| `THOUGHTGATE_ADMIN_PORT` | | `7469` | Admin port for health/metrics (sidecar mode) |
| `THOUGHTGATE_SLACK_BOT_TOKEN` | For approvals | — | Slack bot OAuth token |
| `THOUGHTGATE_SLACK_CHANNEL` | For approvals | `#approvals` | Default channel for approval messages |
| `THOUGHTGATE_REQUEST_TIMEOUT_SECS` | | `300` | Per-request timeout |
| `THOUGHTGATE_MAX_BATCH_SIZE` | | `100` | Maximum JSON-RPC batch array size |
| `THOUGHTGATE_ENVIRONMENT` | | `production` | Environment name |
| `THOUGHTGATE_TELEMETRY_ENABLED` | | `false` | Enable OTLP trace export |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | | OTel default | OTLP collector endpoint |
| `OTEL_SERVICE_NAME` | | `thoughtgate` | OpenTelemetry service name |

> **Note:** In CLI wrapper mode, `THOUGHTGATE_CONFIG` is not needed — use `--thoughtgate-config` instead. The governance port is ephemeral by default (OS-assigned).

### Port Model

ThoughtGate uses an Envoy-inspired 3-port architecture in sidecar mode:

| Port | Name | Purpose |
|------|------|---------|
| 7467 | Outbound | Client requests → upstream (main proxy) |
| 7468 | Inbound | Reserved for webhooks (v0.4+) |
| 7469 | Admin | Health checks (`/health`, `/ready`), metrics |

In CLI wrapper mode, the governance service binds to an **ephemeral port** (OS-assigned, default `--governance-port 0`). Shim processes connect to it automatically.

## Profiles

ThoughtGate supports two runtime profiles to ease adoption:

| Behavior | Production | Development |
|----------|-----------|-------------|
| **Rule enforcement** | Blocking — denied calls return errors | Log-only — decisions logged, calls forwarded |
| **Approvals** | Required — held for Slack reaction | Auto-approved with audit trail |
| **Log prefix** | `BLOCKED` | `WOULD_BLOCK` |
| **Slack adapter** | Required (fails on init error) | Optional (falls back to mock auto-approve) |

**Recommended workflow:** Start with `--profile development` to see what ThoughtGate *would* block, review the logs, tune your rules, then switch to `--profile production`.

```
# Example development mode log output:
INFO WOULD_BLOCK server_id="filesystem" tool="delete_file" decision="approve"
INFO WOULD_BLOCK server_id="database" tool="drop_table" decision="deny"
```

## Deployment

### Local Development (CLI Wrapper)

The fastest way to get started. ThoughtGate wraps your agent, rewrites its MCP config, and restores it on exit:

```bash
# Claude Code
thoughtgate wrap -- claude-code

# Cursor
thoughtgate wrap -- cursor

# VS Code
thoughtgate wrap -- code

# With explicit agent type and config
thoughtgate wrap --agent-type windsurf --config-path ~/.codeium/windsurf/mcp_config.json -- windsurf
```

### Kubernetes Sidecar

ThoughtGate runs as a sidecar container. Identity is automatically inferred from Pod labels — no API keys required.

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
      image: ghcr.io/thoughtgate/thoughtgate:v0.3.0
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
  ghcr.io/thoughtgate/thoughtgate:v0.3.0
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

Slack integration works identically in both CLI wrapper and sidecar modes.

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

## Observability

### Health Endpoints

Available on the admin port (`:7469` in sidecar mode):

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `GET /health` | GET | Liveness probe — always returns `200 OK` |
| `GET /ready` | GET | Readiness probe — `200` when ready, `503` otherwise |
| `GET /metrics` | GET | Prometheus metrics (OpenMetrics text format) |

### Prometheus Metrics

ThoughtGate exposes `thoughtgate_*` metrics on the admin port. Key metrics:

**Counters:**

```
thoughtgate_requests_total{method, tool_name, status}
thoughtgate_decisions_total{gate, outcome}
thoughtgate_errors_total{error_type, method}
thoughtgate_cedar_evaluations_total{decision, policy_id}
thoughtgate_approval_requests_total{channel, outcome}
thoughtgate_upstream_requests_total{target, status_code}
thoughtgate_tasks_created_total{task_type}
thoughtgate_tasks_completed_total{task_type, outcome}
thoughtgate_stdio_messages_total{server_id, direction, method}
thoughtgate_stdio_governance_decisions_total{server_id, decision, profile}
thoughtgate_stdio_framing_errors_total{server_id, error_type}
```

**Histograms:**

```
thoughtgate_request_duration_ms{method, tool_name}
thoughtgate_cedar_evaluation_duration_ms{decision}
thoughtgate_upstream_duration_ms{target}
thoughtgate_approval_wait_duration_s{channel, outcome}
thoughtgate_stdio_approval_latency_seconds{server_id}
```

**Gauges:**

```
thoughtgate_connections_active{transport}
thoughtgate_tasks_pending{task_type}
thoughtgate_cedar_policies_loaded
thoughtgate_uptime_seconds
thoughtgate_stdio_active_servers
```

### Distributed Tracing

ThoughtGate supports OpenTelemetry distributed tracing via OTLP HTTP/protobuf:

```bash
# Enable tracing
export THOUGHTGATE_TELEMETRY_ENABLED=true
export OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4318
export OTEL_SERVICE_NAME=thoughtgate
```

Or configure via YAML:

```yaml
telemetry:
  enabled: true
  otlp:
    endpoint: "http://otel-collector:4318"
    protocol: http/protobuf
  sampling:
    strategy: head
    success_sample_rate: 0.10
  resource:
    service.name: my-thoughtgate
    deployment.environment: staging
```

Features:
- **W3C Trace Context** propagation to upstream MCP servers
- **Head sampling** with configurable success rate
- **Zero overhead** when disabled — no exporters, no network calls
- Automatic K8s resource attributes (`k8s.namespace.name`, `k8s.pod.name`, `k8s.node.name`)

## Performance

ThoughtGate is built for minimal overhead:

| Metric | Target | Description |
|--------|--------|-------------|
| **Binary size** | < 15 MB | Single static binary |
| **Memory (idle)** | < 20 MB | Low footprint sidecar |
| **Latency overhead** | < 2 ms p50 | Minimal proxy overhead |
| **Throughput** | > 10,000 RPS | High capacity under load |
| **Policy evaluation** | < 100 µs p50 | Fast Cedar evaluation |
| **Startup time** | < 100 ms | Fast cold start |

## Project Structure

ThoughtGate is a Cargo workspace with three crates:

```
thoughtgate-core/         # Library: transport-agnostic governance, policy, telemetry
thoughtgate-proxy/        # Binary: HTTP+SSE sidecar for Kubernetes
thoughtgate/              # Binary: CLI wrapper for local development
```

**`thoughtgate-core`** contains all shared logic — Cedar policy evaluation, governance pipeline, Slack integration, task lifecycle, config parsing, and telemetry. Both the proxy and CLI binaries are thin transport layers on top of core.

**`thoughtgate-proxy`** is the HTTP sidecar. It accepts MCP traffic on `:7467`, evaluates governance, and forwards to an upstream MCP server over HTTP.

**`thoughtgate`** is the CLI. The `wrap` subcommand discovers agent configs, rewrites them to route MCP servers through per-server `shim` stdio proxies, and launches the agent. Each shim evaluates governance locally via the shared governance service.

## Known Limitations

| Limitation | Impact | Future |
|------------|--------|--------|
| **In-memory state** | Pending tasks lost on restart | v0.4: Redis persistence |
| **Single upstream** | One MCP server per sidecar instance | v0.4: Multi-upstream routing |
| **Polling-based** | ~5s delay to detect Slack reactions | v0.4: Slack Events API |
| **No response inspection** | Cannot inspect/redact response content | v0.4: Amber path buffering |
| **macOS and Linux only** | No Windows support | Under consideration |
| **No mid-session config** | Config changes require restart | v0.4: Hot-reload via SIGHUP |

## Documentation

- [Architecture](specs/architecture.md) — System design and 4-Gate model
- [MCP Transport](specs/REQ-CORE-003_MCP_Transport_and_Routing.md) — JSON-RPC 2.0 routing
- [Error Handling](specs/REQ-CORE-004_Error_Handling.md) — Error types and codes
- [Operational Lifecycle](specs/REQ-CORE-005_Operational_Lifecycle.md) — State machine
- [stdio Transport](specs/REQ-CORE-008_Stdio_Transport.md) — CLI wrapper and shim design
- [Configuration](specs/REQ-CFG-001_Configuration.md) — YAML schema reference
- [Cedar Policy Engine](specs/REQ-POL-001_Cedar_Policy_Engine.md) — Cedar integration
- [Task Lifecycle](specs/REQ-GOV-001_Task_Lifecycle.md) — SEP-1686 implementation
- [Approval Integration](specs/REQ-GOV-003_Approval_Integration.md) — Slack adapter
- [Performance Metrics](specs/REQ-OBS-001_Performance_Metrics.md) — Benchmarking
- [Telemetry & Tracing](specs/REQ-OBS-002_Telemetry_Tracing_Metrics.md) — OpenTelemetry integration

## Getting Help

- **Bug reports & feature requests:** [GitHub Issues](https://github.com/thoughtgate/thoughtgate/issues)
- **Questions & discussions:** [GitHub Discussions](https://github.com/thoughtgate/thoughtgate/discussions)

## Contributing

```bash
# Clone and build
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo build

# Before submitting a PR
cargo fmt              # Required — CI will reject unformatted code
cargo clippy -- -D warnings
cargo test
```

See [CLAUDE.md](CLAUDE.md) for detailed code standards, safety rules, and commit message conventions.

## License

Apache 2.0
