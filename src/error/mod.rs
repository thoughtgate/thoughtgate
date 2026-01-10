//! Error handling for ThoughtGate.
//!
//! Implements: REQ-CORE-004 (Error Handling)
//!
//! This module defines all error types that can occur in ThoughtGate and provides
//! JSON-RPC 2.0 compliant error response formatting.
//!
//! ## Module Organization
//!
//! - `jsonrpc` - JSON-RPC 2.0 error response structures (REQ-CORE-004)
//! - `proxy` - HTTP proxy error types (REQ-CORE-001, REQ-CORE-002)
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

    // Policy errors (from REQ-POL-001)
    /// Cedar policy engine denied the request.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-008
    #[error("Policy denied: Tool '{tool}' is not permitted")]
    PolicyDenied {
        /// The tool name that was denied
        tool: String,
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

    // Approval errors (from REQ-GOV-002, REQ-GOV-003)
    /// Human reviewer rejected the request.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-012
    #[error("Request for '{tool}' was rejected during approval")]
    ApprovalRejected {
        /// The tool that was rejected
        tool: String,
        /// Optional identifier of who rejected it
        rejected_by: Option<String>,
    },

    /// Approval window expired without a decision.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-013
    #[error("Approval window expired for '{tool}' after {timeout_secs}s")]
    ApprovalTimeout {
        /// The tool that timed out
        tool: String,
        /// The timeout duration in seconds
        timeout_secs: u64,
    },

    // Pipeline errors (from REQ-GOV-002)
    /// An inspector rejected the request.
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
    /// Implements: REQ-CORE-004/EC-ERR-016
    #[error("Policy changed. Request no longer permitted")]
    PolicyDrift {
        /// The task ID affected by policy drift
        task_id: String,
    },

    /// Request context changed during approval.
    ///
    /// Implements: REQ-CORE-004/EC-ERR-017
    #[error("Request context changed during approval")]
    TransformDrift {
        /// The task ID affected by transform drift
        task_id: String,
    },

    // Operational errors
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
    /// Implements: REQ-CORE-004/F-002 (Error Code Mapping)
    ///
    /// Standard JSON-RPC codes (-32700 to -32603) are used for protocol errors.
    /// ThoughtGate custom codes (-32000 to -32013) are used for application errors.
    pub fn to_jsonrpc_code(&self) -> i32 {
        match self {
            // Standard JSON-RPC codes
            Self::ParseError { .. } => -32700,
            Self::InvalidRequest { .. } => -32600,
            Self::MethodNotFound { .. } => -32601,
            Self::InvalidParams { .. } => -32602,
            Self::InternalError { .. } => -32603,

            // ThoughtGate custom codes
            Self::UpstreamConnectionFailed { .. } => -32000,
            Self::UpstreamTimeout { .. } => -32001,
            Self::UpstreamError { .. } => -32002,
            Self::PolicyDenied { .. } => -32003,
            Self::TaskNotFound { .. } => -32004,
            Self::TaskExpired { .. } => -32005,
            Self::TaskCancelled { .. } => -32006,
            Self::ApprovalRejected { .. } => -32007,
            Self::ApprovalTimeout { .. } => -32008,
            Self::RateLimited { .. } => -32009,
            Self::InspectionFailed { .. } => -32010,
            Self::PolicyDrift { .. } => -32011,
            Self::TransformDrift { .. } => -32012,
            Self::ServiceUnavailable { .. } => -32013,
        }
    }

    /// Returns the error type name for metrics and logging.
    ///
    /// Implements: REQ-CORE-004/F-007 (Error Metrics)
    pub fn error_type_name(&self) -> &'static str {
        match self {
            Self::ParseError { .. } => "parse_error",
            Self::InvalidRequest { .. } => "invalid_request",
            Self::MethodNotFound { .. } => "method_not_found",
            Self::InvalidParams { .. } => "invalid_params",
            Self::UpstreamConnectionFailed { .. } => "upstream_connection_failed",
            Self::UpstreamTimeout { .. } => "upstream_timeout",
            Self::UpstreamError { .. } => "upstream_error",
            Self::PolicyDenied { .. } => "policy_denied",
            Self::TaskNotFound { .. } => "task_not_found",
            Self::TaskExpired { .. } => "task_expired",
            Self::TaskCancelled { .. } => "task_cancelled",
            Self::ApprovalRejected { .. } => "approval_rejected",
            Self::ApprovalTimeout { .. } => "approval_timeout",
            Self::RateLimited { .. } => "rate_limited",
            Self::InspectionFailed { .. } => "inspection_failed",
            Self::PolicyDrift { .. } => "policy_drift",
            Self::TransformDrift { .. } => "transform_drift",
            Self::ServiceUnavailable { .. } => "service_unavailable",
            Self::InternalError { .. } => "internal_error",
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

    /// Returns safe details for client consumption (no sensitive data).
    ///
    /// Implements: REQ-CORE-004/NFR-004 (Security - No Data Leaks)
    pub fn safe_details(&self) -> Option<serde_json::Value> {
        match self {
            Self::MethodNotFound { method } => {
                Some(serde_json::json!({ "method": method }))
            }
            Self::PolicyDenied { tool, reason } => {
                let mut details = serde_json::json!({ "tool": tool });
                if let Some(r) = reason {
                    details["reason"] = serde_json::Value::String(r.clone());
                }
                Some(details)
            }
            Self::TaskNotFound { task_id }
            | Self::TaskExpired { task_id }
            | Self::TaskCancelled { task_id }
            | Self::PolicyDrift { task_id }
            | Self::TransformDrift { task_id } => {
                Some(serde_json::json!({ "task_id": task_id }))
            }
            Self::ApprovalRejected { tool, rejected_by } => {
                let mut details = serde_json::json!({ "tool": tool });
                if let Some(by) = rejected_by {
                    details["rejected_by"] = serde_json::Value::String(by.clone());
                }
                Some(details)
            }
            Self::ApprovalTimeout { tool, timeout_secs } => {
                Some(serde_json::json!({
                    "tool": tool,
                    "timeout_secs": timeout_secs
                }))
            }
            Self::InspectionFailed { inspector, .. } => {
                // Don't expose full reason (may contain sensitive data)
                Some(serde_json::json!({ "inspector": inspector }))
            }
            Self::UpstreamError { code, .. } => {
                // Don't expose upstream message (may contain internal details)
                Some(serde_json::json!({ "upstream_code": code }))
            }
            // For other errors, no additional details needed
            _ => None,
        }
    }

    /// Converts error to JSON-RPC error response.
    ///
    /// Implements: REQ-CORE-004/F-004 (Error Response Formatting)
    pub fn to_jsonrpc_error(&self, correlation_id: &str) -> JsonRpcError {
        JsonRpcError {
            code: self.to_jsonrpc_code(),
            message: self.to_string(),
            data: Some(ErrorData {
                correlation_id: correlation_id.to_string(),
                error_type: self.error_type_name().to_string(),
                details: self.safe_details(),
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
            -32004
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
                rejected_by: None
            }
            .to_jsonrpc_code(),
            -32007
        );
        assert_eq!(
            ThoughtGateError::ApprovalTimeout {
                tool: "test".to_string(),
                timeout_secs: 300
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
    /// Verifies: REQ-CORE-004/NFR-004 (Security)
    #[test]
    fn test_no_sensitive_data_leak() {
        // Upstream errors should not expose full message
        let err = ThoughtGateError::UpstreamError {
            code: -1,
            message: "Internal server error with sensitive data".to_string(),
        };
        let details = err.safe_details();
        assert!(details.is_some());
        let json = details.unwrap();
        assert!(!json.to_string().contains("sensitive data"));
        assert!(json.get("upstream_code").is_some());

        // Inspection failures should not expose full reason
        let err = ThoughtGateError::InspectionFailed {
            inspector: "test".to_string(),
            reason: "Sensitive validation details".to_string(),
        };
        let details = err.safe_details();
        assert!(details.is_some());
        let json = details.unwrap();
        assert!(!json.to_string().contains("Sensitive"));
        assert_eq!(json.get("inspector").unwrap(), "test");

        // Connection failures should not expose internal URLs
        let err = ThoughtGateError::UpstreamConnectionFailed {
            url: "http://internal.service:8080".to_string(),
            reason: "Connection refused".to_string(),
        };
        let details = err.safe_details();
        // Should have no details to avoid leaking internal URLs
        assert!(details.is_none());
    }

    /// Tests error message generation follows templates.
    ///
    /// Verifies: REQ-CORE-004/F-003
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
                reason: None
            }
            .to_string(),
            "Policy denied: Tool 'delete_user' is not permitted"
        );

        assert_eq!(
            ThoughtGateError::ApprovalTimeout {
                tool: "dangerous_operation".to_string(),
                timeout_secs: 300
            }
            .to_string(),
            "Approval window expired for 'dangerous_operation' after 300s"
        );
    }

    /// Tests JSON-RPC error response formatting.
    ///
    /// Verifies: REQ-CORE-004/F-004
    #[test]
    fn test_jsonrpc_error_formatting() {
        let err = ThoughtGateError::PolicyDenied {
            tool: "delete_user".to_string(),
            reason: Some("Admin approval required".to_string()),
        };

        let correlation_id = "550e8400-e29b-41d4-a716-446655440000";
        let jsonrpc_err = err.to_jsonrpc_error(correlation_id);

        assert_eq!(jsonrpc_err.code, -32003);
        assert_eq!(
            jsonrpc_err.message,
            "Policy denied: Tool 'delete_user' is not permitted"
        );

        let data = jsonrpc_err.data.unwrap();
        assert_eq!(data.correlation_id, correlation_id);
        assert_eq!(data.error_type, "policy_denied");
        assert!(data.details.is_some());
        assert_eq!(data.retry_after, None);
    }
}
