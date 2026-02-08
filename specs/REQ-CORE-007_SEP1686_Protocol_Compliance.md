# REQ-CORE-007: SEP-1686 Protocol Compliance

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-CORE-007` |
| **Title** | SEP-1686 Protocol Compliance |
| **Type** | Core Component |
| **Status** | Draft |
| **Priority** | **Critical** |
| **Version** | v0.2 |
| **Tags** | `#core` `#sep-1686` `#tasks` `#capability` `#annotation` `#protocol` |

## 1. Context & Decision Rationale

This requirement defines ThoughtGate's compliance with MCP SEP-1686 (Tasks), which became part of the official MCP 2025-11-25 specification. SEP-1686 introduces a "call-now, fetch-later" execution pattern that enables long-running operations without blocking clients.

**Why SEP-1686 is Critical for ThoughtGate:**

Human approval workflows inherently take minutes to hours. Without async task support:
- HTTP connections timeout waiting for approval
- Agents cannot perform other work while waiting
- No visibility into approval status

SEP-1686 solves this by allowing:
- Immediate return of task ID
- Client polling for status
- Result retrieval when ready

**Key Architectural Decisions:**

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Blocking mode | **Implemented in v0.2** | Dual-mode: async SEP-1686 when `params.task` present; blocking when absent (requires approval engine) |
| Task ownership | **Follows YAML config** | `forward` → passthrough; `approve` → ThoughtGate owns; `policy` → Cedar decides |
| Capability advertisement | **Always advertise** | ThoughtGate synthesizes task support for approval-path tools |
| Tool annotation source | **YAML config lookup** | Simple glob-based rules; Cedar only for complex `action: policy` cases |

**Protocol Compliance Requirements:**

Per SEP-1686:
1. Server declares `capabilities.tasks.requests.tools/call: true` during `initialize`
2. Server annotates tools with `taskSupport` during `tools/list`
3. Client includes `task` field in request to opt-in to async mode
4. Server returns task metadata immediately, client polls for result

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-CORE-003 | **Extends** | MCP transport handles `initialize` and `tools/list` interception |
| REQ-CORE-004 | **Uses** | Error responses for TaskRequired, validation failures |
| REQ-CFG-001 | **Uses** | YAML config for routing decisions and annotation computation |
| REQ-POL-001 | **Optionally uses** | Cedar evaluation when YAML specifies `action: policy` |
| REQ-GOV-001 | **Provides to** | Task creation triggered by this layer |
| REQ-GOV-002 | **Coordinates with** | Execution pipeline receives validated requests |

## 3. Intent

The system must:
1. Intercept `initialize` responses and inject task capability
2. Intercept `tools/list` responses and rewrite tool annotations based on YAML config rules
3. Validate incoming `tools/call` requests against annotation contracts
4. Route task-augmented requests through appropriate ownership path (passthrough vs ThoughtGate-owned)
5. Handle `tasks/*` method routing based on task ownership

## 4. Scope

### 4.1 In Scope
- Capability injection during `initialize` handshake
- Tool annotation rewriting during `tools/list`
- YAML config lookup for routing decisions
- Task metadata validation on `tools/call`
- Task ownership determination and routing
- `tasks/*` method routing (TG-owned vs upstream)
- Configuration for annotation rules and overrides

### 4.2 Out of Scope
- Task state machine implementation (REQ-GOV-001)
- Approval workflow (REQ-GOV-002, REQ-GOV-003)
- Upstream task orchestration (REQ-GOV-004)
- Task storage and persistence (REQ-GOV-001)

## 5. Constraints

### 5.1 SEP-1686 Specification Compliance

**Capability Declaration:**
```json
{
  "capabilities": {
    "tasks": {
      "requests": {
        "tools/call": true
      }
    }
  }
}
```

**Tool Annotation Values:**
| Value | Meaning | Client Behavior |
|-------|---------|-----------------|
| `forbidden` | Tool cannot be called with task metadata | Must NOT include `task` field |
| `optional` | Tool supports both sync and async | May include `task` field |
| `required` | Tool must be called with task metadata | Must include `task` field |

**Task Metadata Structure:**
```json
{
  "task": {
    "ttl": 600000
  }
}
```

### 5.2 Configuration

> **Convention:** All duration-based environment variables use the `_SECS` suffix and accept
> integer seconds (not milliseconds). This is consistent across the entire ThoughtGate
> configuration surface.

| Setting | Default | Environment Variable | Status |
|---------|---------|---------------------|--------|
| Default TTL | 600s (10 min) | `THOUGHTGATE_DEFAULT_TASK_TTL_SECS` | Implemented |
| Maximum TTL | 86400s (24 hr) | `THOUGHTGATE_MAX_TASK_TTL_SECS` | Implemented |
| Approval timeout | 600s (10 min) | `THOUGHTGATE_DEFAULT_APPROVAL_TIMEOUT_SECS` | Implemented |
| Force required tools | (none) | `THOUGHTGATE_TASK_FORCE_REQUIRED` | **DEFERRED** |
| Sync forward timeout | 30s | `THOUGHTGATE_UPSTREAM_TIMEOUT_SECS` | Implemented (via upstream client timeout) |

**Sync Forward Timeout:**

For Forward path requests (no approval required), if the upstream tool is synchronous and hangs, ThoughtGate must not leak connections. The `SYNC_FORWARD_TIMEOUT` applies to:
- Forward path requests without task metadata
- Forward path requests with task metadata to `optional` upstream tools that respond synchronously

