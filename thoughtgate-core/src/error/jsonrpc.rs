//! JSON-RPC 2.0 error response structures.
//!
//! Implements: REQ-CORE-004/§6.2 (JSON-RPC Error Response)

use serde::{Deserialize, Serialize};

/// JSON-RPC 2.0 error object.
///
/// Implements: REQ-CORE-004/§6.2
///
/// This structure is embedded in JSON-RPC error responses and follows
/// the JSON-RPC 2.0 specification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Error code (standard or ThoughtGate-specific)
    pub code: i32,

    /// Human-readable error message
    pub message: String,

    /// Additional error data (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorData>,
}

/// Additional error context data.
///
/// Implements: REQ-CORE-004/§6.2 (ErrorData)
///
/// Contains structured error information for debugging and observability.
/// All fields are safe for client consumption (no sensitive data).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ErrorData {
    /// Unique identifier for tracing this error in logs
    pub correlation_id: String,

    /// Which gate rejected the request: "visibility", "governance", "policy", "approval"
    ///
    /// Implements: REQ-CORE-004/F-001 (Gate Error Classification)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,

    /// Tool that was being called when error occurred
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,

    /// Type-specific error details (sanitized)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,

    /// Machine-readable error type name (for metrics/logging)
    pub error_type: String,

    /// Suggested retry delay in seconds (for retriable errors)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jsonrpc_error_serialization() {
        let error = JsonRpcError {
            code: -32003,
            message: "Policy denied access to tool 'delete_user'".to_string(),
            data: Some(ErrorData {
                correlation_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
                gate: Some("policy".to_string()),
                tool: Some("delete_user".to_string()),
                details: None, // Security: No policy details
                error_type: "policy_denied".to_string(),
                retry_after: None,
            }),
        };

        let json = serde_json::to_value(&error).unwrap();

        assert_eq!(json["code"], -32003);
        assert_eq!(
            json["message"],
            "Policy denied access to tool 'delete_user'"
        );
        assert_eq!(
            json["data"]["correlation_id"],
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(json["data"]["error_type"], "policy_denied");
        assert_eq!(json["data"]["gate"], "policy");
        assert_eq!(json["data"]["tool"], "delete_user");
    }

    #[test]
    fn test_error_without_data() {
        let error = JsonRpcError {
            code: -32700,
            message: "Parse error".to_string(),
            data: None,
        };

        let json = serde_json::to_string(&error).unwrap();

        // data field should be omitted when None
        assert!(!json.contains("\"data\""));
    }

    #[test]
    fn test_retry_after_serialization() {
        let error = JsonRpcError {
            code: -32009,
            message: "Rate limited".to_string(),
            data: Some(ErrorData {
                correlation_id: "test-id".to_string(),
                gate: None,
                tool: None,
                details: Some("Retry after 60s".to_string()),
                error_type: "rate_limited".to_string(),
                retry_after: Some(60),
            }),
        };

        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["data"]["retry_after"], 60);
    }

    #[test]
    fn test_optional_fields_omitted() {
        let error = JsonRpcError {
            code: -32603,
            message: "Internal error".to_string(),
            data: Some(ErrorData {
                correlation_id: "test-id".to_string(),
                gate: None,
                tool: None,
                details: None,
                error_type: "internal_error".to_string(),
                retry_after: None,
            }),
        };

        let json_str = serde_json::to_string(&error).unwrap();

        // Optional None fields should be omitted
        assert!(!json_str.contains("\"details\""));
        assert!(!json_str.contains("\"retry_after\""));
        assert!(!json_str.contains("\"gate\""));
        assert!(!json_str.contains("\"tool\""));
    }

    #[test]
    fn test_gate_error_serialization() {
        let error = JsonRpcError {
            code: -32015,
            message: "Tool 'admin_delete' is not available".to_string(),
            data: Some(ErrorData {
                correlation_id: "test-id".to_string(),
                gate: Some("visibility".to_string()),
                tool: Some("admin_delete".to_string()),
                details: None,
                error_type: "tool_not_exposed".to_string(),
                retry_after: None,
            }),
        };

        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["data"]["gate"], "visibility");
        assert_eq!(json["data"]["tool"], "admin_delete");
    }
}
