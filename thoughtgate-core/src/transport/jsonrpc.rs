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
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use uuid::Uuid;

use crate::error::ThoughtGateError;

// ============================================================================
// Fast Correlation ID Generator
// ============================================================================

/// Startup prefix derived from a single Uuid::new_v4() call.
/// The upper 64 bits provide process-level uniqueness.
static CORRELATION_PREFIX: LazyLock<u64> = LazyLock::new(|| {
    let seed = Uuid::new_v4().as_u128();
    (seed >> 64) as u64
});

/// Monotonically increasing counter for the lower 64 bits.
static CORRELATION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a fast correlation ID using a counter-based approach.
///
/// Combines a process-unique prefix (from a single Uuid::new_v4() at startup)
/// with a monotonically increasing counter. This avoids the CSPRNG overhead
/// of Uuid::new_v4() on every request while still producing unique 128-bit IDs.
///
/// The result has correct v4 version and RFC 4122 variant bits set.
pub fn fast_correlation_id() -> Uuid {
    let prefix = *CORRELATION_PREFIX;
    let counter = CORRELATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut combined = ((prefix as u128) << 64) | (counter as u128);
    // Set version 4 (bits 48-51 of the 128-bit value)
    combined = (combined & !(0xF_u128 << 76)) | (0x4_u128 << 76);
    // Set variant 1 - RFC 4122 (bits 64-65)
    combined = (combined & !(0x3_u128 << 62)) | (0x2_u128 << 62);
    Uuid::from_u128(combined)
}

// ============================================================================
// MCP Tasks Protocol Types (Protocol Revision 2025-11-25)
// ============================================================================

/// MCP tool definition as returned by `tools/list`.
///
/// Implements: MCP Tasks Specification (Protocol Revision 2025-11-25)
///
/// This struct represents a tool in the MCP catalog. The `execution` field
/// contains task-related metadata including `taskSupport`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    /// The tool name (unique identifier)
    pub name: String,

    /// Human-readable description of the tool
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// JSON Schema for the tool's input parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,

    /// Execution-related metadata (MCP Tasks Protocol)
    ///
    /// Contains `taskSupport` annotation per MCP spec:
    /// - `forbidden` → client MUST NOT send `params.task`
    /// - `optional` → client MAY send `params.task`
    /// - `required` → client MUST send `params.task`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ToolExecution>,

    /// Additional properties from upstream (preserved as-is)
    #[serde(flatten)]
    pub extra: Option<Value>,
}

/// Execution metadata for a tool (MCP Tasks Protocol).
///
/// Implements: MCP Tasks Specification - Tool-Level Negotiation
///
/// This struct is nested under `execution` in the tool definition
/// to allow for future execution-related extensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecution {
    /// Whether this tool requires task-based async execution.
    ///
    /// Per MCP spec:
    /// - `forbidden` (default if not present) → client MUST NOT attempt task mode
    /// - `optional` → client MAY invoke as task or normal request
    /// - `required` → client MUST invoke as task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_support: Option<TaskSupport>,
}

/// MCP task support mode for tools.
///
/// Implements: MCP Tasks Specification - Tool-Level Negotiation
///
/// This enum indicates whether a tool supports or requires async task execution.
/// Clients use this to determine if they need to include `params.task` in requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskSupport {
    /// Tool cannot be called with task metadata; `params.task` MUST NOT be sent.
    /// Servers SHOULD return -32601 (Method not found) if client attempts.
    Forbidden,
    /// Tool optionally supports task mode; `params.task` MAY be sent
    Optional,
    /// Tool requires task mode; `params.task` MUST be sent.
    /// Servers MUST return -32601 (Method not found) if client does not.
    Required,
}

/// MCP resource definition as returned by `resources/list`.
///
/// Implements: MCP Protocol - Resource Listing
///
/// Resources represent data that can be read by agents. The URI serves
/// as both identifier and access path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDefinition {
    /// The resource URI (unique identifier and access path)
    pub uri: String,

    /// Human-readable name of the resource
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Human-readable description of the resource
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// MIME type of the resource content
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,

    /// Additional properties from upstream (preserved as-is)
    #[serde(flatten)]
    pub extra: Option<Value>,
}

