# ThoughtGate Architecture

| Metadata | Value |
|----------|-------|
| **Title** | System Architecture & Integration |
| **Status** | Draft |
| **Version** | v0.2 |
| **Tags** | `#architecture` `#integration` `#overview` `#mcp` `#hitl` `#4-gate` |

## 1. Purpose

This document provides the **top-level architectural view** of ThoughtGate, showing how individual requirements compose into a coherent system. It serves as:

- Single source of truth for request lifecycle
- Integration guide for component interaction
- Configuration reference
- v0.2 scope definition

**Target deployment:** Kubernetes sidecar (co-located with AI agent in same pod).

## 2. System Overview

### 2.1 What is ThoughtGate?

ThoughtGate is an **MCP (Model Context Protocol) sidecar proxy** that implements **approval workflows** for AI agent tool calls. It intercepts MCP `tools/call` requests, routes them through a 4-gate decision flow (visibility, governance rules, Cedar policies, and approval workflows), and forwards approved operations to upstream MCP servers.

### 2.2 Core Value Proposition

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         WITHOUT ThoughtGate                                 │
│                                                                             │
│   Agent ──────────────────────────────────────────────────────► MCP Server  │
│          "delete_user(id=12345)"                           (executes!)      │
│                                                                             │
│   Problem: Agent autonomously executes dangerous operations                 │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                          WITH ThoughtGate                                   │
│                                                                             │
│   Agent ───► ThoughtGate ───► Approver ───► ThoughtGate ───► MCP Server    │
│              (intercepts)     (approves)    (executes)                      │
│                                                                             │
│   Benefit: Dangerous operations require approval                            │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2.3 Key Capabilities

| Capability | Description | Requirement |
|------------|-------------|-------------|
| **YAML Configuration** | Single config file defines sources, rules, workflows | REQ-CFG-001 |
| **YAML Governance Rules** | First-line routing via simple rules (forward/deny/approve/policy) | REQ-CFG-001 |
| **Cedar Policy Engine** | Complex policy evaluation when YAML delegates via `action: policy` | REQ-POL-001 |
| **Request Forwarding** | Forward approved requests to upstream, pass responses through | REQ-CORE-003 |
| **Approval Workflows** | Human/agent approval via Slack (extensible to A2A, webhooks) | REQ-GOV-001/002/003 |
| **SEP-1686 Tasks** | Async task semantics for long-running approvals | REQ-GOV-001 |

### 2.4 The 4-Gate Decision Model

Every `tools/call` request passes through up to 4 gates:

| Gate | Component | Purpose | Outcome |
|------|-----------|---------|---------|
| 1 | **Visibility** | Hide tools from agents | Visible → continue; Hidden → -32015 |
| 2 | **Governance Rules** | Route by YAML rules | forward/deny/approve/policy |
| 3 | **Cedar Policy** | Complex policy decisions | Permit → continue; Forbid → -32003 |
| 4 | **Approval Workflow** | Human/A2A approval | Approved → forward; Rejected → -32007 |

Not all gates are executed for every request:
- `action: forward` skips Gates 3 and 4
- `action: deny` stops at Gate 2
- `action: approve` skips Gate 3
- `action: policy` executes Gate 3, then Gate 4 if permitted

