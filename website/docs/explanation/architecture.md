---
sidebar_position: 2
---

# Architecture

ThoughtGate is a transparent proxy that sits between AI agents and MCP servers.

## High-Level Overview

```
┌─────────────┐     ┌─────────────────────────────────────┐     ┌─────────────┐
│  AI Agent   │────▶│           ThoughtGate               │────▶│  MCP Server │
│  (Claude,   │◀────│                                     │◀────│  (Tools)    │
│   GPT, etc) │     │  Outbound: 7467    Admin: 7469      │     │             │
└─────────────┘     └─────────────────────────────────────┘     └─────────────┘
```

## 4-Gate Decision Model

ThoughtGate evaluates every request through a 4-gate pipeline:

```
                    ┌─────────────────────────────────────────────────────────┐
                    │                    THOUGHTGATE                          │
                    │                                                         │
  MCP Request ─────▶│  ┌──────────┐    ┌──────────┐    ┌──────────────────┐  │
                    │  │  Gate 1  │───▶│  Gate 2  │───▶│     Gate 3/4     │  │
                    │  │Visibility│    │  Rules   │    │                  │  │
                    │  └──────────┘    └──────────┘    │  forward ──▶ ✓   │  │
                    │                                  │  deny ────▶ ✗   │  │
                    │                                  │  approve ─▶ Task │  │
                    │                                  │  policy ──▶ Cedar│  │
                    │                                  └──────────────────┘  │
                    │                                                         │
                    └─────────────────────────────────────────────────────────┘
```

| Gate | Name | Purpose | Configuration |
|------|------|---------|---------------|
| **Gate 1** | Visibility | Filter which tools agents can see | `sources[].expose` |
| **Gate 2** | Governance Rules | YAML pattern matching | `governance.rules[]` |
| **Gate 3** | Cedar Policy | Complex access control | `cedar.policy_path` |
| **Gate 4** | Human Approval | Slack-based approval workflow | `approval.*` |

## Components

### 1. Transport Layer

Handles JSON-RPC 2.0 message parsing and routing:

- Parses incoming MCP requests
- Preserves request ID types (string, number, null)
- Routes responses back to clients
- Manages connection pooling to upstream

### 2. Governance Engine

Evaluates requests against YAML rules:

- Pattern matching with glob syntax
- First-match-wins evaluation
- Routes to Cedar when `action: policy`
- Creates tasks when `action: approve`

### 3. Cedar Policy Engine (Optional)

For complex access control:

- AWS Cedar policy language
- Hot-reloadable policies
- Attribute-based access control
- Condition evaluation on arguments

### 4. Task Manager

Manages SEP-1686 async tasks:

- Creates tasks for approval requests
- Tracks task state (working, completed, failed, rejected)
- Handles timeouts and cancellation
- Stores results for retrieval

### 5. Approval Adapter

Slack integration for human-in-the-loop:

- Posts approval messages with Block Kit
- Polls for reactions (👍/👎)
- Resolves user display names
- Handles rate limiting

### 6. Admin Server

Provides operational endpoints:

- `/health` — Liveness probe
- `/ready` — Readiness probe
- `/metrics` — Prometheus metrics

## Port Model

ThoughtGate uses an Envoy-inspired 3-port architecture:

| Port | Name | Purpose |
|------|------|---------|
| 7467 | Outbound | Client requests → upstream (main proxy) |
| 7468 | Inbound | Reserved for webhooks (v0.3+) |
| 7469 | Admin | Health checks, metrics |

This separation ensures health checks don't interfere with proxy traffic and allows different security policies per port.

## Request Flow

### Forward Path (No Approval)

```
Agent → ThoughtGate → Upstream → ThoughtGate → Agent
       (Gate 1-2: forward)
```

1. Agent sends `tools/call` request
2. Gate 1 checks visibility (pass)
3. Gate 2 matches rule → `action: forward`
4. Request forwarded to upstream
5. Response returned to agent

### Approval Path (SEP-1686)

```
Agent → ThoughtGate → Task Created → Slack → Human → ThoughtGate → Upstream
       (Gate 1-2: approve)    (polls)   (reacts)  (executes)
```

1. Agent sends `tools/call` request
2. Gate 1 checks visibility (pass)
3. Gate 2 matches rule → `action: approve`
4. Task created with ID `tg_abc123`
5. Immediate response: `{taskId: "tg_abc123", status: "working"}`
6. Slack message posted
7. Agent polls `tasks/get`
8. Human reacts 👍
9. Task status → `completed`
10. Agent calls `tasks/result` to get upstream response

## State Management

### v0.2: In-Memory

:::warning Ephemeral State

In v0.2, all task state is stored in an in-memory `DashMap`. This means:

- **Pending approvals are lost on pod restart**
- **Tasks cannot be shared across multiple instances**
- **No persistence across deployments**

Plan your deployment accordingly. For critical workflows, consider implementing retry logic in your agent to resubmit approval requests after ThoughtGate restarts.

:::

All state is held in memory:
- Pending approval tasks (DashMap)
- Connection pools
- Configuration cache

### Future: Persistent State (v0.3+)

Planned improvements:
- Redis-backed task storage
- Task state survives restarts
- Multi-instance coordination
- Horizontal scaling support

## Deployment Model

ThoughtGate is designed as a **sidecar**:

```yaml
spec:
  containers:
    - name: agent          # Your AI agent
    - name: thoughtgate    # Sidecar proxy
```

Benefits:
- No network hop (localhost)
- Per-agent isolation
- Independent scaling
- Zero-config identity from pod labels

## Performance Characteristics

| Path | Latency Overhead | Description |
|------|------------------|-------------|
| Forward | < 2 ms | Policy eval + forward |
| Deny | < 1 ms | Policy eval + error |
| Approve (initial) | < 5 ms | Task creation + Slack post |
| Approve (total) | Seconds to minutes | Waiting for human |

## Failure Modes

### Upstream Unavailable

- Request fails with `-32000 UpstreamConnectionFailed`
- Readiness probe fails
- Agent receives clear error

### Policy Error

- Request denied (fail-safe)
- Logged as error
- Operator alerted via metrics

### Slack Unavailable

- Task created but approval stuck
- Task eventually times out
- Logged as error

## Next Steps

- Understand the [Security Model](/docs/explanation/security-model)
- Learn to [write governance rules](/docs/how-to/write-policies)