/// MCP prompt definition as returned by `prompts/list`.
///
/// Implements: MCP Protocol - Prompt Listing
///
/// Prompts are templates that can be used to generate messages.
/// They may accept arguments that customize their output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptDefinition {
    /// The prompt name (unique identifier)
    pub name: String,

    /// Human-readable description of the prompt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Arguments the prompt accepts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<Value>>,

    /// Additional properties from upstream (preserved as-is)
    #[serde(flatten)]
    pub extra: Option<Value>,
}

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
    pub jsonrpc: Cow<'static, str>,
    /// Request ID (None for notifications)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<JsonRpcId>,
    /// Method name
    pub method: String,
    /// Method parameters (Arc-wrapped for O(1) clone)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Arc<Value>>,
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
///
/// # ID Serialization
///
/// Per JSON-RPC 2.0 spec, the `id` field is REQUIRED in responses and MUST be:
/// - The same as the request's `id` for success/error responses
/// - `null` if the request `id` could not be determined (e.g., parse error)
///
/// The `id` field always serializes: `None` becomes `"id": null` in JSON.
/// This differs from `JsonRpcRequest` where `None` means "notification" and
/// the field is omitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Always "2.0"
    pub jsonrpc: Cow<'static, str>,
    /// Request ID - always serialized (None becomes null per JSON-RPC 2.0 spec)
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
    /// Implements: REQ-CORE-003/§6.2 (Output: MCP Response)
    ///
    /// # Arguments
    ///
    /// * `id` - The request ID to echo back
    /// * `result` - The result value
    pub fn success(id: Option<JsonRpcId>, result: Value) -> Self {
        Self {
            jsonrpc: Cow::Borrowed(JSONRPC_VERSION),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response.
    ///
    /// Implements: REQ-CORE-003/§6.2 (Output: MCP Response)
    ///
    /// # Arguments
    ///
    /// * `id` - The request ID to echo back. Pass `None` if the request ID
    ///   could not be determined (e.g., parse error) - this serializes as
    ///   `"id": null` per JSON-RPC 2.0 spec.
    /// * `error` - The JSON-RPC error object
    pub fn error(id: Option<JsonRpcId>, error: crate::error::jsonrpc::JsonRpcError) -> Self {
        Self {
            jsonrpc: Cow::Borrowed(JSONRPC_VERSION),
            id,
            result: None,
            error: Some(error),
        }
    }

    /// Create a task-created response for SEP-1686 async workflows.
    ///
    /// Implements: REQ-GOV-002/F-002 (Task Response)
    ///
    /// # Arguments
    ///
    /// * `id` - The request ID to echo back
    /// * `task_id` - The created task ID
    /// * `status` - Initial task status
    /// * `poll_interval` - Recommended poll interval
    ///
    /// # SEP-1686 Compliance
    ///
    /// Returns camelCase fields per SEP-1686 specification:
    /// - `taskId` - Task identifier
    /// - `status` - Current status ("working", "input_required", etc.)
    /// - `pollInterval` - Recommended poll interval in milliseconds
    pub fn task_created(
        id: Option<JsonRpcId>,
        task_id: String,
        status: String,
        poll_interval: std::time::Duration,
    ) -> Self {
        Self {
            jsonrpc: Cow::Borrowed(JSONRPC_VERSION),
            id,
            result: Some(serde_json::json!({
                "taskId": task_id,
                "status": status,
                "pollInterval": poll_interval.as_millis(),
            })),
            error: None,
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
#[derive(Clone)]
pub struct McpRequest {
    /// Original JSON-RPC ID (None for notifications)
    pub id: Option<JsonRpcId>,
    /// Method name
    pub method: String,
    /// Method parameters (Arc-wrapped for O(1) clone on the forward path)
    pub params: Option<Arc<Value>>,
    /// SEP-1686 task metadata (if present)
    pub task_metadata: Option<TaskMetadata>,
    /// Timestamp when request was received
    pub received_at: Instant,
    /// Unique correlation ID for tracing
    pub correlation_id: Uuid,
}

/// Custom Debug implementation that redacts params to prevent PII leakage
/// (tool arguments, resource URIs, etc. may contain sensitive data).
impl std::fmt::Debug for McpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRequest")
            .field("id", &self.id)
            .field("method", &self.method)
            .field("params", &self.params.as_ref().map(|_| "<redacted>"))
            .field("task_metadata", &self.task_metadata)
            .field("correlation_id", &self.correlation_id)
            .finish()
    }
}

impl McpRequest {
    /// Returns true if this is a notification (no ID).
    ///
    /// Notifications do not receive responses per JSON-RPC 2.0.
    ///
    /// Implements: REQ-CORE-003/F-001.3 (Notification Detection)
    #[inline]
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Returns true if this request has SEP-1686 task metadata.
    ///
    /// Task-augmented requests may be handled differently by the
    /// governance layer.
    ///
    /// Implements: REQ-CORE-003/F-003 (SEP-1686 Detection)
    #[inline]
    pub fn is_task_augmented(&self) -> bool {
        self.task_metadata.is_some()
    }

    /// Convert to a JsonRpcRequest for forwarding to upstream.
    ///
    /// Implements: REQ-CORE-003/F-004 (Request Forwarding)
    pub fn to_jsonrpc_request(&self) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: Cow::Borrowed(JSONRPC_VERSION),
            id: self.id.clone(),
            method: self.method.clone(),
            params: self.params.clone(),
        }
    }
}

