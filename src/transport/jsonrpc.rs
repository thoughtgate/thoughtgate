//! JSON-RPC 2.0 types and parsing.
//!
//! Implements: REQ-CORE-003/F-001 (JSON-RPC Parsing)
//!
//! # JSON-RPC 2.0 Compliance
//!
//! - Requests have `id`, `method`, and optional `params`
//! - Notifications are requests without `id`
//! - Batches are arrays of requests/notifications
//! - `id` type (string or integer) MUST be preserved in responses
//!
//! # Security Note
//!
//! This module parses untrusted input. All parsing is done with size limits
//! enforced at the HTTP layer (see server.rs).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::time::Instant;
use uuid::Uuid;

use crate::error::ThoughtGateError;

/// JSON-RPC 2.0 request ID.
///
/// The spec allows string or integer IDs. We preserve the exact type
/// to ensure responses use the same type as requests.
///
/// # Variants
///
/// - `Number(i64)` - Integer ID (e.g., `"id": 1`)
/// - `String(String)` - String ID (e.g., `"id": "abc-123"`)
/// - `Null` - Explicit null ID (e.g., `"id": null`)
///
/// # Important
///
/// Never coerce between types! If the client sends `"id": 1`, respond with
/// `"id": 1`, not `"id": "1"`.
///
/// # Note on Null IDs
///
/// Per JSON-RPC 2.0 spec, `"id": null` is valid (though unusual) and should
/// be echoed back in responses. This is distinct from a missing `id` field,
/// which indicates a notification that requires no response.
///
/// Implements: REQ-CORE-003/F-001.4 (Preserve ID type)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JsonRpcId {
    /// Integer ID (e.g., `"id": 1`)
    Number(i64),
    /// String ID (e.g., `"id": "abc-123"`)
    String(String),
    /// Explicit null ID (e.g., `"id": null`) - valid but unusual
    Null,
}

impl Serialize for JsonRpcId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            JsonRpcId::Number(n) => serializer.serialize_i64(*n),
            JsonRpcId::String(s) => serializer.serialize_str(s),
            JsonRpcId::Null => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for JsonRpcId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(JsonRpcId::Number(i))
                } else {
                    Err(serde::de::Error::custom(
                        "JSON-RPC ID must be integer, not float",
                    ))
                }
            }
            Value::String(s) => Ok(JsonRpcId::String(s)),
            Value::Null => Ok(JsonRpcId::Null),
            _ => Err(serde::de::Error::custom(
                "JSON-RPC ID must be string, integer, or null",
            )),
        }
    }
}

/// Wrapper to distinguish between missing field and explicit null.
/// - `Absent` - field was not present in JSON
/// - `Null` - field was present with value `null`
/// - `Present(T)` - field was present with a non-null value
#[derive(Debug, Clone, Default)]
enum MaybeNull<T> {
    #[default]
    Absent,
    Null,
    Present(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for MaybeNull<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Deserialize to serde_json::Value first to check for null
        let value = Value::deserialize(deserializer)?;
        if value.is_null() {
            Ok(MaybeNull::Null)
        } else {
            // Try to deserialize the value as T
            T::deserialize(value)
                .map(MaybeNull::Present)
                .map_err(serde::de::Error::custom)
        }
    }
}

/// Deserializer that converts MaybeNull<JsonRpcId> to Option<JsonRpcId>
/// where explicit null becomes Some(JsonRpcId::Null)
fn deserialize_optional_id<'de, D>(deserializer: D) -> Result<Option<JsonRpcId>, D::Error>
where
    D: Deserializer<'de>,
{
    match MaybeNull::deserialize(deserializer)? {
        MaybeNull::Absent => Ok(None),
        MaybeNull::Null => Ok(Some(JsonRpcId::Null)),
        MaybeNull::Present(id) => Ok(Some(id)),
    }
}

