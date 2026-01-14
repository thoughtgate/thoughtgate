# REQ-GOV-001: Task Lifecycle & SEP-1686 Compliance

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-GOV-001` |
| **Title** | Task Lifecycle & SEP-1686 Compliance |
| **Type** | Governance Component |
| **Status** | Draft |
| **Priority** | **Critical** |
| **Tags** | `#governance` `#tasks` `#sep-1686` `#state-machine` `#async` `#blocking` |

## 1. Context & Decision Rationale

This requirement defines **task lifecycle management** for ThoughtGate's approval workflows.

### 1.1 Version Scope Overview

| Version | Mode | Task Exposure | Description |
|---------|------|---------------|-------------|
| **v0.2** | **SEP-1686** | Full API | Async tasks with `tasks/*` methods |

> **Note:** Blocking mode (holding HTTP connection during approval) was considered for v0.2 but removed due to fundamental timeout issues. SEP-1686 is now the standard implementation.

### 1.2 Historical: Blocking Mode (Removed)

The blocking mode design was considered but **removed** in favor of SEP-1686:

```
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

**Why Blocking Mode for v0.2?**
- Simplest possible implementation
- Works with all existing MCP clients
- No client-side changes required
- Approval timeouts are typically short (< 30 minutes)

**Limitations of Blocking Mode:**
- HTTP connection may timeout for long approvals
- Agent cannot do other work while waiting
- No visibility into approval progress
- Connection drops lose the request

### 1.3 v0.2: SEP-1686 Mode

SEP-1686 introduces the "task primitive" to MCP, enabling:
- Deferred result retrieval via polling
- Long-running operations that outlive request/response cycles
- Status tracking for async workflows

```
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
| `tasks/list` | ✅ In Scope | Simple (no pagination) |
| `tasks/cancel` | ✅ In Scope | Cancellation |
| Capability advertisement | ✅ In Scope | During initialize (REQ-CORE-007) |
| Tool annotation rewriting | ✅ In Scope | During tools/list (REQ-CORE-007) |
| Task metadata validation | ✅ In Scope | On tools/call (REQ-CORE-007) |
| Metrics and logging | ✅ In Scope | Observability |
| Rate limiting | ❌ Deferred | v0.3+ (nice-to-have) |
| `tasks/list` pagination | ❌ Deferred | v0.3+ (return all initially) |
| SSE notifications | ❌ Deferred | v0.3+ (polling works for v0.2) |
| Blocking mode | ❌ Removed | Replaced by SEP-1686 async |
| Client disconnection detection | ❌ Removed | Less critical with async polling |

## 5. Constraints

### 5.1 v0.2 Configuration

| Setting | Default | Source | Description |
|---------|---------|--------|-------------|
| Approval timeout | From workflow | `approval.<name>.timeout` | Max wait time |
| On timeout action | `deny` | `approval.<name>.on_timeout` | Action when timeout |

**Note:** v0.2 uses workflow-level timeout from YAML configuration (REQ-CFG-001), not global task TTL.

### 5.2 v0.3+ Configuration (Future)

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| Default TTL | 600s (10 min) | `THOUGHTGATE_TASK_DEFAULT_TTL_SECS` |
| Maximum TTL | 86400s (24 hr) | `THOUGHTGATE_TASK_MAX_TTL_SECS` |
| Cleanup interval | 60s | `THOUGHTGATE_TASK_CLEANUP_INTERVAL_SECS` |
| Max pending per principal | 10 | `THOUGHTGATE_TASK_MAX_PENDING_PER_PRINCIPAL` |
| Max pending global | 1000 | `THOUGHTGATE_TASK_MAX_PENDING_GLOBAL` |

### 5.3 SEP-1686 Task States (v0.3+ Reference)

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

### 6.1 v0.2: Pending Approval Tracker

```rust
/// Internal tracking for pending approvals (v0.2)
/// NOT exposed via task API - for observability only
pub struct PendingApproval {
    /// Unique identifier for correlation
    pub id: Uuid,
    
    /// Original request (for logging/metrics)
    pub tool_name: String,
    pub arguments_hash: String,
    
    /// Principal making the request
    pub principal: Principal,
    
    /// Timing
    pub created_at: Instant,
    pub timeout: Duration,
    
    /// Channel to signal completion
    pub completion_tx: oneshot::Sender<ApprovalOutcome>,
    
    /// Client connection state
    pub client_connected: Arc<AtomicBool>,
}

pub enum ApprovalOutcome {
    Approved,
    Rejected { reason: Option<String> },
    Timeout,
    ClientDisconnected,
}
```

