//! Error handling for ThoughtGate.
//!
//! Implements: REQ-CORE-004 (Error Handling)
//!
//! This module defines all error types that can occur in ThoughtGate and provides
//! JSON-RPC 2.0 compliant error response formatting.
//!
//! # v0.1 Error Model
//!
//! Policy actions map to errors as follows:
//!
//! | Action | Success | Error |
//! |--------|---------|-------|
//! | Forward | Upstream response | -32000/-32001/-32002 (Upstream errors) |
//! | Approve | Upstream response | -32007 (Rejected), -32008 (Timeout) |
//! | Reject | N/A | -32003 (PolicyDenied) |
//!
//! ## Module Organization
//!
//! - `jsonrpc` - JSON-RPC 2.0 error response structures (REQ-CORE-004)
//! - `proxy` - HTTP proxy error types (deferred: REQ-CORE-001, REQ-CORE-002)
//! - `ThoughtGateError` - MCP/JSON-RPC error types (REQ-CORE-004)

pub mod jsonrpc;
pub mod proxy;

// Re-export proxy errors for backwards compatibility
pub use proxy::{ProxyError, ProxyResult};

use jsonrpc::{ErrorData, JsonRpcError};
use thiserror::Error;

/// All error types that can occur in ThoughtGate.
///
/// Implements: REQ-CORE-004/§6.1 (Error Conditions)
///
/// Each variant maps to a specific JSON-RPC error code and provides
/// structured error information for clients.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum ThoughtGateError {
    // Protocol errors (from REQ-CORE-003)
    /// Invalid JSON in request body.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-001
    #[error("Invalid JSON: {details}")]
    ParseError {
        /// Description of the parse error
        details: String,
    },

    /// Request is not a valid JSON-RPC 2.0 message.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-002
    #[error("Invalid JSON-RPC request: {details}")]
    InvalidRequest {
        /// Description of what makes the request invalid
        details: String,
    },

    /// The requested method does not exist.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-003
    #[error("Method '{method}' not found")]
    MethodNotFound {
        /// The method name that was not found
        method: String,
    },

    /// The method parameters are invalid.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-004
    #[error("Invalid parameters: {details}")]
    InvalidParams {
        /// Description of the parameter validation failure
        details: String,
    },

    // Upstream errors (from REQ-CORE-003)
    /// Cannot connect to the upstream MCP server.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-005
    #[error("Cannot connect to MCP server")]
    UpstreamConnectionFailed {
        /// The upstream URL that failed
        url: String,
        /// Reason for the connection failure
        reason: String,
    },

    /// Upstream MCP server did not respond in time.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-006
    #[error("MCP server did not respond in time")]
    UpstreamTimeout {
        /// The upstream URL that timed out
        url: String,
        /// The timeout duration in seconds
        timeout_secs: u64,
    },

    /// Upstream MCP server returned an error.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-007
    #[error("MCP server error: {message}")]
    UpstreamError {
        /// The error code from upstream
        code: i32,
        /// The error message from upstream
        message: String,
    },

    // ═══════════════════════════════════════════════════════════
    // Gate 1: Visibility errors (from REQ-CFG-001)
    // ═══════════════════════════════════════════════════════════
    /// Tool is not exposed by the source's visibility configuration.
    ///
    /// Implements: REQ-CORE-004/§5.2 (-32015)
    #[error("Tool '{tool}' is not available")]
    ToolNotExposed {
        /// The tool name that is not exposed
        tool: String,
        /// The source ID that hides this tool
        source_id: String,
    },

    // ═══════════════════════════════════════════════════════════
    // Gate 2: Governance rule errors (from REQ-CFG-001)
    // ═══════════════════════════════════════════════════════════
    /// Governance rule matched with action: deny.
    ///
    /// Implements: REQ-CORE-004/§5.2 (-32014)
    #[error("Tool '{tool}' is denied by governance rules")]
    GovernanceRuleDenied {
        /// The tool name that was denied
        tool: String,
        /// The rule pattern that matched (if safe to expose)
        rule: Option<String>,
    },

    // ═══════════════════════════════════════════════════════════
    // Gate 3: Cedar policy errors (from REQ-POL-001)
    // ═══════════════════════════════════════════════════════════
    /// Cedar policy engine denied the request.
    ///
    /// Implements: REQ-CORE-004/§5.2 (-32003)
    #[error("Policy denied access to tool '{tool}'")]
    PolicyDenied {
        /// The tool name that was denied
        tool: String,
        /// The policy ID that denied (for logging, not exposed to client)
        policy_id: Option<String>,
        /// Optional reason for the denial (does not expose policy internals)
        reason: Option<String>,
    },

    // Task errors (from REQ-GOV-001) - v0.2+
    /// The specified task ID does not exist.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-009
    #[error("Task '{task_id}' not found")]
    TaskNotFound {
        /// The task ID that was not found
        task_id: String,
    },

    /// The task has exceeded its TTL.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-010
    #[error("Task '{task_id}' has expired")]
    TaskExpired {
        /// The task ID that expired
        task_id: String,
    },

    /// The task was cancelled.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-011
    #[error("Task '{task_id}' was cancelled")]
    TaskCancelled {
        /// The task ID that was cancelled
        task_id: String,
    },

    /// The task result is not yet ready.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-012
    #[error("Task '{task_id}' result not ready")]
    TaskResultNotReady {
        /// The task ID whose result is pending
        task_id: String,
    },

    // ═══════════════════════════════════════════════════════════
    // Gate 4: Approval errors (from REQ-GOV-002, REQ-GOV-003)
    // ═══════════════════════════════════════════════════════════
    /// Human reviewer rejected the request.
    ///
    /// Implements: REQ-CORE-004/§5.2 (-32007)
    #[error("Approval rejected for tool '{tool}'")]
    ApprovalRejected {
        /// The tool that was rejected
        tool: String,
        /// Optional identifier of who rejected it
        rejected_by: Option<String>,
        /// The workflow that processed the rejection
        workflow: Option<String>,
    },

    /// Approval window expired without a decision.
    ///
    /// Implements: REQ-CORE-004/§5.2 (-32008)
    #[error("Approval timeout for tool '{tool}' after {timeout_secs}s")]
    ApprovalTimeout {
        /// The tool that timed out
        tool: String,
        /// The timeout duration in seconds
        timeout_secs: u64,
        /// The workflow that timed out
        workflow: Option<String>,
    },

    /// Approval workflow not found in configuration.
    ///
    /// Implements: REQ-CORE-004/§5.2 (-32017)
    #[error("Approval workflow '{workflow}' not found")]
    WorkflowNotFound {
        /// The workflow name that was not found
        workflow: String,
    },

    // Pipeline errors (from REQ-GOV-002) - v0.2+
    /// An inspector rejected the request.
    ///
    /// **Note:** Inspection is deferred to v0.2+. This error is retained for
    /// future use when Amber Path inspection is enabled.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-015
    #[error("Request validation failed: {reason}")]
    InspectionFailed {
        /// The inspector that failed
        inspector: String,
        /// Reason for the failure
        reason: String,
    },

    /// Policy changed between approval and execution.
    ///
    /// **Note:** Policy drift detection is deferred to v0.2+. In v0.1 blocking mode,
    /// approved requests are forwarded immediately without re-evaluation.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-016
    #[error("Policy changed. Request no longer permitted")]
    PolicyDrift {
        /// The task ID affected by policy drift
        task_id: String,
    },

    /// Request context changed during approval.
    ///
    /// **Note:** Transform drift detection is deferred to v0.2+. In v0.1, requests
    /// are not inspected or transformed.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-017
    #[error("Request context changed during approval")]
    TransformDrift {
        /// The task ID affected by transform drift
        task_id: String,
    },

    // ═══════════════════════════════════════════════════════════
    // MCP Tasks Protocol Errors (Protocol Revision 2025-11-25)
    // ═══════════════════════════════════════════════════════════
    /// Client called a task-required tool without `params.task`.
    ///
    /// Implements: MCP Tasks Specification - Tool-Level Negotiation
    /// Uses: -32600 (Invalid request) per MCP spec
    ///
    /// Tools advertised with `execution.taskSupport: "required"` MUST be called
    /// with `params.task` metadata. This error is returned when the client omits it.
    #[error("Task metadata required for tool '{tool}'")]
    TaskRequired {
        /// The tool that requires task metadata
        tool: String,
        /// Hint for the client on how to fix this
        hint: String,
    },

    /// Client sent `params.task` for a tool that forbids it.
    ///
    /// Implements: MCP Tasks Specification - Tool-Level Negotiation
    /// Uses: -32601 (Method not found) per MCP spec
    ///
    /// Tools advertised with `execution.taskSupport: "forbidden"` (or not present)
    /// MUST NOT receive `params.task`. This error is returned when the client
    /// includes task metadata for such tools.
    #[error("Task metadata forbidden for tool '{tool}'")]
    TaskForbidden {
        /// The tool that forbids task metadata
        tool: String,
    },

    // ═══════════════════════════════════════════════════════════
    // Configuration errors (from REQ-CFG-001)
    // ═══════════════════════════════════════════════════════════
    /// Configuration is invalid or cannot be loaded.
    ///
    /// Implements: REQ-CORE-004/§5.2 (-32016)
    #[error("Configuration error: {details}")]
    ConfigurationError {
        /// Description of the configuration error (sanitized)
        details: String,
    },

    // ═══════════════════════════════════════════════════════════
    // Operational errors
    // ═══════════════════════════════════════════════════════════
    /// Too many requests - rate limit exceeded.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-014
    #[error("Too many requests. Retry after {retry_after_secs:?} seconds")]
    RateLimited {
        /// Optional seconds to wait before retrying
        retry_after_secs: Option<u64>,
    },

    /// Service is temporarily unavailable.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-018
    #[error("Service temporarily unavailable")]
    ServiceUnavailable {
        /// Reason for unavailability
        reason: String,
    },

    /// Internal server error - should not happen.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-019
    #[error("Internal error. Reference: {correlation_id}")]
    InternalError {
        /// Correlation ID for debugging
        correlation_id: String,
    },
}

