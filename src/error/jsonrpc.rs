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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorData {
    /// Unique identifier for tracing this error in logs
    pub correlation_id: String,

    /// Machine-readable error type name
    pub error_type: String,

    /// Type-specific error details (sanitized)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,

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
            message: "Policy denied: Tool 'delete_user' is not permitted".to_string(),
            data: Some(ErrorData {
                correlation_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
                error_type: "policy_denied".to_string(),
                details: Some(serde_json::json!({
                    "tool": "delete_user"
                })),
                retry_after: None,
            }),
        };

        let json = serde_json::to_value(&error).unwrap();

        assert_eq!(json["code"], -32003);
        assert_eq!(json["message"], "Policy denied: Tool 'delete_user' is not permitted");
        assert_eq!(json["data"]["correlation_id"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(json["data"]["error_type"], "policy_denied");
        assert_eq!(json["data"]["details"]["tool"], "delete_user");
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
                error_type: "rate_limited".to_string(),
                details: None,
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
                error_type: "internal_error".to_string(),
                details: None,
                retry_after: None,
            }),
        };

        let json_str = serde_json::to_string(&error).unwrap();

        // Optional None fields should be omitted
        assert!(!json_str.contains("\"details\""));
        assert!(!json_str.contains("\"retry_after\""));
    }
}