### 6.2 v0.2: Approval Waiter Interface

```rust
#[async_trait]
pub trait ApprovalWaiter: Send + Sync {
    /// Wait for approval decision (blocking mode)
    /// Returns when: approved, rejected, timeout, or client disconnects
    async fn wait_for_approval(
        &self,
        pending: &PendingApproval,
    ) -> ApprovalOutcome;
    
    /// Record an approval decision (called by REQ-GOV-003)
    fn record_decision(
        &self,
        approval_id: Uuid,
        decision: ApprovalDecision,
    ) -> Result<(), ApprovalError>;
    
    /// Check if client is still connected
    fn is_client_connected(&self, approval_id: Uuid) -> bool;
}

pub enum ApprovalDecision {
    Approved { decided_by: String },
    Rejected { decided_by: String, reason: Option<String> },
}
```

### 6.3 v0.2: Pending Approval Store

```rust
/// In-memory store for pending approvals (v0.2)
pub struct PendingApprovalStore {
    /// Map from approval ID to pending approval
    approvals: DashMap<Uuid, PendingApproval>,
}

impl PendingApprovalStore {
    pub fn new() -> Self {
        Self {
            approvals: DashMap::new(),
        }
    }
    
    /// Register a new pending approval
    pub fn register(&self, approval: PendingApproval) -> Uuid {
        let id = approval.id;
        self.approvals.insert(id, approval);
        id
    }
    
    /// Get pending approval by ID
    pub fn get(&self, id: Uuid) -> Option<dashmap::mapref::one::Ref<Uuid, PendingApproval>> {
        self.approvals.get(&id)
    }
    
    /// Remove pending approval (on completion)
    pub fn remove(&self, id: Uuid) -> Option<PendingApproval> {
        self.approvals.remove(&id).map(|(_, v)| v)
    }
    
    /// Count pending approvals (for metrics)
    pub fn count(&self) -> usize {
        self.approvals.len()
    }
    
    /// Count pending approvals for principal
    pub fn count_for_principal(&self, principal: &str) -> usize {
        self.approvals
            .iter()
            .filter(|entry| entry.principal.app_name == principal)
            .count()
    }
}
```

### 6.4 v0.3+: Full Task Structure (Future Reference)

```rust
/// Full task structure for SEP-1686 mode (v0.3+)
pub struct Task {
    // Identity
    pub id: TaskId,
    
    // Request Data
    pub original_request: ToolCallRequest,
    pub request_hash: String,
    
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
    pub result: Option<ToolCallResult>,
    pub failure: Option<FailureInfo>,
}

pub struct TaskId(pub Uuid);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    // SEP-1686 standard states
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
    // ThoughtGate extensions
    Rejected,
    Expired,
}
```

## 7. Behavior Specification

### 7.1 v0.2: SEP-1686 Approval Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                  v0.2 BLOCKING APPROVAL FLOW                    │
└─────────────────────────────────────────────────────────────────┘

  1. Request arrives (tools/call with action: approve)
     │
     ▼
  2. Create PendingApproval
     │
     ├─ Generate UUID for correlation
     ├─ Hash arguments for logging
     ├─ Create completion channel
     └─ Track client connection state
     │
     ▼
  3. Register with PendingApprovalStore
     │
     ▼
  4. Post approval request to Slack (REQ-GOV-003)
     │
     ▼
  5. Wait for outcome (blocking)
     │
     ├─────────────────────────────────────────────┐
     │                                             │
     │  Poll for:                                  │
     │  • Approval decision from Slack             │
     │  • Timeout expiration                       │
     │  • Client disconnection                     │
     │                                             │
     └─────────────────────────────────────────────┘
     │
     ├─── Approved ─────────────────────┐
     │                                  │
     ├─── Rejected ──► Return -32007    │
     │                                  │
     ├─── Timeout ───► Execute          │
     │                 on_timeout       │
     │                 action           │
     │                                  │
     └─── Disconnected ► Cleanup        │
                        (no response)   │
                                        │
                                        ▼
  6. Forward to upstream ◄──────────────┘
     │
     ▼
  7. Return response to agent

  8. Cleanup: Remove from PendingApprovalStore