/// A single item in a batch request - either valid or invalid.
///
/// Implements: REQ-CORE-003/EC-MCP-006 (Mixed valid/invalid batch results)
#[derive(Debug)]
pub enum BatchItem {
    /// Successfully parsed request
    Valid(McpRequest),
    /// Failed to parse - includes the original ID if extractable
    Invalid {
        /// The request ID if it could be extracted from malformed request
        id: Option<JsonRpcId>,
        /// The error that occurred during parsing
        error: crate::error::ThoughtGateError,
    },
}

/// Parse result that can be a single request, batch, or parse error.
#[derive(Debug)]
pub enum ParsedRequests {
    /// Single request
    Single(McpRequest),
    /// Batch of requests (may contain mix of valid and invalid per EC-MCP-006)
    Batch(Vec<BatchItem>),
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
    // Peek at the first non-whitespace byte to determine single vs batch
    // without parsing the entire payload into an intermediate Value.
    let first_byte = bytes
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .ok_or_else(|| ThoughtGateError::ParseError {
            details: "Invalid JSON: empty input".to_string(),
        })?;

    match first_byte {
        b'{' => {
            // F-001.1: Single request fast path — deserialize directly to
            // RawJsonRpcRequest, skipping the intermediate Value allocation.
            let raw: RawJsonRpcRequest = serde_json::from_slice(bytes).map_err(|e| {
                // Distinguish syntax errors (bad JSON) from semantic errors
                // (valid JSON but invalid field values like float IDs).
                if e.is_syntax() || e.is_eof() {
                    ThoughtGateError::ParseError {
                        details: format!("Invalid JSON: {}", e),
                    }
                } else {
                    ThoughtGateError::InvalidRequest {
                        details: format!("Invalid JSON-RPC structure: {}", e),
                    }
                }
            })?;
            Ok(ParsedRequests::Single(parse_single_from_raw(raw)?))
        }
        b'[' => {
            // F-001.2: Batch request — parse into Vec<Value> because
            // EC-MCP-006 requires extracting IDs from malformed items.
            let arr: Vec<Value> =
                serde_json::from_slice(bytes).map_err(|e| ThoughtGateError::ParseError {
                    details: format!("Invalid JSON: {}", e),
                })?;

            if arr.is_empty() {
                // EC-MCP-006: Empty batch
                return Err(ThoughtGateError::InvalidRequest {
                    details: "Empty batch is not allowed".to_string(),
                });
            }

            // EC-MCP-006: Collect mixed valid/invalid results
            let mut items = Vec::with_capacity(arr.len());
            for item in arr {
                // Try to extract ID before parsing (for error responses)
                let id = item
                    .as_object()
                    .and_then(|obj| obj.get("id"))
                    .and_then(|v| match v {
                        Value::Number(n) => n.as_i64().map(JsonRpcId::Number),
                        Value::String(s) => Some(JsonRpcId::String(s.clone())),
                        Value::Null => Some(JsonRpcId::Null),
                        _ => None,
                    });

                match parse_single_request(item) {
                    Ok(request) => items.push(BatchItem::Valid(request)),
                    Err(error) => items.push(BatchItem::Invalid { id, error }),
                }
            }
            Ok(ParsedRequests::Batch(items))
        }
        _ => {
            // Attempt parse to get a proper serde error message
            serde_json::from_slice::<Value>(bytes)
                .map_err(|e| ThoughtGateError::ParseError {
                    details: format!("Invalid JSON: {}", e),
                })
                .and_then(|_| {
                    // Parsed successfully but isn't object or array
                    Err(ThoughtGateError::InvalidRequest {
                        details: "Request must be an object or array".to_string(),
                    })
                })
        }
    }
}

/// Parse a single JSON-RPC 2.0 request from a JSON value.
///
/// Used for batch items where each element is already a `Value`.
/// Delegates to [`parse_single_from_raw`] after deserialization.
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
    parse_single_from_raw(raw)
}

