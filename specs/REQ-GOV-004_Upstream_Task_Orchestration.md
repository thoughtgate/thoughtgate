# REQ-GOV-004: Upstream Task Orchestration

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-GOV-004` |
| **Title** | Upstream Task Orchestration |
| **Type** | Governance Component |
| **Status** | **DEFERRED (v0.3+)** |
| **Priority** | Medium |
| **Version** | v0.3+ |
| **Tags** | `#governance` `#tasks` `#upstream` `#lazy-polling` `#streaming` `#cancellation` |

> **⚠️ DEFERRED TO v0.3+:** This requirement handles upstream MCP servers that require task-augmented calls. For v0.2, if an upstream tool has `taskSupport: "required"`, ThoughtGate will return an error indicating the tool requires async support not yet implemented. Full upstream task orchestration is deferred to v0.3+.

## 1. Context & Decision Rationale

This requirement defines how ThoughtGate handles upstream MCP servers that require task-augmented calls (SEP-1686 `taskSupport: "required"`). When a tool requires approval AND the upstream requires tasks, ThoughtGate must orchestrate a "nested task" pattern.

**The Problem:**

```
┌─────────────────────────────────────────────────────────────────────────┐
│   Upstream tool: delete_user                                            │
│   Upstream annotation: taskSupport = "required"                         │
│   Cedar policy: Approve                                                 │
│                                                                         │
│   After approval, ThoughtGate tries synchronous call:                   │
│     POST tools/call { "name": "delete_user", ... }  ← No task metadata  │
│                                                                         │
│   Upstream responds:                                                    │
│     ERROR: Task metadata required                                       │
│                                                                         │
│   ❌ Protocol violation                                                 │
└─────────────────────────────────────────────────────────────────────────┘
```

**The Solution:**

During the `Executing` phase of a ThoughtGate-owned task, check upstream's tool annotation and call appropriately:
- `forbidden` → DeferredSync path via `setup_deferred_sync()` - defer execution until `tasks/result` call
- `optional` / `required` → Task path via `execute_with_task()` - create upstream task immediately, lazy-poll on client request

**Key Design Decisions:**

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Upstream call style | Depends on upstream tool annotation | Protocol compliance |
| Nested task visibility | Hidden from client | Client only sees TG task ID |
| Polling model | **Lazy (on-demand)** | Sidecar resource efficiency; no background polling loops |
| Result handling | **Stream without caching** | Bounded memory; works for any result size |
| SSE notifications | **Supported for TG-owned tasks** | Lazy upstream subscription (see F-009, F-010, F-012) |

### 1.1 Lazy Polling Model

ThoughtGate does NOT actively poll upstream tasks. Instead, it proxies client requests on-demand:

```
┌─────────────────────────────────────────────────────────────────────────┐
│   LAZY POLLING (On-Demand)                                              │
│                                                                         │
│   After approval, TG calls upstream with task metadata                  │
│       │                                                                 │
│       ▼                                                                 │
│   TG stores: { upstream_task_id, expires_at }                           │
│   TG returns TG task ID to client                                       │
│   TG state: Executing                                                   │
│                                                                         │
│   ... no background activity ...                                        │
│                                                                         │
│   Client calls tasks/get(tg_task_id)                                    │
│       │                                                                 │
│       ▼                                                                 │
│   TG checks: expired? ──▶ return Expired, cancel upstream               │
│       │                                                                 │
│       ▼                                                                 │
│   TG proxies: upstream.tasks_get(upstream_task_id)                      │
│       │                                                                 │
│       ▼                                                                 │
│   TG maps response, returns to client                                   │
│                                                                         │
│   Client calls tasks/result(tg_task_id)                                 │
│       │                                                                 │
│       ▼                                                                 │
│   TG proxies: upstream.tasks_result(upstream_task_id)                   │
│       │                                                                 │
│       ▼                                                                 │
│   TG streams response to client (zero-copy, chunked)                    │
│   TG transitions to Completed                                           │
│                                                                         │
│   Resources: Zero background tasks per Executing task                   │
│   Memory: O(chunk_size), not O(result_size)                             │
│   Network: Only when client asks                                        │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

**Benefits for sidecar deployment:**
- Zero background work per task
- Scales with client demand, not upstream execution time
- Bounded memory regardless of result size
- Simpler code (no polling loops, no tokio::select! for polling)

### 1.2 Result Handling by Execution Path

ThoughtGate handles results differently based on execution path:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    RESULT HANDLING BY PATH                              │
│                                                                         │
│   SYNC PATH (upstream annotation: forbidden/optional)                   │
│   ─────────────────────────────────────────────────                     │
│   • Upstream returns result immediately                                 │
│   • Result buffered in task (bounded, finite)                           │
│   • Client retrieves via tasks/result → stored bytes returned           │
│   • Memory: O(result_size) but bounded by sync timeout                  │
│                                                                         │
│   TASK PATH (upstream annotation: required)                             │
│   ──────────────────────────────────────────                            │
│   • Upstream returns task ID, executes async                            │
│   • ThoughtGate does NOT cache result                                   │
│   • Client retrieves via tasks/result → streamed from upstream          │
│   • Memory: O(chunk_size) ≈ 64KB constant                               │
│                                                                         │
│   WHY THE DIFFERENCE:                                                   │
│   • Sync results: bounded by timeout, typically small, immediate        │
│   • Task results: potentially huge (data exports), unbounded duration   │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

**Task Path Streaming (No Caching):**

```
   Client ──tasks/result──▶ ThoughtGate ──tasks/result──▶ Upstream
                                   │                           │
                                   │◀── chunked response ──────┘
   Client ◀── chunked proxy ───────┘

   Memory: O(chunk_size) ≈ 64KB
   NOT: O(result_size) which could be 50MB+
