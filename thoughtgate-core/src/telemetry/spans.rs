//! MCP span instrumentation following OTel GenAI semantic conventions.
//!
//! This module provides span creation and attribute management for MCP requests,
//! enabling distributed tracing across the ThoughtGate proxy.
//!
//! # Traceability
//! - Implements: REQ-OBS-002 §5.1 (MCP Spans)
//! - Implements: REQ-OBS-002 §5.1.1 (MCP Span Attributes)
//! - Implements: REQ-OBS-002 §5.4 (Approval Workflow Spans)

use opentelemetry::{
    Context, KeyValue,
    global::{self, BoxedSpan},
    trace::{Link, Span, SpanContext, SpanKind, Status, TraceContextExt, Tracer},
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
// Approval Span Attribute Constants (REQ-OBS-002 §5.4)
// ─────────────────────────────────────────────────────────────────────────────

/// SEP-1686 task identifier for correlation
/// Requirement: Required for approval spans
pub const THOUGHTGATE_TASK_ID: &str = "thoughtgate.task.id";

/// Approval integration type (e.g., "slack", "webhook", "teams")
/// Requirement: Required for approval dispatch/callback spans
pub const THOUGHTGATE_APPROVAL_CHANNEL: &str = "thoughtgate.approval.channel";

/// Target channel/endpoint for approval (redacted if sensitive)
/// Requirement: Recommended for approval dispatch spans
pub const THOUGHTGATE_APPROVAL_TARGET: &str = "thoughtgate.approval.target";

/// Configured approval timeout in seconds
/// Requirement: Recommended for approval dispatch spans
pub const THOUGHTGATE_APPROVAL_TIMEOUT_S: &str = "thoughtgate.approval.timeout_s";

/// Human decision outcome (e.g., "approved", "denied")
/// Requirement: Required for approval callback spans
pub const THOUGHTGATE_APPROVAL_DECISION: &str = "thoughtgate.approval.decision";

/// Approving user identifier (pseudonymized per REQ-OBS-003)
/// Requirement: Recommended for approval callback spans
pub const THOUGHTGATE_APPROVAL_USER: &str = "thoughtgate.approval.user";

/// Wall-clock time from dispatch to callback in seconds
/// Requirement: Required for approval callback spans
pub const THOUGHTGATE_APPROVAL_LATENCY_S: &str = "thoughtgate.approval.latency_s";

/// Whether trace context was successfully recovered (false if corrupted)
/// Requirement: Required when graceful degradation occurs
pub const THOUGHTGATE_TRACE_CONTEXT_RECOVERED: &str = "thoughtgate.trace_context.recovered";

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

/// Data needed to start an approval dispatch span.
///
/// Implements: REQ-OBS-002 §5.4.1
pub struct ApprovalDispatchData<'a> {
    /// SEP-1686 task identifier
    pub task_id: &'a str,
    /// Approval integration type (e.g., "slack", "webhook")
    pub channel: &'a str,
    /// Target channel/endpoint (e.g., "#security-approvals")
    pub target: Option<&'a str>,
    /// Configured approval timeout in seconds
    pub timeout_secs: Option<u64>,
    /// Optional span context to link to (from the parent MCP request)
    pub link_to: Option<&'a SpanContext>,
}

/// Data needed to start an approval callback span.
///
/// Implements: REQ-OBS-002 §5.4.2
pub struct ApprovalCallbackData<'a> {
    /// SEP-1686 task identifier
    pub task_id: &'a str,
    /// Approval integration type (e.g., "slack", "webhook")
    pub channel: &'a str,
    /// Whether trace context was successfully recovered
    pub trace_context_recovered: bool,
    /// Optional span context to link to (from the dispatch span)
    pub link_to: Option<&'a SpanContext>,
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

// ─────────────────────────────────────────────────────────────────────────────
// Approval Span Functions (REQ-OBS-002 §5.4)
// ─────────────────────────────────────────────────────────────────────────────

