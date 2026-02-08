# REQ-GOV-001: Task Lifecycle & SEP-1686 Compliance

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-GOV-001` |
| **Title** | Task Lifecycle & SEP-1686 Compliance |
| **Type** | Governance Component |
| **Status** | Draft |
| **Priority** | **Critical** |
| **Tags** | `#governance` `#tasks` `#sep-1686` `#state-machine` `#async` |

## 1. Context & Decision Rationale

This requirement defines **task lifecycle management** for ThoughtGate's approval workflows.

### 1.1 Version Scope Overview

| Version | Mode | Task Exposure | Description |
|---------|------|---------------|-------------|
| **v0.2** | **SEP-1686** | Full API | Async tasks with `tasks/*` methods |
| **v0.2** | **Blocking** | None | Hold connection, return result directly (no `params.task`) |

### 1.2 v0.2: Blocking Mode

Blocking mode holds the HTTP connection (or stdio pipe) open during approval,
returning the tool result directly. This supports MCP clients that do NOT
implement SEP-1686 task primitives.

**Mode Detection:**

| Client sends | Tool action | Result |
|---|---|---|
| `params.task` present | Approve/Policy | Async SEP-1686 mode |
| `params.task` absent | Approve/Policy | Blocking mode |
| `params.task` absent | Forward/Deny | Normal sync (no approval) |

**Blocking mode requires:**
- An approval engine configured (YAML `approval:` section)
- `taskSupport: "optional"` annotation (not `"required"`)

**Timeout behavior:**
- Workflow `blocking_timeout` → workflow `timeout` → env var → 300s default
- On timeout: return `CallToolResult` with `isError: true` (tool-level, not JSON-RPC -32008)
- Progress heartbeats emitted every 15s (`notifications/progress`) to reset SDK timeout

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                         v0.2 BLOCKING MODE                                  │
│                                                                             │
│   Agent                    ThoughtGate                    Human (Slack)     │
│     │                           │                              │            │
│     │  tools/call               │                              │            │
│     │ ─────────────────────────►│                              │            │
│     │                           │                              │            │
│     │         ┌─────────────────┴─────────────────┐            │            │
│     │         │ HTTP connection held open         │            │            │
│     │         │ Internal tracking for correlation │            │            │
│     │         └─────────────────┬─────────────────┘            │            │
│     │                           │                              │            │
│     │                           │   Post approval request      │            │
│     │                           │ ────────────────────────────►│            │
│     │                           │                              │            │
│     │     (connection blocked)  │      (human reviews)         │            │
│     │              ...          │           ...                │            │
│     │                           │                              │            │
│     │                           │   Reaction (👍/👎)           │            │
│     │                           │◄─────────────────────────────│            │
│     │                           │                              │            │
│     │         ┌─────────────────┴─────────────────┐            │            │
│     │         │ On approve: forward to upstream   │            │            │
│     │         │ On reject: return error           │            │            │
│     │         │ On timeout: return error          │            │            │
│     │         └─────────────────┬─────────────────┘            │            │
│     │                           │                              │            │
│     │  {"result": ...}          │                              │            │
│     │◄──────────────────────────│                              │            │
│     │  (or error response)      │                              │            │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘

