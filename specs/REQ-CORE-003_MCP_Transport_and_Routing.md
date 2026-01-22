# REQ-CORE-003: MCP Transport & Routing

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-CORE-003` |
| **Title** | MCP Transport & Routing |
| **Type** | Core Mechanic |
| **Status** | Draft |
| **Priority** | **Critical** |
| **Tags** | `#mcp` `#transport` `#json-rpc` `#routing` `#governance` `#4-gate` |

## 1. Context & Decision Rationale

This requirement defines the **entry point** for ThoughtGate—how MCP messages are received, parsed, and routed through the 4-gate decision flow. ThoughtGate sits between MCP hosts (agents) and MCP servers (tools), acting as an **Application Layer Gateway** with policy enforcement.

### 1.1 Protocol Foundation

- MCP uses JSON-RPC 2.0 over Streamable HTTP (POST with SSE response) or stdio
- SEP-1686 extends MCP with task-based async execution
- ThoughtGate must be protocol-transparent for non-intercepted methods

### 1.2 Architectural Position

```
┌─────────────┐     ┌─────────────────────────────────────────┐     ┌─────────────┐
│  MCP Host   │────►│           ThoughtGate                   │────►│  MCP Server │
│  (Agent)    │◄────│  [REQ-CORE-003: This Requirement]       │◄────│  (Tools)    │
└─────────────┘     └─────────────────────────────────────────┘     └─────────────┘
```

### 1.3 The 4-Gate Decision Flow

ThoughtGate routes requests through up to 4 gates:

```
  Agent Request (tool + arguments)
         │
         ▼
  ┌──────────────────────────────────────────────────────────────┐
  │ GATE 1: Visibility (REQ-CFG-001: expose config)              │
  │   If tool NOT exposed → 404 Method Not Found                 │
  └──────────────────────────────────────────────────────────────┘
         │
         │ Tool is visible
         ▼
  ┌──────────────────────────────────────────────────────────────┐
  │ GATE 2: Governance Rules (REQ-CFG-001: governance.rules)     │
  │   First matching rule determines action:                     │
  │   - forward → Skip to Forward                                │
  │   - deny → Reject immediately                                │
  │   - approve → Skip to Gate 4                                 │
  │   - policy → Continue to Gate 3                              │
  └──────────────────────────────────────────────────────────────┘
         │
         │ action: policy
         ▼
  ┌──────────────────────────────────────────────────────────────┐
  │ GATE 3: Cedar Policy (REQ-POL-001)                           │
  │   Cedar evaluates policy_id with arguments:                  │
  │   - Permit → Continue to Gate 4                              │
  │   - Forbid → Reject immediately                              │
  └──────────────────────────────────────────────────────────────┘
         │
         │ action: approve OR Cedar Permit
         ▼
  ┌──────────────────────────────────────────────────────────────┐
  │ GATE 4: Approval Workflow (REQ-GOV-001, REQ-GOV-003)         │
  │   Human/A2A approval via configured workflow:                │
  │   - Approved → Forward to upstream                           │
  │   - Rejected → Reject                                        │
  │   - Timeout → on_timeout action                              │
  └──────────────────────────────────────────────────────────────┘
         │
         ▼
  ┌──────────────────────────────────────────────────────────────┐
  │ FORWARD TO UPSTREAM                                          │
  │   Execute tool call, return result                           │
  └──────────────────────────────────────────────────────────────┘
```

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-CFG-001 | **Receives from** | Configuration (sources, governance rules, expose config) |
| REQ-POL-001 | **Invokes** | Cedar policy evaluation (Gate 3) |
| REQ-CORE-004 | **Provides to** | Error responses formatted per spec |
| REQ-CORE-005 | **Coordinates with** | Lifecycle events (startup, shutdown) |
| REQ-GOV-001 | **Provides to** | Task method handling (`tasks/*`) |
| REQ-GOV-003 | **Invokes** | Approval workflow (Gate 4) |

## 3. Intent

The system must:
1. Accept MCP connections from hosts via HTTP+SSE
2. Parse JSON-RPC 2.0 messages correctly (requests, responses, notifications, batches)
3. Route requests through the 4-gate decision flow
4. Forward requests to upstream MCP servers
5. Correlate responses back to originating requests
6. Support SEP-1686 task-augmented requests

