//! Cedar policy engine for ThoughtGate request classification.
//!
//! Implements: REQ-POL-001 (Cedar Policy Engine)
//!
//! # v0.1 Simplified Model
//!
//! The policy engine evaluates requests and returns one of three actions:
//!
//! - **Forward**: Send request directly to upstream
//! - **Approve**: Require human approval before forwarding
//! - **Reject**: Deny the request with an error
//!
//! This replaces the original 4-way classification (Green/Amber/Approval/Red).
//! Green and Amber paths are deferred until response inspection or LLM streaming is needed.

pub mod engine;
pub mod loader;
pub mod principal;

use std::time::Duration;
use thiserror::Error;

// ═══════════════════════════════════════════════════════════════════════════
// v0.1 Simplified Policy Actions
// ═══════════════════════════════════════════════════════════════════════════

/// v0.1 Simplified Policy Actions.
///
/// The result of evaluating Cedar policies against an MCP request.
/// This enum determines how the request is handled.
///
/// # Evaluation Order
///
/// Policies are evaluated in this order:
/// 1. `Forward` - If permitted, send immediately
/// 2. `Approve` - If permitted, require human approval
/// 3. (default) - If nothing permitted, reject
///
/// # Traceability
/// - Implements: REQ-POL-001/§6.2 (Policy Action output)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyAction {
    /// Forward request to upstream immediately.
    ///
    /// The request is sent to the MCP server without human approval.
    /// Response is passed through directly to the agent.
    Forward,

    /// Require human approval before forwarding.
    ///
    /// In v0.1 blocking mode, the HTTP connection is held open until
    /// approval is received via Slack. On approval, the request is
    /// forwarded; on rejection or timeout, an error is returned.
    Approve {
        /// Timeout for the approval workflow.
        /// After this duration, the request fails with ApprovalTimeout.
        timeout: Duration,
    },

    /// Reject the request with a policy denial error.
    ///
    /// Returns JSON-RPC error code -32003 (PolicyDenied) to the agent.
    Reject {
        /// Reason for denial (safe for logging, not user-facing).
        /// Does not expose policy internals.
        reason: String,
    },
}

impl PolicyAction {
    /// Returns `true` if this action forwards the request immediately.
    pub fn is_forward(&self) -> bool {
        matches!(self, PolicyAction::Forward)
    }

    /// Returns `true` if this action requires approval.
    pub fn is_approve(&self) -> bool {
        matches!(self, PolicyAction::Approve { .. })
    }

    /// Returns `true` if this action rejects the request.
    pub fn is_reject(&self) -> bool {
        matches!(self, PolicyAction::Reject { .. })
    }

    /// Returns the approval timeout if this is an Approve action.
    pub fn timeout(&self) -> Option<Duration> {
        match self {
            PolicyAction::Approve { timeout } => Some(*timeout),
            _ => None,
        }
    }
}

impl Default for PolicyAction {
    /// Default action is to reject (fail-closed).
    ///
    /// If no policy explicitly permits the action, it is denied.
    fn default() -> Self {
        PolicyAction::Reject {
            reason: "No policy permits this action".to_string(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Legacy 4-way Policy Decision - DEFERRED TO v0.2+
// ═══════════════════════════════════════════════════════════════════════════

/// Policy decision for request routing (4-way classification).
///
/// **v0.1 Status: DEFERRED** - Use `PolicyAction` instead for v0.1.
///
/// This enum is retained for v0.2+ when Green/Amber path distinction is needed.
/// Each variant corresponds to one of the four traffic paths in ThoughtGate.
///
/// # Traceability
/// - Deferred: REQ-POL-001/§6.2 (4-way Policy Decision Output)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Green Path: Stream through without buffering (zero-copy).
    ///
    /// Deferred: REQ-CORE-001 (Zero-Copy Streaming)
    Green,

    /// Amber Path: Buffer and inspect before forwarding.
    ///
    /// Deferred: REQ-CORE-002 (Buffered Inspection)
    Amber,

    /// Approval Path: Require human/agent approval before proceeding.
    ///
    /// Routes to: REQ-GOV-001/002/003 (Governance)
    Approval {
        /// Suggested timeout for approval decision
        timeout: Duration,
    },

    /// Red Path: Deny the request (policy violation).
    ///
    /// Routes to: REQ-CORE-004 (Error Handling)
    Red {
        /// Reason for denial (safe for logging, not user-facing)
        reason: String,
    },
}

/// Request for policy evaluation.
///
/// Implements: REQ-POL-001/§6.1 (Policy Evaluation Request)
#[derive(Debug, Clone)]
pub struct PolicyRequest {
    /// The principal making the request
    pub principal: Principal,

    /// The resource being accessed
    pub resource: Resource,

    /// Optional context for post-approval re-evaluation
    pub context: Option<PolicyContext>,
}

/// Principal identity (app/service making the request).
///
/// Implements: REQ-POL-001/§6.1 (Principal)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Application name (from HOSTNAME)
    pub app_name: String,

    /// Kubernetes namespace
    pub namespace: String,

    /// Kubernetes ServiceAccount name
    pub service_account: String,

    /// Assigned roles for RBAC
    pub roles: Vec<String>,
}

/// Resource being accessed (MCP tool or method).
///
/// Implements: REQ-POL-001/§6.1 (Resource)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// MCP tool call (e.g., "delete_user")
    ToolCall {
        /// Tool name
        name: String,
        /// Upstream server identifier
        server: String,
    },

    /// Generic MCP method (e.g., "resources/read")
    McpMethod {
        /// Method name
        method: String,
        /// Upstream server identifier
        server: String,
    },
}

/// Context for policy evaluation (approval grants, etc.).
///
/// Implements: REQ-POL-001/§6.1 (PolicyContext)
#[derive(Debug, Clone)]
pub struct PolicyContext {
    /// Approval grant for post-approval re-evaluation
    pub approval_grant: Option<ApprovalGrant>,
}

/// Approval grant from human/agent approver.
///
/// Implements: REQ-POL-001/§6.1 (ApprovalGrant)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalGrant {
    /// Task ID that was approved
    pub task_id: String,

