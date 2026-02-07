//! Task domain types: Principal, requests, approvals, failures, and the Task struct.
//!
//! Implements: REQ-GOV-001/§6.1

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::error::TaskError;
use super::helpers::{compute_poll_interval, hash_request};
use super::status::TaskStatus;
use crate::governance::engine::TimeoutAction;

// Re-export Sep1686TaskId as TaskId for v0.2 compatibility
pub use crate::protocol::Sep1686TaskId as TaskId;

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
///
/// Note: This type is not re-exported from the governance module's public API.
/// It can be imported from `thoughtgate_core::governance::task::ApprovalRecord`
/// if needed for advanced use cases.
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
///
/// Note: This type is not re-exported from the governance module's public API.
/// It can be imported from `thoughtgate_core::governance::task::FailureStage`
/// if needed for advanced use cases.
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
///
/// Note: This type is not re-exported from the governance module's public API.
/// It can be imported from `thoughtgate_core::governance::task::FailureInfo`
/// if needed for advanced use cases.
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
// Task Transition
// ============================================================================

/// Record of a task state transition for audit trail.
///
/// Implements: REQ-GOV-001/F-001.3
///
/// Note: This type is not re-exported from the governance module's public API.
/// It can be imported from `thoughtgate_core::governance::task::TaskTransition`
/// if needed for advanced use cases.
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
    ///
    /// Returns an error if the request arguments exceed the maximum JSON
    /// nesting depth, which would prevent reliable integrity hashing.
    pub fn new(
        original_request: ToolCallRequest,
        pre_approval_transformed: ToolCallRequest,
        principal: Principal,
        ttl: Duration,
        on_timeout: TimeoutAction,
    ) -> Result<Self, String> {
        let id = TaskId::new();
        let now = Utc::now();
        // Clamp TTL to max 30 days if conversion fails (overflow for extremely large durations)
        let max_ttl = chrono::Duration::days(30);
        let chrono_ttl = chrono::Duration::from_std(ttl).unwrap_or(max_ttl);
        let expires_at = now + chrono_ttl;
        // Hash the transformed request - this is what the human approves
        // The hash verifies integrity of the approved content, not the raw input
        let request_hash = hash_request(&pre_approval_transformed)?;
        let poll_interval = compute_poll_interval(ttl);

        Ok(Self {
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
        })
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
