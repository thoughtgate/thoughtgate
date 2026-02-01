//! Task lifecycle management for ThoughtGate approval workflows.
//!
//! Implements: REQ-GOV-001 (Task Lifecycle)
//!
//! This module provides:
//! - Task data structures for tracking approval state
//! - TaskStatus state machine with valid transitions
//! - In-memory TaskStore with concurrent access support
//! - TTL enforcement and expiration cleanup
//! - Rate limiting per principal
//!
//! ## v0.2 Updates
//!
//! - **SEP-1686 Task IDs** - Tasks use `tg_<nanoid>` format for SEP-1686 compliance
//! - **Task method handlers** - `tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel`
//! - **Status conversion** - `TaskStatus` ↔ `Sep1686Status`
//!
//! ## Constraints
//!
//! - **In-memory storage** - Tasks are lost on restart

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Notify;

// Re-export Sep1686TaskId as TaskId for v0.2 compatibility
pub use crate::protocol::Sep1686TaskId as TaskId;

// Import TimeoutAction from engine module
use super::engine::TimeoutAction;

// ============================================================================
// JSON-RPC ID (for MCP request tracking)
// ============================================================================

// Re-export the canonical JsonRpcId from transport layer.
// This ensures consistent serialization (manual Serialize/Deserialize impls)
// and avoids divergent behavior between transport and governance.
pub use crate::transport::jsonrpc::JsonRpcId;

// ============================================================================
// Principal
// ============================================================================

/// Identity of the requesting agent/application.
///
/// Implements: REQ-GOV-001/§6.1
///
/// Includes K8s identity fields (namespace, service_account, roles) to preserve
/// principal context through the approval pipeline. These fields are populated
/// from [`crate::policy::Principal`] during Gate 4 conversion and used during
/// post-approval policy re-evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    /// Application or agent name
    pub app_name: String,
    /// Optional user identifier
    pub user_id: Option<String>,
    /// Optional session identifier
    pub session_id: Option<String>,
    /// Kubernetes namespace (from ServiceAccount mount)
    pub namespace: String,
    /// Kubernetes service account name (from JWT token)
    pub service_account: String,
    /// RBAC roles (from policy or external source)
    pub roles: Vec<String>,
}

impl Principal {
    /// Creates a new principal with only an app name.
    ///
    /// K8s fields default to `"default"` / empty. Use [`Principal::from_policy`]
    /// in production code to preserve full identity context.
    ///
    /// Implements: REQ-GOV-001/§6.1
    #[must_use]
    pub fn new(app_name: impl Into<String>) -> Self {
        Self {
            app_name: app_name.into(),
            user_id: None,
            session_id: None,
            namespace: "default".to_string(),
            service_account: "default".to_string(),
            roles: vec![],
        }
    }

    /// Creates a principal with full K8s identity from a policy principal.
    ///
    /// Preserves namespace, service_account, and roles so that post-approval
    /// policy re-evaluation uses the correct identity context.
    ///
    /// Implements: REQ-GOV-001/§6.1, REQ-POL-001/F-006
    #[must_use]
    pub fn from_policy(
        app_name: impl Into<String>,
        namespace: impl Into<String>,
        service_account: impl Into<String>,
        roles: Vec<String>,
    ) -> Self {
        Self {
            app_name: app_name.into(),
            user_id: None,
            session_id: None,
            namespace: namespace.into(),
            service_account: service_account.into(),
            roles,
        }
    }

    /// Returns a key suitable for rate limiting lookups.
    ///
    /// Implements: REQ-GOV-001/F-009.1
    ///
    /// The key includes user_id and session_id when present to provide proper
    /// isolation between different principals within the same app. Format:
    /// - `app_name` (base case)
    /// - `app_name:user_id` (with user)
    /// - `app_name::session_id` (with session only, double colon indicates no user)
    /// - `app_name:user_id:session_id` (with both)
    #[must_use]
    pub fn rate_limit_key(&self) -> String {
        match (&self.user_id, &self.session_id) {
            (Some(user), Some(session)) => format!("{}:{}:{}", self.app_name, user, session),
            (Some(user), None) => format!("{}:{}", self.app_name, user),
            (None, Some(session)) => format!("{}::{}", self.app_name, session),
            (None, None) => self.app_name.clone(),
        }
    }
}

// ============================================================================
// Tool Call Request/Result
// ============================================================================

/// A tool call request from the agent.
///
/// Implements: REQ-GOV-001/§6.1
///
/// Note: Despite the name, this struct is used for all governable MCP methods:
/// - `tools/call` - tool invocations
/// - `resources/read` - resource access
/// - `resources/subscribe` - resource subscriptions
/// - `prompts/get` - prompt retrieval
///
/// The `method` field stores the original MCP method to ensure correct
/// reconstruction when forwarding to upstream after approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRequest {
    /// Original MCP method (e.g., "tools/call", "resources/read", "prompts/get")
    pub method: String,
    /// Resource name being accessed (tool name, resource URI, or prompt name)
    pub name: String,
    /// Request arguments as JSON
    pub arguments: serde_json::Value,
    /// Original MCP request ID
    pub mcp_request_id: JsonRpcId,
}

/// Result of a tool call execution.
///
/// Implements: REQ-GOV-001/§6.1
///
/// Uses camelCase for MCP protocol compliance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    /// Result content from upstream
    pub content: serde_json::Value,
    /// Whether the tool call was successful
    pub is_error: bool,
}

// ============================================================================
// Approval Types
// ============================================================================

/// Decision made by an approver.
///
/// Implements: REQ-GOV-001/§6.1
///
/// Note: The `Rejected` variant's `reason` field is public by virtue of the enum
/// being public, allowing cross-crate construction and pattern matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    /// Request was approved
    Approved,
    /// Request was rejected with optional reason
    Rejected {
        /// Reason for rejection (if provided)
        reason: Option<String>,
    },
}

/// Record of an approval decision.
///
/// Implements: REQ-GOV-001/§6.1
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// The approval decision
    pub decision: ApprovalDecision,
    /// Who made the decision (username, system, etc.)
    pub decided_by: String,
    /// When the decision was made
    pub decided_at: DateTime<Utc>,
    /// How long the approval is valid for execution
    pub approval_valid_until: DateTime<Utc>,
    /// Additional metadata from approver
    pub metadata: Option<serde_json::Value>,
}

// ============================================================================
// Failure Types
// ============================================================================

/// Stage at which a task failed.
///
/// Implements: REQ-GOV-001/§6.1
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureStage {
    /// Failed during pre-approval inspection
    PreHitlInspection,
    /// Approval timeout exceeded
    ApprovalTimeout,
    /// Approver rejected the request
    ApprovalRejected,
    /// Policy changed between approval and execution
    PolicyDrift,
    /// Failed during post-approval inspection
    PostHitlInspection,
    /// Request transformed differently than expected
    TransformDrift,
    /// Upstream MCP server error
    UpstreamError,
    /// Service is shutting down
    ServiceShutdown,
    /// Task in unexpected state for the requested operation
    InvalidTaskState,
}

/// Information about why a task failed.
///
/// Implements: REQ-GOV-001/§6.1
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureInfo {
    /// Stage at which the failure occurred
    pub stage: FailureStage,
    /// Human-readable failure reason
    pub reason: String,
    /// Whether the operation can be retried
    pub retriable: bool,
}

// ============================================================================
// Task Status
// ============================================================================

/// Task lifecycle status.
///
/// Implements: REQ-GOV-001/§5.1, F-001
///
/// State machine transitions:
/// - Working → InputRequired (pre-approval complete)
/// - Working → Failed (pre-approval inspection failed)
/// - Working → Expired (TTL exceeded during pre-approval)
/// - InputRequired → Executing (approval received)
/// - InputRequired → Rejected (approver rejected)
/// - InputRequired → Cancelled (agent cancelled)
/// - InputRequired → Expired (TTL exceeded)
/// - Executing → Completed (execution succeeded)
/// - Executing → Failed (execution failed)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Request is being processed (pre-approval inspection)
    Working,
    /// Awaiting external input (approval decision)
    InputRequired,
    /// Executing the approved tool call (internal, not visible to agent)
    Executing,
    /// Task completed successfully
    Completed,
    /// Task failed due to error
    Failed,
    /// Approver rejected the request
    Rejected,
    /// Agent cancelled the task
    Cancelled,
    /// TTL exceeded without completion
    Expired,
}