    /// Who approved it (user ID or agent ID)
    pub approved_by: String,

    /// Unix timestamp when approved
    pub approved_at: i64,
}

/// Policy engine errors.
///
/// Implements: REQ-POL-001/§6.4 (Errors)
#[derive(Debug, Error, Clone)]
pub enum PolicyError {
    /// Policy file not found
    #[error("Policy file not found: {path}")]
    FileNotFound {
        /// Path that was not found
        path: String,
    },

    /// Policy syntax error
    #[error("Policy parse error at line {line:?}: {details}")]
    ParseError {
        /// Error details
        details: String,
        /// Line number (if available)
        line: Option<usize>,
    },

    /// Schema validation failed
    #[error("Schema validation failed: {details}")]
    SchemaValidation {
        /// Validation error details
        details: String,
    },

    /// Identity inference failed
    #[error("Identity error: {details}")]
    IdentityError {
        /// Error details
        details: String,
    },

    /// Cedar engine error
    #[error("Cedar engine error: {details}")]
    CedarError {
        /// Error details
        details: String,
    },
}

/// Policy loading source.
///
/// Implements: REQ-POL-001/§6.3 (PolicySource)
#[derive(Debug, Clone)]
pub enum PolicySource {
    /// Loaded from ConfigMap file
    ConfigMap {
        /// File path
        path: String,
        /// When it was loaded
        loaded_at: std::time::SystemTime,
    },

    /// Loaded from environment variable
    Environment {
        /// When it was loaded
        loaded_at: std::time::SystemTime,
    },

    /// Embedded default policies
    Embedded,
}

/// Policy engine statistics.
///
/// Implements: REQ-POL-001/§6.3 (PolicyStats)
#[derive(Debug, Clone, Default)]
pub struct PolicyStats {
    /// Number of policies loaded
    pub policy_count: usize,

    /// Last successful reload time
    pub last_reload: Option<std::time::SystemTime>,

    /// Total number of reloads
    pub reload_count: u64,

    /// Total number of evaluations
    pub evaluation_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─────────────────────────────────────────────────────────────────────────
    // v0.1 PolicyAction tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_forward_action() {
        let action = PolicyAction::Forward;
        assert!(action.is_forward());
        assert!(!action.is_approve());
        assert!(!action.is_reject());
        assert_eq!(action.timeout(), None);
    }

    #[test]
    fn test_approve_action() {
        let action = PolicyAction::Approve {
            timeout: Duration::from_secs(300),
        };
        assert!(!action.is_forward());
        assert!(action.is_approve());
        assert!(!action.is_reject());
        assert_eq!(action.timeout(), Some(Duration::from_secs(300)));
    }

    #[test]
    fn test_reject_action() {
        let action = PolicyAction::Reject {
            reason: "Not permitted".to_string(),
        };
        assert!(!action.is_forward());
        assert!(!action.is_approve());
        assert!(action.is_reject());
        assert_eq!(action.timeout(), None);
    }

    #[test]
    fn test_default_is_reject() {
        let action = PolicyAction::default();
        assert!(action.is_reject());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Legacy PolicyDecision tests (retained for v0.2+)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_policy_decision_variants() {
        let green = PolicyDecision::Green;
        assert!(matches!(green, PolicyDecision::Green));

        let amber = PolicyDecision::Amber;
        assert!(matches!(amber, PolicyDecision::Amber));

        let approval = PolicyDecision::Approval {
            timeout: Duration::from_secs(300),
        };
        assert!(matches!(approval, PolicyDecision::Approval { .. }));

        let red = PolicyDecision::Red {
            reason: "Test denial".to_string(),
        };
        assert!(matches!(red, PolicyDecision::Red { .. }));
    }

    #[test]
    fn test_principal_creation() {
        let principal = Principal {
            app_name: "test-app".to_string(),
            namespace: "production".to_string(),
            service_account: "default".to_string(),
            roles: vec!["user".to_string()],
        };

        assert_eq!(principal.app_name, "test-app");
        assert_eq!(principal.namespace, "production");
        assert_eq!(principal.roles.len(), 1);
    }

    #[test]
    fn test_resource_variants() {
        let tool = Resource::ToolCall {
            name: "delete_user".to_string(),
            server: "mcp-server".to_string(),
        };
        assert!(matches!(tool, Resource::ToolCall { .. }));

        let method = Resource::McpMethod {
            method: "resources/read".to_string(),
            server: "mcp-server".to_string(),
        };
        assert!(matches!(method, Resource::McpMethod { .. }));
    }
}