```

**Why task path doesn't cache:**
- Tool results can be arbitrarily large (file contents, data exports)
- 10 concurrent tasks × 50MB results = 500MB RAM = sidecar OOM
- Streaming uses constant memory regardless of result size

**Why sync path does cache:**
- Sync execution has timeout (default 60s)
- Results are bounded by what can complete in that timeout
- Uniform API requires client to call `tasks/result` (can't stream during `tools/call`)

### 1.3 SSE Notification Forwarding (Conditional)

SEP-1686 supports `notifications/tasks/status` via SSE for push-based status updates. ThoughtGate provides **bidirectional SSE forwarding**, but **only when upstream supports SSE**:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    CONDITIONAL SSE FORWARDING                           │
│                                                                         │
│   CASE 1: Upstream SUPPORTS SSE                                         │
│   ──────────────────────────────                                        │
│   Initialize: TG detects upstream SSE capability                        │
│   TG advertises: notifications.tasks.status = true                      │
│                                                                         │
│   Client ◀───────SSE────────▶ ThoughtGate ◀───────SSE────────▶ Upstream│
│                                                                         │
│   1. Client subscribes to TG task status (SSE)                          │
│   2. TG subscribes to upstream task status (SSE)                        │
│   3. Upstream sends notification → TG translates → forwards to client   │
│                                                                         │
│   CASE 2: Upstream DOES NOT support SSE                                 │
│   ─────────────────────────────────────────                             │
│   Initialize: TG detects NO upstream SSE capability                     │
│   TG DOES NOT advertise: notifications.tasks.status                     │
│                                                                         │
│   Client ──────polling──────▶ ThoughtGate ──────polling──────▶ Upstream │
│                                                                         │
│   Client must poll for updates (no SSE available)                       │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

**Why conditional:**
- With lazy polling, TG does not actively monitor task state
- Without upstream SSE events, TG has nothing to forward
- Advertising SSE without upstream support would leave clients on idle connections

**Hybrid model (when SSE available):**
- SSE provides instant notifications
- Polling provides fallback if SSE disconnects
- Clients can use either or both

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-CORE-003 | **Uses** | Upstream HTTP client for task calls and streaming |
| REQ-CORE-007 | **Uses** | Tool annotation cache for upstream requirements |
| REQ-GOV-001 | **Coordinates with** | Task state transitions during execution |
| REQ-GOV-002 | **Extends** | Execution pipeline post-approval |
| REQ-GOV-003 | **Receives from** | Approval decision triggers execution |

## 3. Intent

The system must:
1. Detect when upstream tool requires task-augmented calls
2. Execute upstream with appropriate call style based on annotation
3. Store upstream task ID for lazy polling
4. Proxy `tasks/get` requests to upstream on-demand
5. Stream `tasks/result` responses without buffering
6. Handle upstream task cancellation on TG task expiry/cancellation
7. Provide observability into nested task state

## 4. Scope

### 4.1 In Scope
- Upstream tool annotation lookup during execution
- Task-augmented upstream calls when required
- Upstream task ID storage for lazy polling
- On-demand proxying of `tasks/get` to upstream
- Streaming proxy of `tasks/result` to upstream
- Upstream task cancellation on expiry/cancellation
- Background cleanup for expired Executing tasks
- Execution timeout handling
- **SSE notification forwarding (bidirectional)**
- **TG-generated SSE notifications for internal state transitions**

### 4.2 Out of Scope
- Active polling loops (explicitly rejected for sidecar efficiency)
- Result caching/storage (all results streamed directly to client)
- Task state machine (REQ-GOV-001)
- Approval workflow (REQ-GOV-002, REQ-GOV-003)
- Tool annotation computation (REQ-CORE-007)
- Client-facing task API (REQ-GOV-001)

> **Design Principle: Execute on Demand**
> 
> ThoughtGate never stores results. For all execution paths:
> - `forbidden`: Execute when client calls `tasks/result` (deferred)
> - `optional`/`required`: Stream from upstream when client calls `tasks/result`
> 
> This eliminates memory pressure and avoids wasted work if client abandons task.

### 4.3 Entry Point: Handoff from REQ-GOV-002

This component is invoked by REQ-GOV-002 (Approval Pipeline) after Post-Approval Inspection completes:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    REQ-GOV-002 → REQ-GOV-004 HANDOFF                    │
│                                                                         │
│   REQ-GOV-002 completes:                                                │
│   1. Policy decision (Approve)                                          │
│   2. Pre-Approval Inspection                                            │
│   3. Task creation (InputRequired)                                      │
│   4. Approval wait + validation                                         │
│   5. Post-Approval Inspection                                           │
│                                                                         │
│   Then calls:                                                           │
│       upstream_orchestrator.execute(task, request)                      │
│                                                                         │
│   REQ-GOV-004 takes over:                                               │
│   1. Annotation lookup                                                  │
│   2. Execution path selection (DeferredSync or Task)                    │
│   3. Task state management (Approved → Executing → Completed)           │
│   4. Result streaming                                                   │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

See REQ-GOV-002 v0.2 Updates for the complete pipeline handoff diagram.

### 4.4 Deployment Constraints

> **IMPORTANT:** Upstream MCP server result retention SHOULD exceed the maximum 
> ThoughtGate task TTL. If upstream purges results before clients retrieve them, 
> results will be lost and clients will receive an error.

| Configuration | Recommendation |
|---------------|----------------|
| TG max TTL | 24 hours (default) |
| Upstream result retention | ≥ 24 hours |

## 5. Constraints

### 5.1 Configuration

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| Upstream connect timeout | 5s | `THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS` |
| Upstream request timeout | 30s | `THOUGHTGATE_UPSTREAM_REQUEST_TIMEOUT_SECS` |
| Upstream cancel timeout | 5s | `THOUGHTGATE_UPSTREAM_CANCEL_TIMEOUT_SECS` |
| **Lazy poll proxy timeout** | 10s | `THOUGHTGATE_LAZY_POLL_TIMEOUT_SECS` |
| Streaming chunk size | 64KB | `THOUGHTGATE_STREAM_CHUNK_SIZE_BYTES` |
| SSE forwarding enabled | true | `THOUGHTGATE_SSE_FORWARDING_ENABLED` |
| SSE reconnect delay | 5s | `THOUGHTGATE_SSE_RECONNECT_DELAY_SECS` |
| SSE reconnect max attempts | 3 | `THOUGHTGATE_SSE_RECONNECT_MAX_ATTEMPTS` |
| **SSE client idle timeout** | 5m | `THOUGHTGATE_SSE_CLIENT_IDLE_TIMEOUT_SECS` |

**Lazy Poll Proxy Timeout:** When client calls `tasks/get` or `tasks/result`, TG proxies to upstream. If upstream doesn't respond within this timeout, return error to client (they can retry).

**SSE Client Idle Timeout:** If an SSE client connection has no activity (no events sent, no pings acknowledged) for this duration, close the connection and clean up resources. Prevents resource leaks from abandoned connections.

### 5.2 Upstream Task Lifecycle

ThoughtGate must handle all SEP-1686 task states from upstream during lazy polling:

| Upstream Status | ThoughtGate Action (on tasks/get) |
|-----------------|-----------------------------------|
| `working` | Return `working` to client |
| `input_required` | Return `working` to client (TG can't provide upstream's input) |
| `completed` | Return `completed` to client |
| `failed` | Return `failed` to client, transition TG task to Failed |
| `cancelled` | Return `failed` to client, transition TG task to Failed |

| Upstream Status | ThoughtGate Action (on tasks/result) |
|-----------------|--------------------------------------|
| `completed` | Stream result to client, transition TG task to Completed |
| `working` / `input_required` | Return error: ResultNotReady |
| `failed` / `cancelled` | Return error with failure info |
| Unknown task | Return error: UpstreamTaskNotFound |

### 5.3 TTL Propagation

When calling upstream with task metadata, propagate remaining TTL:

```
TG Task TTL remaining = original_ttl - (now - created_at)
Upstream Task TTL = min(TG_TTL_remaining, UPSTREAM_MAX_TTL)
```

This ensures upstream task doesn't outlive TG task.

### 5.4 Memory Constraints

| Resource | Bound | Notes |
|----------|-------|-------|
| Per-task state | ~500 bytes | upstream_task_id, expires_at, metadata |
| Streaming buffer | chunk_size (64KB) | Per active `tasks/result` stream |
| No result caching | 0 bytes | Results are never stored in TG |

## 6. Interfaces

### 6.1 Executing Task State

State stored for tasks in Executing state:

```rust
pub struct ExecutingTaskState {
    /// Upstream task ID (if task-augmented call)
    pub upstream_task_id: Option<String>,
    /// Execution path taken
    pub execution_path: ExecutionPath,
    /// When TG task expires
    pub expires_at: Instant,
    /// Whether result has been successfully delivered to client
    pub result_delivered: bool,
}

pub enum ExecutionPath {
    /// Deferred sync execution (upstream forbidden) - execute on tasks/result
    DeferredSync,
    /// Task-augmented call (upstream optional/required) - stream from upstream
    Task,
}
```

**No Result Storage:**

| Path | When Executed | Result Handling |
|------|---------------|-----------------|
| DeferredSync | On `tasks/result` call | Execute upstream, stream directly |
| Task | After approval | Stream from upstream on `tasks/result` |

### 6.2 Upstream Execution Result

```rust
pub enum UpstreamExecutionResult {
    /// Deferred execution - will execute on tasks/result (for forbidden annotation)
    Deferred,
    /// Task created - awaiting client polling (for optional/required annotation)
    TaskCreated {
        upstream_task_id: String,
        initial_status: String,
        poll_interval: Option<Duration>,
    },
}

pub enum UpstreamExecutionError {
    /// Upstream returned error on initial call
    CallFailed { error: McpError },
    /// Upstream unreachable
    ConnectionFailed { error: String },
    /// Upstream request timeout
    Timeout { phase: String },
    /// TG task TTL expired before execution started
    TtlExpired,
    /// TG task was cancelled before execution started
    Cancelled,
}
```

### 6.3 Lazy Poll Result

```rust
pub enum LazyPollResult {
    /// Upstream task status retrieved
    Status {
        status: String,
        status_message: Option<String>,
        poll_interval: Option<Duration>,
    },
    /// TG task has expired
    Expired,
    /// Upstream task not found (may have been purged)
    UpstreamNotFound,
    /// Upstream unreachable
    UpstreamUnavailable { error: String },
}
```

### 6.4 Streaming Result

```rust
/// Streamed from upstream to client without buffering
pub struct StreamingResult {
    /// Content type from upstream
    pub content_type: String,
    /// Chunked body stream
    pub body: impl Stream<Item = Result<Bytes, Error>>,
}

pub enum StreamingError {
    /// Upstream task not ready
    ResultNotReady { status: String },
    /// Upstream task failed
    UpstreamFailed { message: String },
    /// Upstream task not found
    UpstreamNotFound,
    /// TG task expired
    TtlExpired,
    /// Stream interrupted
    StreamInterrupted { error: String },
}

pub enum SseError {
    /// SSE not supported (upstream doesn't support, so TG didn't advertise)
    NotSupported { reason: String },
    /// Task not found
    TaskNotFound,
    /// Task is passthrough (not TG-owned), SSE not available
    PassthroughTask,
    /// Connection failed
    ConnectionFailed { error: String },
}
```

### 6.5 Task Metadata for Upstream

```rust
pub struct UpstreamTaskMetadata {
    /// TTL in milliseconds (propagated from TG task)
    pub ttl: u64,
}

impl UpstreamTaskMetadata {
    pub fn from_tg_task(task: &Task) -> Self {
        let remaining = task.expires_at.saturating_duration_since(Instant::now());
        Self {
            ttl: remaining.as_millis() as u64,
        }
    }
}
```

### 6.6 SSE Registry

```rust
use tokio::sync::mpsc;
use std::collections::HashMap;
use parking_lot::RwLock;

/// Registry managing bidirectional SSE subscriptions
pub struct SseRegistry {
    /// Client subscriptions: tg_task_id → list of client SSE senders
    client_subs: RwLock<HashMap<TaskId, Vec<ClientSseSender>>>,
    /// Upstream subscriptions: tg_task_id → upstream SSE handle
    upstream_subs: RwLock<HashMap<TaskId, UpstreamSseHandle>>,
    /// Task ID mapping: upstream_task_id → tg_task_id (for notification translation)
    task_id_map: RwLock<HashMap<String, TaskId>>,
}

/// Sender for pushing notifications to a connected client
pub struct ClientSseSender {
    tx: mpsc::Sender<SseEvent>,
    connected_at: Instant,
}

/// Handle to an upstream SSE subscription
pub struct UpstreamSseHandle {
    /// Upstream task ID being watched
    upstream_task_id: String,
    /// Cancellation token to stop the subscription
    cancel: CancellationToken,
    /// Connection state
    state: UpstreamSseState,
}

