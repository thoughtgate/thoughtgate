//! Small helper functions and types for MCP request handling.

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::StatusCode;
use bytes::Bytes;

use thoughtgate_core::error::ThoughtGateError;
use thoughtgate_core::transport::jsonrpc::{JsonRpcId, JsonRpcResponse, McpRequest};

/// RAII guard that decrements the aggregate buffer counter on drop.
///
/// Ensures buffered_bytes is always decremented even if the handler panics.
pub(super) struct BufferGuard<'a> {
    pub(super) counter: &'a AtomicUsize,
    pub(super) size: usize,
}

impl<'a> Drop for BufferGuard<'a> {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.size, Ordering::Release);
    }
}

/// Check if a method is a list method that requires response filtering.
pub(super) fn is_list_method(method: &str) -> bool {
    matches!(method, "tools/list" | "resources/list" | "prompts/list")
}

/// Extract tool name from tools/call request params.
///
/// Implements: REQ-OBS-002 §5.1.1 (gen_ai.tool.name attribute)
pub(super) fn extract_tool_name(request: &McpRequest) -> Option<&str> {
    if request.method != "tools/call" {
        return None;
    }
    request.params.as_ref()?.get("name")?.as_str()
}

/// Build JSON bytes from a JsonRpcResponse.
pub(super) fn json_bytes(response: &JsonRpcResponse) -> (StatusCode, Bytes) {
    match serde_json::to_vec(response) {
        Ok(bytes) => (StatusCode::OK, Bytes::from(bytes)),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(
                br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}"#,
            ),
        ),
    }
}

/// Build error bytes from a ThoughtGateError.
///
/// # Arguments
///
/// * `id` - The request ID (may be None for parse errors)
/// * `error` - The ThoughtGateError
/// * `correlation_id` - Correlation ID for the error response
pub(super) fn error_bytes(
    id: Option<JsonRpcId>,
    error: &ThoughtGateError,
    correlation_id: &str,
) -> (StatusCode, Bytes) {
    let jsonrpc_error = error.to_jsonrpc_error(correlation_id);
    let response = JsonRpcResponse::error(id, jsonrpc_error);

    // JSON-RPC errors still return HTTP 200
    match serde_json::to_vec(&response) {
        Ok(bytes) => (StatusCode::OK, Bytes::from(bytes)),
        Err(_) => (
            StatusCode::OK,
            Bytes::from_static(
                br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}"#,
            ),
        ),
    }
}