impl ThoughtGateError {
    /// Maps error to JSON-RPC 2.0 error code.
    ///
    /// Implements: REQ-CORE-004/§5.1, §5.2 (Error Code Mapping)
    ///
    /// Standard JSON-RPC codes (-32700 to -32603) are used for protocol errors.
    /// ThoughtGate custom codes (-32000 to -32017) are used for application errors.
    pub fn to_jsonrpc_code(&self) -> i32 {
        match self {
            // Standard JSON-RPC codes
            Self::ParseError { .. } => -32700,
            Self::InvalidRequest { .. } => -32600,
            Self::MethodNotFound { .. } => -32601,
            Self::InvalidParams { .. } => -32602,
            Self::InternalError { .. } => -32603,

            // ThoughtGate custom codes: Upstream (-32000 to -32002)
            Self::UpstreamConnectionFailed { .. } => -32000,
            Self::UpstreamTimeout { .. } => -32001,
            Self::UpstreamError { .. } => -32002,

            // ThoughtGate custom codes: Gate 3 - Cedar Policy (-32003)
            Self::PolicyDenied { .. } => -32003,

            // ThoughtGate custom codes: Task errors (-32005, -32006, -32020)
            // Note: TaskNotFound uses -32602 (Invalid params) per MCP Tasks spec
            Self::TaskNotFound { .. } => -32602,
            Self::TaskExpired { .. } => -32005,
            Self::TaskCancelled { .. } => -32006,
            Self::TaskResultNotReady { .. } => -32020,

            // ThoughtGate custom codes: Gate 4 - Approval (-32007, -32008, -32017)
            Self::ApprovalRejected { .. } => -32007,
            Self::ApprovalTimeout { .. } => -32008,
            Self::WorkflowNotFound { .. } => -32017,

            // ThoughtGate custom codes: Rate limiting (-32009)
            Self::RateLimited { .. } => -32009,

            // ThoughtGate custom codes: Pipeline errors (-32010 to -32012)
            Self::InspectionFailed { .. } => -32010,
            Self::PolicyDrift { .. } => -32011,
            Self::TransformDrift { .. } => -32012,

            // ThoughtGate custom codes: Operational (-32013)
            Self::ServiceUnavailable { .. } => -32013,

            // ThoughtGate custom codes: Gate 2 - Governance (-32014)
            Self::GovernanceRuleDenied { .. } => -32014,

            // ThoughtGate custom codes: Gate 1 - Visibility (-32015)
            Self::ToolNotExposed { .. } => -32015,

            // ThoughtGate custom codes: Configuration (-32016)
            Self::ConfigurationError { .. } => -32016,

            // MCP Tasks Protocol errors (standard JSON-RPC codes per MCP spec)
            // TaskRequired: -32600 (Invalid request) - task augmentation required
            // TaskForbidden: -32601 (Method not found) - task mode not supported
            Self::TaskRequired { .. } => -32600,
            Self::TaskForbidden { .. } => -32601,
        }
    }

