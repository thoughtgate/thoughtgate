//! Transport-agnostic JSON-RPC 2.0 message classification.
//!
//! Implements: REQ-CORE-008 §6.5.1 (Transport-Agnostic Classification)
//!
//! This module provides lightweight classification of JSON-RPC messages from
//! a pre-parsed `serde_json::Value`. It is used by both the HTTP transport
//! (REQ-CORE-003) and the stdio shim (REQ-CORE-008) to determine message kind
//! without duplicating classification logic.
//!
//! The existing `transport::jsonrpc` module handles full HTTP-specific parsing
//! from raw bytes. This module operates one level higher on already-parsed JSON.

pub use crate::transport::jsonrpc::JsonRpcId;

/// Transport-agnostic JSON-RPC 2.0 message classification.
///
/// Determined by presence/absence of `id` and `method` fields:
/// - Request: has both `id` and `method`
/// - Response: has `id` but no `method`
/// - Notification: has `method` but no `id`
///
/// Implements: REQ-CORE-008 §6.5.1
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonRpcMessageKind {
    /// Has both `id` and `method` — a request expecting a response.
    Request { id: JsonRpcId, method: String },
    /// Has `id` but no `method` — a response to a previous request.
    Response { id: JsonRpcId },
    /// Has `method` but no `id` — a fire-and-forget notification.
    Notification { method: String },
}

/// Classify a parsed JSON-RPC value without taking ownership.
///
/// Validates the `"jsonrpc": "2.0"` version field, then classifies based on
/// the presence of `id` and `method` fields.
///
/// # Arguments
///
/// * `value` - A reference to a parsed JSON value (typically from an NDJSON line
///   or HTTP body).
///
/// # Errors
///
/// Returns `JsonRpcClassifyError` if:
/// - The `jsonrpc` field is missing or not `"2.0"` (`InvalidVersion`)
/// - The `id` field is present but not a valid JSON-RPC ID (`InvalidId`)
/// - Neither `id` nor `method` is present (`Unclassifiable`)
///
/// Implements: REQ-CORE-008 §6.5.1
pub fn classify_jsonrpc(
    value: &serde_json::Value,
) -> Result<JsonRpcMessageKind, JsonRpcClassifyError> {
    // Validate "jsonrpc": "2.0" presence
    let version = value.get("jsonrpc").and_then(|v| v.as_str());
    if version != Some("2.0") {
        return Err(JsonRpcClassifyError::InvalidVersion);
    }

    let id = value
        .get("id")
        .map(parse_id)
        .transpose()
        .map_err(|_| JsonRpcClassifyError::InvalidId)?;
    let method = value
        .get("method")
        .and_then(|v| v.as_str())
        .map(String::from);

    match (id, method) {
        (Some(id), Some(method)) => Ok(JsonRpcMessageKind::Request { id, method }),
        (Some(id), None) => Ok(JsonRpcMessageKind::Response { id }),
        (None, Some(method)) => Ok(JsonRpcMessageKind::Notification { method }),
        (None, None) => Err(JsonRpcClassifyError::Unclassifiable),
    }
}

/// Parse a JSON value into a `JsonRpcId`.
///
/// Accepts string, integer, or null. Rejects floats, booleans, arrays, objects.
fn parse_id(value: &serde_json::Value) -> Result<JsonRpcId, ()> {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(JsonRpcId::Number(i))
            } else {
                Err(()) // float IDs are invalid
            }
        }
        serde_json::Value::String(s) => Ok(JsonRpcId::String(s.clone())),
        serde_json::Value::Null => Ok(JsonRpcId::Null),
        _ => Err(()), // arrays, objects, booleans are invalid
    }
}