## 3. Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                              ThoughtGate Sidecar (v0.2)                          │
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                     REQ-CORE-003: MCP Transport                         │   │
│  │                                                                         │   │
│  │   ┌─────────────┐    ┌─────────────┐    ┌─────────────────────────┐    │   │
│  │   │  HTTP+SSE   │    │  JSON-RPC   │    │    Method Router        │    │   │
│  │   │  Listener   │───►│   Parser    │───►│                         │    │   │
│  │   │  (Port 8080)│    │             │    │  tools/call → 4-Gate    │    │   │
│  │   └─────────────┘    └─────────────┘    │  tasks/*    → TaskMgr   │    │   │
│  │                                         │  other      → Upstream  │    │   │
│  │                                         └───────────┬─────────────┘    │   │
│  └─────────────────────────────────────────────────────┼──────────────────┘   │
│                                                        │                       │
│  ┌─────────────────────────────────────────────────────┼──────────────────┐   │
│  │                     4-GATE DECISION FLOW            │                  │   │
│  │                                                     ▼                  │   │
│  │  ┌─────────────────────────────────────────────────────────────────┐  │   │
│  │  │ GATE 1: Visibility (REQ-CFG-001)                                │  │   │
│  │  │   expose: allowlist | blocklist | all                           │  │   │
│  │  │   If NOT visible → -32015 Tool Not Exposed                      │  │   │
│  │  └────────────────────────────┬────────────────────────────────────┘  │   │
│  │                               │                                       │   │
│  │                               ▼                                       │   │
│  │  ┌─────────────────────────────────────────────────────────────────┐  │   │
│  │  │ GATE 2: Governance Rules (REQ-CFG-001)                          │  │   │
│  │  │   governance.rules: first match wins                            │  │   │
│  │  │     action: forward → Skip to upstream                          │  │   │
│  │  │     action: deny    → -32014 Governance Rule Denied             │  │   │
│  │  │     action: approve → Skip to Gate 4                            │  │   │
│  │  │     action: policy  → Continue to Gate 3                        │  │   │
│  │  └────────────────────────────┬────────────────────────────────────┘  │   │
│  │                               │                                       │   │
│  │            ┌──────────────────┴───────────────────┐                   │   │
│  │            │ action: policy                       │                   │   │
│  │            ▼                                      ▼                   │   │
│  │  ┌─────────────────────────────┐       ┌─────────────────────────┐   │   │
│  │  │ GATE 3: Cedar Policy        │       │ action: approve         │   │   │
│  │  │ (REQ-POL-001)               │       │ (skip Gate 3)           │   │   │
│  │  │                             │       └────────────┬────────────┘   │   │
│  │  │ policy_id from YAML rule    │                    │                │   │
│  │  │ Permit → Gate 4             │                    │                │   │
│  │  │ Forbid → -32003 Denied      │                    │                │   │
│  │  └────────────────┬────────────┘                    │                │   │
│  │                   │                                 │                │   │
│  │                   └───────────────┬─────────────────┘                │   │
│  │                                   │                                  │   │
│  │                                   ▼                                  │   │
│  │  ┌─────────────────────────────────────────────────────────────────┐  │   │
│  │  │ GATE 4: Approval Workflow (REQ-GOV-001/002/003)                  │  │   │
│  │  │   approval: workflow from YAML rule (or "default")              │  │   │
│  │  │   Approved → Forward to upstream                                │  │   │
│  │  │   Rejected → -32007 Approval Rejected                           │  │   │
│  │  │   Timeout  → on_timeout action (deny by default)                │  │   │
│  │  └────────────────────────────┬────────────────────────────────────┘  │   │
│  │                               │                                       │   │
│  └───────────────────────────────┼───────────────────────────────────────┘   │
│                                  │                                           │
│                                  ▼                                           │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │                         UPSTREAM MCP SERVER                              │ │
│  │                         (from sources[0].url)                            │ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## 4. Request Lifecycle

### 4.1 Request Lifecycle (v0.2 - 4-Gate Flow)

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                     REQUEST LIFECYCLE (v0.2 - 4-Gate Flow)                       │
│                                                                                 │
│   1. MCP Request Received                                                       │
│      • POST /mcp/v1 with JSON-RPC payload                                       │
│      • Generate correlation_id (UUID v4)                                        │
│      • Parse method and params                                                  │
│                                                                                 │
│   2. Method Routing                                                             │
│      • tools/call    → 4-Gate Decision Flow                                     │
│      • tools/list    → Filter by visibility (Gate 1), forward rest              │
│      • tasks/*       → Task Manager (SEP-1686)                                  │
│      • other         → Pass through to upstream                                 │
│                                                                                 │
│   3. Gate 1: Visibility Check                                                   │
│      • Load source expose config from YAML                                      │
│      • Check if tool matches allowlist/blocklist                                │
│      • If NOT visible: Return -32015 (Tool Not Exposed)                         │
│                                                                                 │
│   4. Gate 2: Governance Rules                                                   │
│      • Iterate governance.rules in order                                        │
│      • First matching pattern wins                                              │
│      • action: forward → Skip to step 7                                         │
│      • action: deny    → Return -32014 (Governance Rule Denied)                 │
│      • action: approve → Skip to step 6 (Gate 4)                                │
│      • action: policy  → Continue to step 5 (Gate 3)                            │
│      • No match        → Use governance.defaults.action                         │
│                                                                                 │
│   5. Gate 3: Cedar Policy (only if action: policy)                              │
│      • Build CedarRequest with:                                                 │
│        - Principal (from K8s identity)                                          │
│        - Resource (tool name, arguments)                                        │
│        - Context (policy_id from YAML rule)                                     │
│      • Evaluate Cedar policies                                                  │
│      • Forbid → Return -32003 (Policy Denied)                                   │
│      • Permit → Continue to step 6                                              │
│                                                                                 │
│   6. Gate 4: Approval Workflow (if action: approve OR Cedar Permit)             │
│      • Load workflow config from approval.<workflow_name>                       │
│      • Post approval request to destination (Slack)                             │
│      • v0.2 Blocking Mode: Hold HTTP connection                                 │
│      • Poll for decision (reaction-based)                                       │
│      • Approved → Continue to step 7                                            │
│      • Rejected → Return -32007 (Approval Rejected)                             │
│      • Timeout  → Execute on_timeout action (default: deny)                     │
│                                                                                 │
│   7. Forward to Upstream                                                        │
│      • Load upstream URL from sources[0].url                                    │
│      • Forward request to MCP server                                            │
│      • Return response to agent                                                 │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘

Response handling is the same for all paths: pass it through.
No response inspection or streaming distinction in v0.2.
```

### 4.2 Approval Flow (v0.2 Blocking Mode)

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                     v0.2 BLOCKING APPROVAL FLOW                                 │
│                                                                                 │
│   Agent                    ThoughtGate                    Human (Slack)         │
│     │                           │                              │                │
│     │  tools/call               │                              │                │
│     │  {"name":"delete_user"}   │                              │                │
│     │ ─────────────────────────►│                              │                │
│     │                           │                              │                │
│     │              ┌────────────┴────────────┐                 │                │
│     │              │ Gate 2: action: approve │                 │                │
│     │              │ Hold HTTP connection    │                 │                │
│     │              └────────────┬────────────┘                 │                │
│     │                           │                              │                │
│     │                           │    Slack Message             │                │
│     │                           │ ────────────────────────────►│                │
│     │                           │    (approval request)        │                │
│     │                           │                              │                │
│     │         (HTTP connection held open)                      │                │
│     │              ...          │         (human reviews)      │                │
│     │                           │              ...             │                │
│     │                           │                              │                │
│     │                           │    Reaction/Button           │                │
│     │                           │◄─────────────────────────────│                │
│     │                           │    (👍 approve / 👎 reject)  │                │
│     │                           │                              │                │
│     │              ┌────────────┴────────────┐                 │                │
│     │              │ Check: Client connected?│                 │                │
│     │              └────────────┬────────────┘                 │                │
│     │                           │                              │                │
│     │              ┌────────────┴────────────┐                 │                │
│     │              │ On Approve:             │                 │                │
│     │              │   Forward to Upstream   │                 │                │
│     │              │   Return response       │                 │                │
│     │              │ On Reject:              │                 │                │
│     │              │   Return -32007 error   │                 │                │
│     │              │ On Timeout:             │                 │                │
│     │              │   Return -32008 error   │                 │                │
│     │              └────────────┬────────────┘                 │                │
│     │                           │                              │                │
│     │  {"result": ...}          │                              │                │
│     │◄──────────────────────────│                              │                │
│     │  (or error)               │                              │                │
│     │                           │                              │                │
│     ▼                           ▼                              ▼                │
└─────────────────────────────────────────────────────────────────────────────────┘

From client perspective: Slow tool call, but standard JSON-RPC response.
No SEP-1686 task polling required in v0.2 blocking mode.
```

## 5. Component Integration Matrix

### 5.1 Requirement Dependencies (v0.2)

| Requirement | Depends On | Provides To | v0.2 Status |
|-------------|------------|-------------|-------------|
| REQ-CFG-001 (Configuration) | — | All | **Active** |
| REQ-CORE-001 (Zero-Copy Streaming) | — | REQ-CORE-003 | **Deferred** |
| REQ-CORE-002 (Buffered Inspection) | — | REQ-CORE-003, REQ-GOV-002 | **Deferred** |
| REQ-CORE-003 (MCP Transport) | REQ-CFG-001, REQ-POL-001 | All actions | Active |
| REQ-CORE-004 (Error Handling) | — | All | Active |
| REQ-CORE-005 (Lifecycle) | REQ-CFG-001 | All | Active |
| REQ-POL-001 (Cedar Policy) | REQ-CFG-001 | REQ-CORE-003 | Active (Gate 3) |
| REQ-GOV-001 (Task Lifecycle) | REQ-CFG-001, REQ-CORE-003 | REQ-GOV-002, REQ-GOV-003 | Active (blocking mode) |
| REQ-GOV-002 (Execution Pipeline) | REQ-CFG-001, REQ-POL-001, REQ-GOV-001 | REQ-GOV-003 | Active (simplified) |
| REQ-GOV-003 (Approval Integration) | REQ-CFG-001, REQ-GOV-001, REQ-GOV-002 | External systems | Active |

**v0.2 Key Changes from v0.1:**
- **REQ-CFG-001** is the new configuration requirement (YAML-based)
- **REQ-POL-001** (Cedar) is now Gate 3 only—invoked when `action: policy`
- YAML governance rules handle most routing decisions

### 5.2 Data Flow Between Components (v0.2)

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                            DATA FLOW (v0.2 - 4-Gate)                             │
│                                                                                 │
│   REQ-CORE-003         REQ-CFG-001                                              │
│   ─────────────        ─────────────                                            │
│   McpRequest    ──────► GovernanceEngine                                        │
│   {method,              {config,                                                │
│    params,               rules}                                                 │
│    id}                       │                                                  │
│                              │                                                  │
│            ┌─────────────────┼─────────────────┬─────────────────┐              │
│            │                 │                 │                 │              │
│            ▼                 ▼                 ▼                 ▼              │
│   ┌────────────────┐ ┌────────────────┐ ┌────────────────┐ ┌──────────────┐    │
│   │    FORWARD     │ │     DENY       │ │    APPROVE     │ │    POLICY    │    │
│   │                │ │                │ │                │ │              │    │
│   │ Skip all gates │ │ REQ-CORE-004   │ │ Skip Gate 3    │ │ Gate 3       │    │
│   │ Send upstream  │ │ Return -32014  │ │ Go to Gate 4   │ │ Then Gate 4  │    │
│   └───────┬────────┘ └────────────────┘ └───────┬────────┘ └──────┬───────┘    │
│           │                                     │                 │            │
│           │                                     │                 ▼            │
│           │                                     │    ┌─────────────────────┐   │
│           │                                     │    │ REQ-POL-001         │   │
│           │                                     │    │ CedarEngine         │   │
│           │                                     │    │                     │   │
│           │                                     │    │ CedarRequest {      │   │
│           │                                     │    │   principal,        │   │
│           │                                     │    │   resource,         │   │
│           │                                     │    │   context: {        │   │
│           │                                     │    │     policy_id       │   │
│           │                                     │    │   }                 │   │
│           │                                     │    │ }                   │   │
│           │                                     │    │        │            │   │
│           │                                     │    │   Permit│Forbid     │   │
│           │                                     │    └────────┼────────────┘   │
│           │                                     │             │                │
│           │                                     │    ┌────────┴────────┐       │
│           │                                     │    │                 │       │
│           │                                     │    ▼                 ▼       │
│           │                                     │ Permit           Forbid      │
│           │                                     │    │           -32003        │
│           │                                     │    │                         │
│           │                   ┌─────────────────┴────┘                         │
│           │                   │                                                │
│           │                   ▼                                                │
│           │    ┌─────────────────────────────┐                                 │
│           │    │ REQ-GOV-001/002/003         │                                 │
│           │    │ Approval Workflow (Gate 4)  │                                 │
│           │    │                             │                                 │
│           │    │ Load workflow config        │                                 │
│           │    │ Post to Slack               │                                 │
│           │    │ Poll for decision           │                                 │
│           │    │                             │                                 │
│           │    │ Approved → Forward          │                                 │
│           │    │ Rejected → -32007           │                                 │
│           │    │ Timeout  → on_timeout       │                                 │
│           │    └─────────────┬───────────────┘                                 │
│           │                  │                                                 │
│           └──────────────────┴─────────────────────────────────────────────┐   │
│                                                                            │   │
│                                                                            ▼   │
│                              ┌─────────────────────────────────────────────────┐│
│                              │            McpResponse                          ││
│                              │            {result|error}                       ││
│                              │                                                 ││
│                              │  Responses are passed through directly.         ││
│                              │  No inspection or streaming distinction.        ││
│                              └─────────────────────────────────────────────────┘│
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 5.3 Interface Contracts Summary (v0.2)

| From | To | Interface | Data |
|------|-----|-----------|------|
| CORE-003 | CFG-001 | `Config::load()` | Path → `Config` |
| CORE-003 | CFG-001 | `GovernanceEngine::evaluate()` | tool_name, source_id → `GateResult` |
| CFG-001 | POL-001 | `CedarEngine::evaluate()` | `CedarRequest` → `CedarDecision` |
| CFG-001 | GOV-003 | `ApprovalEngine::request_and_wait()` | request, workflow → `ApprovalDecision` |
| CORE-003 | GOV-001 | `TaskManager::handle()` | SEP-1686 methods |
| GOV-003 | GOV-001 | `TaskManager::record_approval()` | `ApprovalCallback` |
| GOV-003 | External | Slack message | Approval request |
| External | GOV-003 | Reaction/polling | Approval decision |

**Key Interface Changes from v0.1:**
- Added `GovernanceEngine::evaluate()` for YAML rule matching
- Cedar now returns `CedarDecision::Permit|Forbid` (not `Forward|Approve|Reject`)
- `ApprovalEngine` loads workflow config from YAML

## 6. Configuration

### 6.1 Configuration File (v0.2)

ThoughtGate v0.2 uses YAML configuration as the primary configuration method. Environment variables serve as overrides.

**Configuration File Location (Priority Order):**
1. CLI flag: `--config <path>`
2. Environment variable: `$THOUGHTGATE_CONFIG`
3. Default: `/etc/thoughtgate/config.yaml`
4. Fallback: `./config.yaml`

### 6.2 Configuration Schema

```yaml
schema: 1  # Required - config format version

sources:
  - id: upstream                    # Unique source ID
    kind: mcp                       # v0.2: only "mcp" supported
    url: http://mcp-server:8080     # Upstream MCP server URL
    timeout: 30s                    # Request timeout
    expose:                         # Optional: visibility filter (Gate 1)
      mode: all                     # all | allowlist | blocklist
      tools: []                     # Patterns for allowlist/blocklist

governance:
  defaults:
    action: forward                 # Default action: forward | deny | approve | policy
  
  rules:                            # Gate 2: evaluated in order, first match wins
    - match: "delete_*"             # Glob pattern
      action: approve               # Action for matching tools
      approval: default             # Workflow name (optional, defaults to "default")
    
    - match: "transfer_*"
      action: policy                # Delegate to Cedar (Gate 3)
      policy_id: financial          # Required for action: policy
      approval: finance             # Workflow if Cedar permits (Gate 4)
    
    - match: "read_*"
      action: forward               # Skip gates 3 & 4
    
    - match: "admin_*"
      action: deny                  # Block entirely

approval:                           # Gate 4: workflow definitions
  default:
    destination:
      type: slack
      channel: "#approvals"
      token_env: SLACK_BOT_TOKEN    # Env var containing bot token
      mention:                      # Optional: users/groups to @mention
        - "@oncall"
    timeout: 10m                    # Approval timeout
    on_timeout: deny                # Action on timeout: deny
  
  finance:
    destination:
      type: slack
      channel: "#finance-approvals"
      token_env: SLACK_FINANCE_BOT_TOKEN
    timeout: 30m
    on_timeout: deny

cedar:                              # Gate 3: Cedar policy configuration
  policies:                         # Policy file paths
    - /etc/thoughtgate/policies/financial.cedar
    - /etc/thoughtgate/policies/time_restrictions.cedar
  schema: /etc/thoughtgate/schema.cedarschema  # Optional schema validation
```

### 6.3 Environment Variable Overrides

Environment variables can override specific settings but cannot define complex structures like rules or workflows.

#### Core Settings

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTGATE_CONFIG` | `/etc/thoughtgate/config.yaml` | Config file path |
| `THOUGHTGATE_PORT` | `8080` | Listen port |
| `THOUGHTGATE_LOG_LEVEL` | `info` | Log level |
| `THOUGHTGATE_LOG_FORMAT` | `json` | Log format (json/pretty) |

#### Cedar Policy Engine (REQ-POL-001)

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTGATE_POLICY_RELOAD_INTERVAL_SECS` | `10` | Hot-reload interval |
| `THOUGHTGATE_DEV_MODE` | `false` | Enable dev mode (permissive) |
| `THOUGHTGATE_DEV_PRINCIPAL` | `dev-app` | Dev mode principal name |
| `THOUGHTGATE_DEV_NAMESPACE` | `development` | Dev mode namespace |

#### Task Management (REQ-GOV-001)

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTGATE_TASK_DEFAULT_TTL_SECS` | `600` | Default task TTL (10 min) |
| `THOUGHTGATE_TASK_MAX_TTL_SECS` | `86400` | Max task TTL (24 hr) |
| `THOUGHTGATE_TASK_CLEANUP_INTERVAL_SECS` | `60` | Cleanup interval |
| `THOUGHTGATE_TASK_MAX_PENDING_PER_PRINCIPAL` | `10` | Per-principal limit |
| `THOUGHTGATE_TASK_MAX_PENDING_GLOBAL` | `1000` | Global pending limit |

#### Execution Pipeline (REQ-GOV-002)

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTGATE_APPROVAL_VALIDITY_SECS` | `300` | Approval validity window |
| `THOUGHTGATE_TRANSFORM_DRIFT_MODE` | `strict` | `strict` or `permissive` |
| `THOUGHTGATE_EXECUTION_TIMEOUT_SECS` | `30` | Execution timeout |

#### Approval Integration (REQ-GOV-003)

| Variable | Default | Description |
|----------|---------|-------------|
| `SLACK_BOT_TOKEN` | (from config) | Slack Bot OAuth token |
| `THOUGHTGATE_APPROVAL_POLL_INTERVAL_SECS` | `5` | Base poll interval |
| `THOUGHTGATE_APPROVAL_POLL_MAX_INTERVAL_SECS` | `30` | Max poll interval (with backoff) |
| `THOUGHTGATE_SLACK_RATE_LIMIT_PER_SEC` | `1` | Slack API rate limit |
| `THOUGHTGATE_MAX_CONCURRENT_POLLS` | `100` | Max concurrent polling tasks |
| `SLACK_APPROVE_REACTION` | `+1` | Reaction emoji for approval (👍) |
| `SLACK_REJECT_REACTION` | `-1` | Reaction emoji for rejection (👎) |

#### Operational (REQ-CORE-005)

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTGATE_SHUTDOWN_TIMEOUT_SECS` | `30` | Graceful shutdown timeout |
| `THOUGHTGATE_DRAIN_TIMEOUT_SECS` | `25` | Connection drain timeout |
| `THOUGHTGATE_STARTUP_TIMEOUT_SECS` | `15` | Startup timeout |
| `THOUGHTGATE_REQUIRE_UPSTREAM_AT_STARTUP` | `false` | Fail if upstream unreachable |
| `THOUGHTGATE_UPSTREAM_HEALTH_INTERVAL_SECS` | `30` | Upstream health check interval |

## 7. Scope (v0.2)

### 7.1 In Scope

| Component | Status | Notes |
|-----------|--------|-------|
| **YAML Configuration** | Draft | Primary configuration method |
| **4-Gate Decision Flow** | Draft | Visibility → Governance → Cedar → Approval |
| **Sidecar Deployment** | Target | K8s pod co-location with agent |
| MCP Transport (JSON-RPC, HTTP+SSE) | Draft | Single upstream |
| YAML Governance Rules | Draft | First-line routing (forward/deny/approve/policy) |
| Cedar Policy Engine | Draft | Gate 3 - complex policy decisions |
| Request Forwarding | Draft | Pass requests to upstream, return responses |
| Blocking Approval Mode | Draft | Hold connection until approval (v0.2) |
| Slack Integration | Draft | Polling-based approval |
| Health/Readiness | Draft | K8s probes |
| Config Hot-Reload | Draft | Reload YAML without restart |
| Graceful Shutdown | Draft | Connection draining |
| Principal from Pod Labels | Draft | K8s downward API for identity |

### 7.2 Out of Scope (v0.2)

| Feature | Notes |
|---------|-------|
| **Zero-Copy Streaming (Green Path)** | Deferred until response streaming/inspection needed |
| **Buffered Inspection (Amber Path)** | Deferred until request/response inspection needed |
| **SEP-1686 Task Polling** | v0.2 uses blocking mode; async tasks deferred |
| **Gateway Deployment Mode** | Centralized proxy for multiple agents; requires auth |
| **CLI Wrapper** | `thoughtgate wrap -- command` for local dev |
| **Agent Authentication** | API keys, mTLS, JWT for gateway mode |
| Persistent Task Storage | Tasks are in-memory only; lost on restart |
| Teams Adapter | Slack only in v0.2 |
| PagerDuty Adapter | Slack only in v0.2 |
| A2A Approval Protocol | Agent-to-agent approval not supported |
| Multi-Upstream Routing | Single upstream MCP server only |
| Web Dashboard | No approval UI; Slack only |
| CRD-based Policy | YAML config only |

### 7.3 Known Limitations

| Limitation | Impact | Mitigation |
|------------|--------|------------|
| In-memory task storage | Tasks lost on restart | Clients retry on 404 |
| Single upstream | Can't route to multiple MCP servers | Deploy multiple instances |
| Per-component metrics | No unified tracing | Correlation IDs in logs |
| Slack rate limits | Burst approval requests queue | Outbound rate limiter |

## 8. Deployment Architecture

### 8.1 Deployment Model (v0.2)

**ThoughtGate v0.2 is designed exclusively for sidecar deployment in Kubernetes.**

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        v0.2 DEPLOYMENT MODEL: SIDECAR                           │
│                                                                                 │
│   ┌─────────────────────────────────────────────────────────────────────────┐  │
│   │                         Kubernetes Pod                                  │  │
│   │                                                                         │  │
│   │   ┌─────────────────┐           ┌─────────────────────┐                │  │
│   │   │                 │  localhost │                     │                │  │
│   │   │    AI Agent     │───────────►│    ThoughtGate     │────────────────┼──┼──► MCP Server
│   │   │   Container     │   :8080    │      Sidecar       │                │  │
│   │   │                 │            │                     │                │  │
│   │   └─────────────────┘            └─────────────────────┘                │  │
│   │                                                                         │  │
│   │   Identity: Inferred from pod labels / K8s downward API                │  │
│   │   Auth: None required (localhost implicit trust)                        │  │
│   │                                                                         │  │
│   └─────────────────────────────────────────────────────────────────────────┘  │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

**Design Rationale:**
| Aspect | Sidecar Advantage |
|--------|-------------------|
| **Latency** | Localhost communication; no network hop |
| **Identity** | Implicit from K8s pod metadata; no auth headers |
| **Isolation** | Per-agent failure domain; no blast radius |
| **Security** | No credentials in agent code |
| **Alignment** | Matches service mesh patterns (Envoy, Istio) |

### 8.2 Kubernetes Deployment

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: agent-with-thoughtgate
  labels:
    app: my-agent
    thoughtgate.io/principal: "my-agent"      # Used for identity
    thoughtgate.io/namespace: "production"    # Used for policy scoping
spec:
  containers:
  - name: agent
    image: my-agent:latest
    env:
    - name: MCP_SERVER_URL
      value: "http://localhost:8080"  # Points to sidecar
      
  - name: thoughtgate
    image: thoughtgate:v0.2
    ports:
    - containerPort: 8080
    env:
    - name: SLACK_BOT_TOKEN
      valueFrom:
        secretKeyRef:
          name: thoughtgate-secrets
          key: slack-bot-token
    volumeMounts:
    - name: config
      mountPath: /etc/thoughtgate
      readOnly: true
    - name: pod-info
      mountPath: /etc/podinfo
      readOnly: true
      
  volumes:
  - name: config
    configMap:
      name: thoughtgate-config
  - name: pod-info
    downwardAPI:
      items:
      - path: "labels"
        fieldRef:
          fieldPath: metadata.labels
      - path: "namespace"
        fieldRef:
          fieldPath: metadata.namespace
```

### 8.3 Principal Inference (Sidecar Mode)

In sidecar mode, ThoughtGate infers the Principal from Kubernetes metadata:

```
┌─────────────────────────────────────────────────────────────────┐
│                    PRINCIPAL INFERENCE                           │
│                                                                  │
│   Source Priority:                                               │
│   1. Pod label: thoughtgate.io/principal                        │
│   2. Pod label: app.kubernetes.io/name                          │
│   3. Pod label: app                                              │
│   4. Fallback: "unknown"                                         │
│                                                                  │
│   Namespace:                                                     │
│   1. Pod label: thoughtgate.io/namespace                        │
│   2. Actual K8s namespace from downward API                      │
│                                                                  │
│   Result: Principal { namespace: "production", app: "my-agent" }│
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## 9. Security Model

### 9.1 Trust Model (Sidecar)

ThoughtGate v0.2's security model is built around the **sidecar trust assumption**: the agent and ThoughtGate share a pod, so localhost communication is implicitly trusted.

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           TRUST BOUNDARIES                                      │
│                                                                                 │
│   ┌─────────────────────────────────────────────────────────────────────────┐  │
│   │                        TRUSTED ZONE (Pod)                               │  │
│   │                                                                         │  │
│   │   ┌─────────────┐         ┌─────────────────────┐                      │  │
│   │   │    Agent    │ ◄──────► │    ThoughtGate     │                      │  │
│   │   │  Container  │localhost│      Sidecar       │                      │  │
│   │   └─────────────┘         └──────────┬──────────┘                      │  │
│   │                                      │                                  │  │
│   │   Implicit trust: No authentication required                           │  │
│   │   Identity: From pod labels (not from request)                         │  │
│   │                                                                         │  │
│   └──────────────────────────────────────┼──────────────────────────────────┘  │
│                                          │                                     │
│                           ═══════════════╪═══════════════                      │
│                           NETWORK BOUNDARY (mTLS/NetworkPolicy)                │
│                           ═══════════════╪═══════════════                      │
│                                          │                                     │
│                                          ▼                                     │
│   ┌─────────────────────────────────────────────────────────────────────────┐  │
│   │                     SEMI-TRUSTED ZONE (Cluster)                         │  │
│   │                                                                         │  │
│   │   ┌─────────────────────┐         ┌─────────────────────┐              │  │
│   │   │     MCP Server      │         │       Slack         │              │  │
│   │   │    (Upstream)       │         │   (Webhooks/HMAC)   │              │  │
│   │   └─────────────────────┘         └─────────────────────┘              │  │
│   │                                                                         │  │
│   └─────────────────────────────────────────────────────────────────────────┘  │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 9.2 Authentication Summary

| Boundary | Authentication | Notes |
|----------|----------------|-------|
| Agent → ThoughtGate | None (localhost) | Implicit trust in sidecar model |
| ThoughtGate → Upstream | TLS | Optional mTLS for zero-trust clusters |
| ThoughtGate → Slack | Bot Token | OAuth token for API access |
| Human → Slack | Slack auth | Slack handles user identity |

## 10. Observability Standards

### 10.1 Correlation ID Strategy

Every request receives a correlation ID that propagates through all components:

**ID Hierarchy:**
| ID Type | Scope | Format | Example |
|---------|-------|--------|---------|
| `correlation_id` | Single request | UUID v4 | `550e8400-e29b-41d4-a716-446655440000` |
| `task_id` | Approval task lifetime | UUID v4 | `7c9e6679-7425-40de-944b-e07fc1f90ae7` |
| `request_id` | JSON-RPC ID | String or Integer | `1` or `"req-123"` |

**Log Field Convention:**
```json
{
  "timestamp": "2025-01-08T10:30:00.123Z",
  "level": "info",
  "correlation_id": "550e8400-e29b-41d4-a716-446655440000",
  "task_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "component": "governance_engine",
  "event": "gate_evaluation",
  "gate": "2",
  "action": "approve",
  "duration_ms": 2
}
```

### 10.2 Metric Naming Convention

All metrics follow the pattern: `thoughtgate_<component>_<metric>_<unit>`

**Components:**
| Prefix | Component | Requirement |
|--------|-----------|-------------|
| `thoughtgate_config_` | Configuration | REQ-CFG-001 |
| `thoughtgate_transport_` | MCP Transport | REQ-CORE-003 |
| `thoughtgate_gate_` | Gate Evaluation | REQ-CORE-003 |
| `thoughtgate_cedar_` | Cedar Policy | REQ-POL-001 |
| `thoughtgate_tasks_` | Task Management | REQ-GOV-001 |
| `thoughtgate_pipeline_` | Execution Pipeline | REQ-GOV-002 |
| `thoughtgate_approval_` | Approval Integration | REQ-GOV-003 |
| `thoughtgate_errors_` | Error Handling | REQ-CORE-004 |
| `thoughtgate_health_` | Operational | REQ-CORE-005 |

### 10.3 Logging Standards

**Log Levels:**
| Level | Usage |
|-------|-------|
| `ERROR` | Unrecoverable failures, panics, data corruption |
| `WARN` | Recoverable failures, policy rejections, timeouts |
| `INFO` | Request lifecycle events, state transitions |
| `DEBUG` | Detailed flow information (disabled in production) |
| `TRACE` | Byte-level details (disabled in production) |

## 11. Error Code Reference

### 11.1 Consolidated Error Codes

| Code | Name | Gate | Description | Source |
|------|------|------|-------------|--------|
| **JSON-RPC Standard** |||||
| -32700 | Parse error | — | Invalid JSON | REQ-CORE-003 |
| -32600 | Invalid Request | — | Invalid JSON-RPC | REQ-CORE-003 |
| -32601 | Method not found | — | Unknown method | REQ-CORE-004 |
| -32602 | Invalid params | — | Invalid method params | REQ-CORE-004 |
| -32603 | Internal error | — | Server error / panic | REQ-CORE-004 |
| **ThoughtGate Custom** |||||
| -32000 | Connection failed | — | Upstream unreachable | REQ-CORE-004 |
| -32001 | Timeout | — | Upstream timeout | REQ-CORE-004 |
| -32002 | Upstream error | — | Upstream returned error | REQ-CORE-004 |
| -32003 | Policy denied | 3 | Cedar policy forbid | REQ-POL-001 |
| -32004 | Task not found | — | Unknown task ID | REQ-GOV-001 |
| -32005 | Task expired | — | Task TTL exceeded | REQ-GOV-001 |
| -32006 | Task cancelled | — | Task was cancelled | REQ-GOV-001 |
| -32007 | Approval rejected | 4 | Approver rejected request | REQ-GOV-003 |
| -32008 | Approval timeout | 4 | Approval window closed | REQ-GOV-003 |
| -32009 | Rate limited | — | Too many requests | REQ-GOV-001 |
| -32010 | Inspection failed | — | Inspector rejected | REQ-CORE-002 |
| -32011 | Policy drift | — | Policy changed post-approval | REQ-GOV-002 |
| -32012 | Transform drift | — | Request changed post-approval | REQ-GOV-002 |
| -32013 | Service unavailable | — | Capacity/concurrency limit | REQ-CORE-005 |
| -32014 | Governance rule denied | 2 | `action: deny` matched | REQ-CFG-001 |
| -32015 | Tool not exposed | 1 | Tool hidden by `expose` config | REQ-CFG-001 |
| -32016 | Configuration error | — | Invalid configuration | REQ-CFG-001 |
| -32017 | Workflow not found | 4 | Approval workflow not defined | REQ-GOV-003 |

## 12. Cross-Cutting Concerns

### 12.1 Batch Request Handling with Approval

When a JSON-RPC batch request contains items that require different paths:

**Policy:** If ANY request in a batch requires Approval, the ENTIRE batch is task-augmented.

**Rationale:** 
- Prevents partial execution that could leave system in inconsistent state
- Human approver sees complete context of what agent is trying to do
- Simplifies client handling (all-or-nothing)

### 12.2 Approval Handling (v0.2 Blocking Mode)

**v0.2 uses blocking mode, NOT SEP-1686 auto-upgrade.**

When a client sends a `tools/call` request and the governance rules require approval:

1. HTTP connection is held open
2. Approval request sent to Slack
3. Wait for human response (with timeout from workflow config)
4. On APPROVE: Execute tool, return normal result
5. On REJECT: Return error -32007 (ApprovalRejected)
6. On TIMEOUT: Execute on_timeout action (default: return -32008)

**Why Blocking Mode for v0.2:**
- Works with ANY MCP client (no SEP-1686 support required)
- Simple implementation for MVP
- Predictable behavior for agents

### 12.3 Configuration Hot-Reload

ThoughtGate watches the config file and reloads on changes:

| Configuration | Hot-Reloadable? | Notes |
|--------------|-----------------|-------|
| governance.rules | ✅ Yes | New rules apply immediately |
| governance.defaults | ✅ Yes | New default applies immediately |
| approval.* workflows | ✅ Yes | New workflows available |
| cedar.policies | ✅ Yes | Cedar handles its own reload |
| sources[].url | ⚠️ Partial | New connections use new URL |
| sources[].expose | ✅ Yes | Visibility changes immediately |

### 12.4 Startup Behavior with Unavailable Upstream

| Scenario | Behavior | Readiness |
|----------|----------|-----------|
| Upstream unreachable at startup | Start anyway | NOT Ready |
| Upstream becomes unreachable | Continue serving | Ready (degraded) |
| Upstream returns after outage | Resume normal | Ready |

## 13. Glossary

| Term | Definition |
|------|------------|
| **MCP** | Model Context Protocol - standard for AI agent tool communication |
| **SEP-1686** | MCP enhancement proposal for task-based async execution |
| **4-Gate Flow** | Decision flow: Visibility → Governance → Cedar → Approval |
| **Gate 1** | Visibility check - is the tool exposed to the agent? |
| **Gate 2** | Governance rules - YAML-based routing decisions |
| **Gate 3** | Cedar policy - complex policy evaluation (when action: policy) |
| **Gate 4** | Approval workflow - human/A2A approval |
| **Cedar** | Policy language by AWS for fine-grained authorization |
| **Principal** | Entity making a request (identified by K8s namespace + app name) |
| **Forward** | Send request directly to upstream (action in YAML) |
| **Approve** | Require human approval before forwarding (action in YAML) |
| **Deny** | Reject request with error (action in YAML) |
| **Policy** | Delegate to Cedar for complex decisions (action in YAML) |
| **Blocking Mode** | v0.2 approval mode: hold HTTP connection until decision |
| **Task** | SEP-1686 entity tracking async request through approval workflow |
| **Sidecar** | Deployment pattern where ThoughtGate runs alongside agent in same pod |
| **Upstream** | The MCP server that ThoughtGate proxies requests to |
| **Workflow** | Named approval configuration (destination, timeout, on_timeout) |

## 14. Quick Reference

### 14.1 Endpoints

| Endpoint | Port | Purpose |
|----------|------|---------|
| `POST /mcp/v1` | 8080 | MCP JSON-RPC |
| `GET /health` | 8080 | Liveness probe |
| `GET /ready` | 8080 | Readiness probe |
| `GET /metrics` | 8080 | Prometheus metrics |

**Note:** No inbound callback endpoints required - approval uses outbound polling model.

### 14.2 Configuration Quick Start

**Minimal config.yaml:**
```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:3000

governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: approve

approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
    timeout: 10m
    on_timeout: deny
```

**Required environment variable:**
```
SLACK_BOT_TOKEN=xoxb-your-bot-token
```

### 14.3 Decision Flow (Quick Reference - v0.2)

```
Request → Parse JSON-RPC → Route by Method
                              │
            ┌─────────────────┼─────────────────┐
            │                 │                 │
            ▼                 ▼                 ▼
       tools/call        tasks/*           other
            │                 │                 │
            ▼                 ▼                 ▼
       4-Gate Flow      Task Handler      Pass Through
            │                                   │
    ┌───────┼───────┬───────────┐               │
    │       │       │           │               │
    ▼       ▼       ▼           ▼               │
  Gate 1  Gate 2  Gate 3     Gate 4             │
  Visible? Rules   Cedar    Approval            │
    │       │       │           │               │
    │   ┌───┴───┐   │           │               │
    │   │       │   │           │               │
    │ forward deny  │           │               │
    │   │       │   │           │               │
    │   │   -32014  │           │               │
    │   │           │           │               │
    │   │      ┌────┴────┐      │               │
    │   │      │         │      │               │
    │   │   permit    forbid    │               │
    │   │      │      -32003    │               │
    │   │      │                │               │
    │   │      └───────┬────────┘               │
    │   │              │                        │
    │   │         ┌────┴────┐                   │
    │   │         │         │                   │
    │   │      approved  rejected               │
    │   │         │      -32007                 │
    │   │         │                             │
    └───┴─────────┴─────────────────────────────┘
                    │
                    ▼
                Upstream
                    │
                    ▼
              Response → Pass through to agent
```

## 15. References

| Resource | Link |
|----------|------|
| SEP-1686 (Tasks) | https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1686 |
| Model Context Protocol | https://modelcontextprotocol.io/ |
| Cedar Policy Language | https://www.cedarpolicy.com/ |
| JSON-RPC 2.0 | https://www.jsonrpc.org/specification |

## 16. Requirement Index

| ID | Title | Status | Type |
|----|-------|--------|------|
| REQ-CFG-001 | Configuration | **Active** | Core |
| REQ-CORE-001 | Zero-Copy Streaming (Green Path) | **Deferred** | Core |
| REQ-CORE-002 | Buffered Inspection (Amber Path) | **Deferred** | Core |
| REQ-CORE-003 | MCP Transport & Routing | Draft | Core |
| REQ-CORE-004 | Error Handling | Draft | Core |
| REQ-CORE-005 | Operational Lifecycle | Draft | Core |
| REQ-POL-001 | Cedar Policy Engine (Gate 3) | Draft | Policy |
| REQ-GOV-001 | Task Lifecycle & SEP-1686 | Draft (blocking mode) | Governance |
| REQ-GOV-002 | Approval Execution Pipeline | Draft (simplified) | Governance |
| REQ-GOV-003 | Approval Integration (Gate 4) | Draft | Governance |