impl TaskStatus {
    /// Returns true if this is a terminal state.
    ///
    /// Implements: REQ-GOV-001/§5.1
    ///
    /// Terminal states are immutable and indicate the task lifecycle has ended.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Rejected | Self::Cancelled | Self::Expired
        )
    }

    /// Returns true if this state should be visible to agents.
    ///
    /// Implements: REQ-GOV-001/§6.2
    ///
    /// The `Executing` state is internal and should be mapped to `Working`
    /// when exposed to agents via SEP-1686.
    #[must_use]
    pub fn is_agent_visible(&self) -> bool {
        !matches!(self, Self::Executing)
    }

    /// Converts to SEP-1686 status string.
    ///
    /// Implements: REQ-GOV-001/§6.2
    #[must_use]
    pub fn to_sep1686(&self) -> &'static str {
        match self {
            Self::Working | Self::Executing => "working",
            Self::InputRequired => "input_required",
            Self::Completed => "completed",
            Self::Failed | Self::Expired | Self::Rejected => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Checks if a transition from this status to another is valid.
    ///
    /// Implements: REQ-GOV-001/F-001.1, F-001.2
    #[must_use]
    pub fn can_transition_to(&self, to: TaskStatus) -> bool {
        matches!(
            (self, to),
            // From Working
            (TaskStatus::Working, TaskStatus::InputRequired)
                | (TaskStatus::Working, TaskStatus::Failed)
                | (TaskStatus::Working, TaskStatus::Expired)
                // From InputRequired
                | (TaskStatus::InputRequired, TaskStatus::Executing)
                | (TaskStatus::InputRequired, TaskStatus::Rejected)
                | (TaskStatus::InputRequired, TaskStatus::Cancelled)
                | (TaskStatus::InputRequired, TaskStatus::Expired)
                // From Executing
                | (TaskStatus::Executing, TaskStatus::Completed)
                | (TaskStatus::Executing, TaskStatus::Failed)
        )
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Working => write!(f, "working"),
            Self::InputRequired => write!(f, "input_required"),
            Self::Executing => write!(f, "executing"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Rejected => write!(f, "rejected"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Expired => write!(f, "expired"),
        }
    }
}

/// Convert internal TaskStatus to SEP-1686 wire format status.
///
/// Implements: REQ-GOV-001/§6.2, REQ-CORE-007/§6.6
///
/// Mappings:
/// - Working, Executing → Working (Executing is internal-only)
/// - InputRequired → InputRequired
/// - Completed → Completed
/// - Failed, Rejected, Expired → Failed (all error terminal states)
/// - Cancelled → Cancelled
impl From<TaskStatus> for crate::protocol::Sep1686Status {
    fn from(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Working | TaskStatus::Executing => Self::Working,
            TaskStatus::InputRequired => Self::InputRequired,
            TaskStatus::Completed => Self::Completed,
            TaskStatus::Failed | TaskStatus::Rejected | TaskStatus::Expired => Self::Failed,
            TaskStatus::Cancelled => Self::Cancelled,
        }
    }
}

// ============================================================================
// Task Transition
// ============================================================================

/// Record of a task state transition for audit trail.
///
/// Implements: REQ-GOV-001/F-001.3
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTransition {
    /// Previous status
    pub from: TaskStatus,
    /// New status
    pub to: TaskStatus,
    /// When the transition occurred
    pub at: DateTime<Utc>,
    /// Optional reason for the transition
    pub reason: Option<String>,
}

// ============================================================================
// Task
// ============================================================================

/// A task representing an approval workflow.
///
/// Implements: REQ-GOV-001/§6.1
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    // Identity
    /// Unique task identifier
    pub id: TaskId,

    // Request Data
    /// Original request from the agent
    pub original_request: ToolCallRequest,
    /// Request after pre-approval transformation
    pub pre_approval_transformed: ToolCallRequest,
    /// SHA256 hash of request for integrity verification
    pub request_hash: String,

    // Principal
    /// Identity of the requesting agent
    pub principal: Principal,

    // Timing
    /// When the task was created
    pub created_at: DateTime<Utc>,
    /// Time-to-live duration
    pub ttl: Duration,
    /// When the task expires
    pub expires_at: DateTime<Utc>,
    /// Suggested poll interval for agents
    pub poll_interval: Duration,

    // State
    /// Current task status
    pub status: TaskStatus,
    /// Human-readable status message
    pub status_message: Option<String>,
    /// Audit trail of state transitions
    pub transitions: Vec<TaskTransition>,

    // Approval (populated when approved/rejected)
    /// Approval record if decision has been made
    pub approval: Option<ApprovalRecord>,

    // Result (populated when terminal)
    /// Tool call result if completed successfully
    pub result: Option<ToolCallResult>,
    /// Failure info if task failed
    pub failure: Option<FailureInfo>,

    // Timeout behavior
    /// Action to take when approval times out (captured at task creation)
    ///
    /// Implements: REQ-GOV-002/F-006.4 (Timeout Action)
    ///
    /// Stored per-task to ensure "complete with state at decision time" semantics.
    /// If config is hot-reloaded during task lifetime, the original on_timeout
    /// behavior is preserved.
    pub on_timeout: TimeoutAction,
}

impl Task {
    /// Creates a new task in Working state.
    ///
    /// Implements: REQ-GOV-001/F-002
    #[must_use]
    pub fn new(
        original_request: ToolCallRequest,
        pre_approval_transformed: ToolCallRequest,
        principal: Principal,
        ttl: Duration,
        on_timeout: TimeoutAction,
    ) -> Self {
        let id = TaskId::new();
        let now = Utc::now();
        // Clamp TTL to max 30 days if conversion fails (overflow for extremely large durations)
        let max_ttl = chrono::Duration::days(30);
        let chrono_ttl = chrono::Duration::from_std(ttl).unwrap_or(max_ttl);
        let expires_at = now + chrono_ttl;
        // Hash the transformed request - this is what the human approves
        // The hash verifies integrity of the approved content, not the raw input
        let request_hash = hash_request(&pre_approval_transformed);
        let poll_interval = compute_poll_interval(ttl);

        Self {
            id,
            original_request,
            pre_approval_transformed,
            request_hash,
            principal,
            created_at: now,
            ttl,
            expires_at,
            poll_interval,
            status: TaskStatus::Working,
            status_message: None,
            transitions: Vec::new(),
            approval: None,
            result: None,
            failure: None,
            on_timeout,
        }
    }