```

### F-001: Pending Approval Registration (v0.2)

- **F-001.1:** Generate UUID for each pending approval
- **F-001.2:** Store tool name and arguments hash (not full arguments)
- **F-001.3:** Record principal for observability
- **F-001.4:** Create oneshot channel for completion signaling
- **F-001.5:** Track client connection state via `Arc<AtomicBool>`

### F-002: Task Wait (v0.2)

- **F-002.1:** Use `tokio::select!` to wait for multiple conditions
- **F-002.2:** Check for approval decision from Slack polling
- **F-002.3:** Check for timeout expiration
- **F-002.4:** Check for client disconnection
- **F-002.5:** Return first condition that triggers

```rust
async fn wait_for_approval(
    &self,
    pending: &PendingApproval,
) -> ApprovalOutcome {
    let timeout = tokio::time::sleep(pending.timeout);
    let decision_rx = pending.completion_rx.clone();
    
    tokio::select! {
        // Approval decision received
        result = decision_rx => {
            match result {
                Ok(ApprovalDecision::Approved { .. }) => ApprovalOutcome::Approved,
                Ok(ApprovalDecision::Rejected { reason, .. }) => {
                    ApprovalOutcome::Rejected { reason }
                }
                Err(_) => ApprovalOutcome::ClientDisconnected,
            }
        }
        
        // Timeout expired
        _ = timeout => {
            ApprovalOutcome::Timeout
        }
        
        // Client disconnection (checked periodically)
        _ = wait_for_disconnect(pending.client_connected.clone()) => {
            ApprovalOutcome::ClientDisconnected
        }
    }
}
```

### F-003: Client Disconnection Detection (v0.2)

- **F-003.1:** Track TCP connection state
- **F-003.2:** Set `client_connected` to false on disconnect
- **F-003.3:** Cancel pending Slack request on disconnect
- **F-003.4:** Log disconnection with correlation ID
- **F-003.5:** Do NOT execute tool if client disconnects (prevent zombie execution)

### F-004: Timeout Handling (v0.2)

- **F-004.1:** Use workflow timeout from YAML configuration
- **F-004.2:** Execute `on_timeout` action when timeout expires
- **F-004.3:** `on_timeout: deny` returns -32008 error
- **F-004.4:** Log timeout with correlation ID and duration

### F-005: Approval Decision Recording (v0.2)

- **F-005.1:** Receive decision from REQ-GOV-003 (Slack polling)
- **F-005.2:** Signal completion via oneshot channel
- **F-005.3:** If approval not found (expired/cleaned up), log and ignore
- **F-005.4:** Record decision metadata for audit logging

### 7.2 v0.3+: SEP-1686 Task Flow (Future Reference)

```
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

### F-006 to F-012: SEP-1686 Methods (v0.3+ - Deferred)

These features are deferred to v0.3+:

- **F-006:** Task creation with state machine
- **F-007:** Dynamic poll interval computation
- **F-008:** `tasks/get` implementation
- **F-009:** `tasks/result` implementation
- **F-010:** `tasks/list` with pagination
- **F-011:** `tasks/cancel` implementation
- **F-012:** Rate limiting and capacity management

See §10 for full specification reference.

## 8. Non-Functional Requirements

### NFR-001: Observability (v0.2)

**Metrics:**
```
thoughtgate_approvals_pending{principal}
thoughtgate_approvals_total{outcome="approved|rejected|timeout|disconnected"}
thoughtgate_approval_duration_seconds{outcome}
```

**Logging:**
```json
{"level":"info","event":"approval_pending","correlation_id":"abc-123","tool":"delete_user","principal":"app-xyz"}
{"level":"info","event":"approval_complete","correlation_id":"abc-123","outcome":"approved","duration_ms":45000}
{"level":"warn","event":"approval_timeout","correlation_id":"abc-123","tool":"delete_user","timeout_secs":600}
{"level":"warn","event":"client_disconnected","correlation_id":"abc-123","tool":"delete_user"}
```

### NFR-002: Performance (v0.2)

| Metric | Target |
|--------|--------|
| Approval registration | < 1ms |
| Decision recording | < 1ms |
| Memory per pending approval | < 500 bytes |
| Max concurrent pending | 1,000 |

### NFR-003: Reliability (v0.2)

- No zombie executions (tool runs after client disconnect)
- Proper cleanup on all exit paths
- Graceful handling of Slack API failures

## 9. Verification Plan

### 9.1 v0.2 Edge Case Matrix

