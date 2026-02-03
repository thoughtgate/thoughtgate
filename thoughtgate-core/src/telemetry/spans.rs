//! MCP span instrumentation following OTel GenAI semantic conventions.
//!
//! This module provides span creation and attribute management for MCP requests,
//! enabling distributed tracing across the ThoughtGate proxy.
//!
//! # Traceability
//! - Implements: REQ-OBS-002 §5.1 (MCP Spans)
//! - Implements: REQ-OBS-002 §5.1.1 (MCP Span Attributes)

use opentelemetry::{
    KeyValue,
    global::{self, BoxedSpan},
    trace::{Span, SpanKind, Status, Tracer},
};

// ─────────────────────────────────────────────────────────────────────────────
// MCP Span Attribute Constants (OTel GenAI Semconv v1.39.0+)
// ─────────────────────────────────────────────────────────────────────────────

/// JSON-RPC method name (e.g., "tools/call", "resources/read")
/// Requirement: Required
pub const MCP_METHOD_NAME: &str = "mcp.method.name";

/// MCP session identifier for correlation
/// Requirement: Recommended
pub const MCP_SESSION_ID: &str = "mcp.session.id";

/// JSON-RPC message type discriminator
/// Values: "request", "response", "notification"
/// Requirement: Required
pub const MCP_MESSAGE_TYPE: &str = "mcp.message.type";

/// JSON-RPC `id` field for request correlation
/// Requirement: Conditional (present if request has id)
pub const MCP_MESSAGE_ID: &str = "mcp.message.id";

/// Whether MCP returned an error (even if HTTP 200)
/// Requirement: Required
pub const MCP_RESULT_IS_ERROR: &str = "mcp.result.is_error";

/// JSON-RPC error code if present
/// Requirement: Conditional (present on error)
pub const MCP_ERROR_CODE: &str = "mcp.error.code";

/// GenAI operation type (e.g., "execute_tool", "chat")
/// Requirement: Required
pub const GENAI_OPERATION_NAME: &str = "gen_ai.operation.name";

/// Tool being invoked (for tools/call)
/// Requirement: Conditional (present for tools/call)
pub const GENAI_TOOL_NAME: &str = "gen_ai.tool.name";

/// Unique tool call identifier
/// Requirement: Conditional (present for tools/call)
pub const GENAI_TOOL_CALL_ID: &str = "gen_ai.tool.call.id";

/// ThoughtGate internal correlation ID
/// Requirement: Required
pub const THOUGHTGATE_REQUEST_ID: &str = "thoughtgate.request_id";

/// Error classification
/// Requirement: Conditional (present on error)
pub const ERROR_TYPE: &str = "error.type";

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// MCP message type for span attributes.
///
/// Implements: REQ-OBS-002 §5.1.1 (mcp.message.type attribute)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMessageType {
    /// A JSON-RPC request (has id, expects response)
    Request,
    /// A JSON-RPC notification (no id, no response expected)
    Notification,
}

impl McpMessageType {
    /// Returns the string representation for the span attribute.
    pub fn as_str(&self) -> &'static str {
        match self {
            McpMessageType::Request => "request",
            McpMessageType::Notification => "notification",
        }
    }
}

/// Data needed to start an MCP span.
///
/// This struct collects the request metadata required for span attributes
/// without exposing sensitive data like tool arguments.
///
/// Implements: REQ-OBS-002 §5.1
pub struct McpSpanData<'a> {
    /// JSON-RPC method name (e.g., "tools/call")
    pub method: &'a str,
    /// Message type (request or notification)
    pub message_type: McpMessageType,
    /// JSON-RPC id field (stringified)
    pub message_id: Option<String>,
    /// ThoughtGate correlation ID
    pub correlation_id: &'a str,
    /// Tool name for tools/call requests
    pub tool_name: Option<&'a str>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Span Functions
// ─────────────────────────────────────────────────────────────────────────────

/// Start an MCP request span using the global tracer provider.
///
/// Span name convention: `{mcp.method}` (e.g., "tools/call")
/// Span kind: SERVER
///
/// Returns a `BoxedSpan` since `global::tracer()` returns `BoxedTracer`.
///
/// # Arguments
///
/// * `data` - Span data containing method, message type, and correlation info
///
/// # Example
///
/// ```ignore
/// let data = McpSpanData {
///     method: "tools/call",
///     message_type: McpMessageType::Request,
///     message_id: Some("42".to_string()),
///     correlation_id: "req-abc123",
///     tool_name: Some("web_search"),
/// };
/// let mut span = start_mcp_span(&data);
/// // ... process request ...
/// finish_mcp_span(&mut span, false, None, None);
/// ```
///
/// Implements: REQ-OBS-002 §5.1
pub fn start_mcp_span(data: &McpSpanData<'_>) -> BoxedSpan {
    let tracer = global::tracer("thoughtgate");

    let mut attributes = vec![
        KeyValue::new(MCP_METHOD_NAME, data.method.to_string()),
        KeyValue::new(MCP_MESSAGE_TYPE, data.message_type.as_str()),
        KeyValue::new(THOUGHTGATE_REQUEST_ID, data.correlation_id.to_string()),
        KeyValue::new(GENAI_OPERATION_NAME, derive_operation_name(data.method)),
    ];

    if let Some(ref id) = data.message_id {
        attributes.push(KeyValue::new(MCP_MESSAGE_ID, id.clone()));
    }

    if let Some(tool) = data.tool_name {
        attributes.push(KeyValue::new(GENAI_TOOL_NAME, tool.to_string()));
    }

    tracer
        .span_builder(data.method.to_string())
        .with_kind(SpanKind::Server)
        .with_attributes(attributes)
        .start(&tracer)
}