/// Raw JSON-RPC 2.0 request as received from the client.
///
/// This struct handles the wire format before validation. All fields are
/// optional to allow for proper error reporting on malformed requests.
#[derive(Debug, Clone, Deserialize)]
struct RawJsonRpcRequest {
    /// Must be "2.0"
    jsonrpc: Option<String>,
    /// Request ID (absent for notifications, Some(Null) for explicit null)
    #[serde(default, deserialize_with = "deserialize_optional_id")]
    id: Option<JsonRpcId>,
    /// Method name
    method: Option<String>,
    /// Method parameters
    params: Option<Value>,
}

/// JSON-RPC 2.0 version constant.
const JSONRPC_VERSION: &str = "2.0";

/// Validated JSON-RPC 2.0 request.
///
/// After parsing, requests are validated and converted to this type.
/// This struct is used for serialization when forwarding to upstream.
///
/// Implements: REQ-CORE-003/§6.1 (Input: Inbound MCP Request)
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest {
    /// Always "2.0"
    pub jsonrpc: String,
    /// Request ID (None for notifications)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<JsonRpcId>,
    /// Method name
    pub method: String,
    /// Method parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// Returns true if this is a notification (no ID).
    ///
    /// Implements: REQ-CORE-003/F-001.3
    #[inline]
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// JSON-RPC 2.0 response.
///
/// Implements: REQ-CORE-003/§6.2 (Output: MCP Response)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Always "2.0"
    pub jsonrpc: String,
    /// Request ID (must match request, None for notifications)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<JsonRpcId>,
    /// Result (mutually exclusive with error)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error (mutually exclusive with result)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::error::jsonrpc::JsonRpcError>,
}