    /// Returns true if the task has expired.
    ///
    /// Implements: REQ-GOV-001/F-008.2
    #[must_use]
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }

    /// Returns the remaining TTL.
    #[must_use]
    pub fn remaining_ttl(&self) -> Duration {
        let now = Utc::now();
        if now >= self.expires_at {
            Duration::ZERO
        } else {
            (self.expires_at - now).to_std().unwrap_or(Duration::ZERO)
        }
    }

    /// Transitions the task to a new status.
    ///
    /// Implements: REQ-GOV-001/F-001, F-007
    ///
    /// Returns an error if the transition is invalid or the task is in a terminal state.
    pub fn transition(
        &mut self,
        new_status: TaskStatus,
        reason: Option<String>,
    ) -> Result<(), TaskError> {
        // F-001.4: Terminal states are immutable
        if self.status.is_terminal() {
            return Err(TaskError::AlreadyTerminal {
                task_id: self.id.clone(),
                status: self.status,
            });
        }

        // F-001.1, F-001.2: Validate transition
        if !self.status.can_transition_to(new_status) {
            return Err(TaskError::InvalidTransition {
                task_id: self.id.clone(),
                from: self.status,
                to: new_status,
            });
        }

        // F-001.3: Record transition in audit trail
        let transition = TaskTransition {
            from: self.status,
            to: new_status,
            at: Utc::now(),
            reason,
        };
        self.transitions.push(transition);
        self.status = new_status;

        // Update poll interval based on remaining TTL
        self.poll_interval = compute_poll_interval(self.remaining_ttl());

        Ok(())
    }

    /// Transitions with optimistic locking.
    ///
    /// Implements: REQ-GOV-001/F-007
    ///
    /// Only succeeds if the current status matches the expected status.
    pub fn transition_if(
        &mut self,
        expected_status: TaskStatus,
        new_status: TaskStatus,
        reason: Option<String>,
    ) -> Result<(), TaskError> {
        if self.status != expected_status {
            return Err(TaskError::ConcurrentModification {
                task_id: self.id.clone(),
                expected: expected_status,
                actual: self.status,
            });
        }
        self.transition(new_status, reason)
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Computes a SHA256 hash of the request for integrity verification.
///
/// Implements: REQ-GOV-001/F-002.3
///
/// The hash includes only the tool name and arguments, intentionally excluding
/// the `mcp_request_id`. This is because:
/// - The request ID is transport-layer metadata for JSON-RPC correlation
/// - The hash verifies the *semantic content* of what was approved
/// - Two requests with identical tool+arguments perform the same action
///   regardless of their request IDs
///
/// Arguments are serialized using canonical JSON (sorted keys) to ensure
/// deterministic hashing regardless of key insertion order. This is necessary
/// because `serde_json/preserve_order` is enabled transitively by
/// `cedar-policy-core`, making `Value::to_string()` insertion-order dependent.
#[must_use]
pub fn hash_request(request: &ToolCallRequest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(request.name.as_bytes());
    hasher.update(canonical_json(&request.arguments).as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Produces a canonical JSON string with object keys sorted alphabetically.
///
/// This ensures deterministic serialization regardless of the key insertion
/// order used by `serde_json::Value` (which depends on whether `preserve_order`
/// is enabled via feature flags).
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            sorted.sort_by_key(|(k, _)| *k);
            let entries: Vec<String> = sorted
                .into_iter()
                .map(|(k, v)| {
                    let key_str = serde_json::to_string(k).unwrap_or_default();
                    format!("{}:{}", key_str, canonical_json(v))
                })
                .collect();
            format!("{{{}}}", entries.join(","))
        }
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(canonical_json).collect();
            format!("[{}]", items.join(","))
        }
        // Leaf values (strings, numbers, bools, null) serialize deterministically
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Computes the suggested poll interval based on remaining TTL.
///
/// Implements: REQ-GOV-001/F-002.7
///
/// More frequent polling as expiration approaches:
/// - Last minute: poll every 2s
/// - Last 5 min: poll every 5s
/// - Last 15 min: poll every 10s
/// - Otherwise: poll every 30s
fn compute_poll_interval(remaining_ttl: Duration) -> Duration {
    let secs = remaining_ttl.as_secs();
    match secs {
        0..=60 => Duration::from_secs(2),
        61..=300 => Duration::from_secs(5),
        301..=900 => Duration::from_secs(10),
        _ => Duration::from_secs(30),
    }
}

// ============================================================================
// Task Errors
// ============================================================================

/// Errors that can occur during task operations.
///
/// Implements: REQ-GOV-001/§6.5
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TaskError {
    /// Task with the given ID was not found.
    #[error("Task '{task_id}' not found")]
    NotFound {
        /// The task ID that was not found
        task_id: TaskId,
    },

    /// Task has expired.
    #[error("Task '{task_id}' has expired")]
    Expired {
        /// The task ID that expired
        task_id: TaskId,
    },

    /// Task is already in a terminal state.
    #[error("Task '{task_id}' is already in terminal state '{status}'")]
    AlreadyTerminal {
        /// The task ID
        task_id: TaskId,
        /// The current terminal status
        status: TaskStatus,
    },

    /// Invalid state transition.
    #[error("Invalid transition for task '{task_id}': {from} -> {to}")]
    InvalidTransition {
        /// The task ID
        task_id: TaskId,
        /// Current status
        from: TaskStatus,
        /// Attempted new status
        to: TaskStatus,
    },

    /// Concurrent modification detected (optimistic locking failure).
    #[error("Concurrent modification of task '{task_id}': expected {expected}, found {actual}")]
    ConcurrentModification {
        /// The task ID
        task_id: TaskId,
        /// Expected status
        expected: TaskStatus,
        /// Actual status found
        actual: TaskStatus,
    },

    /// Rate limit exceeded for the principal.
    #[error("Rate limit exceeded for principal '{principal}', retry after {retry_after:?}")]
    RateLimited {
        /// The principal that exceeded the limit
        principal: String,
        /// How long to wait before retrying
        retry_after: Duration,
    },

    /// Global capacity exceeded.
    #[error("Global task capacity exceeded")]
    CapacityExceeded,

    /// Result is not yet available.
    #[error("Result not ready for task '{task_id}'")]
    ResultNotReady {
        /// The task ID
        task_id: TaskId,
    },

    /// Internal error.
    #[error("Internal error: {details}")]
    Internal {
        /// Error details
        details: String,
    },
}

// ============================================================================
// Task Store Configuration
// ============================================================================

/// Configuration for the task store.
///
/// Implements: REQ-GOV-001/§5.2
#[derive(Debug, Clone)]
pub struct TaskStoreConfig {
    /// Default TTL for new tasks
    pub default_ttl: Duration,
    /// Maximum TTL allowed
    pub max_ttl: Duration,
    /// Minimum TTL allowed
    pub min_ttl: Duration,
    /// How often to run cleanup
    pub cleanup_interval: Duration,
    /// Maximum pending tasks per principal
    pub max_pending_per_principal: usize,
    /// Maximum pending tasks globally
    pub max_pending_global: usize,
    /// Grace period after terminal before removal
    pub terminal_grace_period: Duration,
}

impl Default for TaskStoreConfig {
    fn default() -> Self {
        Self {
            default_ttl: Duration::from_secs(600),     // 10 minutes
            max_ttl: Duration::from_secs(86400),       // 24 hours
            min_ttl: Duration::from_secs(60),          // 1 minute
            cleanup_interval: Duration::from_secs(60), // 1 minute
            max_pending_per_principal: 10,
            max_pending_global: 1000,
            terminal_grace_period: Duration::from_secs(3600), // 1 hour
        }
    }
}

// ============================================================================
// Task Store
// ============================================================================

/// Internal task entry with metadata for cleanup.
///
/// Stores `Arc<Task>` to avoid deep clones on reads. Mutations use
/// `Arc::make_mut` which clones only when other references exist
/// (copy-on-write semantics).
#[derive(Debug)]
struct TaskEntry {
    /// The task itself (Arc for cheap reads, make_mut for writes)
    task: Arc<Task>,
    /// When the task became terminal (for grace period cleanup)
    terminal_at: Option<DateTime<Utc>>,
    /// Notifier for waiters on this task
    notify: Arc<Notify>,
}

/// In-memory task store with concurrent access support.
///
/// Implements: REQ-GOV-001/§10
///
/// Uses DashMap for lock-free concurrent access to tasks.
#[derive(Debug)]
pub struct TaskStore {
    /// Task storage keyed by TaskId
    tasks: DashMap<TaskId, TaskEntry>,
    /// Index of task IDs by principal for rate limiting and listing
    by_principal: DashMap<String, Vec<TaskId>>,
    /// Configuration
    config: TaskStoreConfig,
    /// Counter for pending (non-terminal) tasks
    pending_count: AtomicUsize,
    /// Atomic counter of pending tasks per principal (keyed by rate_limit_key).
    /// Prevents TOCTOU race between count check and insert in `create()`.
    pending_by_principal: DashMap<String, AtomicUsize>,
}

impl TaskStore {
    /// Creates a new task store with the given configuration.
    ///
    /// Implements: REQ-GOV-001/§10
    #[must_use]
    pub fn new(config: TaskStoreConfig) -> Self {
        Self {
            tasks: DashMap::new(),
            by_principal: DashMap::new(),
            config,
            pending_count: AtomicUsize::new(0),
            pending_by_principal: DashMap::new(),
        }
    }

    /// Creates a new task store with default configuration.
    ///
    /// Implements: REQ-GOV-001/§10
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(TaskStoreConfig::default())
    }

    /// Returns the store configuration.
    ///
    /// Implements: REQ-GOV-001/§5.2
    #[must_use]
    pub fn config(&self) -> &TaskStoreConfig {
        &self.config
    }

    /// Returns the number of pending (non-terminal) tasks.
    ///
    /// Implements: REQ-GOV-001/F-009.3
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending_count.load(Ordering::Acquire)
    }

    /// Returns the total number of tasks (including terminal).
    ///
    /// Implements: REQ-GOV-001/§10
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.tasks.len()
    }

    /// Creates and inserts a new task.
    ///
    /// Implements: REQ-GOV-001/F-002, F-009
    ///
    /// Checks rate limits and capacity before insertion.
    ///
    /// # Concurrency Note
    ///
    /// Global capacity uses a compare_exchange loop for atomic reservation.
    /// The slot is reserved before insertion and rolled back if the per-principal
    /// check fails, ensuring strict enforcement under concurrent access.
    pub fn create(
        &self,
        original_request: ToolCallRequest,
        pre_approval_transformed: ToolCallRequest,
        principal: Principal,
        ttl: Option<Duration>,
        on_timeout: TimeoutAction,
    ) -> Result<Arc<Task>, TaskError> {
        // F-009.3, F-009.4: Atomically reserve a global capacity slot
        loop {
            let current = self.pending_count.load(Ordering::Acquire);
            if current >= self.config.max_pending_global {
                return Err(TaskError::CapacityExceeded);
            }
            if self
                .pending_count
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
            // CAS failed — another thread changed pending_count, retry
        }

        // F-009.1, F-009.2: Atomically check and reserve per-principal slot.
        // Uses atomic increment + check to prevent TOCTOU race between
        // count check and insert under concurrent access.
        let principal_key = principal.rate_limit_key();
        let principal_counter = self
            .pending_by_principal
            .entry(principal_key.clone())
            .or_insert_with(|| AtomicUsize::new(0));
        let prev = principal_counter.fetch_add(1, Ordering::AcqRel);
        if prev >= self.config.max_pending_per_principal {
            // Rollback both counters
            principal_counter.fetch_sub(1, Ordering::Release);
            self.pending_count.fetch_sub(1, Ordering::Release);
            return Err(TaskError::RateLimited {
                principal: principal_key,
                retry_after: Duration::from_secs(60),
            });
        }

        // F-002.4: Apply TTL bounds
        let ttl = ttl.unwrap_or(self.config.default_ttl);
        let ttl = ttl.clamp(self.config.min_ttl, self.config.max_ttl);

        // Create task with on_timeout captured at creation time
        let task = Task::new(
            original_request,
            pre_approval_transformed,
            principal,
            ttl,
            on_timeout,
        );
        let task_id = task.id.clone();
        let task_arc = Arc::new(task);

        // Insert into store
        let entry = TaskEntry {
            task: task_arc.clone(),
            terminal_at: None,
            notify: Arc::new(Notify::new()),
        };
        self.tasks.insert(task_id.clone(), entry);

        // Update principal index
        self.by_principal
            .entry(principal_key)
            .or_default()
            .push(task_id);

        // Global slot was already reserved via compare_exchange above
        Ok(task_arc)
    }

    /// Gets a task by ID.
    ///
    /// Implements: REQ-GOV-001/F-003
    pub fn get(&self, task_id: &TaskId) -> Result<Arc<Task>, TaskError> {
        self.tasks
            .get(task_id)
            .map(|entry| entry.task.clone())
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })
    }

    /// Transitions a task to a new status.
    ///
    /// Implements: REQ-GOV-001/F-001, F-007
    pub fn transition(
        &self,
        task_id: &TaskId,
        new_status: TaskStatus,
        reason: Option<String>,
    ) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        let was_terminal = entry.task.status.is_terminal();
        Arc::make_mut(&mut entry.task).transition(new_status, reason)?;

        // Track when task became terminal
        if !was_terminal && entry.task.status.is_terminal() {
            entry.terminal_at = Some(Utc::now());
            self.pending_count.fetch_sub(1, Ordering::Release);
            // Decrement per-principal counter
            let principal_key = entry.task.principal.rate_limit_key();
            if let Some(counter) = self.pending_by_principal.get(&principal_key) {
                counter.fetch_sub(1, Ordering::Release);
            }
            // Notify any waiters
            entry.notify.notify_waiters();
        }

        Ok(entry.task.clone())
    }

    /// Transitions a task with optimistic locking.
    ///
    /// Implements: REQ-GOV-001/F-007
    pub fn transition_if(
        &self,
        task_id: &TaskId,
        expected_status: TaskStatus,
        new_status: TaskStatus,
        reason: Option<String>,
    ) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        let was_terminal = entry.task.status.is_terminal();
        Arc::make_mut(&mut entry.task).transition_if(expected_status, new_status, reason)?;

        // Track when task became terminal
        if !was_terminal && entry.task.status.is_terminal() {
            entry.terminal_at = Some(Utc::now());
            self.pending_count.fetch_sub(1, Ordering::Release);
            // Decrement per-principal counter
            let principal_key = entry.task.principal.rate_limit_key();
            if let Some(counter) = self.pending_by_principal.get(&principal_key) {
                counter.fetch_sub(1, Ordering::Release);
            }
            // Notify any waiters
            entry.notify.notify_waiters();
        }

        Ok(entry.task.clone())
    }

    /// Records an approval decision on a task.
    ///
    /// Implements: REQ-GOV-001 (called by REQ-GOV-003)
    pub fn record_approval(
        &self,
        task_id: &TaskId,
        decision: ApprovalDecision,
        decided_by: String,
        approval_valid_for: Duration,
    ) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        // Must be in InputRequired state
        if entry.task.status != TaskStatus::InputRequired {
            return Err(TaskError::ConcurrentModification {
                task_id: task_id.clone(),
                expected: TaskStatus::InputRequired,
                actual: entry.task.status,
            });
        }

        let now = Utc::now();
        let approval_valid_until = now
            + chrono::Duration::from_std(approval_valid_for).unwrap_or(chrono::Duration::zero());

        // Transition based on decision
        let (new_status, reason) = match &decision {
            ApprovalDecision::Approved => (TaskStatus::Executing, Some("Approved".to_string())),
            ApprovalDecision::Rejected { reason } => (
                TaskStatus::Rejected,
                reason.clone().or_else(|| Some("Rejected".to_string())),
            ),
        };

        let was_terminal = entry.task.status.is_terminal();
        {
            let task = Arc::make_mut(&mut entry.task);
            task.approval = Some(ApprovalRecord {
                decision,
                decided_by,
                decided_at: now,
                approval_valid_until,
                metadata: None,
            });
            task.transition(new_status, reason)?;
        }

        if !was_terminal && entry.task.status.is_terminal() {
            entry.terminal_at = Some(Utc::now());
            self.pending_count.fetch_sub(1, Ordering::Release);
            entry.notify.notify_waiters();
        }

        Ok(entry.task.clone())
    }

    /// Marks a task as completed with a result.
    ///
    /// Implements: REQ-GOV-001 (called by REQ-GOV-002)
    pub fn complete(
        &self,
        task_id: &TaskId,
        result: ToolCallResult,
    ) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        // Must be in Executing state
        if entry.task.status != TaskStatus::Executing {
            return Err(TaskError::ConcurrentModification {
                task_id: task_id.clone(),
                expected: TaskStatus::Executing,
                actual: entry.task.status,
            });
        }

        {
            let task = Arc::make_mut(&mut entry.task);
            task.result = Some(result);
            task.transition(
                TaskStatus::Completed,
                Some("Execution completed".to_string()),
            )?;
        }
        entry.terminal_at = Some(Utc::now());
        self.pending_count.fetch_sub(1, Ordering::Release);
        entry.notify.notify_waiters();

        Ok(entry.task.clone())
    }

    /// Records an execution result on an auto-approved expired task.
    ///
    /// Unlike `complete()`, this does not transition the task status (it stays
    /// `Expired`). It records the result and approval for audit purposes so
    /// that the task shows evidence of execution after auto-approval.
    pub fn record_auto_approve_result(
        &self,
        task_id: &TaskId,
        result: ToolCallResult,
        approval: super::ApprovalRecord,
    ) -> Result<(), TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        let task = Arc::make_mut(&mut entry.task);
        task.result = Some(result);
        task.approval = Some(approval);
        task.transitions.push(TaskTransition {
            from: TaskStatus::Expired,
            to: TaskStatus::Expired, // Status doesn't change (terminal)
            at: Utc::now(),
            reason: Some("Auto-approved after expiration; executed successfully".to_string()),
        });
        Ok(())
    }

    /// Marks a task as failed with failure info.
    ///
    /// Implements: REQ-GOV-001 (called by REQ-GOV-002)
    pub fn fail(&self, task_id: &TaskId, failure: FailureInfo) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        let was_terminal = entry.task.status.is_terminal();
        if was_terminal {
            return Err(TaskError::AlreadyTerminal {
                task_id: task_id.clone(),
                status: entry.task.status,
            });
        }

        {
            let task = Arc::make_mut(&mut entry.task);
            task.failure = Some(failure.clone());
            task.transition(TaskStatus::Failed, Some(failure.reason))?;
        }
        entry.terminal_at = Some(Utc::now());

        if !was_terminal {
            self.pending_count.fetch_sub(1, Ordering::Release);
        }
        entry.notify.notify_waiters();

        Ok(entry.task.clone())
    }

    /// Cancels a task (only from InputRequired state).
    ///
    /// Implements: REQ-GOV-001/F-006
    ///
    /// This operation is idempotent: cancelling an already-cancelled task
    /// returns success per SEP-1686 spec requirements.
    pub fn cancel(&self, task_id: &TaskId) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        // F-006.1: Only cancel from InputRequired
        if entry.task.status != TaskStatus::InputRequired {
            if entry.task.status.is_terminal() {
                // F-006.2: Idempotency - already cancelled returns success
                if entry.task.status == TaskStatus::Cancelled {
                    return Ok(entry.task.clone());
                }
                // F-006.2b: Other terminal states cannot be cancelled
                return Err(TaskError::AlreadyTerminal {
                    task_id: task_id.clone(),
                    status: entry.task.status,
                });
            }
            // F-006.3: Cannot cancel from non-InputRequired, non-terminal state (e.g., Executing)
            return Err(TaskError::InvalidTransition {
                task_id: task_id.clone(),
                from: entry.task.status,
                to: TaskStatus::Cancelled,
            });
        }

        Arc::make_mut(&mut entry.task).transition(
            TaskStatus::Cancelled,
            Some("Cancelled by agent".to_string()),
        )?;
        entry.terminal_at = Some(Utc::now());
        self.pending_count.fetch_sub(1, Ordering::Release);
        entry.notify.notify_waiters();

        Ok(entry.task.clone())
    }

    /// Expires non-terminal tasks that have exceeded their TTL.
    ///
    /// Implements: REQ-GOV-001/F-008
    ///
    /// Returns the number of tasks expired.
    pub fn expire_overdue(&self) -> usize {
        let now = Utc::now();
        let mut expired = 0;

        // Collect task IDs to expire (to avoid holding locks while modifying)
        let to_expire: Vec<TaskId> = self
            .tasks
            .iter()
            .filter_map(|entry| {
                let task = &entry.task;
                if !task.status.is_terminal() && now > task.expires_at {
                    Some(task.id.clone())
                } else {
                    None
                }
            })
            .collect();

        // Expire each task
        for task_id in to_expire {
            if let Some(mut entry) = self.tasks.get_mut(&task_id) {
                if !entry.task.status.is_terminal()
                    && now > entry.task.expires_at
                    && Arc::make_mut(&mut entry.task)
                        .transition(TaskStatus::Expired, Some("TTL exceeded".to_string()))
                        .is_ok()
                {
                    entry.terminal_at = Some(now);
                    self.pending_count.fetch_sub(1, Ordering::Release);
                    entry.notify.notify_waiters();
                    expired += 1;
                    tracing::warn!(
                        task_id = %task_id,
                        tool = %entry.task.original_request.name,
                        age_seconds = (now - entry.task.created_at).num_seconds(),
                        "Task expired"
                    );
                }
            }
        }

        expired
    }

    /// Removes terminal tasks that have exceeded the grace period.
    ///
    /// Implements: REQ-GOV-001/F-008.4
    ///
    /// Returns the number of tasks removed.
    pub fn cleanup_terminal(&self) -> usize {
        let now = Utc::now();
        let grace_period =
            chrono::Duration::from_std(self.config.terminal_grace_period).unwrap_or_default();

        // Collect task IDs to remove
        let to_remove: Vec<TaskId> = self
            .tasks
            .iter()
            .filter_map(|entry| {
                if let Some(terminal_at) = entry.terminal_at
                    && now - terminal_at > grace_period
                {
                    return Some(entry.task.id.clone());
                }
                None
            })
            .collect();

        let count = to_remove.len();

        // Remove tasks
        for task_id in to_remove {
            if let Some((_, entry)) = self.tasks.remove(&task_id) {
                // Clean up principal index
                let principal_key = entry.task.principal.rate_limit_key();
                if let Some(mut ids) = self.by_principal.get_mut(&principal_key) {
                    ids.retain(|id| id != &task_id);
                    if ids.is_empty() {
                        // Drop the mutable reference before removing to avoid deadlock
                        drop(ids);
                        // Use remove_if to avoid TOCTOU race (another thread may have
                        // added a new ID between the drop and this remove)
                        self.by_principal
                            .remove_if(&principal_key, |_, v| v.is_empty());
                    }
                }
            }
        }

        count
    }

    /// Lists tasks for a principal with pagination.
    ///
    /// Implements: REQ-GOV-001/F-005
    pub fn list_for_principal(
        &self,
        principal: &Principal,
        offset: usize,
        limit: usize,
    ) -> Vec<Arc<Task>> {
        let principal_key = principal.rate_limit_key();
        let task_ids = match self.by_principal.get(&principal_key) {
            Some(ids) => ids.clone(),
            None => return Vec::new(),
        };

        // Collect tasks, sorted by creation time (newest first)
        let mut tasks: Vec<Arc<Task>> = task_ids
            .iter()
            .filter_map(|id| self.tasks.get(id).map(|e| e.task.clone()))
            .collect();

        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        // Apply pagination
        tasks.into_iter().skip(offset).take(limit).collect()
    }

    /// Waits for a task to reach a terminal state.
    ///
    /// Implements: REQ-GOV-001/§10
    ///
    /// Returns the task if it reaches a terminal state within the timeout,
    /// or an error if the timeout is exceeded.
    ///
    /// Uses a loop to handle the race between checking terminal status and
    /// registering for notifications. The Notify::notified() future is created
    /// before checking the status to avoid missing notifications.
    pub async fn wait_for_terminal(
        &self,
        task_id: &TaskId,
        timeout: Duration,
    ) -> Result<Arc<Task>, TaskError> {
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            // Get the notify handle first
            let notify = {
                let entry = self.tasks.get(task_id).ok_or_else(|| TaskError::NotFound {
                    task_id: task_id.clone(),
                })?;
                entry.notify.clone()
            };

            // Create the notified future BEFORE checking status to avoid race
            let notified = notify.notified();

            // Now check if already terminal (after creating notified future)
            {
                let entry = self.tasks.get(task_id).ok_or_else(|| TaskError::NotFound {
                    task_id: task_id.clone(),
                })?;

                if entry.task.status.is_terminal() {
                    return Ok(entry.task.clone());
                }
            }

            // Calculate remaining time
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(TaskError::ResultNotReady {
                    task_id: task_id.clone(),
                });
            }
            let remaining = deadline - now;

            // Wait for notification or timeout
            if tokio::time::timeout(remaining, notified).await.is_err() {
                // Timeout - do one final check
                let entry = self.tasks.get(task_id).ok_or_else(|| TaskError::NotFound {
                    task_id: task_id.clone(),
                })?;

                if entry.task.status.is_terminal() {
                    return Ok(entry.task.clone());
                }
                return Err(TaskError::ResultNotReady {
                    task_id: task_id.clone(),
                });
            }

            // Notified - loop back to check status (handles spurious wakeups)
        }
    }

    /// Marks all pending tasks as failed due to service shutdown.
    ///
    /// Implements: EC-TASK-016
    pub fn fail_all_pending(&self, reason: &str) -> usize {
        let failure = FailureInfo {
            stage: FailureStage::ServiceShutdown,
            reason: reason.to_string(),
            retriable: true,
        };

        let pending_ids: Vec<TaskId> = self
            .tasks
            .iter()
            .filter_map(|entry| {
                if !entry.task.status.is_terminal() {
                    Some(entry.task.id.clone())
                } else {
                    None
                }
            })
            .collect();

        let mut failed = 0;
        for task_id in pending_ids {
            if self.fail(&task_id, failure.clone()).is_ok() {
                failed += 1;
            }
        }

        failed
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request() -> ToolCallRequest {
        ToolCallRequest {
            method: "tools/call".to_string(),
            name: "delete_user".to_string(),
            arguments: serde_json::json!({"user_id": "123"}),
            mcp_request_id: JsonRpcId::Number(1),
        }
    }

    fn test_principal() -> Principal {
        Principal::new("test-app")
    }

    // ========================================================================
    // TaskStatus Tests
    // ========================================================================

    /// Tests that is_terminal correctly identifies terminal states.
    ///
    /// Verifies: REQ-GOV-001/§5.1
    #[test]
    fn test_task_status_is_terminal() {
        assert!(!TaskStatus::Working.is_terminal());
        assert!(!TaskStatus::InputRequired.is_terminal());
        assert!(!TaskStatus::Executing.is_terminal());
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Rejected.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(TaskStatus::Expired.is_terminal());
    }

    /// Tests that is_agent_visible correctly hides internal states.
    ///
    /// Verifies: REQ-GOV-001/§6.2
    #[test]
    fn test_task_status_is_agent_visible() {
        assert!(TaskStatus::Working.is_agent_visible());
        assert!(TaskStatus::InputRequired.is_agent_visible());
        assert!(!TaskStatus::Executing.is_agent_visible()); // Internal only
        assert!(TaskStatus::Completed.is_agent_visible());
        assert!(TaskStatus::Failed.is_agent_visible());
        assert!(TaskStatus::Rejected.is_agent_visible());
        assert!(TaskStatus::Cancelled.is_agent_visible());
        assert!(TaskStatus::Expired.is_agent_visible());
    }

    /// Tests SEP-1686 status mapping.
    ///
    /// Verifies: REQ-GOV-001/§6.2
    #[test]
    fn test_task_status_to_sep1686() {
        assert_eq!(TaskStatus::Working.to_sep1686(), "working");
        assert_eq!(TaskStatus::InputRequired.to_sep1686(), "input_required");
        assert_eq!(TaskStatus::Executing.to_sep1686(), "working"); // Maps to working
        assert_eq!(TaskStatus::Completed.to_sep1686(), "completed");
        assert_eq!(TaskStatus::Failed.to_sep1686(), "failed");
        assert_eq!(TaskStatus::Rejected.to_sep1686(), "failed"); // Maps to failed
        assert_eq!(TaskStatus::Cancelled.to_sep1686(), "cancelled");
        assert_eq!(TaskStatus::Expired.to_sep1686(), "failed"); // Maps to failed
    }

    /// Tests valid state transitions.
    ///
    /// Verifies: REQ-GOV-001/F-001.1
    #[test]
    fn test_state_machine_valid_transitions() {
        // From Working
        assert!(TaskStatus::Working.can_transition_to(TaskStatus::InputRequired));
        assert!(TaskStatus::Working.can_transition_to(TaskStatus::Failed));
        assert!(TaskStatus::Working.can_transition_to(TaskStatus::Expired));

        // From InputRequired
        assert!(TaskStatus::InputRequired.can_transition_to(TaskStatus::Executing));
        assert!(TaskStatus::InputRequired.can_transition_to(TaskStatus::Rejected));
        assert!(TaskStatus::InputRequired.can_transition_to(TaskStatus::Cancelled));
        assert!(TaskStatus::InputRequired.can_transition_to(TaskStatus::Expired));

        // From Executing
        assert!(TaskStatus::Executing.can_transition_to(TaskStatus::Completed));
        assert!(TaskStatus::Executing.can_transition_to(TaskStatus::Failed));
    }

    /// Tests invalid state transitions.
    ///
    /// Verifies: REQ-GOV-001/F-001.2
    #[test]
    fn test_state_machine_invalid_transitions() {
        // Can't go backwards
        assert!(!TaskStatus::InputRequired.can_transition_to(TaskStatus::Working));
        assert!(!TaskStatus::Executing.can_transition_to(TaskStatus::InputRequired));
        assert!(!TaskStatus::Completed.can_transition_to(TaskStatus::Executing));

        // Can't skip states
        assert!(!TaskStatus::Working.can_transition_to(TaskStatus::Executing));
        assert!(!TaskStatus::Working.can_transition_to(TaskStatus::Completed));

        // Terminal states can't transition
        assert!(!TaskStatus::Completed.can_transition_to(TaskStatus::Failed));
        assert!(!TaskStatus::Failed.can_transition_to(TaskStatus::Completed));
        assert!(!TaskStatus::Rejected.can_transition_to(TaskStatus::Cancelled));
    }

    // ========================================================================
    // Task Tests
    // ========================================================================

    /// Tests task creation.
    ///
    /// Verifies: EC-TASK-001
    #[test]
    fn test_task_creation() {
        let task = Task::new(
            test_request(),
            test_request(),
            test_principal(),
            Duration::from_secs(600),
            TimeoutAction::default(),
        );

        assert_eq!(task.status, TaskStatus::Working);
        assert!(
            task.id.is_thoughtgate_owned(),
            "Task ID should be ThoughtGate-owned"
        );
        assert!(task.transitions.is_empty());
        assert!(task.approval.is_none());
        assert!(task.result.is_none());
        assert!(task.failure.is_none());
        assert_eq!(task.on_timeout, TimeoutAction::Deny);
    }

    /// Tests task transition records audit trail.
    ///
    /// Verifies: REQ-GOV-001/F-001.3
    #[test]
    fn test_task_transition_audit_trail() {
        let mut task = Task::new(
            test_request(),
            test_request(),
            test_principal(),
            Duration::from_secs(600),
            TimeoutAction::default(),
        );

        task.transition(
            TaskStatus::InputRequired,
            Some("Ready for approval".to_string()),
        )
        .unwrap();

        assert_eq!(task.status, TaskStatus::InputRequired);
        assert_eq!(task.transitions.len(), 1);
        assert_eq!(task.transitions[0].from, TaskStatus::Working);
        assert_eq!(task.transitions[0].to, TaskStatus::InputRequired);
        assert_eq!(
            task.transitions[0].reason,
            Some("Ready for approval".to_string())
        );
    }

    /// Tests that terminal states are immutable.
    ///
    /// Verifies: REQ-GOV-001/F-001.4
    #[test]
    fn test_terminal_states_immutable() {
        let mut task = Task::new(
            test_request(),
            test_request(),
            test_principal(),
            Duration::from_secs(600),
            TimeoutAction::default(),
        );

        // Get to a terminal state
        task.transition(TaskStatus::InputRequired, None).unwrap();
        task.transition(TaskStatus::Cancelled, None).unwrap();

        // Try to transition from terminal
        let result = task.transition(TaskStatus::Completed, None);
        assert!(matches!(result, Err(TaskError::AlreadyTerminal { .. })));
    }

    /// Tests optimistic locking.
    ///
    /// Verifies: REQ-GOV-001/F-007
    #[test]
    fn test_optimistic_locking() {
        let mut task = Task::new(
            test_request(),
            test_request(),
            test_principal(),
            Duration::from_secs(600),
            TimeoutAction::default(),
        );

        // Correct expected status
        task.transition_if(TaskStatus::Working, TaskStatus::InputRequired, None)
            .unwrap();

        // Wrong expected status
        let result = task.transition_if(TaskStatus::Working, TaskStatus::Executing, None);
        assert!(matches!(
            result,
            Err(TaskError::ConcurrentModification { .. })
        ));
    }

    // ========================================================================
    // TaskStore Tests
    // ========================================================================

    /// Tests task store creation and retrieval.
    ///
    /// Verifies: EC-TASK-001, EC-TASK-002
    #[test]
    fn test_task_store_create_and_get() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        assert_eq!(task.status, TaskStatus::Working);
        assert_eq!(store.pending_count(), 1);

        let retrieved = store.get(&task.id).unwrap();
        assert_eq!(retrieved.id, task.id);
    }

    /// Tests task not found error.
    ///
    /// Verifies: EC-TASK-003
    #[test]
    fn test_task_store_not_found() {
        let store = TaskStore::with_defaults();
        let fake_id = TaskId::new();

        let result = store.get(&fake_id);
        assert!(matches!(result, Err(TaskError::NotFound { .. })));
    }

    /// Tests rate limiting per principal.
    ///
    /// Verifies: EC-TASK-014, REQ-GOV-001/F-009.2
    #[test]
    fn test_rate_limiting_per_principal() {
        let config = TaskStoreConfig {
            max_pending_per_principal: 2,
            ..Default::default()
        };
        let store = TaskStore::new(config);

        // Create up to limit
        store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();
        store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Third should fail
        let result = store.create(
            test_request(),
            test_request(),
            test_principal(),
            None,
            TimeoutAction::default(),
        );
        assert!(matches!(result, Err(TaskError::RateLimited { .. })));

        // Different principal should work
        let other_principal = Principal::new("other-app");
        store
            .create(
                test_request(),
                test_request(),
                other_principal,
                None,
                TimeoutAction::default(),
            )
            .unwrap();
    }

    /// Tests global capacity limit.
    ///
    /// Verifies: EC-TASK-015, REQ-GOV-001/F-009.4
    #[test]
    fn test_global_capacity_limit() {
        let config = TaskStoreConfig {
            max_pending_global: 2,
            max_pending_per_principal: 10,
            ..Default::default()
        };
        let store = TaskStore::new(config);

        store
            .create(
                test_request(),
                test_request(),
                Principal::new("app-1"),
                None,
                TimeoutAction::default(),
            )
            .unwrap();
        store
            .create(
                test_request(),
                test_request(),
                Principal::new("app-2"),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        let result = store.create(
            test_request(),
            test_request(),
            Principal::new("app-3"),
            None,
            TimeoutAction::default(),
        );
        assert!(matches!(result, Err(TaskError::CapacityExceeded)));
    }

    /// Tests task cancellation.
    ///
    /// Verifies: EC-TASK-007, REQ-GOV-001/F-006.1
    #[test]
    fn test_task_cancellation() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Transition to InputRequired first
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        // Cancel
        let cancelled = store.cancel(&task.id).unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(store.pending_count(), 0);
    }

    /// Tests cannot cancel completed task.
    ///
    /// Verifies: EC-TASK-008
    #[test]
    fn test_cannot_cancel_completed() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Complete the task
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();
        store
            .record_approval(
                &task.id,
                ApprovalDecision::Approved,
                "tester".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();
        store
            .complete(
                &task.id,
                ToolCallResult {
                    content: serde_json::json!({}),
                    is_error: false,
                },
            )
            .unwrap();

        // Try to cancel
        let result = store.cancel(&task.id);
        assert!(matches!(result, Err(TaskError::AlreadyTerminal { .. })));
    }

    /// Tests cannot cancel executing task.
    ///
    /// Verifies: EC-TASK-009
    #[test]
    fn test_cannot_cancel_executing() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Move to Executing
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();
        store
            .record_approval(
                &task.id,
                ApprovalDecision::Approved,
                "tester".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();

        // Try to cancel - should fail with InvalidTransition since Executing is not terminal
        let result = store.cancel(&task.id);
        assert!(matches!(
            result,
            Err(TaskError::InvalidTransition {
                from: TaskStatus::Executing,
                to: TaskStatus::Cancelled,
                ..
            })
        ));
    }

    /// Tests task expiration.
    ///
    /// Verifies: EC-TASK-012, REQ-GOV-001/F-008
    #[tokio::test]
    async fn test_task_expiration() {
        let config = TaskStoreConfig {
            min_ttl: Duration::from_millis(10),
            default_ttl: Duration::from_millis(10),
            ..Default::default()
        };
        let store = TaskStore::new(config);

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Transition to InputRequired (expiration is allowed from this state)
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        // Wait for expiration (longer than TTL to ensure expiration)
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Run cleanup
        let expired = store.expire_overdue();
        assert_eq!(expired, 1);

        let task = store.get(&task.id).unwrap();
        assert_eq!(task.status, TaskStatus::Expired);
    }

    /// Tests concurrent modification detection.
    ///
    /// Verifies: EC-TASK-013
    #[test]
    fn test_concurrent_modification() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // First transition succeeds
        store
            .transition_if(
                &task.id,
                TaskStatus::Working,
                TaskStatus::InputRequired,
                None,
            )
            .unwrap();

        // Second transition with wrong expected status fails
        let result = store.transition_if(&task.id, TaskStatus::Working, TaskStatus::Failed, None);
        assert!(matches!(
            result,
            Err(TaskError::ConcurrentModification { .. })
        ));
    }

    /// Tests listing tasks for principal.
    ///
    /// Verifies: EC-TASK-010, EC-TASK-011
    #[test]
    fn test_list_for_principal() {
        let store = TaskStore::with_defaults();
        let principal = test_principal();

        // Create 3 tasks
        for _ in 0..3 {
            store
                .create(
                    test_request(),
                    test_request(),
                    principal.clone(),
                    None,
                    TimeoutAction::default(),
                )
                .unwrap();
        }

        // List all
        let tasks = store.list_for_principal(&principal, 0, 10);
        assert_eq!(tasks.len(), 3);

        // List with pagination
        let page1 = store.list_for_principal(&principal, 0, 2);
        assert_eq!(page1.len(), 2);

        let page2 = store.list_for_principal(&principal, 2, 2);
        assert_eq!(page2.len(), 1);

        // Different principal sees nothing
        let other = Principal::new("other");
        let empty = store.list_for_principal(&other, 0, 10);
        assert!(empty.is_empty());
    }

    /// Tests shutdown marks pending tasks as failed.
    ///
    /// Verifies: EC-TASK-016
    #[test]
    fn test_shutdown_fails_pending() {
        let store = TaskStore::with_defaults();

        // Create 3 pending tasks
        for _ in 0..3 {
            store
                .create(
                    test_request(),
                    test_request(),
                    test_principal(),
                    None,
                    TimeoutAction::default(),
                )
                .unwrap();
        }

        assert_eq!(store.pending_count(), 3);

        // Shutdown
        let failed = store.fail_all_pending("Service shutting down");
        assert_eq!(failed, 3);
        assert_eq!(store.pending_count(), 0);
    }

    /// Tests poll interval computation.
    ///
    /// Verifies: REQ-GOV-001/F-002.7
    #[test]
    fn test_poll_interval_computation() {
        assert_eq!(
            compute_poll_interval(Duration::from_secs(30)),
            Duration::from_secs(2)
        );
        assert_eq!(
            compute_poll_interval(Duration::from_secs(60)),
            Duration::from_secs(2)
        );
        assert_eq!(
            compute_poll_interval(Duration::from_secs(120)),
            Duration::from_secs(5)
        );
        assert_eq!(
            compute_poll_interval(Duration::from_secs(600)),
            Duration::from_secs(10)
        );
        assert_eq!(
            compute_poll_interval(Duration::from_secs(1200)),
            Duration::from_secs(30)
        );
    }

    /// Tests request hash computation.
    ///
    /// Verifies: REQ-GOV-001/F-002.3
    #[test]
    fn test_request_hash() {
        let req1 = test_request();
        let req2 = test_request();
        let req3 = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "other_tool".to_string(),
            arguments: serde_json::json!({}),
            mcp_request_id: JsonRpcId::Number(1),
        };

        let hash1 = hash_request(&req1);
        let hash2 = hash_request(&req2);
        let hash3 = hash_request(&req3);

        // Same request produces same hash
        assert_eq!(hash1, hash2);

        // Different request produces different hash
        assert_ne!(hash1, hash3);

        // Hash is a valid hex string
        assert!(hash1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Tests that canonical JSON hashing produces identical hashes regardless of
    /// key insertion order in `serde_json::Value` objects.
    ///
    /// This is critical because `serde_json/preserve_order` is transitively
    /// enabled by `cedar-policy-core`, making `Value::to_string()` dependent
    /// on insertion order.
    ///
    /// Verifies: REQ-GOV-001/F-002.3
    #[test]
    fn test_canonical_json_hash_key_order_independent() {
        // Build two Value::Objects with different key insertion orders
        let args_ab = serde_json::json!({"a": 1, "b": 2});
        let args_ba = serde_json::json!({"b": 2, "a": 1});

        let req_ab = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "test_tool".to_string(),
            arguments: args_ab,
            mcp_request_id: JsonRpcId::Number(1),
        };
        let req_ba = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "test_tool".to_string(),
            arguments: args_ba,
            mcp_request_id: JsonRpcId::Number(2),
        };

        // Must produce identical hashes despite different key orders
        assert_eq!(hash_request(&req_ab), hash_request(&req_ba));

        // Nested objects should also be order-independent
        let nested_1 = serde_json::json!({"outer": {"z": 3, "a": 1}, "list": [1, 2]});
        let nested_2 = serde_json::json!({"list": [1, 2], "outer": {"a": 1, "z": 3}});

        let req_nested_1 = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "nested_tool".to_string(),
            arguments: nested_1,
            mcp_request_id: JsonRpcId::Number(3),
        };
        let req_nested_2 = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "nested_tool".to_string(),
            arguments: nested_2,
            mcp_request_id: JsonRpcId::Number(4),
        };

        assert_eq!(hash_request(&req_nested_1), hash_request(&req_nested_2));
    }

    /// Tests approval recording and state transition.
    ///
    /// Verifies: REQ-GOV-001 (record_approval)
    #[test]
    fn test_approval_recording() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Move to InputRequired
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        // Approve
        let approved = store
            .record_approval(
                &task.id,
                ApprovalDecision::Approved,
                "approver@example.com".to_string(),
                Duration::from_secs(300),
            )
            .unwrap();

        assert_eq!(approved.status, TaskStatus::Executing);
        assert!(approved.approval.is_some());
        let approval = approved.approval.as_ref().unwrap();
        assert_eq!(approval.decision, ApprovalDecision::Approved);
        assert_eq!(approval.decided_by, "approver@example.com");
    }

    /// Tests rejection recording.
    #[test]
    fn test_rejection_recording() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Move to InputRequired
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        // Reject
        let rejected = store
            .record_approval(
                &task.id,
                ApprovalDecision::Rejected {
                    reason: Some("Too risky".to_string()),
                },
                "approver@example.com".to_string(),
                Duration::from_secs(0),
            )
            .unwrap();

        assert_eq!(rejected.status, TaskStatus::Rejected);
        assert!(rejected.approval.is_some());
    }

    /// Tests full task lifecycle: create → approve → complete.
    #[test]
    fn test_full_lifecycle() {
        let store = TaskStore::with_defaults();

        // Create
        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();
        assert_eq!(task.status, TaskStatus::Working);
        assert_eq!(store.pending_count(), 1);

        // Pre-approval complete
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        // Approve
        store
            .record_approval(
                &task.id,
                ApprovalDecision::Approved,
                "approver".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();

        // Executing (still pending)
        let task = store.get(&task.id).unwrap();
        assert_eq!(task.status, TaskStatus::Executing);
        assert_eq!(store.pending_count(), 1);

        // Complete
        let result = ToolCallResult {
            content: serde_json::json!({"success": true}),
            is_error: false,
        };
        let completed = store.complete(&task.id, result).unwrap();

        assert_eq!(completed.status, TaskStatus::Completed);
        assert!(completed.result.is_some());
        assert_eq!(store.pending_count(), 0);
    }

    /// Tests task failure recording.
    #[test]
    fn test_task_failure() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        let failure = FailureInfo {
            stage: FailureStage::PreHitlInspection,
            reason: "Validation failed".to_string(),
            retriable: false,
        };

        let failed = store.fail(&task.id, failure).unwrap();
        assert_eq!(failed.status, TaskStatus::Failed);
        assert!(failed.failure.is_some());
        assert_eq!(store.pending_count(), 0);
    }

    /// Tests terminal grace period cleanup.
    #[tokio::test]
    async fn test_terminal_cleanup() {
        let config = TaskStoreConfig {
            terminal_grace_period: Duration::from_millis(1),
            ..Default::default()
        };
        let store = TaskStore::new(config);

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Complete it
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();
        store
            .record_approval(
                &task.id,
                ApprovalDecision::Approved,
                "test".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();
        store
            .complete(
                &task.id,
                ToolCallResult {
                    content: serde_json::json!({}),
                    is_error: false,
                },
            )
            .unwrap();

        assert_eq!(store.total_count(), 1);

        // Wait for grace period
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Cleanup
        let removed = store.cleanup_terminal();
        assert_eq!(removed, 1);
        assert_eq!(store.total_count(), 0);
    }

    #[tokio::test]
    async fn test_cleanup_terminal_removes_empty_principal_entries() {
        let config = TaskStoreConfig {
            terminal_grace_period: Duration::from_millis(1),
            ..Default::default()
        };
        let store = TaskStore::new(config);

        let principal = test_principal();

        // Create two tasks for the same principal
        let task1 = store
            .create(
                test_request(),
                test_request(),
                principal.clone(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();
        let task2 = store
            .create(
                test_request(),
                test_request(),
                principal.clone(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Principal should have entries
        let key = principal.rate_limit_key();
        assert!(
            store.by_principal.contains_key(&key),
            "by_principal should contain the principal"
        );

        // Complete both tasks
        for task_id in [&task1.id, &task2.id] {
            store
                .transition(task_id, TaskStatus::InputRequired, None)
                .unwrap();
            store
                .record_approval(
                    task_id,
                    ApprovalDecision::Approved,
                    "test".to_string(),
                    Duration::from_secs(60),
                )
                .unwrap();
            store
                .complete(
                    task_id,
                    ToolCallResult {
                        content: serde_json::json!({}),
                        is_error: false,
                    },
                )
                .unwrap();
        }

        // Wait for grace period
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Cleanup should remove both tasks AND the empty principal entry
        let removed = store.cleanup_terminal();
        assert_eq!(removed, 2);
        assert_eq!(store.total_count(), 0);
        assert!(
            !store.by_principal.contains_key(&key),
            "by_principal should not contain the principal after all tasks cleaned up"
        );
    }
}