    /// Returns the error type name for metrics and logging.
    ///
    /// Implements: REQ-CORE-004/F-004 (Error Metrics)
    pub fn error_type_name(&self) -> &'static str {
        match self {
            Self::ParseError { .. } => "parse_error",
            Self::InvalidRequest { .. } => "invalid_request",
            Self::MethodNotFound { .. } => "method_not_found",
            Self::InvalidParams { .. } => "invalid_params",
            Self::UpstreamConnectionFailed { .. } => "upstream_connection_failed",
            Self::UpstreamTimeout { .. } => "upstream_timeout",
            Self::UpstreamError { .. } => "upstream_error",
            Self::ToolNotExposed { .. } => "tool_not_exposed",
            Self::GovernanceRuleDenied { .. } => "governance_rule_denied",
            Self::PolicyDenied { .. } => "policy_denied",
            Self::TaskNotFound { .. } => "task_not_found",
            Self::TaskExpired { .. } => "task_expired",
            Self::TaskCancelled { .. } => "task_cancelled",
            Self::TaskResultNotReady { .. } => "task_result_not_ready",
            Self::ApprovalRejected { .. } => "approval_rejected",
            Self::ApprovalTimeout { .. } => "approval_timeout",
            Self::WorkflowNotFound { .. } => "workflow_not_found",
            Self::RateLimited { .. } => "rate_limited",
            Self::InspectionFailed { .. } => "inspection_failed",
            Self::PolicyDrift { .. } => "policy_drift",
            Self::TransformDrift { .. } => "transform_drift",
            Self::ConfigurationError { .. } => "configuration_error",
            Self::ServiceUnavailable { .. } => "service_unavailable",
            Self::InternalError { .. } => "internal_error",
            Self::TaskRequired { .. } => "task_required",
            Self::TaskForbidden { .. } => "task_forbidden",
        }
    }

    /// Returns retry-after hint for retriable errors.
    ///
    /// Implements: REQ-CORE-004/F-003.4 (Retry Guidance)
    pub fn retry_after(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after_secs } => *retry_after_secs,
            _ => None,
        }
    }

    /// Returns the gate that rejected the request, if applicable.
    ///
    /// Implements: REQ-CORE-004/F-001 (Gate Error Classification)
    pub fn gate(&self) -> Option<&'static str> {
        match self {
            // Gate 1: Visibility
            Self::ToolNotExposed { .. } => Some("visibility"),

            // Gate 2: Governance
            Self::GovernanceRuleDenied { .. } => Some("governance"),

            // Gate 3: Cedar Policy
            Self::PolicyDenied { .. } => Some("policy"),

            // Gate 4: Approval
            Self::ApprovalRejected { .. }
            | Self::ApprovalTimeout { .. }
            | Self::WorkflowNotFound { .. } => Some("approval"),

            // Non-gate errors
            _ => None,
        }
    }

    /// Returns the tool name associated with this error, if applicable.
    ///
    /// Implements: REQ-CORE-004/§6.3 (Error Data Population Rules)
    pub fn tool(&self) -> Option<&str> {
        match self {
            Self::ToolNotExposed { tool, .. }
            | Self::GovernanceRuleDenied { tool, .. }
            | Self::PolicyDenied { tool, .. }
            | Self::ApprovalRejected { tool, .. }
            | Self::ApprovalTimeout { tool, .. }
            | Self::TaskRequired { tool, .. }
            | Self::TaskForbidden { tool, .. } => Some(tool),
            _ => None,
        }
    }

    /// Returns safe details for client consumption (no sensitive data).
    ///
    /// Implements: REQ-CORE-004/§6.3 (Error Data Population Rules)
    /// Implements: REQ-CORE-004/NFR-002 (Security - No Data Leaks)
    pub fn safe_details(&self) -> Option<String> {
        match self {
            // Gate 1: Visibility - No details (security)
            Self::ToolNotExposed { .. } => None,

            // Gate 2: Governance - No details (security: don't expose rule patterns)
            Self::GovernanceRuleDenied { .. } => None,

            // Gate 3: Policy - No details (security: don't expose policy internals)
            Self::PolicyDenied { .. } => None,

            // Gate 4: Approval
            Self::ApprovalRejected { rejected_by, .. } => rejected_by
                .as_ref()
                .map(|by| format!("Rejected by: {}", by)),
            Self::ApprovalTimeout { timeout_secs, .. } => {
                Some(format!("Timeout after {}s", timeout_secs))
            }
            Self::WorkflowNotFound { workflow } => {
                Some(format!("Check approval.{} in config", workflow))
            }

            // Upstream errors
            Self::UpstreamConnectionFailed { .. } => None, // Don't expose internal URLs
            Self::UpstreamTimeout { timeout_secs, .. } => {
                Some(format!("Timeout after {}s", timeout_secs))
            }
            Self::UpstreamError { code, .. } => {
                // Don't expose upstream message (may contain internal details)
                Some(format!("Upstream error code: {}", code))
            }

            // Task errors
            Self::TaskNotFound { .. } => None,
            Self::TaskExpired { task_id, .. } => Some(format!("Task {} expired", task_id)),
            Self::TaskCancelled { .. } => None,
            Self::TaskResultNotReady { task_id } => {
                Some(format!("Task {} result pending", task_id))
            }

            // Pipeline errors - No details (security)
            Self::InspectionFailed { inspector, .. } => {
                // Only expose inspector name, not reason
                Some(format!("Inspector: {}", inspector))
            }
            Self::PolicyDrift { .. } => None,
            Self::TransformDrift { .. } => None,

            // Configuration errors
            Self::ConfigurationError { details } => Some(details.clone()),

            // Operational errors
            Self::RateLimited { retry_after_secs } => {
                retry_after_secs.map(|s| format!("Retry after {}s", s))
            }
            Self::ServiceUnavailable { reason } => Some(reason.clone()),

            // Protocol errors
            Self::ParseError { details } => Some(details.clone()),
            Self::InvalidRequest { details } => Some(details.clone()),
            Self::MethodNotFound { method } => Some(format!("Method: {}", method)),
            Self::InvalidParams { details } => Some(details.clone()),

            // SEP-1686 Protocol errors
            Self::TaskRequired { hint, .. } => Some(hint.clone()),
            Self::TaskForbidden { .. } => Some("Tool does not support task mode".to_string()),

            // Internal error - correlation ID only (log lookup)
            Self::InternalError { .. } => None,
        }
    }

    /// Converts error to JSON-RPC error response.
    ///
    /// Implements: REQ-CORE-004/§6.4 (Error Mapping Implementation)
    /// Implements: REQ-CORE-004/F-002 (Error Response Formatting)
    pub fn to_jsonrpc_error(&self, correlation_id: &str) -> JsonRpcError {
        JsonRpcError {
            code: self.to_jsonrpc_code(),
            message: self.to_string(),
            data: Some(ErrorData {
                correlation_id: correlation_id.to_string(),
                gate: self.gate().map(String::from),
                tool: self.tool().map(String::from),
                details: self.safe_details(),
                error_type: self.error_type_name().to_string(),
                retry_after: self.retry_after(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests error code mapping for all error types.
    ///
    /// Verifies: REQ-CORE-004/F-002
    #[test]
    fn test_error_code_mapping() {
        // Standard JSON-RPC codes
        assert_eq!(
            ThoughtGateError::ParseError {
                details: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32700
        );
        assert_eq!(
            ThoughtGateError::InvalidRequest {
                details: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32600
        );
        assert_eq!(
            ThoughtGateError::MethodNotFound {
                method: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32601
        );
        assert_eq!(
            ThoughtGateError::InvalidParams {
                details: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32602
        );
        assert_eq!(
            ThoughtGateError::InternalError {
                correlation_id: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32603
        );

        // ThoughtGate custom codes
        assert_eq!(
            ThoughtGateError::UpstreamConnectionFailed {
                url: "http://test".to_string(),
                reason: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32000
        );
        assert_eq!(
            ThoughtGateError::UpstreamTimeout {
                url: "http://test".to_string(),
                timeout_secs: 30
            }
            .to_jsonrpc_code(),
            -32001
        );
        assert_eq!(
            ThoughtGateError::UpstreamError {
                code: -1,
                message: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32002
        );
        assert_eq!(
            ThoughtGateError::PolicyDenied {
                tool: "test".to_string(),
                policy_id: None,
                reason: None
            }
            .to_jsonrpc_code(),
            -32003
        );
        assert_eq!(
            ThoughtGateError::TaskNotFound {
                task_id: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32602 // MCP: Invalid params (taskId not found)
        );
        assert_eq!(
            ThoughtGateError::TaskExpired {
                task_id: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32005
        );
        assert_eq!(
            ThoughtGateError::TaskCancelled {
                task_id: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32006
        );
        assert_eq!(
            ThoughtGateError::ApprovalRejected {
                tool: "test".to_string(),
                rejected_by: None,
                workflow: None
            }
            .to_jsonrpc_code(),
            -32007
        );
        assert_eq!(
            ThoughtGateError::ApprovalTimeout {
                tool: "test".to_string(),
                timeout_secs: 300,
                workflow: None
            }
            .to_jsonrpc_code(),
            -32008
        );
        assert_eq!(
            ThoughtGateError::RateLimited {
                retry_after_secs: Some(60)
            }
            .to_jsonrpc_code(),
            -32009
        );
        assert_eq!(
            ThoughtGateError::InspectionFailed {
                inspector: "test".to_string(),
                reason: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32010
        );
        assert_eq!(
            ThoughtGateError::PolicyDrift {
                task_id: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32011
        );
        assert_eq!(
            ThoughtGateError::TransformDrift {
                task_id: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32012
        );
        assert_eq!(
            ThoughtGateError::ServiceUnavailable {
                reason: "test".to_string()
            }
            .to_jsonrpc_code(),
            -32013
        );
    }

    /// Tests that error type names are consistent.
    ///
    /// Verifies: REQ-CORE-004/F-007
    #[test]
    fn test_error_type_names() {
        assert_eq!(
            ThoughtGateError::ParseError {
                details: "test".to_string()
            }
            .error_type_name(),
            "parse_error"
        );
        assert_eq!(
            ThoughtGateError::PolicyDenied {
                tool: "test".to_string(),
                policy_id: None,
                reason: None
            }
            .error_type_name(),
            "policy_denied"
        );
        assert_eq!(
            ThoughtGateError::InternalError {
                correlation_id: "test".to_string()
            }
            .error_type_name(),
            "internal_error"
        );
    }

    /// Tests that retry_after is only set for rate limit errors.
    ///
    /// Verifies: REQ-CORE-004/F-003.4
    #[test]
    fn test_retry_after() {
        assert_eq!(
            ThoughtGateError::RateLimited {
                retry_after_secs: Some(60)
            }
            .retry_after(),
            Some(60)
        );
        assert_eq!(
            ThoughtGateError::RateLimited {
                retry_after_secs: None
            }
            .retry_after(),
            None
        );
        assert_eq!(
            ThoughtGateError::InternalError {
                correlation_id: "test".to_string()
            }
            .retry_after(),
            None
        );
    }

    /// Tests that sensitive data is not exposed in error details.
    ///
    /// Verifies: REQ-CORE-004/NFR-002 (Security)
    #[test]
    fn test_no_sensitive_data_leak() {
        // Upstream errors should not expose full message
        let err = ThoughtGateError::UpstreamError {
            code: -1,
            message: "Internal server error with sensitive data".to_string(),
        };
        let details = err.safe_details();
        assert!(details.is_some());
        let details_str = details.unwrap();
        assert!(!details_str.contains("sensitive data"));
        assert!(details_str.contains("-1")); // Only code exposed

        // Inspection failures should not expose full reason
        let err = ThoughtGateError::InspectionFailed {
            inspector: "test".to_string(),
            reason: "Sensitive validation details".to_string(),
        };
        let details = err.safe_details();
        assert!(details.is_some());
        let details_str = details.unwrap();
        assert!(!details_str.contains("Sensitive"));
        assert!(details_str.contains("test")); // Inspector name exposed

        // Connection failures should not expose internal URLs
        let err = ThoughtGateError::UpstreamConnectionFailed {
            url: "http://internal.service:8080".to_string(),
            reason: "Connection refused".to_string(),
        };
        let details = err.safe_details();
        // Should have no details to avoid leaking internal URLs
        assert!(details.is_none());

        // Policy denied should not expose policy internals
        let err = ThoughtGateError::PolicyDenied {
            tool: "delete_user".to_string(),
            policy_id: Some("secret_policy".to_string()),
            reason: Some("Internal rule matched".to_string()),
        };
        let details = err.safe_details();
        // Security: No details for policy denied
        assert!(details.is_none());
    }

    /// Tests error message generation follows templates.
    ///
    /// Verifies: REQ-CORE-004/F-002
    #[test]
    fn test_error_message_generation() {
        assert_eq!(
            ThoughtGateError::ParseError {
                details: "unexpected token".to_string()
            }
            .to_string(),
            "Invalid JSON: unexpected token"
        );

        assert_eq!(
            ThoughtGateError::PolicyDenied {
                tool: "delete_user".to_string(),
                policy_id: None,
                reason: None
            }
            .to_string(),
            "Policy denied access to tool 'delete_user'"
        );

        assert_eq!(
            ThoughtGateError::ApprovalTimeout {
                tool: "dangerous_operation".to_string(),
                timeout_secs: 300,
                workflow: None
            }
            .to_string(),
            "Approval timeout for tool 'dangerous_operation' after 300s"
        );
    }

    /// Tests JSON-RPC error response formatting.
    ///
    /// Verifies: REQ-CORE-004/§6.4
    #[test]
    fn test_jsonrpc_error_formatting() {
        let err = ThoughtGateError::PolicyDenied {
            tool: "delete_user".to_string(),
            policy_id: Some("finance_policy".to_string()),
            reason: Some("Admin approval required".to_string()),
        };

        let correlation_id = "550e8400-e29b-41d4-a716-446655440000";
        let jsonrpc_err = err.to_jsonrpc_error(correlation_id);

        assert_eq!(jsonrpc_err.code, -32003);
        assert_eq!(
            jsonrpc_err.message,
            "Policy denied access to tool 'delete_user'"
        );

        let data = jsonrpc_err.data.unwrap();
        assert_eq!(data.correlation_id, correlation_id);
        assert_eq!(data.error_type, "policy_denied");
        assert_eq!(data.gate, Some("policy".to_string()));
        assert_eq!(data.tool, Some("delete_user".to_string()));
        // Security: PolicyDenied should NOT expose details
        assert!(data.details.is_none());
        assert_eq!(data.retry_after, None);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Gate-specific error tests (REQ-CORE-004 §9.1)
    // ═══════════════════════════════════════════════════════════════════════

    /// Tests Gate 1 (Visibility) error format.
    ///
    /// Verifies: REQ-CORE-004/§9.1 test_gate1_error_format
    #[test]
    fn test_gate1_error_format() {
        let err = ThoughtGateError::ToolNotExposed {
            tool: "admin_delete".to_string(),
            source_id: "upstream".to_string(),
        };

        assert_eq!(err.to_jsonrpc_code(), -32015);
        assert_eq!(err.gate(), Some("visibility"));
        assert_eq!(err.tool(), Some("admin_delete"));
        assert_eq!(err.error_type_name(), "tool_not_exposed");

        let jsonrpc = err.to_jsonrpc_error("test-id");
        let data = jsonrpc.data.unwrap();
        assert_eq!(data.gate, Some("visibility".to_string()));
        assert_eq!(data.tool, Some("admin_delete".to_string()));
        // Security: No details for visibility errors
        assert!(data.details.is_none());
    }

    /// Tests Gate 2 (Governance) error format.
    ///
    /// Verifies: REQ-CORE-004/§9.1 test_gate2_error_format
    #[test]
    fn test_gate2_error_format() {
        let err = ThoughtGateError::GovernanceRuleDenied {
            tool: "delete_all".to_string(),
            rule: Some("*_all".to_string()),
        };

        assert_eq!(err.to_jsonrpc_code(), -32014);
        assert_eq!(err.gate(), Some("governance"));
        assert_eq!(err.tool(), Some("delete_all"));
        assert_eq!(err.error_type_name(), "governance_rule_denied");

        let jsonrpc = err.to_jsonrpc_error("test-id");
        let data = jsonrpc.data.unwrap();
        assert_eq!(data.gate, Some("governance".to_string()));
        assert_eq!(data.tool, Some("delete_all".to_string()));
        // Security: rule pattern is redacted (don't expose governance internals)
        assert!(data.details.is_none());
    }

    /// Tests Gate 3 (Policy) error format.
    ///
    /// Verifies: REQ-CORE-004/§9.1 test_gate3_error_format
    #[test]
    fn test_gate3_error_format() {
        let err = ThoughtGateError::PolicyDenied {
            tool: "transfer_funds".to_string(),
            policy_id: Some("finance".to_string()),
            reason: Some("Amount exceeds limit".to_string()),
        };

        assert_eq!(err.to_jsonrpc_code(), -32003);
        assert_eq!(err.gate(), Some("policy"));
        assert_eq!(err.tool(), Some("transfer_funds"));

        let jsonrpc = err.to_jsonrpc_error("test-id");
        let data = jsonrpc.data.unwrap();
        assert_eq!(data.gate, Some("policy".to_string()));
        assert_eq!(data.tool, Some("transfer_funds".to_string()));
        // Security: No policy details exposed
        assert!(data.details.is_none());
    }

    /// Tests Gate 4 (Approval) rejected error format.
    ///
    /// Verifies: REQ-CORE-004/§9.1 test_gate4_rejected_format
    #[test]
    fn test_gate4_rejected_format() {
        let err = ThoughtGateError::ApprovalRejected {
            tool: "deploy_prod".to_string(),
            rejected_by: Some("alice@example.com".to_string()),
            workflow: Some("production".to_string()),
        };

        assert_eq!(err.to_jsonrpc_code(), -32007);
        assert_eq!(err.gate(), Some("approval"));
        assert_eq!(err.tool(), Some("deploy_prod"));

        let jsonrpc = err.to_jsonrpc_error("test-id");
        let data = jsonrpc.data.unwrap();
        assert_eq!(data.gate, Some("approval".to_string()));
        assert_eq!(data.tool, Some("deploy_prod".to_string()));
        assert_eq!(
            data.details,
            Some("Rejected by: alice@example.com".to_string())
        );
    }

    /// Tests Gate 4 (Approval) timeout error format.
    ///
    /// Verifies: REQ-CORE-004/§9.1 test_gate4_timeout_format
    #[test]
    fn test_gate4_timeout_format() {
        let err = ThoughtGateError::ApprovalTimeout {
            tool: "dangerous_op".to_string(),
            timeout_secs: 300,
            workflow: Some("default".to_string()),
        };

        assert_eq!(err.to_jsonrpc_code(), -32008);
        assert_eq!(err.gate(), Some("approval"));
        assert_eq!(err.tool(), Some("dangerous_op"));

        let jsonrpc = err.to_jsonrpc_error("test-id");
        let data = jsonrpc.data.unwrap();
        assert_eq!(data.gate, Some("approval".to_string()));
        assert_eq!(data.details, Some("Timeout after 300s".to_string()));
    }

    /// Tests workflow not found error format.
    ///
    /// Verifies: REQ-CORE-004/§9.1 test_workflow_not_found_format
    #[test]
    fn test_workflow_not_found_format() {
        let err = ThoughtGateError::WorkflowNotFound {
            workflow: "missing_workflow".to_string(),
        };

        assert_eq!(err.to_jsonrpc_code(), -32017);
        assert_eq!(err.gate(), Some("approval"));
        assert!(err.tool().is_none());
        assert_eq!(err.error_type_name(), "workflow_not_found");

        let jsonrpc = err.to_jsonrpc_error("test-id");
        let data = jsonrpc.data.unwrap();
        assert_eq!(data.gate, Some("approval".to_string()));
        assert!(data.tool.is_none());
        assert_eq!(
            data.details,
            Some("Check approval.missing_workflow in config".to_string())
        );
    }

    /// Tests configuration error format.
    ///
    /// Verifies: REQ-CORE-004/§9.1 test_configuration_error_format
    #[test]
    fn test_configuration_error_format() {
        let err = ThoughtGateError::ConfigurationError {
            details: "Invalid schema version".to_string(),
        };

        assert_eq!(err.to_jsonrpc_code(), -32016);
        assert!(err.gate().is_none());
        assert!(err.tool().is_none());
        assert_eq!(err.error_type_name(), "configuration_error");

        let jsonrpc = err.to_jsonrpc_error("test-id");
        let data = jsonrpc.data.unwrap();
        assert!(data.gate.is_none());
        assert_eq!(data.details, Some("Invalid schema version".to_string()));
    }

    /// Tests that correlation ID is always included.
    ///
    /// Verifies: REQ-CORE-004/§9.1 test_error_data_includes_correlation
    #[test]
    fn test_error_data_includes_correlation() {
        let errors = vec![
            ThoughtGateError::ParseError {
                details: "test".to_string(),
            },
            ThoughtGateError::ToolNotExposed {
                tool: "test".to_string(),
                source_id: "test".to_string(),
            },
            ThoughtGateError::InternalError {
                correlation_id: "internal-id".to_string(),
            },
        ];

        for err in errors {
            let jsonrpc = err.to_jsonrpc_error("my-correlation-id");
            let data = jsonrpc.data.unwrap();
            assert_eq!(data.correlation_id, "my-correlation-id");
        }
    }

    /// Tests that gate field is only set for gate errors.
    ///
    /// Verifies: REQ-CORE-004/§9.1 test_error_data_includes_gate
    #[test]
    fn test_error_data_includes_gate() {
        // Gate errors should have gate field
        let gate_errors = vec![
            (
                ThoughtGateError::ToolNotExposed {
                    tool: "t".to_string(),
                    source_id: "s".to_string(),
                },
                "visibility",
            ),
            (
                ThoughtGateError::GovernanceRuleDenied {
                    tool: "t".to_string(),
                    rule: None,
                },
                "governance",
            ),
            (
                ThoughtGateError::PolicyDenied {
                    tool: "t".to_string(),
                    policy_id: None,
                    reason: None,
                },
                "policy",
            ),
            (
                ThoughtGateError::ApprovalRejected {
                    tool: "t".to_string(),
                    rejected_by: None,
                    workflow: None,
                },
                "approval",
            ),
        ];

        for (err, expected_gate) in gate_errors {
            assert_eq!(err.gate(), Some(expected_gate));
        }

        // Non-gate errors should not have gate field
        let non_gate_errors = vec![
            ThoughtGateError::ParseError {
                details: "test".to_string(),
            },
            ThoughtGateError::UpstreamTimeout {
                url: "test".to_string(),
                timeout_secs: 30,
            },
            ThoughtGateError::TaskNotFound {
                task_id: "test".to_string(),
            },
            ThoughtGateError::RateLimited {
                retry_after_secs: None,
            },
        ];

        for err in non_gate_errors {
            assert!(err.gate().is_none());
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // SEP-1686 Error Tests
    // ═══════════════════════════════════════════════════════════════════════

    /// Tests TaskRequired error code and formatting.
    ///
    /// Verifies: MCP Tasks Specification - Tool-Level Negotiation
    #[test]
    fn test_task_required_error() {
        let err = ThoughtGateError::TaskRequired {
            tool: "delete_user".to_string(),
            hint: "Include params.task per tools/list execution.taskSupport annotation".to_string(),
        };

        // Verify error code: -32600 (Invalid request) per MCP spec
        assert_eq!(err.to_jsonrpc_code(), -32600);

        // Verify error type name
        assert_eq!(err.error_type_name(), "task_required");

        // Verify tool extraction
        assert_eq!(err.tool(), Some("delete_user"));

        // Verify safe_details includes hint
        assert!(err.safe_details().is_some());
        assert!(err.safe_details().unwrap().contains("params.task"));
    }

    /// Tests TaskForbidden error code and formatting.
    ///
    /// Verifies: MCP Tasks Specification - Tool-Level Negotiation
    #[test]
    fn test_task_forbidden_error() {
        let err = ThoughtGateError::TaskForbidden {
            tool: "read_file".to_string(),
        };

        // Verify error code: -32601 (Method not found) per MCP spec
        assert_eq!(err.to_jsonrpc_code(), -32601);

        // Verify error type name
        assert_eq!(err.error_type_name(), "task_forbidden");

        // Verify tool extraction
        assert_eq!(err.tool(), Some("read_file"));

        // Verify safe_details provides useful message
        assert!(err.safe_details().is_some());
        assert!(err.safe_details().unwrap().contains("task mode"));
    }

    /// Tests TaskRequired error produces valid JSON-RPC error.
    #[test]
    fn test_task_required_jsonrpc_error() {
        let err = ThoughtGateError::TaskRequired {
            tool: "admin_action".to_string(),
            hint: "Use async task mode".to_string(),
        };

        let jsonrpc_err = err.to_jsonrpc_error("test-correlation-id");

        // -32600 (Invalid request) per MCP spec
        assert_eq!(jsonrpc_err.code, -32600);
        assert!(jsonrpc_err.message.contains("admin_action"));

        // Verify data field contains structured error info
        let data = jsonrpc_err.data.expect("should have data");
        assert_eq!(data.error_type, "task_required");
        assert!(data.details.is_some());
        assert_eq!(data.tool, Some("admin_action".to_string()));
    }

    /// Tests TaskForbidden error produces valid JSON-RPC error.
    #[test]
    fn test_task_forbidden_jsonrpc_error() {
        let err = ThoughtGateError::TaskForbidden {
            tool: "simple_tool".to_string(),
        };

        let jsonrpc_err = err.to_jsonrpc_error("test-correlation-id");

        // -32601 (Method not found) per MCP spec
        assert_eq!(jsonrpc_err.code, -32601);
        assert!(jsonrpc_err.message.contains("simple_tool"));
    }
}