## 4. Scope

### 4.1 In Scope
- JSON-RPC 2.0 parsing and validation
- 4-gate decision flow routing
- HTTP+SSE transport (Streamable HTTP)
- Upstream client (connection, forwarding, response handling)
- Request/response correlation
- SEP-1686 `task` parameter detection
- SSE event streaming for notifications

### 4.2 Out of Scope
- Policy evaluation logic (REQ-POL-001)
- Configuration loading/parsing (REQ-CFG-001)
- Error formatting details (REQ-CORE-004)
- Health endpoints (REQ-CORE-005)
- Task state management (REQ-GOV-001)
- Approval adapter implementation (REQ-GOV-003)
- stdio transport (deferred to future version)

## 5. Constraints

### 5.1 Runtime & Dependencies

| Crate | Purpose | Version |
|-------|---------|---------|
| `tokio` | Async runtime | 1.x (rt-multi-thread) |
| `axum` | HTTP server | 0.7.x |
| `reqwest` | Upstream HTTP client | 0.11.x |
| `serde_json` | JSON parsing | 1.x |
| `uuid` | Request ID generation | 1.x |
| `glob` | Pattern matching for rules | 0.3.x |

### 5.1.1 Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTGATE_OUTBOUND_PORT` | `7467` | Outbound proxy port (MCP traffic) |
| `THOUGHTGATE_ADMIN_PORT` | `7469` | Admin port (health/ready/metrics) |
| `UPSTREAM_URL` | (required) | Upstream server URL (all traffic) |

### 5.2 Protocol Compliance

**JSON-RPC 2.0 Requirements:**
- MUST accept `jsonrpc: "2.0"` field
- MUST handle requests (with `id`), notifications (without `id`), and batches (arrays)
- MUST preserve `id` type (string or integer) in responses
- MUST return proper error codes per JSON-RPC spec

**MCP Streamable HTTP Transport:**
- Client sends: `POST /mcp/v1` with `Content-Type: application/json`
- Server responds with either:
  - `Content-Type: application/json` for simple responses
  - `Content-Type: text/event-stream` for streaming responses
- ThoughtGate MUST support both response types

**SEP-1686 Requirements:**
- MUST detect `task` field in request params
- MUST route task-augmented requests through governance layer
- MUST implement `tasks/*` method family

### 5.3 Connection Management

**3-Port Envoy-Style Architecture:**

ThoughtGate uses a 3-port architecture inspired by Envoy proxy:

| Port | Env Variable | Default | Purpose |
|------|--------------|---------|---------|
| Outbound | `THOUGHTGATE_OUTBOUND_PORT` | 7467 | MCP traffic (agent → upstream) |
| Inbound | (reserved) | 7468 | Future callbacks (not wired in v0.2) |
| Admin | `THOUGHTGATE_ADMIN_PORT` | 7469 | Health, ready, metrics |

**Traffic Flow:**
- Agents connect to the **outbound port** (7467) for MCP requests
- Health probes and metrics are served on the **admin port** (7469)
- The **inbound port** (7468) is reserved for future callback functionality

Upstream server URL is configured via `UPSTREAM_URL` environment variable. This URL is used as the target for **all traffic** in reverse proxy mode.

**Traffic Discrimination:**
ThoughtGate discriminates traffic on the outbound port:
- **MCP traffic**: `POST /mcp/v1` with `Content-Type: application/json` → Buffered inspection via `McpHandler`
- **HTTP passthrough**: All other requests → Zero-copy streaming to `UPSTREAM_URL`

Both traffic types are forwarded to the same upstream URL, but MCP traffic is inspected and subject to policy evaluation.

### 5.4 v0.2 SEP-1686 Mode

In v0.2, approval uses **SEP-1686 task mode**:
- Task ID returned immediately
- Agent polls for status and retrieves result
- Timeout returns error, not task ID

| Scenario | Behavior |
|----------|----------|
| Approval received | Execute tool, update task state to `Completed` |
| Timeout reached | Update task state to `Failed`, execute `on_timeout` action |
| Rejection received | Update task state to `Failed` with rejection reason |
| Task expires (TTL) | Clean up task, log expiry |
| Client polls `tasks/result` | Return current state or block until terminal |