| Scenario | Expected Behavior | Test ID |
|----------|-------------------|---------|
| Approval approved | Forward to upstream, return result | EC-BLK-001 |
| Approval rejected | Return -32007 error | EC-BLK-002 |
| Approval timeout (on_timeout: deny) | Return -32008 error | EC-BLK-003 |
| Client disconnects during wait | Cleanup, no execution | EC-BLK-004 |
| Slack API error during wait | Retry or return -32603 | EC-BLK-005 |
| Upstream error after approval | Return upstream error | EC-BLK-006 |
| Multiple pending for same principal | All tracked independently | EC-BLK-007 |
| Shutdown with pending approvals | Cleanup, return -32603 | EC-BLK-008 |

### 9.2 v0.2 Assertions

**Unit Tests:**
- `test_pending_approval_registration` — Approval registered correctly
- `test_blocking_wait_approved` — Returns on approval
- `test_blocking_wait_rejected` — Returns on rejection
- `test_blocking_wait_timeout` — Returns on timeout
- `test_client_disconnect_detection` — Detects disconnect
- `test_no_zombie_execution` — No execution after disconnect

**Integration Tests:**
- `test_full_blocking_flow_approved` — Complete approved flow
- `test_full_blocking_flow_rejected` — Complete rejected flow
- `test_full_blocking_flow_timeout` — Complete timeout flow

## 10. v0.3+ Reference: Full SEP-1686 Specification

This section documents the full SEP-1686 implementation for future reference. **Not implemented in v0.2.**

### 10.1 Task State Machine

```
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

### 10.2 Task Store Interface (v0.3+)

```rust
#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn create(&self, task: Task) -> Result<TaskId, TaskError>;
    async fn get(&self, id: &TaskId) -> Result<Option<Task>, TaskError>;
    async fn update(&self, id: &TaskId, update: TaskUpdate) -> Result<Task, TaskError>;
    async fn transition(&self, id: &TaskId, to: TaskStatus, expected: TaskStatus) -> Result<Task, TaskError>;
    async fn list(&self, principal: &Principal, cursor: Option<String>, limit: usize) -> Result<TaskList, TaskError>;
    async fn delete(&self, id: &TaskId) -> Result<(), TaskError>;
    async fn cleanup_expired(&self) -> Result<usize, TaskError>;
}
```

### 10.3 SEP-1686 Method Handlers (v0.3+)

```rust
// tasks/get
async fn handle_tasks_get(&self, params: TasksGetParams) -> JsonRpcResponse {
    let task = self.store.get(&params.task_id).await?;
    // Map internal states to SEP-1686 visible states
    // Return task status and timing info
}

// tasks/result
async fn handle_tasks_result(&self, params: TasksResultParams) -> JsonRpcResponse {
    let task = self.store.get(&params.task_id).await?;
    if task.status.is_terminal() {
        // Return result or failure
    } else {
        // Block until terminal or timeout
    }
}

// tasks/list
async fn handle_tasks_list(&self, params: TasksListParams) -> JsonRpcResponse {
    // Return paginated list of tasks for principal
}

// tasks/cancel
async fn handle_tasks_cancel(&self, params: TasksCancelParams) -> JsonRpcResponse {
    // Cancel if in InputRequired state
    // Return error if already terminal or executing
}
```

## 11. Definition of Done

### 11.1 v0.2 Definition of Done

- [ ] `PendingApproval` struct defined with all fields
- [ ] `PendingApprovalStore` implemented with registration/lookup/removal
- [ ] Blocking wait implemented with `tokio::select!`
- [ ] Client disconnection detection working
- [ ] Timeout handling with `on_timeout` action
- [ ] Approval decision recording from REQ-GOV-003
- [ ] No zombie execution (tool never runs after disconnect)
- [ ] Metrics for pending count and outcomes
- [ ] All edge cases (EC-BLK-001 to EC-BLK-008) covered
- [ ] Integration with REQ-GOV-002 (pipeline) and REQ-GOV-003 (Slack)

### 11.2 v0.3+ Definition of Done (Future)

- [ ] Full Task structure with SEP-1686 states
- [ ] State machine with valid transitions only
- [ ] In-memory storage with TTL cleanup
- [ ] All `tasks/*` methods implemented
- [ ] Rate limiting enforced
- [ ] Capability advertisement during initialize
- [ ] Tool annotation rewriting during tools/list