/// Validate and convert a raw JSON-RPC request into an [`McpRequest`].
///
/// This is the shared validation core used by both the single-request fast
/// path (direct `from_slice`) and the batch path (via `parse_single_request`).
///
/// Implements: REQ-CORE-003/F-001.6, F-001.7, F-003
fn parse_single_from_raw(raw: RawJsonRpcRequest) -> Result<McpRequest, ThoughtGateError> {
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

    // F-001.7: Generate correlation ID (counter-based, avoids CSPRNG per request)
    let correlation_id = fast_correlation_id();

    // F-003: Extract SEP-1686 task metadata
    let task_metadata = extract_task_metadata(&raw.params);

    Ok(McpRequest {
        id: raw.id,
        method,
        params: raw.params.map(Arc::new),
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

        if let ParsedRequests::Batch(items) = result.expect("should parse") {
            assert_eq!(items.len(), 2);
            // Extract valid requests and check methods
            let req0 = match &items[0] {
                BatchItem::Valid(r) => r,
                _ => panic!("Expected valid request at index 0"),
            };
            let req1 = match &items[1] {
                BatchItem::Valid(r) => r,
                _ => panic!("Expected valid request at index 1"),
            };
            assert_eq!(req0.method, "a");
            assert_eq!(req1.method, "b");
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

    /// Verifies TTL clamping to MAX_TTL_MS (24 hours)
    #[test]
    fn test_ttl_clamped_to_max() {
        // TTL of 48 hours (exceeds 24 hour max)
        let excessive_ttl = 48 * 60 * 60 * 1000u64; // 48 hours in ms
        let json = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"task":{{"ttl":{}}}}}}}"#,
            excessive_ttl
        );
        let result = parse_jsonrpc(json.as_bytes());

        if let Ok(ParsedRequests::Single(req)) = result {
            assert!(req.is_task_augmented());
            let metadata = req.task_metadata.expect("should have metadata");
            // TTL should be clamped to MAX_TTL_MS (24 hours)
            let expected_max = std::time::Duration::from_millis(MAX_TTL_MS);
            assert_eq!(metadata.ttl, Some(expected_max));
            // Verify it's not the excessive value
            assert_ne!(
                metadata.ttl,
                Some(std::time::Duration::from_millis(excessive_ttl))
            );
        } else {
            panic!("Expected task-augmented request");
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

        if let Ok(ParsedRequests::Batch(items)) = result {
            assert_eq!(items.len(), 3);
            // Extract valid requests and check notification status
            let req0 = match &items[0] {
                BatchItem::Valid(r) => r,
                _ => panic!("Expected valid request at index 0"),
            };
            let req1 = match &items[1] {
                BatchItem::Valid(r) => r,
                _ => panic!("Expected valid request at index 1"),
            };
            let req2 = match &items[2] {
                BatchItem::Valid(r) => r,
                _ => panic!("Expected valid request at index 2"),
            };
            assert!(!req0.is_notification());
            assert!(req1.is_notification());
            assert!(!req2.is_notification());
        } else {
            panic!("Expected batch");
        }
    }

    /// Verifies: EC-MCP-006 (Mixed validity batch - all invalid items become BatchItem::Invalid)
    #[test]
    fn test_json_array_not_object_in_batch() {
        let json = br#"[1, 2, 3]"#;
        let result = parse_jsonrpc(json);
        // Per EC-MCP-006, invalid items in batch become BatchItem::Invalid rather than failing the whole batch
        if let Ok(ParsedRequests::Batch(items)) = result {
            assert_eq!(items.len(), 3);
            for item in items {
                assert!(matches!(item, BatchItem::Invalid { .. }));
            }
        } else {
            panic!("Expected batch with invalid items");
        }
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

    /// Verifies: JSON-RPC 2.0 spec - error response with unknown id serializes as null
    #[test]
    fn test_jsonrpc_response_error_unknown_id_serializes_as_null() {
        let error = crate::error::jsonrpc::JsonRpcError {
            code: -32700,
            message: "Parse error".to_string(),
            data: None,
        };
        // When we can't determine the request ID (e.g., parse error), pass None
        let response = JsonRpcResponse::error(None, error);

        let serialized = serde_json::to_string(&response).expect("should serialize");
        // Per JSON-RPC 2.0 spec, id MUST be present and null when unknown
        assert!(serialized.contains("\"id\":null"));
        assert!(serialized.contains("\"error\""));
        assert!(serialized.contains("-32700"));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // MCP Tasks Protocol Tests (Protocol Revision 2025-11-25)
    // ═══════════════════════════════════════════════════════════════════════

    /// Tests that TaskSupport serializes as lowercase strings per MCP spec.
    #[test]
    fn test_task_support_serialization() {
        use super::TaskSupport;

        // Test all variants serialize correctly per MCP spec
        assert_eq!(
            serde_json::to_string(&TaskSupport::Required).unwrap(),
            "\"required\""
        );
        assert_eq!(
            serde_json::to_string(&TaskSupport::Optional).unwrap(),
            "\"optional\""
        );
        assert_eq!(
            serde_json::to_string(&TaskSupport::Forbidden).unwrap(),
            "\"forbidden\""
        );
    }

    /// Tests that TaskSupport deserializes from lowercase strings.
    #[test]
    fn test_task_support_deserialization() {
        use super::TaskSupport;

        assert_eq!(
            serde_json::from_str::<TaskSupport>("\"required\"").unwrap(),
            TaskSupport::Required
        );
        assert_eq!(
            serde_json::from_str::<TaskSupport>("\"optional\"").unwrap(),
            TaskSupport::Optional
        );
        assert_eq!(
            serde_json::from_str::<TaskSupport>("\"forbidden\"").unwrap(),
            TaskSupport::Forbidden
        );
    }

    /// Tests ToolDefinition serialization with execution.taskSupport per MCP spec.
    #[test]
    fn test_tool_definition_serialization() {
        use super::{TaskSupport, ToolDefinition, ToolExecution};

        let tool = ToolDefinition {
            name: "delete_user".to_string(),
            description: Some("Deletes a user".to_string()),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "user_id": { "type": "string" }
                }
            })),
            execution: Some(ToolExecution {
                task_support: Some(TaskSupport::Required),
            }),
            extra: None,
        };

        let serialized = serde_json::to_string(&tool).expect("should serialize");

        // Check camelCase field names and nested execution structure
        assert!(serialized.contains("\"name\":\"delete_user\""));
        assert!(serialized.contains("\"description\":\"Deletes a user\""));
        assert!(serialized.contains("\"inputSchema\""));
        // Per MCP spec: execution.taskSupport (nested)
        assert!(serialized.contains("\"execution\""));
        assert!(serialized.contains("\"taskSupport\":\"required\""));
    }

    /// Tests ToolDefinition deserialization from upstream response.
    #[test]
    fn test_tool_definition_deserialization() {
        use super::ToolDefinition;

        let json = r#"{
            "name": "read_file",
            "description": "Reads a file",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                }
            }
        }"#;

        let tool: ToolDefinition = serde_json::from_str(json).expect("should parse");
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.description, Some("Reads a file".to_string()));
        assert!(tool.input_schema.is_some());
        assert!(tool.execution.is_none()); // Not set by upstream
    }

    /// Tests ToolDefinition deserialization with execution.taskSupport from upstream.
    #[test]
    fn test_tool_definition_with_execution_deserialization() {
        use super::{TaskSupport, ToolDefinition};

        let json = r#"{
            "name": "long_running_task",
            "description": "A task that takes time",
            "execution": {
                "taskSupport": "required"
            }
        }"#;

        let tool: ToolDefinition = serde_json::from_str(json).expect("should parse");
        assert_eq!(tool.name, "long_running_task");
        assert!(tool.execution.is_some());
        let execution = tool.execution.unwrap();
        assert_eq!(execution.task_support, Some(TaskSupport::Required));
    }

    /// Tests ToolDefinition preserves extra fields from upstream.
    #[test]
    fn test_tool_definition_extra_fields() {
        use super::ToolDefinition;

        let json = r#"{
            "name": "custom_tool",
            "description": "A custom tool",
            "customField": "custom_value",
            "anotherField": 123
        }"#;

        let tool: ToolDefinition = serde_json::from_str(json).expect("should parse");
        assert_eq!(tool.name, "custom_tool");

        // Re-serialize and check extra fields are preserved
        let reserialized = serde_json::to_string(&tool).expect("should serialize");
        assert!(reserialized.contains("\"customField\":\"custom_value\""));
        assert!(reserialized.contains("\"anotherField\":123"));
    }

    /// Tests ToolDefinition with execution.taskSupport set to forbidden per MCP spec.
    #[test]
    fn test_tool_definition_task_support_forbidden() {
        use super::{TaskSupport, ToolDefinition, ToolExecution};

        let tool = ToolDefinition {
            name: "simple_tool".to_string(),
            description: None,
            input_schema: None,
            execution: Some(ToolExecution {
                task_support: Some(TaskSupport::Forbidden),
            }),
            extra: None,
        };

        let serialized = serde_json::to_string(&tool).expect("should serialize");
        // Per MCP spec: "forbidden" (not "none")
        assert!(serialized.contains("\"execution\""));
        assert!(serialized.contains("\"taskSupport\":\"forbidden\""));
    }
}