Key characteristics:
• Agent sees normal tools/call → response (just slow)
• No task ID exposed to agent
• No tasks/* methods available
• Works with ANY MCP client (no SEP-1686 support required)
```

### 1.3 v0.2: SEP-1686 Async Mode

SEP-1686 introduces the "task primitive" to MCP, enabling:
- Deferred result retrieval via polling
- Long-running operations that outlive request/response cycles
- Status tracking for async workflows

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                         v0.2 SEP-1686 MODE                                  │
│                                                                             │
│   Agent                    ThoughtGate                    Human (Slack)     │
│     │                           │                              │            │
│     │  tools/call               │                              │            │
│     │  (with task field)        │                              │            │
│     │ ─────────────────────────►│                              │            │
│     │                           │                              │            │
│     │  {"taskId": "abc-123",    │   Post approval request      │            │
│     │   "status": "working"}    │ ────────────────────────────►│            │
│     │◄──────────────────────────│                              │            │
│     │                           │                              │            │
│     │  (agent free to do        │      (human reviews)         │            │
│     │   other work)             │           ...                │            │
│     │                           │                              │            │
│     │  tasks/get                │                              │            │
│     │ ─────────────────────────►│                              │            │
│     │  {"status": "working"}    │                              │            │
│     │◄──────────────────────────│                              │            │
│     │                           │   Reaction (👍/👎)           │            │
│     │           ...             │◄─────────────────────────────│            │
│     │                           │                              │            │
│     │  tasks/result             │                              │            │
│     │ ─────────────────────────►│                              │            │
│     │  {"result": ...}          │                              │            │
│     │◄──────────────────────────│                              │            │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘

Key characteristics:
• Agent receives task ID immediately
• Agent polls for status
• Agent retrieves result when ready
• Requires SEP-1686-aware client
```

## 2. Dependencies

| Requirement | Relationship | v0.2 | v0.3+ |
|-------------|--------------|------|-------|
| REQ-CFG-001 | **Receives from** | Timeout configuration | Task configuration |
| REQ-CORE-003 | **Receives from** | — | MCP routing for `tasks/*` methods |
| REQ-CORE-004 | **Provides to** | Error formatting | Error formatting |
| REQ-CORE-005 | **Coordinates with** | Shutdown handling | Shutdown handling |
| REQ-POL-001 | **Receives from** | Approval decisions | Approval decisions |
| REQ-GOV-002 | **Provides to** | — | Task state for pipeline |
| REQ-GOV-003 | **Coordinates with** | Approval decisions | Approval decisions |

## 3. Intent

### 3.1 v0.2 Intent (SEP-1686 Mode)

The system must additionally:
1. Implement SEP-1686 task state machine
2. Store tasks with request data for later execution
3. Handle `tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel` methods
4. Manage task TTL and expiration
5. Support concurrent access with proper synchronization
6. Rate limit task creation to prevent abuse
7. Advertise task capability during initialize
8. Rewrite tool annotations during tools/list

## 4. Scope

### 4.1 v0.2 Scope (SEP-1686)

| Component | Status | Notes |
|-----------|--------|-------|
| Task data structure | ✅ In Scope | Full SEP-1686 |
| Task state machine | ✅ In Scope | Working → Completed/Failed |
| In-memory task storage | ✅ In Scope | With TTL cleanup |
| TTL cleanup background task | ✅ In Scope | Periodic expired task removal |
| `tasks/get` | ✅ In Scope | Status retrieval |
| `tasks/result` | ✅ In Scope | Result retrieval |
| `tasks/list` | ✅ In Scope | MCP-compliant cursor pagination |
| `tasks/cancel` | ✅ In Scope | Cancellation |
| Capability advertisement | ✅ In Scope | During initialize (REQ-CORE-007) |
| Tool annotation rewriting | ✅ In Scope | During tools/list (REQ-CORE-007) |
| Task metadata validation | ✅ In Scope | On tools/call (REQ-CORE-007) |
| Metrics and logging | ✅ In Scope | Observability |
| Rate limiting | ✅ In Scope | Via `governor` crate (lock-free) |
| Admission control | ✅ In Scope | `max_pending_global` with atomic `compare_exchange` |
| SSE notifications | ❌ Deferred | v0.3+ (polling works for v0.2) |
| Blocking mode | ✅ In Scope | Dual-mode: async SEP-1686 when `params.task` present; blocking when absent |
| Client disconnection detection | ❌ Removed | Less critical with async polling |

## 5. Constraints

### 5.1 v0.2 Configuration

| Setting | Default | Source | Description |
|---------|---------|--------|-------------|
| Approval timeout | From workflow | `approval.<name>.timeout` | Max wait time |
| On timeout action | `deny` | `approval.<name>.on_timeout` | Action when timeout |

**Note:** v0.2 uses workflow-level timeout from YAML configuration (REQ-CFG-001), not global task TTL.

### 5.2 Task Store Configuration

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| Default TTL | 600s (10 min) | `THOUGHTGATE_TASK_DEFAULT_TTL_SECS` |
| Maximum TTL | 86400s (24 hr) | `THOUGHTGATE_TASK_MAX_TTL_SECS` |
| Cleanup interval | 60s | `THOUGHTGATE_TASK_CLEANUP_INTERVAL_SECS` |
| Max pending per principal | 10 | `THOUGHTGATE_TASK_MAX_PENDING_PER_PRINCIPAL` |
| Max pending global | 1000 | `THOUGHTGATE_TASK_MAX_PENDING_GLOBAL` |
| Max pending bytes | 256 MB | `THOUGHTGATE_MAX_PENDING_BYTES` |

### 5.3 SEP-1686 Task States

| State | Meaning | Terminal? |
|-------|---------|-----------|
| `working` | Request is being processed | No |
| `input_required` | Awaiting external input (approval) | No |
| `completed` | Success, result available | Yes |
| `failed` | Error occurred | Yes |
| `cancelled` | Cancelled by client | Yes |

**Additional ThoughtGate States:**

| State | Meaning | Terminal? |
|-------|---------|-----------|
| `rejected` | Approver rejected request | Yes |
| `expired` | TTL exceeded | Yes |

## 6. Interfaces

### 6.1 v0.2: Task Structure

```rust
/// Task structure for SEP-1686 task lifecycle (v0.2)
/// Used for task management, approval tracking, and observability
pub struct Task {
    /// Unique identifier (tg_nanoid format, e.g., `tg_a1b2c3d4e5f6g7h8i9j0k`)
    pub id: Sep1686TaskId,
    
    /// Original request (for logging/metrics and deferred execution)
    pub tool_name: String,
    pub arguments_hash: String,
    pub original_request: McpRequest,
    
    /// Principal making the request
    pub principal: Principal,
    
    /// Timing
    pub created_at: Instant,
    pub timeout: Duration,
    
    /// Current state (SEP-1686 compatible)
    pub status: TaskStatus,

    /// Approval record (populated when decision is made)
    pub approval: Option<ApprovalRecord>,
}

/// SEP-1686 compatible task states
pub enum TaskStatus {
    /// Request is being processed (initial state)
    Working,
    /// Awaiting external input (approval decision)
    InputRequired,
    /// Executing the approved tool call
    Executing,
    /// Completed successfully
    Completed,
    /// Error occurred
    Failed,
    /// Cancelled by client
    Cancelled,
    /// Approver rejected request
    Rejected,
    /// TTL exceeded
    Expired,
}

/// Record of an approval decision
pub struct ApprovalRecord {
    /// The approval decision
    pub decision: ApprovalDecision,
    /// Who made the decision
    pub decided_by: String,
    /// When the decision was made
    pub decided_at: DateTime<Utc>,
}

/// Unified outcome enum for approval decisions
pub enum ApprovalOutcome {
    Approved { by: String },
    Rejected { by: String, reason: Option<String> },
    Timeout,
    Shutdown,
}
```

### 6.2 v0.2: Task Store Interface

```rust
/// Thread-safe in-memory store for tasks with concurrent access support.
///
/// Uses DashMap for lock-free concurrent access. Each task entry has a
/// per-entry `tokio::sync::Notify` for efficient wakeup of waiters
/// (e.g., tasks/result blocking until terminal state).
pub struct TaskStore {
    /// Task storage keyed by TaskId
    tasks: DashMap<TaskId, TaskEntry>,
    /// Index of task IDs by principal for rate limiting and listing
    by_principal: DashMap<String, Vec<TaskId>>,
    /// Configuration
    config: TaskStoreConfig,
    /// Counter for pending (non-terminal) tasks
    pending_count: AtomicUsize,
}

/// Internal task entry with metadata for cleanup.
struct TaskEntry {
    /// The task itself (Arc for cheap reads, make_mut for writes)
    task: Arc<Task>,
    /// When the task became terminal (for grace period cleanup)
    terminal_at: Option<DateTime<Utc>>,
    /// Notifier for waiters on this task (per-entry, not broadcast)
    notify: Arc<Notify>,
    /// Estimated size of this task's arguments in bytes
    estimated_bytes: usize,
}

impl TaskStore {
    /// Create a new task and store it.
    /// Enforces max_pending_per_principal and max_pending_global limits.
    pub fn create(&self, task: Task) -> Result<TaskId, TaskError>;

    /// Get a task by ID (returns Arc<Task> for cheap cloning).
    pub fn get(&self, id: &TaskId) -> Option<Arc<Task>>;

    /// Transition a task to a new status (validates allowed transitions).
    /// Notifies waiters via the per-entry Notify.
    pub fn transition(&self, id: &TaskId, to: TaskStatus) -> Result<Arc<Task>, TaskError>;

    /// Wait for a task to reach a terminal state.
    /// Uses per-entry tokio::sync::Notify for efficient wakeup.
    pub async fn wait_for_terminal(&self, id: &TaskId, timeout: Duration) -> Result<Arc<Task>, TaskError>;

    /// List tasks for a principal with offset-based pagination.
    pub fn list_for_principal(&self, principal: &str, offset: usize, limit: usize) -> Vec<Arc<Task>>;

    /// Remove expired and terminal-grace-period tasks.
    pub fn cleanup_expired(&self) -> usize;
}
```

> **Note:** The `ApprovalOutcome` enum (defined in §6.1) is used consistently throughout
> the approval workflow. `ApprovalDecision` and `ApprovalRecord` track the decision details.

### 6.3 v0.2: Task Store Configuration

```rust
/// Configuration for the task store.
///
/// Implements: REQ-GOV-001/§5.2
pub struct TaskStoreConfig {
    /// Default TTL for new tasks
    pub default_ttl: Duration,            // 600s (10 min)
    /// Maximum TTL allowed
    pub max_ttl: Duration,                // 86400s (24 hr)
    /// Minimum TTL allowed
    pub min_ttl: Duration,                // 60s (1 min)
    /// How often to run cleanup
    pub cleanup_interval: Duration,       // 60s
    /// Maximum pending tasks per principal
    pub max_pending_per_principal: usize,  // 10
    /// Maximum pending tasks globally
    pub max_pending_global: usize,         // 1000
    /// Grace period after terminal before removal
    pub terminal_grace_period: Duration,   // 3600s (1 hr)
    /// Maximum total bytes for pending task arguments (H-004)
    pub max_pending_bytes: usize,          // 256 MB
}
```

### 6.4 Full Task Structure Reference

```rust
/// Full task structure for SEP-1686 mode.
/// Note: This is the same Task struct used in v0.2 (§6.1).
/// TaskId is an alias for Sep1686TaskId (tg_nanoid format).
pub struct Task {
    // Identity
    pub id: TaskId,                          // Sep1686TaskId (tg_nanoid)

    // Request Data
    pub original_request: serde_json::Value,
    pub tool_name: String,
    pub arguments_hash: String,

    // Principal
    pub principal: Principal,

    // Timing
    pub created_at: DateTime<Utc>,
    pub ttl: Duration,
    pub expires_at: DateTime<Utc>,
    pub poll_interval: Duration,

    // State
    pub status: TaskStatus,
    pub status_message: Option<String>,
    pub transitions: Vec<TaskTransition>,

    // Approval
    pub approval: Option<ApprovalRecord>,

    // Result
    pub result: Option<serde_json::Value>,
    pub failure: Option<FailureInfo>,

    // Timeout behavior
    pub on_timeout: TimeoutAction,
}

// TaskId = Sep1686TaskId (re-exported alias)
pub use crate::protocol::Sep1686TaskId as TaskId;
```

## 7. Behavior Specification

### 7.1 v0.2: SEP-1686 Approval Flow

```text
┌─────────────────────────────────────────────────────────────────┐
│                  v0.2 SEP-1686 ASYNC FLOW                    │
└─────────────────────────────────────────────────────────────────┘

  1. Request arrives (tools/call with action: approve)
     │
     ▼
  2. Create Task with SEP-1686 state machine
     │
     ├─ Generate Sep1686TaskId (tg_nanoid)
     ├─ Hash arguments for logging
     ├─ Set initial state: Working
     └─ Store in TaskStore
     │
     ▼
  3. Return TaskId immediately to client
     │
     ▼
  4. Background: Post approval request to Slack (REQ-GOV-003)
     │
     ▼
  5. Background: Poll for approval decision
     │
     ├─────────────────────────────────────────────┐
     │                                             │
     │  Poll for:                                  │
     │  • Approval decision from Slack             │
     │  • Timeout expiration (TTL)                 │
     │                                             │
     └─────────────────────────────────────────────┘
     │
     ├─── Approved ──► State → Executing ──► Forward to upstream
     │                                        │
     │                                        ▼
     │                                   State → Completed
     │
     ├─── Rejected ──► State → Failed (rejected)
     │
     └─── Timeout ───► Execute on_timeout action
                       │
                       ├─ deny: State → Failed (timeout)
                       └─ future: escalate, auto-approve

  6. Client polls tasks/result to retrieve outcome

  7. Cleanup: TTL-based expiry from TaskStore
```

### F-001: Pending Approval Registration (v0.2)

- **F-001.1:** Generate `Sep1686TaskId` (tg_nanoid format) for each task
- **F-001.2:** Store tool name, arguments hash, and original request
- **F-001.3:** Record principal for observability
- **F-001.4:** Initialize state as `Working`
- **F-001.5:** Register in `TaskStore`

### F-002: Background Approval Polling (v0.2)

- **F-002.1:** Spawn background task via `tokio::spawn` (non-blocking)
- **F-002.2:** Poll Slack adapter for approval decision
- **F-002.3:** Check for timeout expiration
- **F-002.4:** Update task state on decision via `TaskStore::transition()`
- **F-002.5:** Notify waiters via per-entry `tokio::sync::Notify`

```rust
/// Background polling task (spawned, non-blocking)
async fn poll_for_approval(
    store: Arc<TaskStore>,
    id: TaskId,
    adapter: Arc<dyn ApprovalAdapter>,
    reference: ApprovalReference,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    let mut poll_interval = Duration::from_secs(5);

    loop {
        if Instant::now() >= deadline {
            let _ = store.transition(&id, TaskStatus::Expired);
            return;
        }

        tokio::time::sleep(poll_interval).await;

        match adapter.poll_for_decision(&reference).await {
            Ok(Some(ApprovalOutcome::Approved { by })) => {
                let _ = store.transition(&id, TaskStatus::Executing);
                return;
            }
            Ok(Some(ApprovalOutcome::Rejected { by, reason })) => {
                let _ = store.transition(&id, TaskStatus::Rejected);
                return;
            }
            Ok(None) => {
                // No decision yet, continue polling
            }
            Err(e) => {
                tracing::warn!(id = %id, error = %e, "Polling error");
            }
        }

        // Exponential backoff
        poll_interval = (poll_interval * 2).min(Duration::from_secs(30));
    }
}
```

### F-003: Task State Notification (v0.2)

- **F-003.1:** Use per-entry `tokio::sync::Notify` for efficient wakeup of waiters
- **F-003.2:** Notify is scoped to a single task (no filtering needed)
- **F-003.3:** Support multiple waiters per task via `Notify::notify_waiters()`
- **F-003.4:** Entry cleanup handled by TTL-based expiry and grace period

### F-004: Timeout Handling (v0.2)

- **F-004.1:** Use workflow timeout from YAML configuration
- **F-004.2:** Execute `on_timeout` action when timeout expires
- **F-004.3:** `on_timeout: deny` returns -32008 error
- **F-004.4:** Log timeout with correlation ID and duration

### F-005: Approval Decision Recording (v0.2)

- **F-005.1:** Receive decision from REQ-GOV-003 (Slack polling)
- **F-005.2:** Signal completion via per-entry `tokio::sync::Notify`
- **F-005.3:** If approval not found (expired/cleaned up), log and ignore
- **F-005.4:** Record decision metadata for audit logging

### 7.2 v0.3+: SEP-1686 Task Flow (Future Reference)

```text
┌─────────────────────────────────────────────────────────────────┐
│                  v0.3+ SEP-1686 TASK FLOW                       │
└─────────────────────────────────────────────────────────────────┘

  1. Request arrives (tools/call with task field)
     │
     ▼
  2. Create Task in Working state
     │
     ▼
  3. Return task-augmented response immediately
     {"taskId": "abc-123", "status": "working"}
     │
     ▼
  4. Transition to InputRequired
     │
     ▼
  5. Post approval request to Slack
     │
     ▼
  (Agent polls via tasks/get)
     │
     ▼
  6. Receive approval decision
     │
     ├─── Approved ──► Transition to Working (Executing)
     │                        │
     │                        ▼
     │                 Forward to upstream
     │                        │
     │                        ▼
     │                 Transition to Completed
     │
     ├─── Rejected ──► Transition to Rejected
     │
     └─── Timeout ───► Transition to Expired

  7. Agent retrieves result via tasks/result
```

### F-006 to F-011: SEP-1686 Task API (v0.2)

These features are implemented in v0.2:

- **F-006:** Task creation with state machine
- **F-007:** Dynamic poll interval computation
- **F-008:** `tasks/get` implementation (status retrieval)
- **F-009:** `tasks/result` implementation (result streaming)
- **F-010:** `tasks/list` implementation (cursor-based pagination, PAGE_SIZE=20)
- **F-011:** `tasks/cancel` implementation

- **F-012:** Rate limiting and capacity management (`max_pending_per_principal`, `max_pending_global`)

See §10 for state machine reference.

## 8. Non-Functional Requirements

### NFR-001: Observability (v0.2)

**Metrics:**
```text
thoughtgate_tasks_pending{principal}
thoughtgate_tasks_total{outcome="approved|rejected|timeout|expired"}
thoughtgate_task_duration_seconds{outcome}
thoughtgate_task_data_bytes{quantile}  # Track task payload sizes
```

**Logging:**
```json
{"level":"info","event":"task_created","task_id":"abc-123","tool":"delete_user","principal":"app-xyz"}
{"level":"info","event":"task_completed","task_id":"abc-123","outcome":"approved","duration_ms":45000}
{"level":"warn","event":"task_timeout","task_id":"abc-123","tool":"delete_user","timeout_secs":600}
{"level":"warn","event":"task_expired","task_id":"abc-123","tool":"delete_user","ttl_secs":3600}
```

### NFR-002: Performance (v0.2)

| Metric | Target |
|--------|--------|
| Approval registration | < 1ms |
| Decision recording | < 1ms |
| Memory per pending approval | < 500 bytes |
| Max concurrent pending | 1,000 |

### NFR-003: Reliability (v0.2)

- No orphaned executions (tool runs after task expires or is cancelled)
- Proper cleanup on all exit paths (TTL-based expiry)
- Graceful handling of Slack API failures

### NFR-004: Memory Pressure Handling (v0.2)

| Metric | Threshold | Behavior |
|--------|-----------|----------|
| Pending tasks | > 1000 | Reject new tasks with -32013 |
| Task data size | > 1MB per task | Reject task with -32602 |
| Total memory | > 80% RSS limit | Log warning, continue |
| Total memory | > 95% RSS limit | Reject new tasks with -32013 |

**Rationale:** Memory pressure can cascade to OOM kills, affecting all in-flight requests. Early rejection with clear errors is preferable to silent failures.

## 9. Verification Plan

### 9.1 v0.2 Edge Case Matrix

| Scenario | Expected Behavior | Test ID |
|----------|-------------------|---------|
| Approval approved | Task state → Approved, execute on tasks/result | EC-TASK-001 |
| Approval rejected | Task state → Failed(-32007) | EC-TASK-002 |
| Approval timeout (on_timeout: deny) | Task state → Failed(-32008) | EC-TASK-003 |
| Slack API error during polling | Retry with backoff or fail task | EC-TASK-004 |
| Upstream error after approval | Task state → Failed with upstream error | EC-TASK-005 |
| Multiple pending for same principal | All tasks tracked independently | EC-TASK-006 |
| Shutdown with pending tasks | Cancel tasks, state → Failed(-32603) | EC-TASK-007 |
| Task TTL expiry | Task state → Expired | EC-TASK-008 |
| tasks/cancel on already-cancelled task | Return success (idempotent) | EC-TASK-009 |
| tasks/cancel on Completed task | Return -32602 (Invalid params per MCP spec) | EC-TASK-010 |
| tasks/result on InputRequired task | Block until terminal or return status | EC-TASK-011 |
| Concurrent tasks/result calls | First gets result, others get error | EC-TASK-012 |
| TTL cleanup runs during task access | Atomic check, return expired if race | EC-TASK-013 |
| TaskStore memory limit reached | Reject new tasks with -32013 | EC-TASK-014 |
| Very rapid task creation | Apply rate limiting if enabled | EC-TASK-015 |
| Task created with past TTL | Immediately expire | EC-TASK-016 |

### 9.2 v0.2 Assertions

**Unit Tests:**
- `test_task_creation` — Task created with correct initial state
- `test_task_state_approved` — Transitions to Approved on approval
- `test_task_state_rejected` — Transitions to Failed on rejection
- `test_task_state_timeout` — Transitions to Failed on timeout
- `test_background_poller_spawn` — Poller spawned correctly
- `test_tasks_get_status` — Returns correct task status

**Integration Tests:**
- `test_full_async_flow_approved` — Complete approved flow with polling
- `test_full_async_flow_rejected` — Complete rejected flow
- `test_full_async_flow_timeout` — Complete timeout flow

## 10. SEP-1686 State Machine Reference

This section documents the SEP-1686 state machine. **Implemented in v0.2.**

### 10.1 Task State Machine

```text
┌─────────────────────────────────────────────────────────────────┐
│                    SEP-1686 STATE MACHINE                       │
│                                                                 │
│                         ┌─────────┐                             │
│                         │ Working │                             │
│                         └────┬────┘                             │
│                              │                                  │
│              ┌───────────────┼───────────────┐                  │
│              │               │               │                  │
│              ▼               ▼               ▼                  │
│     ┌─────────────┐   ┌───────────┐   ┌──────────┐             │
│     │InputRequired│   │ Completed │   │  Failed  │             │
│     └──────┬──────┘   └───────────┘   └──────────┘             │
│            │                                                    │
│    ┌───────┼───────┬───────────┐                               │
│    │       │       │           │                               │
│    ▼       ▼       ▼           ▼                               │
│ ┌──────┐ ┌────┐ ┌────────┐ ┌─────────┐                        │
│ │Cancel│ │Work│ │Rejected│ │ Expired │                        │
│ │-led  │ │-ing│ │        │ │         │                        │
│ └──────┘ └──┬─┘ └────────┘ └─────────┘                        │
│             │                                                  │
│     ┌───────┴───────┐                                          │
│     │               │                                          │
│     ▼               ▼                                          │
│ ┌─────────┐   ┌──────────┐                                     │
│ │Completed│   │  Failed  │                                     │
│ └─────────┘   └──────────┘                                     │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### 10.2 Task Store Interface

```rust
/// Concrete in-memory task store (see §6.2 for full interface).
/// TaskId = Sep1686TaskId (tg_nanoid format).
impl TaskStore {
    fn create(&self, task: Task) -> Result<TaskId, TaskError>;
    fn get(&self, id: &TaskId) -> Option<Arc<Task>>;
    fn transition(&self, id: &TaskId, to: TaskStatus) -> Result<Arc<Task>, TaskError>;
    async fn wait_for_terminal(&self, id: &TaskId, timeout: Duration) -> Result<Arc<Task>, TaskError>;
    fn list_for_principal(&self, principal: &str, offset: usize, limit: usize) -> Vec<Arc<Task>>;
    fn cleanup_expired(&self) -> usize;
}
```

### 10.3 SEP-1686 Method Handlers

```rust
// tasks/get — returns task status and metadata
fn handle_tasks_get(&self, params: TasksGetRequest) -> JsonRpcResponse {
    let task = self.store.get(&params.task_id)?;
    // Map internal TaskStatus to Sep1686Status
    // Return Sep1686TaskMetadata with poll_interval
}

// tasks/result — blocks until terminal state, returns result
async fn handle_tasks_result(&self, params: TasksResultRequest) -> JsonRpcResponse {
    // If already terminal, return result immediately
    // Otherwise wait_for_terminal with timeout
    // On approval: forward to upstream, return CallToolResult
}

// tasks/list — cursor-based pagination (PAGE_SIZE=20)
fn handle_tasks_list(&self, params: TasksListRequest) -> JsonRpcResponse {
    // Return paginated list of tasks for principal
    // Server-controlled page size, cursor-only client pagination
}

// tasks/cancel — cancel if in cancellable state
fn handle_tasks_cancel(&self, params: TasksCancelRequest) -> JsonRpcResponse {
    // Cancel if in Working or InputRequired state
    // Return error if already terminal or executing
}
```

## 11. Definition of Done

### 11.1 v0.2 Definition of Done

- [ ] `Task` struct with SEP-1686 states (`Working`, `InputRequired`, `Executing`, `Completed`, `Failed`, `Cancelled`, `Rejected`, `Expired`)
- [ ] `TaskStore` with in-memory storage, per-entry `Notify`, and TTL cleanup
- [ ] State machine with valid transitions only
- [ ] `tasks/get` — return task by ID
- [ ] `tasks/result` — return result or block until terminal
- [ ] `tasks/cancel` — cancel if in `InputRequired` state
- [ ] Capability advertisement during `initialize` (advertises Task API support)
- [ ] Tool annotation rewriting during `tools/list`
- [ ] Background poller for approval integration (via REQ-GOV-003)
- [ ] Timeout handling with `on_timeout` action
- [ ] Approval decision recording from REQ-GOV-003
- [ ] Metrics for task count, states, and outcomes
- [ ] All edge cases (EC-TASK-001 to EC-TASK-008) covered
- [ ] Integration with REQ-GOV-002 (pipeline) and REQ-GOV-003 (Slack)

### 11.2 v0.3+ Definition of Done (Future)

- [x] `tasks/list` with cursor-based pagination (PAGE_SIZE=20) — implemented in v0.2
- [x] Rate limiting enforced (`max_pending_per_principal`, `max_pending_global`) — implemented in v0.2
- [ ] SSE notifications for task state changes
- [ ] Upstream task orchestration (REQ-GOV-004)