```
┌─────────────────────────────────────────────────────────────────────────┐
│   SYNC FORWARD TIMEOUT                                                  │
│                                                                         │
│   Client ──tools/call──▶ ThoughtGate ──forward──▶ Upstream              │
│                              │                        │                 │
│                              │     (upstream hangs)   │                 │
│                              │                        ▼                 │
│                         T=60s timeout fires                             │
│                              │                                          │
│                              ▼                                          │
│   Client ◀──error: UpstreamTimeout──                                    │
│                                                                         │
│   Without this timeout: connection leaked, resources exhausted          │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

> **Note:** This timeout is separate from task TTL. Task TTL governs the approval 
> workflow lifecycle. Sync forward timeout prevents resource leaks on direct 
> upstream calls that never complete.

**Force Required Configuration:** *(DEFERRED)*

> Force-required overrides are not yet implemented. Tools requiring approval are configured
> via YAML `action: approve` rules in the governance config. The environment variable
> `THOUGHTGATE_TASK_FORCE_REQUIRED` is reserved for future use.

```yaml
# DEFERRED: Environment variable format (comma-separated)
# THOUGHTGATE_TASK_FORCE_REQUIRED=tool1,tool2,tool3
#
# DEFERRED: Config file format
# task_annotation_overrides:
#   force_required:
#     - delete_user
#     - transfer_funds
```

### 5.3 YAML-Based Annotation Computation

ThoughtGate computes tool annotations from **YAML config lookup**, not Cedar policy analysis.

**Annotation Decision Matrix:**

| YAML Action | Upstream Tasks | Annotation | Rationale |
|-------------|----------------|------------|-----------|
| `forward` | Yes | `optional` | Client may use tasks, not required |
| `forward` | No | `forbidden` | Upstream can't handle task metadata |
| `approve` | Any | `optional` | Supports both async (SEP-1686) and blocking mode |
| `deny` | Any | `forbidden` | Tool blocked, no execution possible |
| `policy` | Any | `optional` | Supports both async (SEP-1686) and blocking mode |

**Lookup Algorithm:**

```rust
fn compute_annotation(
    tool: &str,
    config: &PolicyConfig,
    upstream_supports_tasks: bool,
) -> TaskSupportAnnotation {
    let action = config.get_action(tool); // YAML glob matching
    
    match action {
        Action::Forward if upstream_supports_tasks => TaskSupportAnnotation::Optional,
        Action::Forward => TaskSupportAnnotation::Forbidden,
        Action::Approve => TaskSupportAnnotation::Optional, // Supports blocking mode
        Action::Deny => TaskSupportAnnotation::Forbidden,
        Action::Policy => TaskSupportAnnotation::Optional, // Supports blocking mode
    }
}
```

**Why YAML instead of Cedar partial-eval?**

| Aspect | Cedar Partial-Eval | YAML Lookup |
|--------|-------------------|-------------|
| Complexity | High (experimental API) | Low (config lookup) |
| Performance | O(policies × rules) | O(rules) |
| Predictability | Depends on policy structure | Direct mapping |
| User understanding | Requires Cedar knowledge | Obvious from config |

Cedar is still used for runtime decisions when YAML specifies `action: policy`.

### 5.4 Upstream Task Capability Detection

ThoughtGate MUST detect whether upstream supports tasks during `initialize`:

```rust
fn upstream_supports_tasks(init_response: &InitializeResponse) -> bool {
    init_response
        .capabilities
        .as_ref()
        .and_then(|c| c.tasks.as_ref())
        .and_then(|t| t.requests.as_ref())
        .and_then(|r| r.tools_call)
        .unwrap_or(false)
}
```

**Critical Constraint:** If upstream does NOT support tasks, ThoughtGate MUST strip `execution.taskSupport` for ALL tools where the YAML action is `forward` or `deny`. This prevents clients from sending task metadata that upstream cannot handle.

**Rationale:**

```
┌─────────────────────────────────────────────────────────────────────────┐
│   WHY THIS MATTERS                                                      │
│                                                                         │
│   ThoughtGate advertises task capability (for approval workflows)       │
│   BUT upstream may not support tasks                                    │
│                                                                         │
│   If tool annotation is "optional" or missing:                          │
│     Client may send: tools/call { task: { ttl: 60000 } }               │
│     Cedar returns: Forward (no approval needed)                         │
│     ThoughtGate forwards to upstream WITH task metadata                 │
│     Upstream doesn't understand task field → ERROR                      │
│                                                                         │
│   Solution: Mark non-approval tools as "forbidden" when upstream        │
│             doesn't support tasks                                       │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

**Storage:** Cache `upstream_supports_tasks` boolean after `initialize` handshake. Use in annotation computation.

## 6. Interfaces

### 6.0 Capability Cache