/// Errors that can occur during JSON-RPC message classification.
///
/// Implements: REQ-CORE-008 §6.5.1
#[derive(Debug, thiserror::Error)]
pub enum JsonRpcClassifyError {
    /// The `jsonrpc` field is missing or not `"2.0"`.
    #[error("missing or invalid jsonrpc version field")]
    InvalidVersion,
    /// The `id` field is present but not a valid JSON-RPC ID (string, integer, or null).
    #[error("invalid id field")]
    InvalidId,
    /// The message has neither `id` nor `method` — cannot be classified.
    #[error("message has neither id nor method")]
    Unclassifiable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_classify_request() {
        let val = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {}});
        let kind = classify_jsonrpc(&val).unwrap();
        assert_eq!(
            kind,
            JsonRpcMessageKind::Request {
                id: JsonRpcId::Number(1),
                method: "tools/call".to_string()
            }
        );
    }

    #[test]
    fn test_classify_response() {
        let val = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
        let kind = classify_jsonrpc(&val).unwrap();
        assert_eq!(
            kind,
            JsonRpcMessageKind::Response {
                id: JsonRpcId::Number(1)
            }
        );
    }

    #[test]
    fn test_classify_notification() {
        let val = json!({"jsonrpc": "2.0", "method": "initialized"});
        let kind = classify_jsonrpc(&val).unwrap();
        assert_eq!(
            kind,
            JsonRpcMessageKind::Notification {
                method: "initialized".to_string()
            }
        );
    }

    #[test]
    fn test_classify_missing_version() {
        let val = json!({"id": 1, "method": "x"});
        let err = classify_jsonrpc(&val).unwrap_err();
        assert!(matches!(err, JsonRpcClassifyError::InvalidVersion));
    }

    #[test]
    fn test_classify_wrong_version() {
        let val = json!({"jsonrpc": "1.0", "id": 1, "method": "x"});
        let err = classify_jsonrpc(&val).unwrap_err();
        assert!(matches!(err, JsonRpcClassifyError::InvalidVersion));
    }

    #[test]
    fn test_classify_unclassifiable() {
        let val = json!({"jsonrpc": "2.0"});
        let err = classify_jsonrpc(&val).unwrap_err();
        assert!(matches!(err, JsonRpcClassifyError::Unclassifiable));
    }

    #[test]
    fn test_classify_integer_id() {
        let val = json!({"jsonrpc": "2.0", "id": 42, "method": "ping"});
        let kind = classify_jsonrpc(&val).unwrap();
        assert_eq!(
            kind,
            JsonRpcMessageKind::Request {
                id: JsonRpcId::Number(42),
                method: "ping".to_string()
            }
        );
    }

    #[test]
    fn test_classify_string_id() {
        let val = json!({"jsonrpc": "2.0", "id": "abc-123", "method": "ping"});
        let kind = classify_jsonrpc(&val).unwrap();
        assert_eq!(
            kind,
            JsonRpcMessageKind::Request {
                id: JsonRpcId::String("abc-123".to_string()),
                method: "ping".to_string()
            }
        );
    }

    #[test]
    fn test_classify_null_id() {
        // null id with no method = response (unusual but valid per JSON-RPC 2.0)
        let val = json!({"jsonrpc": "2.0", "id": null, "result": "ok"});
        let kind = classify_jsonrpc(&val).unwrap();
        assert_eq!(
            kind,
            JsonRpcMessageKind::Response {
                id: JsonRpcId::Null
            }
        );
    }

    #[test]
    fn test_classify_error_response() {
        let val = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "error": {"code": -32600, "message": "Invalid Request"}
        });
        let kind = classify_jsonrpc(&val).unwrap();
        assert_eq!(
            kind,
            JsonRpcMessageKind::Response {
                id: JsonRpcId::Number(5)
            }
        );
    }

    #[test]
    fn test_classify_invalid_id_type() {
        // boolean id is invalid per JSON-RPC 2.0
        let val = json!({"jsonrpc": "2.0", "id": true, "method": "x"});
        let err = classify_jsonrpc(&val).unwrap_err();
        assert!(matches!(err, JsonRpcClassifyError::InvalidId));
    }
}