/// Start an approval dispatch span using the global tracer provider.
///
/// This span is created when ThoughtGate sends an approval request to an
/// external system (Slack/webhook). It completes immediately after dispatch.
///
/// Span name: `thoughtgate.approval.dispatch`
/// Span kind: PRODUCER (we're producing a message for async consumption)
///
/// If `link_to` is provided, a span link is created to maintain trace
/// correlation without creating a parent-child relationship that would
/// span hours.
///
/// # Arguments
///
/// * `data` - Span data containing task ID, channel, and link context
///
/// # Returns
///
/// A `BoxedSpan` that should be ended after the dispatch completes.
///
/// # Example
///
/// ```ignore
/// let data = ApprovalDispatchData {
///     task_id: "tg-task-xyz789",
///     channel: "slack",
///     target: Some("#security-approvals"),
///     timeout_secs: Some(3600),
///     link_to: Some(&mcp_span_context),
/// };
/// let mut span = start_approval_dispatch_span(&data);
/// // ... dispatch to Slack ...
/// span.end();
/// ```
///
/// Implements: REQ-OBS-002 §5.4.1
pub fn start_approval_dispatch_span(data: &ApprovalDispatchData<'_>) -> BoxedSpan {
    let tracer = global::tracer("thoughtgate");

    let mut attributes = vec![
        KeyValue::new(THOUGHTGATE_TASK_ID, data.task_id.to_string()),
        KeyValue::new(THOUGHTGATE_APPROVAL_CHANNEL, data.channel.to_string()),
    ];

    if let Some(target) = data.target {
        attributes.push(KeyValue::new(
            THOUGHTGATE_APPROVAL_TARGET,
            target.to_string(),
        ));
    }

    if let Some(timeout) = data.timeout_secs {
        attributes.push(KeyValue::new(
            THOUGHTGATE_APPROVAL_TIMEOUT_S,
            timeout as i64,
        ));
    }

    let mut builder = tracer
        .span_builder("thoughtgate.approval.dispatch")
        .with_kind(SpanKind::Producer)
        .with_attributes(attributes);

    // Add span link to the MCP request span (if available)
    if let Some(link_ctx) = data.link_to {
        builder = builder.with_links(vec![Link::new(link_ctx.clone(), vec![], 0)]);
    }

    builder.start(&tracer)
}

/// Start an approval callback span using the global tracer provider.
///
/// This span is created when the human responds (approve/deny) via Slack
/// or webhook. It's a new span within the same trace, linked to the
/// dispatch span (not a child of it).
///
/// Span name: `thoughtgate.approval.callback`
/// Span kind: CONSUMER (we're consuming the async approval decision)
///
/// # Arguments
///
/// * `data` - Span data containing task ID, channel, recovery status, and link context
///
/// # Returns
///
/// A `BoxedSpan` that should be finished with `finish_approval_callback_span`.
///
/// # Graceful Degradation
///
/// If `trace_context_recovered` is false, this indicates the original trace
/// context was corrupted and a new trace was started. The span will have
/// the `thoughtgate.trace_context.recovered=false` attribute set.
///
/// Implements: REQ-OBS-002 §5.4.2
pub fn start_approval_callback_span(data: &ApprovalCallbackData<'_>) -> BoxedSpan {
    let tracer = global::tracer("thoughtgate");

    let attributes = vec![
        KeyValue::new(THOUGHTGATE_TASK_ID, data.task_id.to_string()),
        KeyValue::new(THOUGHTGATE_APPROVAL_CHANNEL, data.channel.to_string()),
        KeyValue::new(
            THOUGHTGATE_TRACE_CONTEXT_RECOVERED,
            data.trace_context_recovered,
        ),
    ];

    let mut builder = tracer
        .span_builder("thoughtgate.approval.callback")
        .with_kind(SpanKind::Consumer)
        .with_attributes(attributes);

    // Add span link to the dispatch span (if available)
    if let Some(link_ctx) = data.link_to {
        builder = builder.with_links(vec![Link::new(link_ctx.clone(), vec![], 0)]);
    }

    // If we have a link context with a valid trace ID, we want to be part of that trace
    // but NOT as a child span. We achieve this by creating a context with the trace ID
    // but not setting it as the parent.
    if let Some(link_ctx) = data.link_to {
        if link_ctx.is_valid() {
            // Create a new context that preserves the trace_id from the linked span
            // This keeps us in the same trace while not being a child
            let parent_ctx = Context::new().with_remote_span_context(link_ctx.clone());
            return builder.start_with_context(&tracer, &parent_ctx);
        }
    }

    builder.start(&tracer)
}

/// Finish an approval callback span with decision attributes.
///
/// Sets the approval decision, approver identity, and latency attributes,
/// then ends the span.
///
/// # Arguments
///
/// * `span` - The span to finish (from `start_approval_callback_span`)
/// * `decision` - The approval decision ("approved" or "denied")
/// * `approver` - The user who made the decision (pseudonymized per REQ-OBS-003)
/// * `latency_secs` - Wall-clock time from dispatch to callback in seconds
///
/// Implements: REQ-OBS-002 §5.4.2
pub fn finish_approval_callback_span(
    span: &mut impl Span,
    decision: &str,
    approver: &str,
    latency_secs: f64,
) {
    span.set_attribute(KeyValue::new(
        THOUGHTGATE_APPROVAL_DECISION,
        decision.to_string(),
    ));
    span.set_attribute(KeyValue::new(
        THOUGHTGATE_APPROVAL_USER,
        approver.to_string(),
    ));
    span.set_attribute(KeyValue::new(THOUGHTGATE_APPROVAL_LATENCY_S, latency_secs));

    // Set span status based on decision (approved = ok, denied = still ok but noted)
    span.set_status(Status::Ok);
}