```rust
/// Cached state from initialize handshake
pub struct CapabilityCache {
    /// Whether upstream MCP server supports SEP-1686 tasks
    upstream_supports_tasks: AtomicBool,
    /// Whether upstream MCP server supports SSE task notifications
    upstream_supports_task_sse: AtomicBool,
    /// Whether the initialize handshake has been completed at least once
    has_initialized: AtomicBool,
}

impl CapabilityCache {
    pub fn set_upstream_supports_tasks(&self, supports: bool) {
        self.upstream_supports_tasks.store(supports, Ordering::SeqCst);
        self.has_initialized.store(true, Ordering::Release);
    }

    pub fn set_upstream_supports_task_sse(&self, supports: bool) {
        self.upstream_supports_task_sse.store(supports, Ordering::SeqCst);
    }

    pub fn upstream_supports_tasks(&self) -> bool {
        self.upstream_supports_tasks.load(Ordering::SeqCst)
    }

    pub fn upstream_supports_task_sse(&self) -> bool {
        self.upstream_supports_task_sse.load(Ordering::SeqCst)
    }

    pub fn has_initialized(&self) -> bool {
        self.has_initialized.load(Ordering::Acquire)
    }

    /// Invalidates all cached capabilities (e.g., on upstream reconnect).
    pub fn invalidate(&self) {
        self.upstream_supports_tasks.store(false, Ordering::SeqCst);
        self.upstream_supports_task_sse.store(false, Ordering::SeqCst);
        self.has_initialized.store(false, Ordering::Release);
    }
}
```

> **Implementation note:** The implementation uses a simple `AtomicBool` flag rather than a
> `RwLock<Instant>` timestamp. The boolean is sufficient because cache invalidation is triggered
> explicitly by `invalidate()` on upstream reconnect, not by time-based expiry.

### 6.1 Initialize Interception

**Input: Upstream `initialize` response (with SSE support)**
```json
{
  "protocolVersion": "2025-11-25",
  "capabilities": {
    "tools": { "listChanged": true },
    "notifications": {
      "tasks": { "status": true }
    }
  },
  "serverInfo": { "name": "upstream-server" }
}
```

**Output: Modified response to client (SSE advertised)**
```json
{
  "protocolVersion": "2025-11-25",
  "capabilities": {
    "tools": { "listChanged": true },
    "tasks": {
      "requests": {
        "tools/call": true
      }
    },
    "notifications": {
      "tasks": { "status": true }
    }
  },
  "serverInfo": { "name": "upstream-server" }
}
```

**Input: Upstream `initialize` response (NO SSE support)**
```json
{
  "protocolVersion": "2025-11-25",
  "capabilities": {
    "tools": { "listChanged": true }
  },
  "serverInfo": { "name": "upstream-server" }
}
```

**Output: Modified response to client (SSE NOT advertised)**
```json
{
  "protocolVersion": "2025-11-25",
  "capabilities": {
    "tools": { "listChanged": true },
    "tasks": {
      "requests": {
        "tools/call": true
      }
    }
  },
  "serverInfo": { "name": "upstream-server" }
}
```

**Critical:** ThoughtGate MUST NOT advertise `notifications.tasks.status` unless upstream supports it. Without upstream SSE, ThoughtGate cannot generate meaningful notifications (lazy polling model has no active state monitoring).

### 6.2 Tools/List Interception

**Input: Upstream `tools/list` response**
```json
{
  "tools": [
    {
      "name": "read_file",
      "description": "Read a file",
      "inputSchema": { ... }
    },
    {
      "name": "delete_user",
      "description": "Delete a user account",
      "inputSchema": { ... },
      "execution": {
        "taskSupport": "optional"
      }
    }
  ]
}
```

**Output: Modified response to client**

> **MCP Protocol Revision 2025-11-25:** The `taskSupport` field is nested under `execution`,
> not `annotations`. The correct field path is `execution.taskSupport`.

```json
{
  "tools": [
    {
      "name": "read_file",
      "description": "Read a file",
      "inputSchema": { ... },
      "execution": {
        "taskSupport": "forbidden"
      }
    },
    {
      "name": "delete_user",
      "description": "Delete a user account",
      "inputSchema": { ... },
      "execution": {
        "taskSupport": "optional"
      }
    }
  ]
}
```

### 6.3 Task Metadata Validation