### 5.5 Upstream Client

- **Connection Pooling:** Maintain persistent connections to upstream
- **Retry Policy:** No automatic retry (let caller handle)
- **Timeout:** Per-request timeout from config
- **TLS:** Support HTTPS upstreams, verify certificates by default

## 6. Interfaces

### 6.1 Input: Inbound MCP Request

```
POST /mcp/v1 HTTP/1.1
Content-Type: application/json

{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "delete_user",
    "arguments": { "user_id": "123" },
    "task": { "ttl": 600000 }           // Optional: SEP-1686
  }
}
```

### 6.2 Output: MCP Response

**Success (direct):**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": { ... }
}
```

**Success (task-augmented, per SEP-1686):**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "taskId": "abc-123",
    "status": "working",
    "createdAt": "2025-01-08T10:30:00Z",
    "ttl": 600000,
    "pollInterval": 5000
  }
}
```

**Error:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32003,
    "message": "Policy denied",
    "data": { "correlation_id": "uuid" }
  }
}
```

### 6.3 Internal: Parsed Request Structure

```rust
pub struct McpRequest {
    pub id: Option<JsonRpcId>,           // None for notifications
    pub method: String,
    pub params: Option<serde_json::Value>,
    pub task_metadata: Option<TaskMetadata>,  // SEP-1686
    
    // Internal tracking
    pub received_at: Instant,
    pub correlation_id: Uuid,
}

pub struct TaskMetadata {
    pub ttl: Option<Duration>,
}

pub enum JsonRpcId {
    String(String),
    Number(i64),
}
```

### 6.4 Internal: Gate Routing

```rust
/// Result of 4-gate evaluation
pub enum GateResult {
    /// Forward to upstream immediately
    Forward,
    
    /// Deny with error
    Deny { 
        code: i32, 
        message: String,
        source: DenySource,
    },
    
    /// Require approval (Gate 4)
    Approve {
        workflow: String,
        timeout: Duration,
    },
    
    /// Request is not visible (404)
    NotExposed,
}

