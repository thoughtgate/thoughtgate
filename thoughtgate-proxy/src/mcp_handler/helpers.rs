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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    fn make_request(method: &str, params: Option<serde_json::Value>) -> McpRequest {
        McpRequest {
            id: Some(JsonRpcId::Number(1)),
            method: method.to_string(),
            params: params.map(Arc::new),
            task_metadata: None,
            received_at: Instant::now(),
            correlation_id: uuid::Uuid::nil(),
        }
    }

    // ── is_list_method ────────────────────────────────────────────────

    #[test]
    fn test_is_list_method_tools_list() {
        assert!(is_list_method("tools/list"));
    }

    #[test]
    fn test_is_list_method_resources_list() {
        assert!(is_list_method("resources/list"));
    }

    #[test]
    fn test_is_list_method_prompts_list() {
        assert!(is_list_method("prompts/list"));
    }

    #[test]
    fn test_is_list_method_tools_call() {
        assert!(!is_list_method("tools/call"));
    }

    #[test]
    fn test_is_list_method_arbitrary() {
        assert!(!is_list_method("custom/method"));
    }

    // ── extract_tool_name ─────────────────────────────────────────────

    #[test]
    fn test_extract_tool_name_valid() {
        let request = make_request(
            "tools/call",
            Some(serde_json::json!({"name": "delete_user", "arguments": {}})),
        );
        assert_eq!(extract_tool_name(&request), Some("delete_user"));
    }

    #[test]
    fn test_extract_tool_name_not_tools_call() {
        let request = make_request(
            "resources/read",
            Some(serde_json::json!({"name": "my_resource"})),
        );
        assert_eq!(extract_tool_name(&request), None);
    }

    #[test]
    fn test_extract_tool_name_no_params() {
        let request = make_request("tools/call", None);
        assert_eq!(extract_tool_name(&request), None);
    }

    #[test]
    fn test_extract_tool_name_missing_name_field() {
        let request = make_request("tools/call", Some(serde_json::json!({"arguments": {}})));
        assert_eq!(extract_tool_name(&request), None);
    }

    // ── BufferGuard ───────────────────────────────────────────────────

    #[test]
    fn test_buffer_guard_decrements_on_drop() {
        let counter = AtomicUsize::new(100);
        {
            let _guard = BufferGuard {
                counter: &counter,
                size: 42,
            };
            assert_eq!(counter.load(Ordering::Acquire), 100);
        }
        assert_eq!(counter.load(Ordering::Acquire), 58);
    }

    // ── json_bytes ────────────────────────────────────────────────────

    #[test]
    fn test_json_bytes_success_response() {
        let response =
            JsonRpcResponse::success(Some(JsonRpcId::Number(1)), serde_json::json!("ok"));
        let (status, bytes) = json_bytes(&response);
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["result"], "ok");
    }

    // ── error_bytes ───────────────────────────────────────────────────

    #[test]
    fn test_error_bytes_returns_200_with_jsonrpc_error() {
        let error = ThoughtGateError::PolicyDenied {
            tool: "test_tool".to_string(),
            policy_id: Some("policy-1".to_string()),
            reason: Some("forbidden".to_string()),
        };
        let (status, bytes) = error_bytes(Some(JsonRpcId::Number(42)), &error, "corr-123");
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["id"], 42);
        assert_eq!(parsed["error"]["code"], -32003);
    }

    #[test]
    fn test_error_bytes_with_null_id() {
        let error = ThoughtGateError::ParseError {
            details: "bad json".to_string(),
        };
        let (status, bytes) = error_bytes(None, &error, "corr-456");
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(parsed["id"].is_null());
        assert_eq!(parsed["error"]["code"], -32700);
    }
}