**Input: `tools/call` request**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "delete_user",
    "arguments": { "user_id": "123" },
    "task": { "ttl": 600000 }
  }
}
```

**Validation Result:**

The implementation uses `Result<bool, ThoughtGateError>` rather than a custom enum:

```rust
/// Returns `Ok(true)` if the request should use blocking approval mode
/// (no task metadata, but an approval engine is available).
/// Returns `Ok(false)` for async SEP-1686 mode or non-approval actions.
/// Returns `Err(ThoughtGateError::TaskRequired { .. })` when task metadata
/// is required but missing and no approval engine is available.
pub(super) fn validate_task_metadata(
    request: &mut McpRequest,
    action: &Action,
    tool_name: &str,
    upstream_supports_tasks: bool,
    has_approval_engine: bool,
) -> Result<bool, ThoughtGateError>
```

| Return value | Meaning |
|--------------|---------|
| `Ok(false)` | Async SEP-1686 mode (task metadata present) or sync forward path |
| `Ok(true)` | Blocking approval mode (no task metadata, approval engine available) |
| `Err(TaskRequired)` | Approval-path tool, no task metadata, no approval engine |

For `forward`/`deny` actions, task metadata is silently stripped when upstream does not support tasks.

### 6.4 Task Ownership Determination

> **Implementation note:** Task ownership is determined implicitly rather than via a separate
> `TaskOwnership` enum. Ownership is inferred from the `tg_` prefix on task IDs:
> - Tasks with `tg_` prefix are ThoughtGate-owned (created by the approval workflow).
> - All other task IDs are upstream-owned (passed through transparently).
>
> The routing decision is made inline by the YAML config action lookup and (optionally)
> Cedar policy evaluation, without an intermediate `TaskRoutingDecision` struct.

### 6.5 Task ID Format

ThoughtGate-owned tasks use a distinguishable prefix with a 21-character hex body
derived from a UUID v4:

```
tg_<21-hex-chars>  → ThoughtGate-owned task
<any-other>        → Upstream-owned task (passthrough)
```

The body is the first 21 characters of a UUID v4 in simple (no-dash) hex format.
This provides sufficient uniqueness (~84 bits of entropy) while keeping IDs compact.

**Examples:**
- `tg_a1b2c3d4e5f6a7b8c9d0e` → Route `tasks/*` to ThoughtGate
- `abc-123-def-456` → Route `tasks/*` to upstream

```rust
pub const TASK_ID_PREFIX: &str = "tg_";
pub const TASK_ID_BODY_LENGTH: usize = 21;

impl Sep1686TaskId {
    pub fn new() -> Self {
        let body = &uuid::Uuid::new_v4().simple().to_string()[..TASK_ID_BODY_LENGTH];
        Self(format!("{}{}", TASK_ID_PREFIX, body))
    }
}
```

### 6.6 Task Response Formats (SEP-1686)

**`tools/call` with task metadata - Response:**

When client sends `tools/call` with task metadata and TG creates a task, the response is a task reference (not the tool result):

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "taskId": "tg_a1b2c3d4e5f6a7b8c9d0e",
    "status": "working",
    "statusMessage": "Awaiting approval",
    "pollInterval": 5000
  }
}
```

> **Implementation note:** Tasks are created with initial status `working` (not `input_required`).
> The `statusMessage` field is always included in the creation response to provide human-readable
> context (e.g., "Awaiting approval"). It is conditionally included in subsequent `tasks/get`
> responses only when a status message is set.

| Field | Type | Description |
|-------|------|-------------|
| `taskId` | string | Unique task identifier (format: `tg_<21-hex-chars>`) |
| `status` | string | SEP-1686 status: `working`, `input_required`, `completed`, `failed`, `cancelled` |
| `statusMessage` | string? | Human-readable status context; always present in creation response |
| `pollInterval` | number? | Suggested poll interval in milliseconds |

**`tasks/get` Response:**

Same format as `tools/call` task response. See REQ-GOV-001 `TasksGetResponse`.

**`tasks/list` Response:**

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "tasks": [
      {
        "taskId": "tg_a1b2c3d4e5f6a7b8c9d0e",
        "status": "working",
        "createdAt": "2026-01-12T22:30:00Z",
        "toolName": "delete_user"
      }
    ],
    "nextCursor": "eyJvZmZzZXQiOjEwfQ=="
  }
}
```

Pagination uses cursor-based approach. See REQ-GOV-001 `TasksListResponse`.

**`tasks/result` Response:**

Returns the actual tool result (same as sync `tools/call` response). Streamed for large results.

**`tasks/cancel` Response:**

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "taskId": "tg_a1b2c3d4e5f6a7b8c...",
    "status": "cancelled"
  }
}
```

### 6.7 SSE Event Format

SSE notifications use standard `text/event-stream` format:

```
event: task_status
data: {"taskId":"tg_a1b2c3d4e5f6a7b8c...","status":"completed","timestamp":"2026-01-12T22:35:00Z"}

event: task_status
data: {"taskId":"tg_a1b2c3d4e5f6a7b8c...","status":"failed","statusMessage":"Upstream error","timestamp":"2026-01-12T22:36:00Z"}
```

Keep-alive pings:
```
:ping
```

See REQ-GOV-004 F-012 for TG-generated SSE events.

### 6.8 Error Responses

**TaskRequired Error:**

Per the MCP Tasks Specification, `TaskRequired` uses standard JSON-RPC error code `-32600`
(InvalidRequest), not a custom code.

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32600,
    "message": "Task metadata required for tool 'delete_user'",
    "data": {
      "tool": "delete_user",
      "hint": "Include params.task per tools/list execution.taskSupport annotation"
    }
  }
}
```

> **Note:** There is no separate `TaskForbidden` error. When a client sends task metadata for
> a `forward`/`deny` action tool where upstream does not support tasks, the metadata is silently
> stripped and the request is forwarded without it. This avoids breaking forward compatibility
> as upstreams gradually add task support.

## 7. Functional Requirements

### F-001: Capability Injection

- **F-001.1:** Intercept `initialize` response from upstream
- **F-001.2:** Detect and store whether upstream supports tasks:
  ```rust
  let upstream_supports_tasks = response
      .capabilities.tasks.requests.tools_call
      .unwrap_or(false);
  self.capability_cache.set_upstream_supports_tasks(upstream_supports_tasks);
  ```
- **F-001.3:** Detect and store whether upstream supports SSE task notifications:
  ```rust
  let upstream_supports_task_sse = response
      .capabilities.notifications.tasks.status
      .unwrap_or(false);
  self.capability_cache.set_upstream_supports_task_sse(upstream_supports_task_sse);
  ```
- **F-001.4:** Inject `capabilities.tasks.requests.tools/call: true` (always, for approval workflow)
- **F-001.5:** Conditionally include `capabilities.notifications.tasks.status`:
  ```rust
  // ONLY advertise SSE if upstream supports it
  if upstream_supports_task_sse {
      response.capabilities.notifications = Some(NotificationCapabilities {
          tasks: Some(TaskNotifications { status: true }),
      });
  } else {
      // Do NOT advertise - we cannot deliver SSE without upstream support
      response.capabilities.notifications = None;
  }
  ```
- **F-001.6:** Preserve other existing upstream capabilities
- **F-001.7:** Forward modified response to client
- **F-001.8:** Log capability detection at INFO level:
  ```
  INFO Upstream capabilities: tasks=true sse=true
  INFO Upstream capabilities: tasks=false sse=false
  ```

### F-002: Tool Annotation Computation (YAML-Based)

- **F-002.1:** For each tool from upstream `tools/list` response, compute annotation inline:
  - Look up YAML action using glob matching (see REQ-CFG-001 §4.4)
  - Apply annotation decision matrix from Section 5.3
- **F-002.2:** Annotations are computed per-request (no separate cache or startup build step)
- **F-002.3:** Config hot-reload and upstream reconnect are handled automatically since annotations are always recomputed from current config state
- **F-002.5:** Log annotation decisions at INFO level:
  ```
  INFO Tool annotation: delete_user -> optional (yaml_action=approve)
  INFO Tool annotation: read_file -> optional (yaml_action=forward, upstream_tasks=true)
  INFO Tool annotation: read_config -> forbidden (yaml_action=forward, upstream_tasks=false)
  INFO Tool annotation: drop_table -> forbidden (yaml_action=deny)
  INFO Tool annotation: transfer_funds -> optional (yaml_action=policy)
  ```
- **F-002.6:** Metric: `thoughtgate_tools_by_annotation{annotation="required|optional|forbidden"}`

### F-003: Tool Annotation Injection

- **F-003.1:** Intercept `tools/list` response from upstream
- **F-003.2:** For each tool, compute annotation inline using YAML config action lookup
- **F-003.3:** Inject `execution.taskSupport` field per MCP Protocol Revision 2025-11-25:
  ```json
  {
    "name": "delete_user",
    "execution": {
      "taskSupport": "optional"
    }
  }
  ```
- **F-003.4:** Forward modified response to client
- **F-003.5:** No explicit cache invalidation needed; annotations are recomputed from current config on each interception

### F-004: Task Metadata Validation

- **F-004.1:** On `tools/call`, extract tool name from params
- **F-004.2:** Look up YAML config action for tool
- **F-004.3:** Check for presence of `task` field in params
- **F-004.4:** Validate per matrix:

| Action | Task Present | Approval Engine | Result |
|--------|--------------|-----------------|--------|
| `approve`/`policy` | Yes | Any | Async SEP-1686 path |
| `approve`/`policy` | No | Yes | Blocking approval mode |
| `approve`/`policy` | No | No | Error: TaskRequired (-32600) |
| `forward`/`deny` | Yes (upstream no tasks) | Any | Strip task metadata, forward |
| `forward`/`deny` | Yes (upstream has tasks) | Any | Forward with task metadata |
| `forward`/`deny` | No | Any | Sync forward path |

> **Note:** Tools with `action: approve` or `action: policy` are annotated
> `"optional"` (not `"required"`). When `params.task` is absent, the proxy
> detects blocking mode and holds the connection open for approval.

- **F-004.5:** If task metadata present, validate TTL:
  - Must be positive integer (milliseconds in the wire format, converted to `Duration` internally)
  - Cap at `THOUGHTGATE_MAX_TASK_TTL_SECS` (default: 86400s / 24 hours)
  - Default to `THOUGHTGATE_DEFAULT_TASK_TTL_SECS` (default: 600s / 10 minutes) if not specified

### F-005: Task Ownership Routing (YAML-First)

- **F-005.1:** After task metadata validation passes, get YAML action for tool
- **F-005.2:** If `action: policy`, evaluate Cedar to get Forward/Approve/Reject
- **F-005.3:** Determine ownership based on action and upstream annotation:

| YAML Action | Task Metadata | Upstream Annotation | Ownership | Notes |
|-------------|---------------|---------------------|-----------|-------|
| `forward` | Present | `optional`/`required` | Upstream | Passthrough with task |
| `forward` | Present | `forbidden` | **ThoughtGate** | Accelerated approve |
| `forward` | Absent | Any | N/A | Sync passthrough |
| `approve` | Present | Any | ThoughtGate | Normal async approval flow |
| `approve` | Absent | Any | **ThoughtGate (blocking)** | Holds connection, polls for approval |
| `deny` | Any | Any | N/A | Return error |
| `policy` → Forward | Present | `optional`/`required` | Upstream | Cedar said forward |
| `policy` → Forward | Present | `forbidden` | **ThoughtGate** | Accelerated approve |
| `policy` → Approve | Present | Any | ThoughtGate | Cedar said approve (async) |
| `policy` → Approve | Absent | Any | **ThoughtGate (blocking)** | Cedar said approve, holds connection |
| `policy` → Reject | Any | Any | N/A | Return error |

**Flow:**
```rust
async fn determine_ownership(
    tool: &str,
    task_metadata: Option<&TaskMetadata>,
    config: &PolicyConfig,
    policy_engine: &PolicyEngine,
    upstream_annotation: TaskSupportAnnotation,
) -> Result<TaskOwnership, Error> {
    let yaml_action = config.get_action(tool);
    
    // For most actions, YAML is the final decision
    let effective_action = match yaml_action {
        Action::Policy => {
            // Delegate to Cedar for complex decision
            match policy_engine.evaluate(request)? {
                PolicyAction::Forward => Action::Forward,
                PolicyAction::Approve { .. } => Action::Approve,
                PolicyAction::Reject { reason } => return Err(PolicyDenied(reason)),
            }
        }
        action => action,
    };
    
    // Now route based on effective action
    match (effective_action, task_metadata, upstream_annotation) {
        (Action::Deny, _, _) => Err(PolicyDenied("Tool blocked")),
        (Action::Forward, None, _) => Ok(TaskOwnership::SyncPassthrough),
        (Action::Forward, Some(_), TaskSupportAnnotation::Forbidden) => {
            Ok(TaskOwnership::ThoughtGate { accelerated: true })
        }
        (Action::Forward, Some(_), _) => Ok(TaskOwnership::Upstream),
        (Action::Approve, None, _) => Ok(TaskOwnership::ThoughtGateBlocking), // Blocking mode
        (Action::Approve, Some(_), _) => Ok(TaskOwnership::ThoughtGate { accelerated: false }),
        _ => unreachable!(),
    }
}
```

**Forward + Task + Forbidden (Accelerated Approve):**

When YAML/Cedar decides Forward but client sent task metadata and upstream forbids tasks:
- TG cannot pass task metadata to upstream (would violate upstream's contract)
- TG cannot return sync result (would violate client's contract - they sent task, expect task response)
- Solution: TG owns the task, skips approval wait, proceeds directly to execution

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    FORWARD + TASK + FORBIDDEN                           │
│                                                                         │
│   Client sends: tools/call + task metadata                              │
│   YAML/Cedar decides: Forward (no approval needed)                      │
│   Upstream annotation: forbidden (can't accept task)                    │
│                                                                         │
│   Result: "Accelerated Approve" - TG creates task, skips approval,      │
│           goes directly to Approved state (deferred execution)          │
│                                                                         │
│   Flow:                                                                 │
│   1. Create TG task (tg_xxx)                                            │
│   2. Skip InputRequired (no approval needed)                            │
│   3. Enter Approved state (DeferredSync path)                           │
│   4. Return task response to client                                     │
│   5. Execute on tasks/result (same as normal deferred sync)             │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

- **F-005.4:** For ThoughtGate ownership: generate `tg_<21-hex-chars>` task ID
- **F-005.5:** For upstream ownership: forward request as-is, return upstream's task ID
- **F-005.6:** For accelerated approve: create TG task in Approved state (skip InputRequired)

### F-006: Tasks/* Method Routing

- **F-006.1:** On `tasks/get`, `tasks/result`, `tasks/cancel`:
- **F-006.2:** Extract task ID from params
- **F-006.3:** If task ID starts with `tg_`: route to ThoughtGate task manager
- **F-006.4:** Otherwise: proxy to upstream

**F-006.5: `tasks/list` Behavior**

> **DEFERRED to v0.3+:** Merging `tasks/list` results with upstream tasks is deferred.
> The current implementation only returns ThoughtGate-owned tasks. Upstream task
> enumeration requires REQ-GOV-004 (Upstream Task Orchestration), which is out of scope.

In the current implementation, `tasks/list` returns only ThoughtGate-owned tasks with
cursor-based pagination (server-controlled page size of 20):

```rust
pub fn handle_tasks_list(
    &self,
    req: TasksListRequest,
    principal: &Principal,
) -> TasksListResponse {
    const PAGE_SIZE: usize = 20;
    let offset = req.cursor.as_ref()
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(0);
    let tasks = self.store.list_for_principal(principal, offset, PAGE_SIZE + 1);
    // ... pagination logic
}
```

**Task ID Convention:**
- `tg_*` prefix: ThoughtGate-owned tasks (approval workflow)
- Other IDs: Upstream-owned tasks (routed to upstream via `tasks/get`, `tasks/result`, `tasks/cancel`)

- **F-006.6:** Pagination: Server-controlled page size (currently 20), cursor-only (no client limit)

### F-007: Annotation Computation (Inline Per-Request)

> **Implementation note:** The implementation does NOT use a cached annotation store.
> Annotations are computed inline on each `tools/list` response interception. This
> simplifies the design by avoiding cache invalidation complexity (config reload,
> upstream reconnect, new tools). The cost is negligible since annotation computation
> is a simple YAML action lookup per tool.

- **F-007.1:** Compute `execution.taskSupport` annotations inline when intercepting `tools/list` responses
- **F-007.2:** For each tool, look up the YAML governance action and apply the annotation decision matrix (Section 5.3)
- **F-007.3:** For `forward`/`deny` actions when upstream does not support tasks, strip any upstream `execution.taskSupport` annotation
- **F-007.4:** For `approve`/`policy` actions, set `execution.taskSupport: "optional"`
- **F-007.5:** Store `upstream_supports_tasks` in `CapabilityCache`, update on each `initialize`
- **F-007.6:** No explicit cache invalidation needed; annotations are always fresh per-request

## 8. Non-Functional Requirements

### NF-001: Performance

- **NF-001.1:** Capability injection adds < 1ms latency to `initialize`
- **NF-001.2:** Annotation rewriting adds < 5ms latency to `tools/list`
- **NF-001.3:** Task metadata validation adds < 1ms latency to `tools/call`
- **NF-001.4:** YAML rule matching at startup completes in < 100ms for 1000 rules

### NF-002: Reliability

- **NF-002.1:** If YAML config load fails at startup, exit with clear error
- **NF-002.2:** If upstream `tools/list` fails, return error to client (no cache to fall back on)
- **NF-002.3:** Config reload failures do not affect annotation computation (annotations are derived from current config state)

### NF-003: Observability

- **NF-003.1:** Log annotation decisions at startup (INFO level):
  ```
  INFO Tool annotation: delete_user -> optional (yaml_action=approve)
  INFO Tool annotation: read_file -> optional (yaml_action=forward, upstream_tasks=true)
  ```
- **NF-003.2:** Metric: `thoughtgate_tools_by_annotation{annotation="required|optional|forbidden"}`
- **NF-003.3:** Metric: `thoughtgate_task_validation_errors_total{error="required"}`

## 9. Testing Strategy

### 9.1 Unit Tests

| Test Case | Input | Expected Output |
|-----------|-------|-----------------|
| Capability injection (upstream has none) | `{"capabilities":{}}` | `{"capabilities":{"tasks":{"requests":{"tools/call":true}}}}` |
| Capability injection (upstream has tasks) | `{"capabilities":{"tasks":{...}}}` | Preserved upstream capability |
| Upstream capability detection (has tasks) | `{"capabilities":{"tasks":{"requests":{"tools/call":true}}}}` | `upstream_supports_tasks = true` |
| Upstream capability detection (no tasks) | `{"capabilities":{}}` | `upstream_supports_tasks = false` |
| SSE capability detection (has SSE) | `{"capabilities":{"notifications":{"tasks":{"status":true}}}}` | `upstream_supports_task_sse = true` |
| SSE capability detection (no SSE) | `{"capabilities":{}}` | `upstream_supports_task_sse = false` |
| SSE conditional advertisement (upstream has SSE) | Upstream with SSE | Response includes `notifications.tasks.status` |
| SSE conditional advertisement (upstream no SSE) | Upstream without SSE | Response does NOT include `notifications` |
| Annotation: upstream no tasks + no approval | Tool with no approval path, upstream no tasks | `forbidden` |
| Annotation: upstream no tasks + approval possible | Tool with approval path, upstream no tasks | `required` |
| Annotation: upstream has tasks + no approval | Tool with no approval path, upstream has tasks | Preserve upstream annotation |
| Annotation: forbidden + no approval | Tool with no Cedar approval path | `forbidden` |
| Annotation: forbidden + approval possible | Tool with Cedar approval path | `required` |
| Annotation: force_required override | Tool in force_required config | `required` regardless of Cedar |
| Validation: approve action + no task + no engine | Request without task field, no approval engine | TaskRequired error (-32600) |
| Validation: forward + task + no upstream tasks | Request with task field, upstream no tasks | Task metadata stripped, forward |
| Task ID routing: tg_ prefix | `tg_abc123` | Route to ThoughtGate |
| Task ID routing: other | `abc123` | Route to upstream |

### 9.2 Integration Tests

| Scenario | Setup | Expected Result |
|----------|-------|-----------------|
| Full handshake | Client → initialize → tools/list → tools/call | Correct annotations, task created |
| Policy reload | Change policy, trigger reload | Annotations updated, cache invalidated |
| Mixed ownership | Some tools Forward, some Approve | Correct routing per tool |
| Upstream no tasks + approval tool | Upstream without task cap, tool needs approval | Tool marked `required`, TG handles task |
| Upstream no tasks + forward tool | Upstream without task cap, tool always forwards | Tool marked `forbidden`, client can't use tasks |
| **Forward + task + forbidden** | Cedar Forward, client sends task, upstream forbids | TG creates task, skips approval, Approved state |
| **Forward + task + optional** | Cedar Forward, client sends task, upstream optional | Passthrough to upstream with task |
| **Accelerated approve flow** | Forward + task + forbidden → tasks/result | Deferred execution, result streamed |
| **tasks/list** (v0.2) | TG has tasks | Only TG tasks returned (upstream merge deferred to v0.3+) |
| Upstream reconnect | Upstream restarts with different capabilities | Capability cache invalidated, annotations recomputed on next tools/list |
| Upstream with SSE | Upstream supports SSE | TG advertises SSE, client can subscribe |
| Upstream without SSE | Upstream no SSE support | TG does NOT advertise SSE |
| Upstream SSE changes on reconnect | Upstream gains/loses SSE | Capability cache updated, client sees change |

### 9.3 Compliance Tests

| SEP-1686 Requirement | Test |
|---------------------|------|
| Capability advertisement | Verify `capabilities.tasks` in initialize response |
| Conditional SSE advertisement | Verify `notifications.tasks.status` only when upstream supports |
| Tool annotation presence | Verify all tools have `taskSupport` annotation |
| Task metadata handling | Verify task ID returned for task-augmented calls |
| Task method routing | Verify `tasks/*` methods work for both ownership types |

### 9.4 SSE Edge Case Tests

| Test Case | Setup | Expected Behavior |
|-----------|-------|-------------------|
| Client subscribes SSE when advertised | Upstream has SSE, TG advertises | Subscription succeeds |
| Client subscribes SSE when not advertised | Upstream no SSE, TG doesn't advertise | Client shouldn't attempt (protocol) |
| Client ignores capability, attempts SSE | Force SSE subscribe without capability | Return error: SSE not supported |
| Upstream loses SSE on reconnect | Had SSE, reconnects without | Next initialize doesn't advertise SSE |
| Upstream gains SSE on reconnect | No SSE, reconnects with | Next initialize advertises SSE |

### 9.5 Task API Edge Cases

| Scenario | Expected Behavior | Test ID |
|----------|-------------------|---------|
| Malformed task metadata in request | Return -32602 | EC-SEP-001 |
| Task TTL = 0 | Reject with validation error | EC-SEP-002 |
| Task TTL exceeds max allowed | Clamp to max, log warning | EC-SEP-003 |
| Empty tools/list from upstream | Valid state, no tools exposed | EC-SEP-004 |
| tasks/get for non-existent task | Return -32602 (Invalid params per MCP spec) | EC-SEP-005 |
| tasks/result for non-existent task | Return -32602 (Invalid params per MCP spec) | EC-SEP-006 |
| tasks/cancel for non-existent task | Return -32602 (Invalid params per MCP spec) | EC-SEP-007 |
| tasks/cancel for terminal task | Return -32602 (Invalid params per MCP spec) | EC-SEP-008 |
| tasks/result called while still Working | Block until terminal or return current | EC-SEP-009 |
| Upstream changes capabilities on reconnect | Invalidate cache, rebuild annotations | EC-SEP-010 |

## 10. Open Questions

| Question | Status | Resolution |
|----------|--------|------------|
| ~~Should `tasks/list` merge TG and upstream tasks?~~ | Deferred | Planned for v0.3+ (requires REQ-GOV-004). Current implementation returns TG-owned tasks only. |
| ~~How to handle upstream `tools/list` pagination?~~ | Resolved | Fetch all pages at startup; see Section 10.1 |
| ~~Cedar `partial-eval` for annotations?~~ | Removed | v0.8: Replaced with YAML-based annotation lookup |

### 10.1 Upstream `tools/list` Pagination

**Resolution:** Fetch all pages during tools/list interception.

```rust
async fn fetch_all_tools(upstream: &UpstreamClient) -> Result<Vec<Tool>, Error> {
    let mut all_tools = Vec::new();
    let mut cursor: Option<String> = None;
    
    loop {
        let response = upstream.tools_list(cursor.as_deref()).await?;
        all_tools.extend(response.tools);
        
        match response.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    
    Ok(all_tools)
}
```

**Limitation:** For upstreams with many tools (1000+), tools/list responses may be slow. This is acceptable because:
1. Annotations are computed inline per-request (no startup cost)
2. Most MCP servers have <100 tools
3. Tools/list is called infrequently compared to tools/call

**Configuration:** *(DEFERRED)*

> The following environment variables are reserved for future use when upstream
> `tools/list` pagination is implemented:
> - `THOUGHTGATE_TOOLS_FETCH_TIMEOUT_SECS`: Max time to fetch all pages
> - `THOUGHTGATE_TOOLS_MAX_PAGES`: Safety limit on pagination

## 11. References

| Resource | Link |
|----------|------|
| SEP-1686 Specification | https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1686 |
| MCP 2025-11-25 Spec | https://modelcontextprotocol.io/ |
| REQ-CFG-001 Configuration | ./REQ-CFG-001_Configuration.md |
| glob crate (pattern matching) | https://docs.rs/glob/ |

## 12. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 0.1 | 2026-01-12 | - | Initial draft |
| 0.2 | 2026-01-12 | - | Added upstream task capability detection for annotation |
| 0.3 | 2026-01-12 | - | Added conditional SSE capability advertisement |
| 0.4 | 2026-01-12 | - | Added sync forward timeout configuration (Gemini feedback) |
| 0.5 | 2026-01-12 | - | **Forward + Task + Forbidden:** Accelerated approve path. **tasks/list:** Merge with source indication. |
| 0.6 | 2026-01-12 | - | Added SEP-1686 wire formats: task response, tasks/list pagination, SSE event format. |
| 0.7 | 2026-01-12 | - | **Grok feedback:** Resolved tools/list pagination (fetch all pages), added partial-eval failure logging. |
| 0.8 | 2026-01-13 | - | **YAML-first config:** Replaced Cedar partial-eval with YAML-based annotation. Added REQ-CFG-001 dependency. F-002, F-003, F-005 updated. |
| 0.9 | 2026-02-08 | - | **Align with implementation:** §6.0 `last_initialize` → `has_initialized: AtomicBool`. §6.2 `annotations.taskSupport` → `execution.taskSupport` per MCP 2025-11-25. §6.3 `TaskValidationResult` → `Result<bool, ThoughtGateError>`. §6.4 `TaskOwnership` noted as implicit. §6.5 task ID format `tg_<uuid>` → `tg_<21-hex-chars>`. §6.6 initial status `input_required` → `working`, added `statusMessage`. §6.8 TaskRequired code `-32009` → `-32600`, removed TaskForbidden. F-006.5 tasks/list merge deferred to v0.3+. §5.2 env vars use `_SECS` suffix, marked unimplemented vars. F-007 annotation cache → inline computation per-request. |