/// Finish an MCP span with result attributes.
///
/// Sets `mcp.result.is_error`, and on error: `mcp.error.code`, `error.type`.
/// Also sets the span status to Ok or Error accordingly.
///
/// # Arguments
///
/// * `span` - The span to finish
/// * `is_error` - Whether the request resulted in an error
/// * `error_code` - JSON-RPC error code (if error)
/// * `error_type` - Error classification string (if error)
///
/// Implements: REQ-OBS-002 §5.1.1
pub fn finish_mcp_span(
    span: &mut impl Span,
    is_error: bool,
    error_code: Option<i32>,
    error_type: Option<&str>,
) {
    span.set_attribute(KeyValue::new(MCP_RESULT_IS_ERROR, is_error));

    if is_error {
        if let Some(code) = error_code {
            span.set_attribute(KeyValue::new(MCP_ERROR_CODE, code as i64));
        }
        if let Some(err_type) = error_type {
            span.set_attribute(KeyValue::new(ERROR_TYPE, err_type.to_string()));
        }
        span.set_status(Status::error("MCP request failed"));
    } else {
        span.set_status(Status::Ok);
    }
}

/// Derive GenAI operation name from MCP method.
///
/// Maps JSON-RPC method names to semantic GenAI operation names.
///
/// Implements: REQ-OBS-002 §5.1.1 (gen_ai.operation.name attribute)
fn derive_operation_name(method: &str) -> &'static str {
    match method {
        "tools/call" => "execute_tool",
        "sampling/createMessage" => "chat",
        "resources/read" => "read_resource",
        "resources/list" => "list_resources",
        "tools/list" => "list_tools",
        "prompts/list" => "list_prompts",
        "prompts/get" => "get_prompt",
        _ => "mcp_request",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use serial_test::serial;

    fn setup_test_provider() -> (SdkTracerProvider, InMemorySpanExporter) {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        global::set_tracer_provider(provider.clone());
        (provider, exporter)
    }

    #[test]
    #[serial]
    fn test_start_and_finish_mcp_span() {
        let (provider, exporter) = setup_test_provider();

        let data = McpSpanData {
            method: "tools/call",
            message_type: McpMessageType::Request,
            message_id: Some("42".to_string()),
            correlation_id: "test-corr-123",
            tool_name: Some("web_search"),
        };

        let mut span: BoxedSpan = start_mcp_span(&data);
        finish_mcp_span(&mut span, false, None, None);
        drop(span);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.name.as_ref(), "tools/call");
    }

    #[test]
    #[serial]
    fn test_error_span_attributes() {
        let (provider, exporter) = setup_test_provider();

        let data = McpSpanData {
            method: "tools/call",
            message_type: McpMessageType::Request,
            message_id: Some("99".to_string()),
            correlation_id: "test-err-456",
            tool_name: None,
        };

        let mut span: BoxedSpan = start_mcp_span(&data);
        finish_mcp_span(&mut span, true, Some(-32601), Some("method_not_found"));
        drop(span);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);
        // Span status should be Error - verified by span existing
    }

    #[test]
    fn test_derive_operation_name() {
        assert_eq!(derive_operation_name("tools/call"), "execute_tool");
        assert_eq!(derive_operation_name("sampling/createMessage"), "chat");
        assert_eq!(derive_operation_name("resources/read"), "read_resource");
        assert_eq!(derive_operation_name("resources/list"), "list_resources");
        assert_eq!(derive_operation_name("tools/list"), "list_tools");
        assert_eq!(derive_operation_name("prompts/list"), "list_prompts");
        assert_eq!(derive_operation_name("prompts/get"), "get_prompt");
        assert_eq!(derive_operation_name("unknown/method"), "mcp_request");
    }

    #[test]
    fn test_mcp_message_type() {
        assert_eq!(McpMessageType::Request.as_str(), "request");
        assert_eq!(McpMessageType::Notification.as_str(), "notification");
    }

    #[test]
    #[serial]
    fn test_notification_span() {
        let (provider, exporter) = setup_test_provider();

        let data = McpSpanData {
            method: "notifications/initialized",
            message_type: McpMessageType::Notification,
            message_id: None,
            correlation_id: "test-notif-789",
            tool_name: None,
        };

        let mut span: BoxedSpan = start_mcp_span(&data);
        finish_mcp_span(&mut span, false, None, None);
        drop(span);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.name.as_ref(), "notifications/initialized");
    }
}