impl JsonRpcResponse {
    /// Create a success response.
    ///
    /// # Arguments
    ///
    /// * `id` - The request ID to echo back
    /// * `result` - The result value
    pub fn success(id: Option<JsonRpcId>, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response.
    ///
    /// # Arguments
    ///
    /// * `id` - The request ID to echo back (may be None if parsing failed)
    /// * `error` - The JSON-RPC error object
    pub fn error(id: Option<JsonRpcId>, error: crate::error::jsonrpc::JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// SEP-1686 task metadata extracted from request params.
///
/// When a request contains `params.task`, it indicates the client supports
/// task-based async execution per SEP-1686.
///
/// Implements: REQ-CORE-003/F-003 (SEP-1686 Detection)
#[derive(Debug, Clone)]
pub struct TaskMetadata {
    /// Task TTL in milliseconds (from `params.task.ttl`)
    pub ttl: Option<std::time::Duration>,
}

/// Parsed and validated MCP request with internal tracking.
///
/// This is the internal representation used after parsing. It includes
/// metadata for tracing and correlation.
///
/// Implements: REQ-CORE-003/§6.3 (Internal: Parsed Request Structure)
#[derive(Debug, Clone)]
pub struct McpRequest {
    /// Original JSON-RPC ID (None for notifications)
    pub id: Option<JsonRpcId>,
    /// Method name
    pub method: String,
    /// Method parameters
    pub params: Option<Value>,
    /// SEP-1686 task metadata (if present)
    pub task_metadata: Option<TaskMetadata>,
    /// Timestamp when request was received
    pub received_at: Instant,
    /// Unique correlation ID for tracing
    pub correlation_id: Uuid,
}

impl McpRequest {
    /// Returns true if this is a notification (no ID).
    ///
    /// Notifications do not receive responses per JSON-RPC 2.0.
    #[inline]
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Returns true if this request has SEP-1686 task metadata.
    ///
    /// Task-augmented requests may be handled differently by the
    /// governance layer.
    #[inline]
    pub fn is_task_augmented(&self) -> bool {
        self.task_metadata.is_some()
    }

    /// Convert to a JsonRpcRequest for forwarding to upstream.
    pub fn to_jsonrpc_request(&self) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: self.id.clone(),
            method: self.method.clone(),
            params: self.params.clone(),
        }
    }
}

/// Parse result that can be a single request, batch, or parse error.
#[derive(Debug)]
pub enum ParsedRequests {
    /// Single request
    Single(McpRequest),
    /// Batch of requests
    Batch(Vec<McpRequest>),
}

/// Parse JSON bytes into JSON-RPC 2.0 request(s).
///
/// Implements: REQ-CORE-003/F-001 (JSON-RPC Parsing)
///
/// # Arguments
///
/// * `bytes` - Raw JSON bytes from the HTTP request body
///
/// # Returns
///
/// * `Ok(ParsedRequests)` - Successfully parsed request(s)
/// * `Err(ThoughtGateError::ParseError)` - Malformed JSON (-32700)
/// * `Err(ThoughtGateError::InvalidRequest)` - Invalid JSON-RPC structure (-32600)
///
/// # Handles
///
/// - F-001.1: Single request objects
/// - F-001.2: Batch requests (JSON arrays)
/// - F-001.3: Notifications (requests without `id`)
/// - F-001.5: Malformed JSON (-32700)
/// - F-001.6: Invalid JSON-RPC structure (-32600)
/// - F-001.7: Generate correlation_id (UUID v4)
///
/// # Edge Cases
///
/// - EC-MCP-002: Malformed JSON returns ParseError
/// - EC-MCP-003: Missing jsonrpc field returns InvalidRequest
/// - EC-MCP-006: Empty batch returns InvalidRequest
pub fn parse_jsonrpc(bytes: &[u8]) -> Result<ParsedRequests, ThoughtGateError> {
    // F-001.5: Parse JSON
    let value: Value = serde_json::from_slice(bytes).map_err(|e| ThoughtGateError::ParseError {
        details: format!("Invalid JSON: {}", e),
    })?;

    match value {
        Value::Array(arr) => {
            // F-001.2: Batch request
            if arr.is_empty() {
                // EC-MCP-006: Empty batch
                return Err(ThoughtGateError::InvalidRequest {
                    details: "Empty batch is not allowed".to_string(),
                });
            }
            let mut requests = Vec::with_capacity(arr.len());
            for item in arr {
                requests.push(parse_single_request(item)?);
            }
            Ok(ParsedRequests::Batch(requests))
        }
        Value::Object(_) => {
            // F-001.1: Single request
            Ok(ParsedRequests::Single(parse_single_request(value)?))
        }
        _ => {
            // Invalid structure - neither object nor array
            Err(ThoughtGateError::InvalidRequest {
                details: "Request must be an object or array".to_string(),
            })
        }
    }
}

/// Parse a single JSON-RPC 2.0 request from a JSON value.
///
/// Implements: REQ-CORE-003/F-001 (Parse JSON-RPC 2.0)
///
/// # Arguments
///
/// * `value` - A JSON value that should be a request object
///
/// # Returns
///
/// * `Ok(McpRequest)` - Successfully parsed and validated request
/// * `Err(ThoughtGateError::InvalidRequest)` - Invalid JSON-RPC structure
fn parse_single_request(value: Value) -> Result<McpRequest, ThoughtGateError> {
    let raw: RawJsonRpcRequest =
        serde_json::from_value(value).map_err(|e| ThoughtGateError::InvalidRequest {
            details: format!("Invalid JSON-RPC structure: {}", e),
        })?;

    // F-001.6: Validate JSON-RPC version
    match raw.jsonrpc.as_deref() {
        Some("2.0") => {}
        Some(v) => {
            return Err(ThoughtGateError::InvalidRequest {
                details: format!("Invalid jsonrpc version: expected \"2.0\", got \"{}\"", v),
            });
        }
        None => {
            return Err(ThoughtGateError::InvalidRequest {
                details: "Missing required field: jsonrpc".to_string(),
            });
        }
    }

    // Validate method is present
    let method = raw.method.ok_or_else(|| ThoughtGateError::InvalidRequest {
        details: "Missing required field: method".to_string(),
    })?;

    // F-001.7: Generate correlation ID
    let correlation_id = Uuid::new_v4();

    // F-003: Extract SEP-1686 task metadata
    let task_metadata = extract_task_metadata(&raw.params);

    Ok(McpRequest {
        id: raw.id,
        method,
        params: raw.params,
        task_metadata,
        received_at: Instant::now(),
        correlation_id,
    })
}

/// Maximum allowed TTL: 24 hours in milliseconds.
/// This prevents unreasonably large values that could cause issues downstream.
const MAX_TTL_MS: u64 = 24 * 60 * 60 * 1000; // 24 hours

/// Extract SEP-1686 task metadata from params.
///
/// Implements: REQ-CORE-003/F-003 (SEP-1686 Detection)
///
/// # Arguments
///
/// * `params` - The optional params object from the request
///
/// # Returns
///
/// * `Some(TaskMetadata)` if `params.task` exists
/// * `None` otherwise
///
/// # TTL Validation
///
/// TTL values are clamped to MAX_TTL_MS (24 hours) to prevent issues with
/// extremely large values.
fn extract_task_metadata(params: &Option<Value>) -> Option<TaskMetadata> {
    let params = params.as_ref()?;
    let task = params.get("task")?;

    let ttl = task
        .get("ttl")
        .and_then(|v| v.as_u64())
        .map(|ms| std::time::Duration::from_millis(ms.min(MAX_TTL_MS)));

    Some(TaskMetadata { ttl })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies: EC-MCP-001 (Valid JSON-RPC request)
    #[test]
    fn test_parse_valid_single_request() {
        let json = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"test"}}"#;
        let result = parse_jsonrpc(json);
        assert!(result.is_ok());