pub enum DenySource {
    GovernanceRule,    // action: deny
    CedarPolicy,       // Cedar forbid
    ApprovalRejected,  // Human rejected
    ApprovalTimeout,   // Timeout reached
}
```

### 6.5 Internal: Governance Engine Interface

```rust
/// Governance engine evaluates Gates 1-3
pub trait GovernanceEngine: Send + Sync {
    /// Evaluate request through gates 1-3
    /// Returns GateResult indicating next action
    fn evaluate(
        &self,
        tool_name: &str,
        source_id: &str,
        arguments: &serde_json::Value,
        principal: &Principal,
    ) -> GateResult;
}
```

## 7. Functional Requirements

### F-001: JSON-RPC Parsing

The transport layer MUST parse incoming JSON according to JSON-RPC 2.0:

- **F-001.1:** Accept single request objects
- **F-001.2:** Accept batch requests (JSON arrays)
- **F-001.3:** Detect notifications (requests without `id`)
- **F-001.4:** Preserve `id` type (string or integer) for response correlation
- **F-001.5:** Reject malformed JSON with error code -32700
- **F-001.6:** Reject invalid JSON-RPC structure with error code -32600
- **F-001.7:** Generate `correlation_id` (UUID v4) for each request

### F-002: Method Routing

The transport layer MUST route methods to appropriate handlers:

| Method Pattern | Route To | Notes |
|----------------|----------|-------|
| `tools/call` | Governance Engine (4-gate flow) | Primary interception point |
| `tools/list` | Governance Engine | Filter by visibility (Gate 1) |
| `tasks/get` | Task Handler | SEP-1686 |
| `tasks/result` | Task Handler | SEP-1686 |
| `tasks/list` | Task Handler | SEP-1686 |
| `tasks/cancel` | Task Handler | SEP-1686 |
| `resources/*` | Governance Engine | Subject to policy |
| `prompts/*` | Governance Engine | Subject to policy |
| `*` (unknown) | Pass Through | Forward to upstream |

### F-003: Gate 1 - Visibility Check

```rust
fn check_visibility(tool_name: &str, source: &Source) -> bool {
    let expose = source.expose.as_ref().unwrap_or(&ExposeConfig::All);
    expose.is_visible(tool_name)
}
```

- **F-003.1:** Check tool visibility against `expose` config
- **F-003.2:** Return 404 (-32601) if tool not visible
- **F-003.3:** `expose: all` makes all tools visible (default)
- **F-003.4:** `expose: allowlist` shows only matching patterns
- **F-003.5:** `expose: blocklist` hides matching patterns

### F-004: Gate 2 - Governance Rules

```rust
fn evaluate_governance(tool_name: &str, source_id: &str) -> RuleMatch {
    for rule in &config.governance.rules {
        // Check source filter
        if let Some(filter) = &rule.source {
            if !filter.matches(source_id) {
                continue;
            }
        }
        
        // Check pattern match (first match wins)
        if glob_match(&rule.pattern, tool_name) {
            return RuleMatch {
                action: rule.action,
                policy_id: rule.policy_id.clone(),
                approval_workflow: rule.approval.clone(),
            };
        }
    }
    
    // No match - use default
    RuleMatch {
        action: config.governance.defaults.action,
        policy_id: None,
        approval_workflow: None,
    }
}
```

- **F-004.1:** Match rules in order (first match wins)
- **F-004.2:** Filter by `source` if specified
- **F-004.3:** Use glob patterns for matching
- **F-004.4:** Fall through to `governance.defaults.action`
- **F-004.5:** Return action + policy_id + approval workflow

### F-005: Gate 3 - Cedar Policy

Invoked only when `action: policy`:

```rust
if rule_match.action == Action::Policy {
    let policy_id = rule_match.policy_id.expect("required");
    
    let cedar_request = CedarRequest {
        principal,
        resource: Resource::ToolCall {
            name: tool_name.to_string(),
            server: source_id.to_string(),
            arguments: arguments.clone(),
        },
        context: CedarContext {
            policy_id,
            source_id: source_id.to_string(),
            time: current_time_context(),
        },
    };
    
    match cedar_engine.evaluate(&cedar_request) {
        CedarDecision::Permit => {
            // Continue to Gate 4
            GateResult::Approve {
                workflow: rule_match.approval_workflow.unwrap_or("default".into()),
                timeout: get_workflow_timeout(&rule_match.approval_workflow),
            }
        }
        CedarDecision::Forbid { reason, .. } => {
            GateResult::Deny {
                code: -32003,
                message: "Policy denied".into(),
                source: DenySource::CedarPolicy,
            }
        }
    }
}
```

- **F-005.1:** Build CedarRequest with tool arguments
- **F-005.2:** Pass `policy_id` from YAML rule
- **F-005.3:** Cedar Permit → continue to approval
- **F-005.4:** Cedar Forbid → deny immediately

### F-006: Gate 4 - Approval Workflow (SEP-1686)

Invoked when `action: approve` or Cedar permits. Returns Task ID immediately.

```rust
async fn execute_approval(
    request: &McpRequest,
    workflow: &str,
    timeout: Duration,
    task_manager: Arc<TaskManager>,
) -> Result<McpResponse, ThoughtGateError> {
    let workflow_config = config.approval.get(workflow)
        .ok_or(ConfigError::UndefinedWorkflow)?;
    
    // v0.2: SEP-1686 async mode - return Task ID immediately
    // start_approval spawns a background poller and returns immediately
    let task_id = approval_engine
        .start_approval(request, workflow, task_manager.clone())
        .await?;
    
    // Return SEP-1686 task response immediately
    // Client will poll via tasks/get and retrieve result via tasks/result
    Ok(McpResponse::task_created(task_id, TaskStatus::InputRequired))
}
```

**SEP-1686 Flow:**
1. `start_approval` posts to Slack and spawns background poller
2. Task ID returned to client immediately (< 100ms)
3. Background task polls Slack for reaction
4. On approval: task state → Approved (execution deferred to `tasks/result`)
5. On rejection: task state → Failed with -32007
6. On timeout: task state → Failed with -32008

- **F-006.1:** Load workflow config from YAML
- **F-006.2:** Start approval workflow (non-blocking)
- **F-006.3:** Return Task ID immediately with `input_required` status
- **F-006.4:** Background poller updates task state on decision
- **F-006.5:** Upstream execution triggered by `tasks/result` call (see REQ-GOV-001)

### F-007: Upstream Forwarding

- **F-007.1:** Maintain connection pool to upstream MCP server
- **F-007.2:** Forward requests with original headers (minus hop-by-hop)
- **F-007.3:** Apply configurable timeout per request
- **F-007.4:** Return upstream response to caller
- **F-007.5:** Handle upstream connection failures (delegate to REQ-CORE-004)

### F-008: Response Correlation

- **F-008.1:** Track in-flight requests by correlation ID
- **F-008.2:** Match responses to original requests
- **F-008.3:** Handle response timeout (delegate to REQ-CORE-004)
- **F-008.4:** Support concurrent requests to same upstream

### F-009: SEP-1686 Detection

- **F-009.1:** Detect `task` field in request `params`
- **F-009.2:** Extract `ttl` from task metadata
- **F-009.3:** Flag request as task-augmented for downstream processing
- **F-009.4:** Validate task metadata structure

### F-010: SSE Streaming

- **F-010.1:** Support SSE for server-to-client notifications
- **F-010.2:** Forward upstream SSE events to client
- **F-010.3:** Inject ThoughtGate notifications (e.g., `notifications/tasks/status`)
- **F-010.4:** Handle client disconnect (close upstream connection)

## 8. Complete Request Flow Example

```rust
async fn handle_tools_call(request: McpRequest) -> Result<McpResponse, Error> {
    let tool_name = request.tool_name();
    let arguments = request.arguments();
    let source_id = config.sources[0].id();  // v0.2: single source
    let principal = infer_principal()?;
    
    // ═══════════════════════════════════════════════════════════
    // GATE 1: Visibility
    // ═══════════════════════════════════════════════════════════
    let source = &config.sources[0];
    if !check_visibility(&tool_name, source) {
        return Err(ThoughtGateError::ToolNotExposed { tool: tool_name });
    }
    
    // ═══════════════════════════════════════════════════════════
    // GATE 2: Governance Rules
    // ═══════════════════════════════════════════════════════════
    let rule_match = evaluate_governance(&tool_name, source_id);
    
    match rule_match.action {
        Action::Forward => {
            // Skip gates 3 & 4, forward immediately
            return upstream.forward(&request).await;
        }
        Action::Deny => {
            return Err(ThoughtGateError::GovernanceRuleDenied { 
                tool: tool_name,
                rule: rule_match.pattern,
            });
        }
        Action::Approve => {
            // Skip gate 3, go to gate 4
            let workflow = rule_match.approval_workflow.unwrap_or("default".into());
            let timeout = get_workflow_timeout(&workflow);
            return execute_approval(&request, &workflow, timeout).await;
        }
        Action::Policy => {
            // Continue to gate 3
        }
    }
    
    // ═══════════════════════════════════════════════════════════
    // GATE 3: Cedar Policy
    // ═══════════════════════════════════════════════════════════
    let policy_id = rule_match.policy_id.expect("required for action: policy");
    
    let cedar_request = CedarRequest {
        principal: principal.clone(),
        resource: Resource::ToolCall {
            name: tool_name.clone(),
            server: source_id.to_string(),
            arguments: arguments.clone(),
        },
        context: CedarContext {
            policy_id: policy_id.clone(),
            source_id: source_id.to_string(),
            time: current_time_context(),
        },
    };
    
    match cedar_engine.evaluate(&cedar_request) {
        CedarDecision::Forbid { reason, .. } => {
            return Err(ThoughtGateError::PolicyDenied { 
                tool: tool_name,
                reason,
            });
        }
        CedarDecision::Permit => {
            // Continue to gate 4
        }
    }
    
    // ═══════════════════════════════════════════════════════════
    // GATE 4: Approval Workflow
    // ═══════════════════════════════════════════════════════════
    let workflow = rule_match.approval_workflow.unwrap_or("default".into());
    let timeout = get_workflow_timeout(&workflow);
    
    execute_approval(&request, &workflow, timeout).await
}
```

## 9. Non-Functional Requirements

### NFR-001: Observability

**Metrics:**
```
mcp_requests_total{method, status, gate}
mcp_request_duration_seconds{method, quantile}
mcp_gate_evaluations_total{gate, result}
mcp_upstream_requests_total{status}
mcp_upstream_duration_seconds{quantile}
mcp_connections_active
thoughtgate_approval_started_total{workflow}
thoughtgate_approval_decision_duration_seconds{quantile}
thoughtgate_upstream_reconnects_total  # Track upstream connection stability
```

**Logging:**
```json
{
  "level": "info",
  "message": "Request completed",
  "correlation_id": "uuid",
  "method": "tools/call",
  "tool": "delete_user",
  "gates": {
    "visibility": "pass",
    "governance": { "action": "policy", "rule": "delete_*" },
    "cedar": { "decision": "permit", "policy_id": "deletion_policy" },
    "approval": { "decision": "approved", "workflow": "default" }
  },
  "duration_ms": 1523
}
```

### NFR-002: Performance

| Metric | Target |
|--------|--------|
| Parse latency (P99) | < 1ms |
| Gate 1-2 evaluation (P99) | < 0.5ms |
| Gate 3 (Cedar) (P99) | < 1ms |
| Total routing latency (P99) | < 3ms |
| Max concurrent requests | 10,000 |
| Memory per request | < 64KB average |

### NFR-003: Reliability

- **Connection pooling:** Reuse upstream connections
- **Backpressure:** Reject new requests when at max concurrency (503)
- **Timeout handling:** Fail fast on slow upstreams
- **Config reload:** No request drops during hot-reload

## 10. Testing Requirements

### 10.1 Unit Tests

| Test | Description |
|------|-------------|
| `test_gate1_visibility_all` | All tools visible by default |
| `test_gate1_visibility_allowlist` | Only allowlisted tools visible |
| `test_gate1_visibility_blocklist` | Blocklisted tools hidden |
| `test_gate2_rule_matching` | First matching rule wins |
| `test_gate2_source_filter` | Source filter works |
| `test_gate2_default_action` | Falls through to default |
| `test_gate3_cedar_permit` | Cedar permit continues flow |
| `test_gate3_cedar_forbid` | Cedar forbid denies |
| `test_gate4_approval_approved` | Approval forwards request |
| `test_gate4_approval_rejected` | Rejection returns error |
| `test_gate4_approval_timeout` | Timeout follows on_timeout |

### 10.2 Integration Tests

| Test | Description |
|------|-------------|
| `test_full_forward_flow` | action: forward → upstream |
| `test_full_deny_flow` | action: deny → error |
| `test_full_approve_flow` | action: approve → approval → upstream |
| `test_full_policy_permit_flow` | action: policy → permit → approval → upstream |
| `test_full_policy_forbid_flow` | action: policy → forbid → error |
| `test_config_hot_reload` | Config changes apply without restart |

### 10.3 Edge Case Matrix

| Scenario | Expected Behavior | Test ID |
|----------|-------------------|---------|
| Unknown JSON-RPC method | Return -32601 | EC-MCP-001 |
| Malformed JSON body | Return -32700 | EC-MCP-002 |
| Valid JSON, invalid JSON-RPC structure | Return -32600 | EC-MCP-003 |
| Request body exceeds size limit | Return -32600 with size error | EC-MCP-004 |
| Batch request (array of requests) | Process each, return array | EC-MCP-005 |
| Batch with mix of valid/invalid | Return mixed results | EC-MCP-006 |
| Notification (null ID) | Process, no response | EC-MCP-007 |
| SSE connection drops mid-stream | Clean up, log, close upstream | EC-MCP-008 |
| Upstream unreachable on tools/call | Return -32000 | EC-MCP-009 |
| Upstream timeout on tools/call | Return -32001 | EC-MCP-010 |
| Concurrent initialize requests | Serialize, return same result | EC-MCP-011 |
| tools/call for tool not in cached list | Re-fetch tools/list, then evaluate | EC-MCP-012 |

## 11. Error Codes

| Code | Name | Gate | Description |
|------|------|------|-------------|
| -32601 | Method not found | 1 | Tool not exposed |
| -32003 | Policy Denied | 3 | Cedar forbid |
| -32007 | Approval Rejected | 4 | Human rejected |
| -32008 | Approval Timeout | 4 | Timeout reached |
| -32014 | Governance Rule Denied | 2 | action: deny matched |

See REQ-CORE-004 for complete error handling specification.