pub enum UpstreamSseState {
    Connected,
    Reconnecting { attempt: u32 },
    Disconnected,
}
```

### 6.7 SSE Notification Types

```rust
/// Notification sent to clients via SSE
#[derive(Serialize)]
pub struct TaskStatusNotification {
    pub task_id: TaskId,
    pub status: String,
    pub status_message: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// SSE event wrapper
pub enum SseEvent {
    /// Task status update
    TaskStatus(TaskStatusNotification),
    /// Keep-alive ping
    Ping,
    /// Connection closing
    Close { reason: String },
}

impl SseEvent {
    pub fn to_sse_data(&self) -> String {
        match self {
            SseEvent::TaskStatus(n) => {
                format!("event: notifications/tasks/status\ndata: {}\n\n", 
                        serde_json::to_string(n).unwrap())
            }
            SseEvent::Ping => ":ping\n\n".to_string(),
            SseEvent::Close { reason } => {
                format!("event: close\ndata: {}\n\n", reason)
            }
        }
    }
}
```

### 6.8 SSE Capability Detection

```rust
/// Check if upstream supports SSE for task notifications
pub struct UpstreamSseCapability {
    /// Upstream advertises notifications.tasks.status
    pub supports_task_notifications: bool,
    /// SSE endpoint URL (if different from main endpoint)
    pub sse_endpoint: Option<String>,
}

impl UpstreamSseCapability {
    pub fn from_initialize_response(resp: &InitializeResponse) -> Self {
        let supports = resp
            .capabilities
            .as_ref()
            .and_then(|c| c.notifications.as_ref())
            .and_then(|n| n.tasks.as_ref())
            .map(|t| t.status.unwrap_or(false))
            .unwrap_or(false);
        
        Self {
            supports_task_notifications: supports,
            sse_endpoint: None, // Use main endpoint
        }
    }
}
```

## 7. Functional Requirements

### F-001: Upstream Annotation Lookup

- **F-001.1:** Before upstream execution, lookup tool annotation from cache
- **F-001.2:** If annotation not in cache, fetch via `tools/list` and cache
- **F-001.3:** Handle missing annotation as `optional` (conservative)

### F-002: Execution Path Selection

```
┌─────────────────────────────────────────────────────────────────────────┐
│                     EXECUTION PATH SELECTION                            │
│                                                                         │
│   Input: Approved tool call ready for execution                         │
│                                                                         │
│   1. Lookup upstream annotation for tool_name                           │
│                                                                         │
│   2. Select execution path:                                             │
│      ├── forbidden → setup_deferred_sync()                              │
│      │   └── Mark execution path as DeferredSync                        │
│      │   └── Remain in Approved state                                   │
│      │   └── Execution happens on tasks/result call                     │
│      │                                                                  │
│      └── optional/required → execute_with_task()                        │
│          └── Create upstream task immediately                           │
│          └── Store upstream_task_id                                     │
│          └── Transition to Executing state                              │
│          └── Client polls, then calls tasks/result to stream            │
│                                                                         │
│   KEY INSIGHT: Deferred execution for `forbidden` avoids wasted work    │
│   if client abandons task. Upstream doesn't execute until result needed.│
│                                                                         │
│   UNIFORM API: Both paths return task response, not result.             │
│   Client always calls tasks/result to retrieve actual result.           │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

- **F-002.1:** Route to `setup_deferred_sync` for `forbidden` annotation
- **F-002.2:** Route to `execute_with_task` for `optional` or `required` annotation
- **F-002.3:** Log execution path selection at DEBUG level

### F-003: Deferred Sync Setup (Forbidden Annotation)

For tools where upstream annotation is `forbidden`, defer execution until client requests result:

```rust
async fn setup_deferred_sync(
    task: &mut Task,
    task_manager: &TaskManager,
) -> Result<UpstreamExecutionResult, UpstreamExecutionError> {
    // Check expiry before setup
    if task.is_expired() {
        return Err(UpstreamExecutionError::TtlExpired);
    }
    
    // Check cancellation
    if task.is_cancelled() {
        return Err(UpstreamExecutionError::Cancelled);
    }
    
    // Mark execution path - actual execution deferred to tasks/result
    task.set_executing_state(ExecutingTaskState {
        upstream_task_id: None,  // No upstream task for deferred sync
        execution_path: ExecutionPath::DeferredSync,
        expires_at: task.expires_at,
        result_delivered: false,
    });
    
    // Task remains in Approved state (exposed as "working" to client)
    // Execution will happen when client calls tasks/result
    
    Ok(UpstreamExecutionResult::Deferred)
}
```

**Deferred Execution Flow:**

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    DEFERRED SYNC EXECUTION FLOW                         │
│                                                                         │
│   1. Client: tools/call { task: { ttl: 600000 }, ... }                  │
│                                                                         │
│   2. TG: Creates task (InputRequired) - awaiting approval               │
│                                                                         │
│   3. Approval received → task transitions to Approved                   │
│                                                                         │
│   4. TG: Checks annotation = forbidden → setup_deferred_sync()          │
│      └── Marks path as DeferredSync                                     │
│      └── NO execution yet                                               │
│                                                                         │
│   5. TG → Client: { taskId: "tg_xyz", status: "working" }               │
│                                                                         │
│   6. Client: tasks/get { taskId: "tg_xyz" }                             │
│      └── TG returns: { status: "working" } (still approved, not done)   │
│                                                                         │
│   7. Client: tasks/result { taskId: "tg_xyz" }                          │
│      └── TG: NOW executes upstream synchronously                        │
│      └── TG: Streams result directly to client                          │
│      └── TG: Transitions to Completed                                   │
│                                                                         │
│   WHY DEFERRED:                                                         │
│   - No wasted work if client abandons task                              │
│   - No result storage needed                                            │
│   - Execution happens exactly when result is needed                     │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

- **F-003.1:** Mark execution path as DeferredSync
- **F-003.2:** Task remains in Approved state (exposed as "working")
- **F-003.3:** Return Deferred result to caller
- **F-003.4:** Actual execution happens in F-006 (tasks/result handler)

### F-004: Task-Augmented Execution (Optional/Required Annotation)

For tools where upstream annotation is `optional` or `required`, create upstream task immediately after approval:

```rust
async fn execute_with_task(
    task: &mut Task,
    request: &ToolCallRequest,
    upstream: &UpstreamClient,
) -> Result<UpstreamExecutionResult, UpstreamExecutionError> {
    // Check expiry before starting
    if task.is_expired() {
        return Err(UpstreamExecutionError::TtlExpired);
    }
    
    let task_metadata = UpstreamTaskMetadata::from_tg_task(task);
    
    // Create upstream task
    let task_response = upstream.call_tool_with_task(request, task_metadata).await?;
    
    // Store upstream task ID for lazy polling
    task.set_executing_state(ExecutingTaskState {
        upstream_task_id: Some(task_response.task_id.clone()),
        execution_path: ExecutionPath::Task,
        expires_at: task.expires_at,
        result_delivered: false,
    });
    
    Ok(UpstreamExecutionResult::TaskCreated {
        upstream_task_id: task_response.task_id,
        initial_status: task_response.status,
        poll_interval: task_response.poll_interval,
    })
}
```

- **F-004.1:** Compute remaining TTL from TG task
- **F-004.2:** Send `tools/call` with task metadata containing TTL
- **F-004.3:** Store `upstream_task_id` in TG task state
- **F-004.4:** Return TG task response to client (with TG task ID)
- **F-004.5:** TG task remains in `Executing` state

### F-005: Lazy Poll Handler (tasks/get)

Handle `tasks/get` based on task state and execution path:

```rust
async fn handle_tasks_get(
    task_id: &TaskId,
    task_manager: &TaskManager,
    upstream: &UpstreamClient,
) -> Result<TaskStatus, TaskError> {
    let task = task_manager.get(task_id)?;
    
    match task.state() {
        TaskState::Completed => {
            Ok(TaskStatus::completed())
        }
        TaskState::InputRequired => {
            Ok(TaskStatus::input_required("Awaiting approval"))
        }
        TaskState::Approved => {
            // Approved but not yet executed
            // For DeferredSync: execution happens on tasks/result
            // Return "working" - client should call tasks/result when ready
            Ok(TaskStatus::working("Approved, awaiting result retrieval"))
        }
        TaskState::Executing => {
            match task.execution_path() {
                ExecutionPath::DeferredSync => {
                    // Should not happen - DeferredSync stays in Approved until tasks/result
                    Err(TaskError::InvalidState)
                }
                ExecutionPath::Task => {
                    // Task path: proxy to upstream
                    handle_tasks_get_task_path(&task, upstream).await
                }
            }
        }
        TaskState::Failed | TaskState::Cancelled | TaskState::Expired => {
            Ok(TaskStatus::from_terminal_state(task.state()))
        }
    }
}

/// Task execution path: proxy to upstream
async fn handle_tasks_get_task_path(
    task: &Task,
    upstream: &UpstreamClient,
) -> Result<TaskStatus, TaskError> {
    // Check TG task expiry first
    if task.is_expired() {
        // Cancel upstream task (best effort)
        if let Some(upstream_id) = task.upstream_task_id() {
            let _ = upstream.tasks_cancel(upstream_id).await;
        }
        return Ok(TaskStatus::expired());
    }
    
    let upstream_id = task.upstream_task_id()
        .ok_or(TaskError::InvalidState)?;
    
    // Proxy to upstream with timeout
    let lazy_poll_timeout = config.lazy_poll_timeout;
    
    match tokio::time::timeout(
        lazy_poll_timeout,
        upstream.tasks_get(upstream_id),
    ).await {
        Ok(Ok(status)) => {
            // Map upstream status to TG status
            let mapped = map_upstream_status(&status);
            
            // If upstream terminal with failure, transition TG task
            if status.status == "failed" || status.status == "cancelled" {
                task_manager.transition(task.id, TaskState::Failed).await?;
            }
            
            Ok(mapped)
        }
        Ok(Err(UpstreamError::NotFound)) => {
            // Upstream purged the task
            Err(TaskError::UpstreamTaskNotFound)
        }
        Ok(Err(e)) => {
            Err(TaskError::UpstreamUnavailable(e.to_string()))
        }
        Err(_) => {
            // Timeout - don't fail TG task, client can retry
            Err(TaskError::Timeout { phase: "lazy_poll_proxy".into() })
        }
    }
}
```

- **F-005.1:** Route by task state: Completed/Failed/Cancelled return status directly
- **F-005.2:** InputRequired returns "awaiting approval" status
- **F-005.3:** Approved returns "working" (deferred execution awaiting tasks/result)
- **F-005.4:** Executing + DeferredSync: should not occur (invalid state)
- **F-005.5:** Executing + Task path: check TG task expiry before proxying
- **F-005.6:** If expired, cancel upstream task and return Expired status
- **F-005.7:** Proxy `tasks/get` to upstream **with timeout** (`THOUGHTGATE_LAZY_POLL_TIMEOUT_SECS`)
- **F-005.8:** Map upstream status to TG-compatible response
- **F-005.9:** Pass through `poll_interval` from upstream
- **F-005.10:** On upstream terminal failure, transition TG task to Failed
- **F-005.11:** **CRITICAL:** MUST NOT call upstream `tasks/result` during `tasks/get` handling
- **F-005.12:** On timeout, return error to client (they can retry); don't fail TG task

> ⚠️ **Race Condition Prevention:** When `tasks/get` returns `status: "completed"`, 
> ThoughtGate MUST NOT proactively call `tasks/result` to cache/verify the result. 
> Doing so would consume the result, and when the client subsequently calls 
> `tasks/result`, the upstream may return "already retrieved" or similar error.
>
> The correct flow is:
> 1. Client calls `tasks/get` → TG proxies to upstream `tasks/get` → returns status only
> 2. Client sees `completed` → Client calls `tasks/result` → TG proxies to upstream `tasks/result`
>
> ThoughtGate is a transparent proxy for task status; it never preemptively fetches results.

### F-006: Result Handler (tasks/result)

Handle `tasks/result` based on task state and execution path:

```rust
async fn handle_tasks_result(
    task: &mut Task,
    request: &ToolCallRequest,  // Original request stored in task
    upstream: &UpstreamClient,
    client_response: &mut Response,
    task_manager: &TaskManager,
    config: &Config,
) -> Result<(), TaskError> {
    // Check state first
    match task.state() {
        TaskState::Approved => {
            // Deferred sync: execute NOW
            handle_tasks_result_deferred_sync(
                task, request, upstream, client_response, task_manager, config
            ).await
        }
        TaskState::Executing => {
            match task.execution_path() {
                ExecutionPath::DeferredSync => {
                    // Should not happen - DeferredSync stays in Approved
                    Err(TaskError::InvalidState)
                }
                ExecutionPath::Task => {
                    // Task path: stream from upstream
                    handle_tasks_result_task(task, upstream, client_response, task_manager).await
                }
            }
        }
        TaskState::Completed => {
            // Already completed - this shouldn't happen for streaming results
            // Client should only call tasks/result once
            Err(TaskError::ResultAlreadyDelivered)
        }
        TaskState::InputRequired => {
            Err(TaskError::NotReady { reason: "Awaiting approval".into() })
        }
        _ => {
            Err(TaskError::TerminalState { state: task.state().to_string() })
        }
    }
}

/// Deferred sync execution: execute upstream NOW and stream result
async fn handle_tasks_result_deferred_sync(
    task: &mut Task,
    request: &ToolCallRequest,
    upstream: &UpstreamClient,
    client_response: &mut Response,
    task_manager: &TaskManager,
    config: &Config,
) -> Result<(), TaskError> {
    // Check expiry before execution
    if task.is_expired() {
        task_manager.transition(&task.id, TaskState::Expired).await?;
        return Err(TaskError::Expired);
    }
    
    // Check cancellation
    if task.is_cancelled() {
        return Err(TaskError::Cancelled);
    }
    
    // Transition to Executing (briefly, during upstream call)
    task_manager.transition(&task.id, TaskState::Executing).await?;
    
    // Execute upstream synchronously with timeout
    let upstream_result = tokio::time::timeout(
        config.sync_forward_timeout,
        upstream.call_tool(request),
    ).await;
    
    match upstream_result {
        Ok(Ok(response)) => {
            // Stream response directly to client (no buffering)
            stream_response(response, client_response).await?;
            
            // Mark delivered and transition to Completed
            task.set_result_delivered(true);
            task_manager.transition(&task.id, TaskState::Completed).await?;
            
            Ok(())
        }
        Ok(Err(e)) => {
            // Upstream returned error
            task_manager.transition(&task.id, TaskState::Failed).await?;
            Err(TaskError::UpstreamError { error: e.to_string() })
        }
        Err(_) => {
            // Timeout
            task_manager.transition(&task.id, TaskState::Failed).await?;
            Err(TaskError::Timeout { phase: "deferred_sync_execution".into() })
        }
    }
}

/// Task execution path: stream from upstream task
async fn handle_tasks_result_task(
    task: &mut Task,
    upstream: &UpstreamClient,
    client_response: &mut Response,
    task_manager: &TaskManager,
) -> Result<(), TaskError> {
    // Check TG task expiry
    if task.is_expired() {
        if let Some(upstream_id) = task.upstream_task_id() {
            let _ = upstream.tasks_cancel(upstream_id).await;
        }
        return Err(TaskError::Expired);
    }
    
    let upstream_id = task.upstream_task_id()
        .ok_or(TaskError::InvalidState)?;
    
    // Proxy to upstream
    let upstream_result = upstream.tasks_result_stream(upstream_id).await?;
    
    // Stream to client without buffering
    stream_response(upstream_result, client_response).await?;
    
    // Mark result as delivered and transition to Completed
    task.set_result_delivered(true);
    task_manager.transition(&task.id, TaskState::Completed).await?;
    
    Ok(())
}

async fn stream_response(
    upstream: UpstreamResponse,
    client: &mut Response,
) -> Result<(), StreamingError> {
    // Set headers
    client.set_content_type(upstream.content_type);
    
    // Stream chunks
    let mut body = upstream.body;
    while let Some(chunk) = body.next().await {
        let bytes = chunk.map_err(|e| StreamingError::StreamInterrupted { 
            error: e.to_string() 
        })?;
        client.write_chunk(bytes).await?;
    }
    
    Ok(())
}
```

**Result Handling by Execution Path:**

| Path | Task State | `tasks/result` Behavior |
|------|------------|-------------------------|
| DeferredSync | Approved | Execute upstream NOW, stream result |
| Task | Executing | Stream from upstream task |

- **F-006.1:** Route based on task state first, then execution path
- **F-006.2:** Approved + DeferredSync: execute upstream synchronously on-demand
- **F-006.3:** DeferredSync: apply SYNC_FORWARD_TIMEOUT during execution
- **F-006.4:** DeferredSync: stream response directly to client (no buffering)
- **F-006.5:** Task path: check expiry, cancel upstream if expired
- **F-006.6:** Task path: stream from upstream `tasks/result`
- **F-006.7:** Task path: proxy chunks without buffering
- **F-006.8:** Mark `result_delivered = true` on success
- **F-006.9:** Transition to Completed after successful streaming
- **F-006.10:** On error, transition to Failed with appropriate error
- **F-006.11:** Handle upstream "not ready" by returning ResultNotReady error

### F-007: Upstream Task Cancellation

- **F-007.1:** On TG task expiry (detected during lazy poll or background cleanup), cancel upstream task
- **F-007.2:** On client cancellation of TG task, cancel upstream task
- **F-007.3:** On ThoughtGate shutdown, cancel all upstream tasks for Executing TG tasks
- **F-007.4:** Apply `UPSTREAM_CANCEL_TIMEOUT` to cancellation calls
- **F-007.5:** Log cancellation attempts and results
- **F-007.6:** Best-effort: don't fail TG task if upstream cancel fails
- **F-007.7:** **Cancel during streaming:** If cancellation requested while `tasks/result` is streaming:
  - Mark task as "cancel pending"
  - Allow in-flight stream to complete (don't interrupt)
  - Transition to Cancelled after stream completes or errors
  - Rationale: Cancellation means "don't start new work," not "abort delivery"

```rust
async fn cancel_upstream_task(
    upstream: &UpstreamClient,
    upstream_task_id: &str,
    timeout: Duration,
) {
    match tokio::time::timeout(
        timeout,
        upstream.tasks_cancel(upstream_task_id),
    ).await {
        Ok(Ok(_)) => {
            info!("Cancelled upstream task: {}", upstream_task_id);
        }
        Ok(Err(e)) => {
            warn!("Failed to cancel upstream task {}: {}", upstream_task_id, e);
        }
        Err(_) => {
            warn!("Timeout cancelling upstream task: {}", upstream_task_id);
        }
    }
}

/// Cancel with streaming awareness
async fn handle_cancel_request(
    task: &mut Task,
    upstream: &UpstreamClient,
    task_manager: &TaskManager,
) -> Result<(), TaskError> {
    if task.is_streaming() {
        // Mark for cancellation after stream completes
        task.set_cancel_pending(true);
        Ok(())
    } else {
        // Cancel immediately
        if let Some(upstream_id) = task.upstream_task_id() {
            cancel_upstream_task(upstream, upstream_id, CANCEL_TIMEOUT).await;
        }
        task_manager.transition(&task.id, TaskState::Cancelled).await
    }
}
```

### F-008: Background Cleanup for Executing Tasks

Background cleanup handles Executing tasks that expire without client interaction:

```rust
async fn cleanup_executing_tasks(
    task_manager: &TaskManager,
    upstream: &UpstreamClient,
) {
    let expired = task_manager.find_expired_in_state(TaskState::Executing).await;
    
    for task in expired {
        // Cancel upstream task
        if let Some(upstream_id) = task.upstream_task_id() {
            cancel_upstream_task(upstream, upstream_id, CANCEL_TIMEOUT).await;
        }
        
        // Transition to Expired
        let _ = task_manager.transition(&task.id, TaskState::Expired).await;
        
        info!("Expired Executing task: {} (upstream: {:?})", 
              task.id, task.upstream_task_id());
    }
}
```

- **F-008.1:** Run as part of periodic cleanup loop (see REQ-GOV-001)
- **F-008.2:** Find Executing tasks past TTL
- **F-008.3:** Cancel upstream task (best effort)
- **F-008.4:** Transition TG task to Expired
- **F-008.5:** Log expiration with upstream task ID

### F-009: SSE Client Endpoint

Provide SSE endpoint for clients to subscribe to task status notifications.

**Prerequisite:** This endpoint is only available when ThoughtGate advertised `notifications.tasks.status` capability (i.e., upstream supports SSE).

```rust
async fn handle_sse_subscribe(
    task_id: TaskId,
    registry: Arc<SseRegistry>,
    task_manager: Arc<TaskManager>,
    capability_cache: Arc<CapabilityCache>,
) -> Result<Sse<impl Stream<Item = Event>>, TaskError> {
    // CRITICAL: Reject if SSE was not advertised
    if !capability_cache.upstream_supports_task_sse() {
        return Err(TaskError::SseNotSupported {
            reason: "Server does not support task notifications".into(),
        });
    }
    
    // Verify task exists and is owned by ThoughtGate
    let task = task_manager.get(&task_id)?;
    
    if !task_id.starts_with("tg_") {
        // Not a TG-owned task, cannot subscribe
        return Err(TaskError::SseNotSupported {
            reason: "SSE only available for approval-required tasks".into(),
        });
    }
    
    if task.is_terminal() {
        // For terminal tasks, send final status and close
        return Ok(single_event_sse(task.status_notification()));
    }
    
    // Create channel for this client
    let (tx, rx) = mpsc::channel(16);
    
    // Register subscription
    registry.add_client_sub(&task_id, ClientSseSender {
        tx,
        connected_at: Instant::now(),
    }).await;
    
    // Return SSE stream
    let stream = ReceiverStream::new(rx)
        .map(|event| Event::default().data(event.to_sse_data()));
    
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text(":ping")
    ))
}
```

- **F-009.1:** Reject subscription if SSE capability not advertised (upstream doesn't support)
- **F-009.2:** Reject subscription for non-TG-owned tasks (passthrough tasks)
- **F-009.3:** Verify task exists
- **F-009.4:** For terminal tasks, return single event with final status
- **F-009.5:** Register client in SSE registry
- **F-009.6:** Return SSE stream with keep-alive pings
- **F-009.7:** Clean up registration when client disconnects
- **F-009.8:** **Idle timeout:** Close connection if no activity for `THOUGHTGATE_SSE_CLIENT_IDLE_TIMEOUT_SECS` (prevents resource leaks from abandoned connections)

### F-010: Upstream SSE Subscription (Lazy)

**Critical: Upstream SSE is lazy.** Only subscribe to upstream SSE when a client subscribes to TG's SSE for that task. If clients only poll (`tasks/get`), never connect to upstream SSE.

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    LAZY SSE SUBSCRIPTION                                │
│                                                                         │
│   Client polls (tasks/get):                                             │
│   - TG proxies to upstream tasks/get                                    │
│   - NO upstream SSE connection                                          │
│   - Zero background resource usage                                      │
│                                                                         │
│   Client subscribes (SSE):                                              │
│   - FIRST client subscription triggers upstream SSE connection          │
│   - Subsequent clients share the same upstream connection               │
│   - When LAST client unsubscribes, close upstream SSE                   │
│                                                                         │
│   WHY LAZY:                                                             │
│   - Many clients will only poll, never use SSE                          │
│   - Spawning SSE forwarder per task wastes resources                    │
│   - Consistent with lazy polling philosophy                             │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

```rust
/// Called when client subscribes to TG task SSE (from F-009)
async fn handle_client_sse_subscribe(
    task: &Task,
    registry: Arc<SseRegistry>,
    upstream: Arc<UpstreamClient>,
    config: &SseConfig,
) -> Result<impl Stream<Item = SseEvent>, SseError> {
    // ... validation from F-009 ...
    
    // Register client subscription
    let (tx, rx) = mpsc::channel(16);
    registry.add_client_sub(&task.id, ClientSseSender { tx, ... }).await;
    
    // LAZY: Check if we need to start upstream subscription
    // Only if this is the FIRST client for this task AND task has upstream_task_id
    if let Some(upstream_task_id) = task.upstream_task_id() {
        let needs_upstream = registry.client_count(&task.id).await == 1;
        
        if needs_upstream && upstream.supports_task_sse() {
            // First client - start upstream SSE subscription
            start_upstream_sse_subscription(
                registry.clone(),
                task.id.clone(),
                upstream_task_id.clone(),
                upstream,
                config,
            ).await;
        }
    }
    
    Ok(ReceiverStream::new(rx))
}

/// Called when client disconnects from SSE
async fn handle_client_sse_disconnect(
    task_id: &TaskId,
    client_id: usize,
    registry: Arc<SseRegistry>,
) {
    // Remove client
    registry.remove_client_sub(task_id, client_id).await;
    
    // LAZY: Check if we should stop upstream subscription
    // Only if this was the LAST client for this task
    let remaining_clients = registry.client_count(task_id).await;
    
    if remaining_clients == 0 {
        // Last client disconnected - stop upstream SSE
        if let Some(handle) = registry.remove_upstream_sub(task_id).await {
            handle.cancel.cancel();
        }
    }
}

/// Start upstream SSE subscription (called lazily on first client)
async fn start_upstream_sse_subscription(
    registry: Arc<SseRegistry>,
    tg_task_id: TaskId,
    upstream_task_id: String,
    upstream: Arc<UpstreamClient>,
    config: &SseConfig,
) {
    // Check if upstream supports SSE
    if !upstream.supports_task_sse() {
        debug!("Upstream doesn't support SSE, clients will receive TG-generated events only");
        return;
    }
    
    // Check if SSE forwarding is enabled
    if !config.sse_forwarding_enabled {
        return;
    }
    
    // Register task ID mapping for notification translation
    registry.add_task_mapping(&upstream_task_id, &tg_task_id).await;
    
    // Create cancellation token for this subscription
    let cancel = CancellationToken::new();
    
    // Spawn forwarder task
    spawn_sse_forwarder(
        registry.clone(),
        tg_task_id.clone(),
        upstream_task_id.clone(),
        upstream,
        cancel.clone(),
        config.clone(),
    );
    
    // Store handle for cleanup
    registry.add_upstream_sub(&tg_task_id, UpstreamSseHandle {
        upstream_task_id,
        cancel,
        state: UpstreamSseState::Connected,
    }).await;
}

fn spawn_sse_forwarder(
    registry: Arc<SseRegistry>,
    tg_task_id: TaskId,
    upstream_task_id: String,
    upstream: Arc<UpstreamClient>,
    cancel: CancellationToken,
    config: SseConfig,
) {
    tokio::spawn(async move {
        let mut reconnect_attempts = 0;
        
        loop {
            // Connect to upstream SSE
            let result = upstream.subscribe_task_status(&upstream_task_id).await;
            
            match result {
                Ok(mut stream) => {
                    reconnect_attempts = 0; // Reset on successful connect
                    
                    loop {
                        tokio::select! {
                            biased;
                            
                            _ = cancel.cancelled() => {
                                debug!("SSE forwarder cancelled: {}", tg_task_id);
                                break;
                            }
                            
                            event = stream.next() => {
                                match event {
                                    Some(Ok(notification)) => {
                                        // Translate and forward
                                        let tg_notification = TaskStatusNotification {
                                            task_id: tg_task_id.clone(),
                                            status: notification.status.clone(),
                                            status_message: notification.status_message,
                                            timestamp: Utc::now(),
                                        };
                                        
                                        registry.broadcast_to_clients(
                                            &tg_task_id,
                                            SseEvent::TaskStatus(tg_notification),
                                        ).await;
                                        
                                        // If terminal, cleanup and exit
                                        if is_terminal_status(&notification.status) {
                                            break;
                                        }
                                    }
                                    Some(Err(e)) => {
                                        warn!("SSE stream error: {} - {}", tg_task_id, e);
                                        break; // Will attempt reconnect
                                    }
                                    None => {
                                        debug!("SSE stream ended: {}", tg_task_id);
                                        break; // Will attempt reconnect
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to connect upstream SSE: {} - {}", tg_task_id, e);
                }
            }
            
            // Check if we should reconnect
            if cancel.is_cancelled() {
                break;
            }
            
            reconnect_attempts += 1;
            if reconnect_attempts > config.reconnect_max_attempts {
                info!("Max SSE reconnect attempts reached: {}", tg_task_id);
                break;
            }
            
            // Wait before reconnecting
            tokio::time::sleep(config.reconnect_delay).await;
        }
        
        // Cleanup
        registry.remove_upstream_sub(&tg_task_id).await;
        registry.remove_task_mapping(&upstream_task_id).await;
    });
}
```

- **F-010.1:** Check upstream SSE capability during `initialize`
- **F-010.2:** **LAZY:** Subscribe to upstream ONLY when first client subscribes to TG task SSE
- **F-010.3:** Translate upstream task ID to TG task ID in notifications
- **F-010.4:** Forward translated notifications to all subscribed clients
- **F-010.5:** Handle reconnection on upstream SSE disconnect
- **F-010.6:** Stop upstream subscription when last client unsubscribes OR task reaches terminal state
- **F-010.7:** Clean up registrations when forwarder exits
- **F-010.8:** If client only polls (tasks/get), NEVER connect to upstream SSE

### F-011: SSE Registry Management

```rust
impl SseRegistry {
    /// Add client subscription
    pub async fn add_client_sub(&self, task_id: &TaskId, sender: ClientSseSender) {
        let mut subs = self.client_subs.write();
        subs.entry(task_id.clone())
            .or_insert_with(Vec::new)
            .push(sender);
    }
    
    /// Remove disconnected client
    pub async fn remove_client_sub(&self, task_id: &TaskId, sender_id: usize) {
        let mut subs = self.client_subs.write();
        if let Some(clients) = subs.get_mut(task_id) {
            clients.retain(|s| s.id != sender_id);
            if clients.is_empty() {
                subs.remove(task_id);
            }
        }
    }
    
    /// Broadcast event to all clients subscribed to a task
    pub async fn broadcast_to_clients(&self, task_id: &TaskId, event: SseEvent) {
        let subs = self.client_subs.read();
        if let Some(clients) = subs.get(task_id) {
            for client in clients {
                // Non-blocking send, drop if channel full
                let _ = client.tx.try_send(event.clone());
            }
        }
    }
    
    /// Clean up all subscriptions for a task (on task completion/expiry)
    pub async fn cleanup_task(&self, task_id: &TaskId) {
        // Cancel upstream subscription
        if let Some(handle) = self.upstream_subs.write().remove(task_id) {
            handle.cancel.cancel();
        }
        
        // Notify and remove client subscriptions
        if let Some(clients) = self.client_subs.write().remove(task_id) {
            for client in clients {
                let _ = client.tx.try_send(SseEvent::Close {
                    reason: "task_completed".to_string(),
                });
            }
        }
        
        // Remove task mapping
        // (handled by forwarder cleanup)
    }
}
```

- **F-011.1:** Thread-safe registration of client subscriptions
- **F-011.2:** Automatic cleanup of disconnected clients
- **F-011.3:** Non-blocking broadcast to multiple clients
- **F-011.4:** Task ID mapping for notification translation
- **F-011.5:** Full cleanup on task completion or expiry

### F-012: TG-Generated SSE Notifications

ThoughtGate generates SSE notifications for internal state transitions (not just upstream-forwarded events):

```rust
impl TaskManager {
    /// Notify SSE clients of TG-internal state transitions
    async fn notify_state_transition(
        &self,
        task: &Task,
        new_status: &str,
        message: Option<&str>,
    ) {
        let notification = TaskStatusNotification {
            task_id: task.id.clone(),
            status: new_status.to_string(),
            status_message: message.map(String::from),
            timestamp: Utc::now(),
        };
        
        self.sse_registry
            .broadcast_to_clients(&task.id, SseEvent::TaskStatus(notification))
            .await;
    }
}

/// State transitions that trigger TG-generated SSE events
impl TaskManager {
    pub async fn transition(&self, task_id: &TaskId, new_state: TaskState) -> Result<(), TaskError> {
        let task = self.get_mut(task_id)?;
        let old_state = task.state();
        
        // Validate transition
        if !old_state.can_transition_to(&new_state) {
            return Err(TaskError::InvalidTransition { from: old_state, to: new_state });
        }
        
        // Apply transition
        task.set_state(new_state);
        
        // Generate SSE notification for TG-internal transitions
        match (old_state, new_state) {
            // Approval received - execution starting
            (TaskState::InputRequired, TaskState::Approved) => {
                self.notify_state_transition(&task, "working", Some("Approved, preparing execution")).await;
            }
            
            // Approval received - immediate task creation
            (TaskState::InputRequired, TaskState::Executing) => {
                self.notify_state_transition(&task, "working", Some("Approved, execution started")).await;
            }
            
            // Deferred execution completing
            (TaskState::Approved, TaskState::Executing) => {
                // Brief transition during deferred sync - no notification needed
            }
            
            // Execution completed (covers both deferred sync and task path)
            (_, TaskState::Completed) => {
                self.notify_state_transition(&task, "completed", None).await;
            }
            
            // Client cancellation
            (_, TaskState::Cancelled) => {
                self.notify_state_transition(&task, "cancelled", None).await;
            }
            
            // TTL expiry
            (_, TaskState::Expired) => {
                self.notify_state_transition(&task, "failed", Some("Task expired")).await;
            }
            
            // Execution failed
            (_, TaskState::Failed) => {
                self.notify_state_transition(&task, "failed", task.failure_reason()).await;
            }
            
            // Rejection (from approval workflow)
            (TaskState::InputRequired, TaskState::Rejected) => {
                self.notify_state_transition(&task, "failed", Some("Request rejected")).await;
            }
            
            _ => {
                // Other transitions don't need SSE (or are covered by upstream)
            }
        }
        
        Ok(())
    }
}
```

**TG-Generated SSE Events:**

| Transition | SSE Status | Message | When |
|------------|------------|---------|------|
| InputRequired → Approved | `working` | "Approved, preparing execution" | Approval received, deferred path |
| InputRequired → Executing | `working` | "Approved, execution started" | Approval received, task path |
| Any → Completed | `completed` | None | Execution finished successfully |
| Any → Cancelled | `cancelled` | None | Client cancelled |
| Any → Expired | `failed` | "Task expired" | TTL exceeded |
| Any → Failed | `failed` | (failure reason) | Execution error |
| InputRequired → Rejected | `failed` | "Request rejected" | Approver rejected |

**Why TG Generates These:**

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    SSE NOTIFICATION SOURCES                             │
│                                                                         │
│   UPSTREAM-GENERATED (forwarded by TG):                                 │
│   - Task execution progress updates                                     │
│   - Partial results / streaming status                                  │
│   - Upstream-side completion                                            │
│                                                                         │
│   TG-GENERATED (F-012):                                                 │
│   - Approval workflow transitions                                       │
│   - Deferred execution triggers                                         │
│   - Cancellation / expiry / rejection                                   │
│                                                                         │
│   Without F-012, clients would need to poll for approval status,        │
│   defeating the purpose of SSE for the full task lifecycle.             │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

- **F-012.1:** Generate SSE on InputRequired → Approved transition
- **F-012.2:** Generate SSE on InputRequired → Executing transition  
- **F-012.3:** Generate SSE on any terminal state transition (Completed/Failed/Cancelled/Expired)
- **F-012.4:** Generate SSE on rejection
- **F-012.5:** Include status_message where meaningful
- **F-012.6:** Use consistent status values per SEP-1686
- **F-012.7:** Only broadcast to clients with active SSE subscriptions

## 8. Non-Functional Requirements

### NF-001: Performance

- **NF-001.1:** Lazy poll proxy overhead < 10ms (excluding upstream latency)
- **NF-001.2:** Streaming throughput ≥ 100MB/s (limited by network, not TG)
- **NF-001.3:** Zero background CPU usage per Executing task (without SSE)
- **NF-001.4:** SSE notification forwarding latency < 50ms

### NF-002: Resource Efficiency

- **NF-002.1:** Per-task memory ≤ 1KB (no result caching)
- **NF-002.2:** Streaming buffer = chunk_size only (default 64KB)
- **NF-002.3:** SSE connections are lightweight (mostly idle, event-driven)
- **NF-002.4:** SSE forwarder uses single tokio task per Executing task with upstream SSE

### NF-003: Reliability

- **NF-003.1:** Survive upstream temporary unavailability (return error, client retries)
- **NF-003.2:** Handle upstream task disappearing (return UpstreamTaskNotFound)
- **NF-003.3:** Best-effort upstream cancellation on TG task expiry
- **NF-003.4:** SSE disconnect triggers automatic reconnection (configurable attempts)
- **NF-003.5:** Polling continues to work if SSE unavailable (hybrid mode)

### NF-004: Observability

**Metrics:**
- `thoughtgate_upstream_execution_total{path="sync|task", status="success|error"}`
- `thoughtgate_upstream_lazy_polls_total{status="success|error|not_found"}`
- `thoughtgate_upstream_result_streams_total{status="success|error"}`
- `thoughtgate_upstream_result_bytes_total` (counter)
- `thoughtgate_upstream_cancellations_total{result="success|failed|timeout"}`
- `thoughtgate_sse_client_connections_active` (gauge)
- `thoughtgate_sse_upstream_connections_active` (gauge)
- `thoughtgate_sse_notifications_forwarded_total`
- `thoughtgate_sse_reconnections_total{result="success|failed"}`

**Logging:**
```
DEBUG Starting upstream execution: tool=delete_user path=task
DEBUG Created upstream task: upstream_task_id=abc-123
DEBUG Lazy poll: tg_task=tg_xyz upstream_task=abc-123 status=working
DEBUG Streaming result: tg_task=tg_xyz upstream_task=abc-123 bytes=1048576
INFO  Upstream execution complete: tool=delete_user path=task
DEBUG SSE client connected: tg_task=tg_xyz client_count=1
DEBUG SSE upstream subscribed: tg_task=tg_xyz upstream_task=abc-123
DEBUG SSE notification forwarded: tg_task=tg_xyz status=completed
```

**Tracing:**
- Span: `upstream_execution`
  - Attribute: `tool.name`
  - Attribute: `execution.path`
  - Attribute: `upstream.task_id`
- Span: `lazy_poll` (per client poll)
- Span: `stream_result`
  - Attribute: `bytes_streamed`
- Span: `sse_forward`
  - Attribute: `upstream.task_id`
  - Attribute: `client_count`

## 9. State Diagram

```
┌─────────────────────────────────────────────────────────────────────────┐
│              LAZY POLLING + SSE NOTIFICATION STATE MACHINE              │
│                                                                         │
│   ┌─────────────┐                                                       │
│   │   Start     │                                                       │
│   └──────┬──────┘                                                       │
│          │                                                              │
│          ▼                                                              │
│   ┌─────────────────┐                                                   │
│   │ Lookup          │                                                   │
│   │ Annotation      │                                                   │
│   └────────┬────────┘                                                   │
│            │                                                            │
│     ┌──────┴──────┐                                                     │
│     │             │                                                     │
│     ▼             ▼                                                     │
│ forbidden     optional/required                                         │
│     │             │                                                     │
│     ▼             ▼                                                     │
│ ┌────────────┐ ┌────────────────┐                                       │
│ │ Deferred   │ │ Create         │                                       │
│ │ Sync Setup │ │ Upstream Task  │                                       │
│ │ (no exec)  │ └───────┬────────┘                                       │
│ └─────┬──────┘         │                                                │
│       │                ▼                                                │
│       │         ┌────────────────┐                                      │
│       ▼         │ Store Task ID  │                                      │
│ ┌────────────┐  │ + Subscribe    │◀── If upstream supports SSE          │
│ │ Approved   │  │   Upstream SSE │                                      │
│ │ (waiting   │  └───────┬────────┘                                      │
│ │  for       │          │                                               │
│ │  result    │          ▼                                               │
│ │  request)  │   ┌────────────────┐                                     │
│ └─────┬──────┘   │   Executing    │◀────────────────┐                   │
│       │          │                │                 │                   │
│       │          └───────┬────────┘                 │                   │
│       │                  │                          │                   │
│       │     ┌────────────┼────────────┬─────────┐   │                   │
│       │     │            │            │         │   │                   │
│       │     ▼            ▼            ▼         ▼   │                   │
│       │  tasks/get   tasks/result  expiry/   SSE    │                   │
│       │  (working)   (stream from  cancel   notif   │                   │
│       │     │         upstream)       │         │   │                   │
│       │     │            │            ▼         │   │                   │
│       │     │            ▼       ┌──────────┐  │   │                   │
│       │     │       ┌──────────┐ │ Cancel   │  │   │                   │
│       │     │       │ Stream   │ │ Upstream │  │   │                   │
│       │     │       │ from     │ │ + SSE    │  │   │                   │
│       │     │       │ upstream │ └────┬─────┘  │   │                   │
│       │     │       └────┬─────┘      │         │   │                   │
│       │     │            │            │         │   │                   │
│       │     │            ├─working────┘         │   │                   │
│       │     │            │      (still exec)────┼───┘                   │
│       │     │            │                      │                       │
│       │     │            │         ┌────────────┘                       │
│       │     │            │         │ (upstream notifies completion)     │
│       │     │            ▼         ▼                                    │
│       │     │         success   completed                               │
│       │     │            │         │                                    │
│       ▼     │            │         ▼                                    │
│  tasks/result            │    ┌──────────┐                              │
│  (execute NOW)           │    │ Forward  │                              │
│       │                  │    │ to       │                              │
│       │                  │    │ clients  │                              │
│       │                  │    └────┬─────┘                              │
│       │                  │         │                                    │
│       ▼                  ▼         ▼                                    │
│   ┌──────────────────────────────────┐                                  │
│   │          Completed               │                                  │
│   │   (cleanup SSE subscriptions)    │                                  │
│   └──────────────────────────────────┘                                  │
│                                                                         │
│   DEFERRED EXECUTION: For `forbidden`, upstream executes on             │
│   tasks/result call - no wasted work, no storage needed.                │
│                                                                         │
│   UNIFORM API: Both paths return task response to tools/call.           │
│   Client always calls tasks/result to retrieve actual result.           │
│                                                                         │
│   HYBRID MODE: Polling and SSE work in parallel                         │
│   - Client can poll OR subscribe to SSE OR both                         │
│   - SSE provides instant notifications                                  │
│   - Polling provides fallback if SSE unavailable                        │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

**Key Differences from Active Polling:**

| Aspect | Active Polling | Lazy Polling |
|--------|----------------|--------------|
| Background loops | Yes, per task | No |
| State transitions | Automatic | Triggered by client |
| Memory per task | Result cached | Just task ID |
| CPU when idle | Polling | Zero |

## 10. Testing Strategy

### 10.1 Unit Tests

| Test Case | Setup | Expected Result |
|-----------|-------|-----------------|
| Sync path selection | Annotation = `optional` | `execute_sync` called |
| Task path selection | Annotation = `required` | `execute_with_task` called |
| Sync execution streams | Mock upstream response | Chunks streamed to client |
| Task creation | Mock upstream task response | upstream_task_id stored |
| Lazy poll: working | Mock upstream returns `working` | Return working to client |
| Lazy poll: completed | Mock upstream returns `completed` | Return completed to client |
| Lazy poll: failed | Mock upstream returns `failed` | Transition TG task to Failed |
| Lazy poll: expired TG task | TG task past TTL | Cancel upstream, return Expired |
| Stream result: success | Mock upstream result stream | Chunks proxied, TG task Completed |
| Stream result: not ready | Mock upstream not ready | Return ResultNotReady error |
| Stream result: upstream gone | Mock upstream 404 | Return UpstreamTaskNotFound error |
| Cancellation | Trigger cancel | Upstream cancel called |
| SSE capability check: supported | upstream_supports_task_sse = true | SSE endpoint available |
| SSE capability check: not supported | upstream_supports_task_sse = false | SSE endpoint rejects |
| SSE client subscribe: valid | TG task, SSE supported | SSE stream returned |
| SSE client subscribe: terminal | Completed task | Single event, stream closed |
| SSE client subscribe: passthrough task | Non-tg_ task ID | Reject with PassthroughTask error |
| SSE client subscribe: SSE not advertised | SSE not supported | Reject with NotSupported error |
| SSE registry: add/remove | Multiple clients | Correct tracking |
| SSE broadcast | Multiple subscribers | All receive notification |
| Deferred sync setup | Annotation = forbidden | Path set to DeferredSync, state = Approved |
| Deferred sync: tasks/get | Task in Approved state | Returns "working" status |
| Deferred sync: tasks/result triggers execution | Task in Approved, client calls tasks/result | Upstream executed, result streamed |
| Deferred sync: timeout | Upstream hangs on tasks/result | SYNC_FORWARD_TIMEOUT triggers, task fails |
| Deferred sync: no storage | After deferred execution | No result stored in task |
| tasks/result routing: deferred path | state=Approved, execution_path=DeferredSync | Executes upstream, streams result |
| tasks/result routing: task path | execution_path=Task | Streams from upstream |

### 10.2 Integration Tests

| Scenario | Setup | Expected Result |
|----------|-------|-----------------|
| Full deferred sync flow | Upstream with forbidden annotation | Approval → Approved state → tasks/result triggers execution → stream result |
| Deferred sync: client abandons | Approval received, client never calls tasks/result | No execution happens, task expires |
| Full task flow | Upstream with required annotation | Create task, lazy poll, stream result |
| Optional uses task path | Upstream with optional annotation | Create upstream task, lazy poll, stream result |
| Large result streaming | Upstream returns 100MB (task path) | Constant memory, successful stream |
| Client abandons task | Create task, never poll | Background cleanup cancels upstream |
| Upstream slow | Upstream task takes 5 minutes | Multiple lazy polls succeed |
| Upstream purges result | Upstream purges before client streams | UpstreamTaskNotFound error |
| SSE full flow | Upstream with SSE support | TG advertises SSE, notifications forwarded |
| SSE + polling hybrid | Client polls and subscribes SSE | Both work, consistent state |
| SSE upstream disconnect | Kill upstream SSE mid-task | Reconnect attempts, fallback to polling |
| No upstream SSE: handshake | Upstream without SSE | TG does NOT advertise SSE capability |
| No upstream SSE: client attempt | Client tries SSE subscribe | Error: SSE not supported |
| TG-SSE: approval received | InputRequired → Approved | SSE notification with status="working" |
| TG-SSE: execution started | InputRequired → Executing | SSE notification with status="working" |
| TG-SSE: completed | Any → Completed | SSE notification with status="completed" |
| TG-SSE: cancelled | Any → Cancelled | SSE notification with status="cancelled" |
| TG-SSE: expired | Any → Expired | SSE notification with status="failed", message="Task expired" |
| TG-SSE: rejected | InputRequired → Rejected | SSE notification with status="failed", message="Request rejected" |
| TG-SSE: no subscribers | State transition | No error, notification sent to empty registry |

### 10.3 Resource Tests

| Scenario | Measurement | Expected |
|----------|-------------|----------|
| 100 concurrent Executing tasks | Memory usage | < 1MB total |
| Streaming 1GB result | Peak memory | < 1MB (chunk buffer only) |
| No client polling | CPU usage | Zero per task (without SSE) |
| 100 SSE client connections | Memory usage | < 5MB total |
| 100 SSE upstream subscriptions | File descriptors | 100 connections, properly cleaned up |

### 10.4 Chaos Tests

| Scenario | Injection | Expected Behavior |
|----------|-----------|-------------------|
| Upstream crash after task created | Kill upstream | Next poll returns error |
| Network partition during stream | Block traffic | Stream fails, client retries |
| ThoughtGate restart | Restart TG | Executing tasks lost, clients get 404 |
| SSE upstream disconnect | Drop SSE connection | Reconnect up to max attempts |
| SSE client disconnect | Close client connection | Registry cleaned up |
| Mass SSE reconnect | Disconnect all upstream SSE | Staggered reconnection, no thundering herd |

### 10.5 SSE Edge Case Tests

| Test Case | Setup | Expected Behavior |
|-----------|-------|-------------------|
| SSE advertised, client subscribes | Upstream has SSE | Subscription succeeds, notifications flow |
| SSE not advertised, client subscribes | Upstream no SSE | `SseError::NotSupported` returned |
| Passthrough task SSE | Client subscribes to non-tg_ task | `SseError::PassthroughTask` returned |
| Upstream gains SSE on reconnect | Initially no SSE, reconnect with SSE | TG starts advertising SSE |
| Upstream loses SSE on reconnect | Initially has SSE, reconnect without | TG stops advertising SSE |
| Client subscribes after SSE lost | SSE was available, now isn't | Error on new subscriptions |
| Existing SSE clients when SSE lost | Active SSE, upstream loses capability | Existing streams close gracefully |
| Task completes via SSE | Upstream sends completed notification | Client notified, can fetch result |
| Task fails via SSE | Upstream sends failed notification | Client notified with error |
| Multiple clients same task | 3 clients subscribe to same task | All receive same notifications |
| Client subscribes mid-execution | Task already executing | Client receives subsequent notifications |
| Upstream SSE reconnect success | Disconnect, then reconnect | Notifications resume |
| Upstream SSE reconnect exhausted | Disconnect, max retries reached | Log warning, polling-only mode |
| **Lazy SSE: polling only** | Client only calls tasks/get, never subscribes SSE | NO upstream SSE connection created |
| **Lazy SSE: first subscriber** | First client subscribes to task SSE | Upstream SSE connection created |
| **Lazy SSE: last unsubscribe** | Last client disconnects from task SSE | Upstream SSE connection closed |
| **Lazy SSE: multiple tasks** | 10 tasks, only 2 have SSE subscribers | Only 2 upstream SSE connections |

## 11. Open Questions

| Question | Status | Resolution |
|----------|--------|------------|
| Retry policy for transient upstream errors on lazy poll | Resolved | Client retries; TG doesn't retry (documented) |
| Multiple tasks/result calls | Resolved | Single-consumer pattern; second caller may get error |
| SSE support for TG-owned tasks | Resolved | Supported via bidirectional forwarding, **conditional on upstream support** |
| SSE conditional advertisement | Resolved | TG only advertises SSE if upstream supports (see REQ-CORE-007 F-001) |
| Upstream purges before client streams | Acknowledged | Document deployment constraint; client gets error |
| SSE reconnection backoff strategy | Open | Suggest fixed delay with jitter |
| SSE keepalive interval | Open | Default 15s, configurable |
| ~~TTL expiry during execution~~ | Resolved | No grace period; client sets adequate TTL (documented in limitations) |
| ~~Concurrent tasks/result~~ | Resolved | Single-consumer pattern (documented in limitations) |
| ~~Cancel during streaming~~ | Resolved | Complete in-flight stream; F-007.7 added |

## 12. Limitations (v0.2)

| Limitation | Impact | Mitigation |
|------------|--------|------------|
| No result storage | All results streamed directly to client | Execute on-demand, no wasted work |
| Result can only be streamed once (if upstream purges after) | Rare edge case | Most clients call tasks/result once |
| No retry on lazy poll errors | Client sees error | Client can retry the poll |
| Deferred execution: forbidden tools execute on tasks/result | Slight latency on tasks/result call | Usually subsecond for sync tools |
| SSE only when upstream supports | Without upstream SSE, TG doesn't advertise SSE | Polling is always available |
| SSE forwarder spawned per task with subscribers | Only tasks with SSE clients have forwarder | Lazy: no overhead if clients only poll |
| SSE capability changes require re-initialize | If upstream gains/loses SSE | Rare; clients re-check on connect |
| No output inspection | Results streamed without inspection | v0.2 scope; see note below |
| **TTL expiry during execution** | Work may be lost if TTL expires mid-execution | Client must set TTL > approval_time + execution_time |
| **Single `tasks/result` consumer** | Second concurrent caller may get error | Clients coordinate; first caller wins |
| **No grace period on TTL** | Strict enforcement even if execution almost done | Simplicity; client controls via adequate TTL |

### TTL Guidance

Clients SHOULD set TTL considering:
```
TTL > expected_approval_time + expected_execution_time + buffer
```

If TTL expires during execution:
1. TG cancels upstream task (best effort)
2. Client receives Expired status
3. **Work may be lost** - upstream may have partially executed

TG does NOT provide grace periods. This is by design:
- Simplifies state machine
- Client has full control via TTL value
- Avoids ambiguity about "almost done"

### Concurrent `tasks/result` Behavior

If two clients call `tasks/result` simultaneously for the same task:

| Path | Behavior |
|------|----------|
| DeferredSync | First triggers execution, second waits or errors (implementation-dependent) |
| Task (upstream) | First streams, second may get "already retrieved" from upstream |

This is a **single-consumer pattern** consistent with MCP semantics. Clients should coordinate if multiple consumers need the result.

### Cancellation During Streaming

If `tasks/cancel` is called while `tasks/result` is actively streaming:

```
1. Client A: tasks/result → streaming begins
2. Client B: tasks/cancel
3. TG: Marks task as "cancel pending"
4. Streaming completes (or errors)
5. TG: Transitions to Cancelled
```

**In-flight streams complete.** Cancellation means "don't start new work," not "abort delivery." This prevents partial results and simplifies error handling.

> **Future-Proofing Note:** v0.2 streams all results directly from upstream to client 
> without inspection. Future implementations of Output Inspection (Inspection stages for 
> responses) will require selectively disabling streaming for tools that need PII 
> redaction or output validation. The streaming architecture supports this via 
> per-tool configuration.

## 13. References

| Resource | Link |
|----------|------|
| SEP-1686 Task States | https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1686 |
| REQ-CORE-007 Protocol Compliance | ./REQ-CORE-007_SEP1686_Protocol_Compliance.md |
| REQ-GOV-001 Task Lifecycle | ./REQ-GOV-001_Task_Lifecycle.md |
| REQ-GOV-002 Execution Pipeline | ./REQ-GOV-002_Approval_Execution_Pipeline.md |

## 14. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 0.1 | 2026-01-12 | - | Initial draft with active polling |
| 0.2 | 2026-01-12 | - | Switched to lazy polling model for sidecar efficiency |
| 0.3 | 2026-01-12 | - | Added streaming results (no caching) for memory bounds |
| 0.4 | 2026-01-12 | - | Added bidirectional SSE notification forwarding |
| 0.5 | 2026-01-12 | - | SSE is conditional on upstream support (don't advertise what we can't deliver) |
| 0.6 | 2026-01-12 | - | Added F-005.7 race condition prevention; future-proofing note for output inspection (Gemini feedback) |
| 0.7 | 2026-01-12 | - | (Superseded) Uniform task API with result storage |
| 0.8 | 2026-01-12 | - | **Deferred execution for forbidden annotation:** No result storage, execute on tasks/result call. Adds Approved state. |
| 0.9 | 2026-01-12 | - | **F-012:** TG-generated SSE for internal state transitions. Added REQ-GOV-002 handoff documentation. |
| 0.10 | 2026-01-12 | - | **Documented limitations:** TTL expiry behavior, single-consumer pattern, cancel during streaming (F-007.7). |
| 0.11 | 2026-01-12 | - | **Lazy SSE (Gemini feedback):** F-010 updated - upstream SSE only connects when client subscribes, not on task creation. |
| 0.12 | 2026-01-12 | - | **Timeouts (Grok feedback):** F-005.12 lazy poll proxy timeout, F-009.8 SSE client idle timeout. |