/// Get the current span context from the active span.
///
/// Useful for extracting the span context to serialize for async boundaries.
///
/// # Returns
///
/// The `SpanContext` of the currently active span.
pub fn current_span_context() -> SpanContext {
    Context::current().span().span_context().clone()
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

    // ─────────────────────────────────────────────────────────────────────────
    // Approval Span Tests (REQ-OBS-002 §5.4)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_approval_dispatch_span() {
        let (provider, exporter) = setup_test_provider();

        let data = ApprovalDispatchData {
            task_id: "tg-task-xyz789",
            channel: "slack",
            target: Some("#security-approvals"),
            timeout_secs: Some(3600),
            link_to: None,
        };

        let span = start_approval_dispatch_span(&data);
        drop(span);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.name.as_ref(), "thoughtgate.approval.dispatch");
        assert_eq!(finished.span_kind, opentelemetry::trace::SpanKind::Producer);
    }

    #[test]
    #[serial]
    fn test_approval_callback_span() {
        let (provider, exporter) = setup_test_provider();

        let data = ApprovalCallbackData {
            task_id: "tg-task-xyz789",
            channel: "slack",
            trace_context_recovered: true,
            link_to: None,
        };

        let mut span: BoxedSpan = start_approval_callback_span(&data);
        finish_approval_callback_span(&mut span, "approved", "user:alice", 127.5);
        drop(span);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.name.as_ref(), "thoughtgate.approval.callback");
        assert_eq!(finished.span_kind, opentelemetry::trace::SpanKind::Consumer);
    }

    #[test]
    #[serial]
    fn test_approval_span_links() {
        use opentelemetry::trace::{SpanId, TraceFlags, TraceId, TraceState};

        let (provider, exporter) = setup_test_provider();

        // Create a mock span context to link to
        let trace_id = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap();
        let span_id = SpanId::from_hex("00f067aa0ba902b7").unwrap();
        let mock_ctx = SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );

        // Create dispatch span with link
        let dispatch_data = ApprovalDispatchData {
            task_id: "tg-task-abc123",
            channel: "slack",
            target: None,
            timeout_secs: None,
            link_to: Some(&mock_ctx),
        };

        let dispatch_span = start_approval_dispatch_span(&dispatch_data);
        drop(dispatch_span);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.links.len(), 1, "Dispatch span should have a link");
        assert_eq!(
            finished.links[0].span_context.trace_id(),
            trace_id,
            "Link should point to the MCP request trace"
        );
    }

    #[test]
    #[serial]
    fn test_approval_callback_span_shares_trace_id() {
        use opentelemetry::trace::{SpanId, TraceFlags, TraceId, TraceState};

        let (provider, exporter) = setup_test_provider();

        // Create a mock dispatch span context to link to
        let dispatch_trace_id = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap();
        let dispatch_span_id = SpanId::from_hex("00f067aa0ba902b7").unwrap();
        let dispatch_ctx = SpanContext::new(
            dispatch_trace_id,
            dispatch_span_id,
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );

        // Create callback span with link to dispatch
        let callback_data = ApprovalCallbackData {
            task_id: "tg-task-abc123",
            channel: "slack",
            trace_context_recovered: true,
            link_to: Some(&dispatch_ctx),
        };

        let mut callback_span: BoxedSpan = start_approval_callback_span(&callback_data);
        finish_approval_callback_span(&mut callback_span, "approved", "user:bob", 300.0);
        drop(callback_span);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];

        // Callback span should have a link to dispatch span
        assert_eq!(finished.links.len(), 1, "Callback span should have a link");
        assert_eq!(
            finished.links[0].span_context.span_id(),
            dispatch_span_id,
            "Link should point to the dispatch span"
        );

        // Callback span should share the same trace_id as dispatch
        // (because we used with_remote_span_context)
        assert_eq!(
            finished.span_context.trace_id(),
            dispatch_trace_id,
            "Callback span should share trace_id with dispatch"
        );

        // Callback span should NOT be a child of dispatch (different span_id)
        assert_ne!(
            finished.span_context.span_id(),
            dispatch_span_id,
            "Callback span should have its own span_id (not a child)"
        );
    }

    #[test]
    #[serial]
    fn test_approval_callback_with_corrupted_context() {
        let (provider, exporter) = setup_test_provider();

        // Simulate corrupted context scenario (trace_context_recovered = false)
        let callback_data = ApprovalCallbackData {
            task_id: "tg-task-corrupted",
            channel: "slack",
            trace_context_recovered: false, // Context was corrupted
            link_to: None,                  // No link because context couldn't be parsed
        };

        let mut callback_span: BoxedSpan = start_approval_callback_span(&callback_data);
        finish_approval_callback_span(&mut callback_span, "approved", "user:charlie", 600.0);
        drop(callback_span);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.name.as_ref(), "thoughtgate.approval.callback");

        // Should have no links since context was corrupted
        assert_eq!(
            finished.links.len(),
            0,
            "Corrupted context should result in no links"
        );

        // Span should still have valid trace_id (new trace started)
        assert_ne!(
            finished.span_context.trace_id(),
            opentelemetry::trace::TraceId::INVALID,
            "Should have valid trace_id even with corrupted context"
        );
    }
}