        if let ParsedRequests::Single(req) = result.expect("should parse") {
            assert_eq!(req.id, Some(JsonRpcId::Number(1)));
            assert_eq!(req.method, "tools/call");
            assert!(!req.is_notification());
            assert!(req.params.is_some());
        } else {
            panic!("Expected single request");
        }
    }

    /// Verifies: EC-MCP-004 (Notification - no id)
    #[test]
    fn test_parse_notification() {
        let json = br#"{"jsonrpc":"2.0","method":"notifications/progress"}"#;
        let result = parse_jsonrpc(json);
        assert!(result.is_ok());

        if let ParsedRequests::Single(req) = result.expect("should parse") {
            assert!(req.is_notification());
            assert_eq!(req.id, None);
            assert_eq!(req.method, "notifications/progress");
        } else {
            panic!("Expected single request");
        }
    }

    /// Verifies: EC-MCP-005 (Batch request)
    #[test]
    fn test_parse_batch() {
        let json =
            br#"[{"jsonrpc":"2.0","id":1,"method":"a"},{"jsonrpc":"2.0","id":2,"method":"b"}]"#;
        let result = parse_jsonrpc(json);
        assert!(result.is_ok());

        if let ParsedRequests::Batch(reqs) = result.expect("should parse") {
            assert_eq!(reqs.len(), 2);
            assert_eq!(reqs[0].method, "a");
            assert_eq!(reqs[1].method, "b");
        } else {
            panic!("Expected batch");
        }
    }

    /// Verifies: EC-MCP-006 (Empty batch)
    #[test]
    fn test_parse_empty_batch_error() {
        let json = br#"[]"#;
        let result = parse_jsonrpc(json);
        assert!(matches!(
            result,
            Err(ThoughtGateError::InvalidRequest { .. })
        ));

        if let Err(ThoughtGateError::InvalidRequest { details }) = result {
            assert!(details.contains("Empty batch"));
        }
    }

    /// Verifies: EC-MCP-002 (Malformed JSON)
    #[test]
    fn test_parse_malformed_json_error() {
        let json = br#"{"invalid json"#;
        let result = parse_jsonrpc(json);
        assert!(matches!(result, Err(ThoughtGateError::ParseError { .. })));
    }

    /// Verifies: EC-MCP-003 (Missing jsonrpc field)
    #[test]
    fn test_parse_missing_jsonrpc_field() {
        let json = br#"{"id":1,"method":"test"}"#;
        let result = parse_jsonrpc(json);
        assert!(matches!(
            result,
            Err(ThoughtGateError::InvalidRequest { .. })
        ));

        if let Err(ThoughtGateError::InvalidRequest { details }) = result {
            assert!(details.contains("jsonrpc"));
        }
    }

    /// Verifies: EC-MCP-013 (Integer ID preserved)
    #[test]
    fn test_preserve_integer_id() {
        let json = br#"{"jsonrpc":"2.0","id":42,"method":"test"}"#;
        let result = parse_jsonrpc(json);

        if let Ok(ParsedRequests::Single(req)) = result {
            assert_eq!(req.id, Some(JsonRpcId::Number(42)));

            // Verify serialization preserves type
            let jsonrpc_req = req.to_jsonrpc_request();
            let serialized = serde_json::to_string(&jsonrpc_req).expect("should serialize");
            assert!(serialized.contains("\"id\":42"));
            assert!(!serialized.contains("\"id\":\"42\""));
        } else {
            panic!("Expected single request with integer ID");
        }
    }

    /// Verifies: EC-MCP-014 (String ID preserved)
    #[test]
    fn test_preserve_string_id() {
        let json = br#"{"jsonrpc":"2.0","id":"abc-123","method":"test"}"#;
        let result = parse_jsonrpc(json);

        if let Ok(ParsedRequests::Single(req)) = result {
            assert_eq!(req.id, Some(JsonRpcId::String("abc-123".to_string())));

            // Verify serialization preserves type
            let jsonrpc_req = req.to_jsonrpc_request();
            let serialized = serde_json::to_string(&jsonrpc_req).expect("should serialize");
            assert!(serialized.contains("\"id\":\"abc-123\""));
        } else {
            panic!("Expected single request with string ID");
        }
    }

    /// Verifies: EC-MCP-008 (SEP-1686 task metadata)
    #[test]
    fn test_extract_task_metadata() {
        let json = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"test","task":{"ttl":600000}}}"#;
        let result = parse_jsonrpc(json);

        if let Ok(ParsedRequests::Single(req)) = result {
            assert!(req.is_task_augmented());
            let metadata = req.task_metadata.expect("should have metadata");
            assert_eq!(metadata.ttl, Some(std::time::Duration::from_millis(600000)));
        } else {
            panic!("Expected task-augmented request");
        }
    }

    #[test]
    fn test_no_task_metadata() {
        let json = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"test"}}"#;
        let result = parse_jsonrpc(json);

        if let Ok(ParsedRequests::Single(req)) = result {
            assert!(!req.is_task_augmented());
            assert!(req.task_metadata.is_none());
        } else {
            panic!("Expected request without task metadata");
        }
    }

    #[test]
    fn test_invalid_jsonrpc_version() {
        let json = br#"{"jsonrpc":"1.0","id":1,"method":"test"}"#;
        let result = parse_jsonrpc(json);
        assert!(matches!(
            result,
            Err(ThoughtGateError::InvalidRequest { .. })
        ));

        if let Err(ThoughtGateError::InvalidRequest { details }) = result {
            assert!(details.contains("2.0"));
        }
    }

    #[test]
    fn test_missing_method() {
        let json = br#"{"jsonrpc":"2.0","id":1}"#;
        let result = parse_jsonrpc(json);
        assert!(matches!(
            result,
            Err(ThoughtGateError::InvalidRequest { .. })
        ));

        if let Err(ThoughtGateError::InvalidRequest { details }) = result {
            assert!(details.contains("method"));
        }
    }

    #[test]
    fn test_null_id() {
        // Per JSON-RPC 2.0 spec, `"id": null` is a valid (though unusual) request
        // that should have its null ID echoed back in the response.
        // This is distinct from a missing `id` field (notification).
        let json = br#"{"jsonrpc":"2.0","id":null,"method":"test"}"#;
        let result = parse_jsonrpc(json);

        if let Ok(ParsedRequests::Single(req)) = result {
            // Explicit null should be preserved as Some(JsonRpcId::Null)
            assert_eq!(req.id, Some(JsonRpcId::Null));
            // This is NOT a notification - it expects a response with id: null
            assert!(!req.is_notification());
        } else {
            panic!("Expected request with null ID");
        }
    }

    #[test]
    fn test_missing_id_is_notification() {
        // Missing id field = notification (no response expected)
        let json = br#"{"jsonrpc":"2.0","method":"test"}"#;
        let result = parse_jsonrpc(json);

        if let Ok(ParsedRequests::Single(req)) = result {
            assert_eq!(req.id, None);
            assert!(req.is_notification());
        } else {
            panic!("Expected notification");
        }
    }

    #[test]
    fn test_correlation_id_generated() {
        let json = br#"{"jsonrpc":"2.0","id":1,"method":"test"}"#;
        let result = parse_jsonrpc(json);

        if let Ok(ParsedRequests::Single(req)) = result {
            // Correlation ID should be a valid UUID
            assert!(!req.correlation_id.is_nil());
        } else {
            panic!("Expected single request");
        }
    }

    #[test]
    fn test_batch_with_notifications() {
        let json = br#"[
            {"jsonrpc":"2.0","id":1,"method":"a"},
            {"jsonrpc":"2.0","method":"notify"},
            {"jsonrpc":"2.0","id":2,"method":"b"}
        ]"#;
        let result = parse_jsonrpc(json);

        if let Ok(ParsedRequests::Batch(reqs)) = result {
            assert_eq!(reqs.len(), 3);
            assert!(!reqs[0].is_notification());
            assert!(reqs[1].is_notification());
            assert!(!reqs[2].is_notification());
        } else {
            panic!("Expected batch");
        }
    }

    #[test]
    fn test_json_array_not_object_in_batch() {
        let json = br#"[1, 2, 3]"#;
        let result = parse_jsonrpc(json);
        assert!(matches!(
            result,
            Err(ThoughtGateError::InvalidRequest { .. })
        ));
    }

    #[test]
    fn test_float_id_rejected() {
        let json = br#"{"jsonrpc":"2.0","id":1.5,"method":"test"}"#;
        let result = parse_jsonrpc(json);
        assert!(matches!(
            result,
            Err(ThoughtGateError::InvalidRequest { .. })
        ));
    }

    #[test]
    fn test_jsonrpc_response_success() {
        let response = JsonRpcResponse::success(
            Some(JsonRpcId::Number(1)),
            serde_json::json!({"result": "ok"}),
        );

        let serialized = serde_json::to_string(&response).expect("should serialize");
        assert!(serialized.contains("\"jsonrpc\":\"2.0\""));
        assert!(serialized.contains("\"id\":1"));
        assert!(serialized.contains("\"result\""));
        assert!(!serialized.contains("\"error\""));
    }

    #[test]
    fn test_jsonrpc_response_error() {
        let error = crate::error::jsonrpc::JsonRpcError {
            code: -32600,
            message: "Invalid Request".to_string(),
            data: None,
        };
        let response = JsonRpcResponse::error(Some(JsonRpcId::Number(1)), error);

        let serialized = serde_json::to_string(&response).expect("should serialize");
        assert!(serialized.contains("\"error\""));
        assert!(serialized.contains("-32600"));
        assert!(!serialized.contains("\"result\""));
    }
}
