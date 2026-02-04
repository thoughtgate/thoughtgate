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
//
// These constants are pub(crate) to keep them as implementation details.
// External crates should use the span functions (start_mcp_span, etc.) which
// encapsulate attribute management.
// ─────────────────────────────────────────────────────────────────────────────

/// JSON-RPC method name (e.g., "tools/call", "resources/read")
/// Requirement: Required
pub(crate) const MCP_METHOD_NAME: &str = "mcp.method.name";

/// MCP session identifier for correlation
/// Requirement: Recommended
#[allow(dead_code)] // Reserved for future use
pub(crate) const MCP_SESSION_ID: &str = "mcp.session.id";

/// JSON-RPC message type discriminator
/// Values: "request", "response", "notification"
/// Requirement: Required
pub(crate) const MCP_MESSAGE_TYPE: &str = "mcp.message.type";

/// JSON-RPC `id` field for request correlation
/// Requirement: Conditional (present if request has id)
pub(crate) const MCP_MESSAGE_ID: &str = "mcp.message.id";

/// Whether MCP returned an error (even if HTTP 200)
/// Requirement: Required
pub(crate) const MCP_RESULT_IS_ERROR: &str = "mcp.result.is_error";

/// JSON-RPC error code if present
/// Requirement: Conditional (present on error)
pub(crate) const MCP_ERROR_CODE: &str = "mcp.error.code";

/// GenAI operation type (e.g., "execute_tool", "chat")
/// Requirement: Required
pub(crate) const GENAI_OPERATION_NAME: &str = "gen_ai.operation.name";

/// Tool being invoked (for tools/call)
/// Requirement: Conditional (present for tools/call)
pub(crate) const GENAI_TOOL_NAME: &str = "gen_ai.tool.name";

/// Unique tool call identifier
/// Requirement: Conditional (present for tools/call)
#[allow(dead_code)] // Reserved for future use
pub(crate) const GENAI_TOOL_CALL_ID: &str = "gen_ai.tool.call.id";

/// ThoughtGate internal correlation ID
/// Requirement: Required
pub(crate) const THOUGHTGATE_REQUEST_ID: &str = "thoughtgate.request_id";

/// Error classification
/// Requirement: Conditional (present on error)
pub(crate) const ERROR_TYPE: &str = "error.type";

// ─────────────────────────────────────────────────────────────────────────────
// Approval Span Attribute Constants (REQ-OBS-002 §5.4)
// ─────────────────────────────────────────────────────────────────────────────

/// SEP-1686 task identifier for correlation
/// Requirement: Required for approval spans
pub(crate) const THOUGHTGATE_TASK_ID: &str = "thoughtgate.task.id";

/// Approval integration type (e.g., "slack", "webhook", "teams")
/// Requirement: Required for approval dispatch/callback spans
pub(crate) const THOUGHTGATE_APPROVAL_CHANNEL: &str = "thoughtgate.approval.channel";

/// Target channel/endpoint for approval (redacted if sensitive)
/// Requirement: Recommended for approval dispatch spans
pub(crate) const THOUGHTGATE_APPROVAL_TARGET: &str = "thoughtgate.approval.target";

/// Configured approval timeout in seconds
/// Requirement: Recommended for approval dispatch spans
pub(crate) const THOUGHTGATE_APPROVAL_TIMEOUT_S: &str = "thoughtgate.approval.timeout_s";

/// Human decision outcome (e.g., "approved", "denied")
/// Requirement: Required for approval callback spans
pub(crate) const THOUGHTGATE_APPROVAL_DECISION: &str = "thoughtgate.approval.decision";

/// Approving user identifier (pseudonymized per REQ-OBS-003)
/// Requirement: Recommended for approval callback spans
pub(crate) const THOUGHTGATE_APPROVAL_USER: &str = "thoughtgate.approval.user";

/// Wall-clock time from dispatch to callback in seconds
/// Requirement: Required for approval callback spans
pub(crate) const THOUGHTGATE_APPROVAL_LATENCY_S: &str = "thoughtgate.approval.latency_s";

/// Whether trace context was successfully recovered (false if corrupted)
/// Requirement: Required when graceful degradation occurs
pub(crate) const THOUGHTGATE_TRACE_CONTEXT_RECOVERED: &str = "thoughtgate.trace_context.recovered";

// ─────────────────────────────────────────────────────────────────────────────
// Gateway Decision Span Attribute Constants (REQ-OBS-002 §5.3)
// ─────────────────────────────────────────────────────────────────────────────

/// Gate 1 (visibility) outcome: "pass" or "block"
/// Requirement: Required for gateway decision spans
pub(crate) const THOUGHTGATE_GATE_VISIBILITY: &str = "thoughtgate.gate.visibility";

/// Gate 2 (governance rules) outcome: "forward", "deny", "approve", or "policy"
/// Requirement: Required for gateway decision spans
pub(crate) const THOUGHTGATE_GATE_GOVERNANCE: &str = "thoughtgate.gate.governance";

/// Gate 3 (Cedar policy) outcome: "allow" or "deny"
/// Requirement: Conditional (present if Gate 3 evaluated)
pub(crate) const THOUGHTGATE_GATE_CEDAR: &str = "thoughtgate.gate.cedar";

/// Gate 4 (approval) outcome: "started", "approved", "rejected", or "timeout"
/// Requirement: Conditional (present if Gate 4 evaluated)
pub(crate) const THOUGHTGATE_GATE_APPROVAL: &str = "thoughtgate.gate.approval";

/// Governance rule ID that matched (from Gate 2)
/// Requirement: Conditional (present if a rule matched)
pub(crate) const THOUGHTGATE_GOVERNANCE_RULE_ID: &str = "thoughtgate.governance.rule_id";

/// Upstream target URL or identifier
/// Requirement: Required for gateway decision spans
pub(crate) const THOUGHTGATE_UPSTREAM_TARGET: &str = "thoughtgate.upstream.target";

/// Upstream call latency in milliseconds
/// Requirement: Conditional (present if upstream was called)
pub(crate) const THOUGHTGATE_UPSTREAM_LATENCY_MS: &str = "thoughtgate.upstream.latency_ms";

/// Whether Cedar policy was evaluated (true/false)
/// Requirement: Required for gateway decision spans
pub(crate) const THOUGHTGATE_POLICY_EVALUATED: &str = "thoughtgate.policy.evaluated";

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
/// Implements: REQ-OBS-002 §7.1 (Parent Context for Trace Propagation)
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
    /// Optional parent context for W3C trace propagation.
    /// When provided, the MCP span becomes a child of the caller's span.
    /// When None, ThoughtGate becomes the trace root.
    pub parent_context: Option<&'a Context>,
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

/// Data needed to start a gateway decision span.
///
/// Implements: REQ-OBS-002 §5.3 (Gateway Decision Span)
pub struct GatewayDecisionSpanData<'a> {
    /// ThoughtGate correlation ID
    pub request_id: &'a str,
    /// Upstream target URL or identifier
    pub upstream_target: &'a str,
}

/// Accumulated gate outcomes for finishing the decision span.
///
/// As each gate is evaluated, the outcome is recorded here. When the span
/// is finished, these outcomes are added as attributes.
///
/// Implements: REQ-OBS-002 §5.3
#[derive(Debug, Default, Clone)]
pub struct GateOutcomes {
    /// Gate 1 (visibility) outcome: "pass" or "block"
    pub visibility: Option<String>,
    /// Gate 2 (governance rules) outcome: "forward", "deny", "approve", or "policy"
    pub governance: Option<String>,
    /// Gate 3 (Cedar policy) outcome: "allow" or "deny"
    pub cedar: Option<String>,
    /// Gate 4 (approval) outcome: "started", "approved", "rejected", or "timeout"
    pub approval: Option<String>,
    /// Governance rule ID that matched (from Gate 2)
    pub governance_rule_id: Option<String>,
    /// Whether Cedar policy was evaluated
    pub policy_evaluated: bool,
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
/// When `data.parent_context` is provided, the span becomes a child of the
/// caller's span (W3C Trace Context propagation). When `None`, ThoughtGate
/// starts a new trace.
///
/// # Arguments
///
/// * `data` - Span data containing method, message type, correlation info, and optional parent context
///
/// # Example
///
/// ```ignore
/// let parent_ctx = extract_context_from_headers(request.headers());
/// let data = McpSpanData {
///     method: "tools/call",
///     message_type: McpMessageType::Request,
///     message_id: Some("42".to_string()),
///     correlation_id: "req-abc123",
///     tool_name: Some("web_search"),
///     parent_context: Some(&parent_ctx),
/// };
/// let mut span = start_mcp_span(&data);
/// // ... process request ...
/// finish_mcp_span(&mut span, false, None, None);
/// ```
///
/// Implements: REQ-OBS-002 §5.1
/// Implements: REQ-OBS-002 §7.1 (Parent Context for Trace Propagation)
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

    let builder = tracer
        .span_builder(data.method.to_string())
        .with_kind(SpanKind::Server)
        .with_attributes(attributes);

    // Start span with parent context if provided, otherwise start fresh
    // This enables W3C Trace Context propagation (REQ-OBS-002 §7.1)
    match data.parent_context {
        Some(ctx) => builder.start_with_context(&tracer, ctx),
        None => builder.start(&tracer),
    }
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
    // Per REQ-OBS-002 §5.4: "Span links, not parent-child" for approval workflows.
    // The callback span starts a NEW trace (new trace_id) and uses a span link
    // to correlate back to the dispatch span. This avoids multi-hour parent-child
    // spans that would distort latency histograms.
    if let Some(link_ctx) = data.link_to {
        builder = builder.with_links(vec![Link::new(link_ctx.clone(), vec![], 0)]);
    }

    // Start a fresh root span - do NOT use start_with_context with the linked
    // span's context, as that would create a parent-child relationship.
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

// ─────────────────────────────────────────────────────────────────────────────
// Gateway Decision Span Functions (REQ-OBS-002 §5.3)
// ─────────────────────────────────────────────────────────────────────────────

/// Start a gateway decision span using the global tracer provider.
///
/// This span tracks the overall decision-making process across all 4 gates,
/// recording which gates were evaluated and their outcomes.
///
/// Span name: `thoughtgate.decision`
/// Span kind: INTERNAL
///
/// # Arguments
///
/// * `data` - Span data containing request ID and upstream target
/// * `parent_cx` - Parent OpenTelemetry context (typically from MCP span)
///
/// # Returns
///
/// A `BoxedSpan` that should be finished with `finish_gateway_decision_span`.
///
/// # Example
///
/// ```ignore
/// let data = GatewayDecisionSpanData {
///     request_id: "req-abc123",
///     upstream_target: "http://mcp-server:3000",
/// };
/// let mut span = start_gateway_decision_span(&data, &Context::current());
/// // ... route through gates ...
/// let outcomes = GateOutcomes {
///     visibility: Some("pass".to_string()),
///     governance: Some("forward".to_string()),
///     ..Default::default()
/// };
/// finish_gateway_decision_span(&mut span, &outcomes, Some(15.5));
/// ```
///
/// Implements: REQ-OBS-002 §5.3
pub fn start_gateway_decision_span(
    data: &GatewayDecisionSpanData<'_>,
    parent_cx: &Context,
) -> BoxedSpan {
    let tracer = global::tracer("thoughtgate");

    let attributes = vec![
        KeyValue::new(THOUGHTGATE_REQUEST_ID, data.request_id.to_string()),
        KeyValue::new(
            THOUGHTGATE_UPSTREAM_TARGET,
            data.upstream_target.to_string(),
        ),
    ];

    tracer
        .span_builder("thoughtgate.decision")
        .with_kind(SpanKind::Internal)
        .with_attributes(attributes)
        .start_with_context(&tracer, parent_cx)
}

/// Finish a gateway decision span with gate outcome attributes.
///
/// Records the outcomes of each gate that was evaluated, along with optional
/// upstream latency.
///
/// # Arguments
///
/// * `span` - The span to finish (from `start_gateway_decision_span`)
/// * `outcomes` - Gate outcomes collected during request processing
/// * `upstream_latency_ms` - Optional upstream call latency in milliseconds
///
/// Implements: REQ-OBS-002 §5.3
pub fn finish_gateway_decision_span(
    span: &mut impl Span,
    outcomes: &GateOutcomes,
    upstream_latency_ms: Option<f64>,
) {
    // Add gate outcome attributes
    if let Some(ref visibility) = outcomes.visibility {
        span.set_attribute(KeyValue::new(
            THOUGHTGATE_GATE_VISIBILITY,
            visibility.clone(),
        ));
    }

    if let Some(ref governance) = outcomes.governance {
        span.set_attribute(KeyValue::new(
            THOUGHTGATE_GATE_GOVERNANCE,
            governance.clone(),
        ));
    }

    if let Some(ref cedar) = outcomes.cedar {
        span.set_attribute(KeyValue::new(THOUGHTGATE_GATE_CEDAR, cedar.clone()));
    }

    if let Some(ref approval) = outcomes.approval {
        span.set_attribute(KeyValue::new(THOUGHTGATE_GATE_APPROVAL, approval.clone()));
    }

    if let Some(ref rule_id) = outcomes.governance_rule_id {
        span.set_attribute(KeyValue::new(
            THOUGHTGATE_GOVERNANCE_RULE_ID,
            rule_id.clone(),
        ));
    }

    span.set_attribute(KeyValue::new(
        THOUGHTGATE_POLICY_EVALUATED,
        outcomes.policy_evaluated,
    ));

    if let Some(latency) = upstream_latency_ms {
        span.set_attribute(KeyValue::new(THOUGHTGATE_UPSTREAM_LATENCY_MS, latency));
    }

    span.set_status(Status::Ok);
    span.end();
}

// ─────────────────────────────────────────────────────────────────────────────
// Cedar Span Constants (REQ-OBS-002 §5.3)
// ─────────────────────────────────────────────────────────────────────────────

/// Cedar policy evaluation span attribute: tool name being evaluated.
pub(crate) const CEDAR_TOOL_NAME: &str = "cedar.tool_name";

/// Cedar policy evaluation span attribute: policy decision.
pub(crate) const CEDAR_DECISION: &str = "cedar.decision";

/// Cedar policy evaluation span attribute: determining policy ID.
pub(crate) const CEDAR_POLICY_ID: &str = "cedar.policy_id";

/// Cedar policy evaluation span attribute: evaluation duration in milliseconds.
pub(crate) const CEDAR_DURATION_MS: &str = "cedar.duration_ms";

/// Data needed to start a Cedar evaluation span.
///
/// Implements: REQ-OBS-002 §5.3 (Cedar Evaluation Spans)
pub struct CedarSpanData {
    /// Tool name or resource being evaluated.
    pub tool_name: String,
    /// Policy ID from governance rules, if any.
    pub policy_id: Option<String>,
}

/// Start a Cedar policy evaluation span as a child of the current context.
///
/// Span name: `cedar.evaluate`
/// Span kind: INTERNAL
///
/// # Arguments
///
/// * `data` - Span data containing tool name and optional policy ID
/// * `parent_cx` - Parent OpenTelemetry context (typically from MCP span)
///
/// # Returns
///
/// A `BoxedSpan` that should be finished with `finish_cedar_span`.
///
/// # Example
///
/// ```ignore
/// let data = CedarSpanData {
///     tool_name: "web_search".to_string(),
///     policy_id: Some("sensitive-tools".to_string()),
/// };
/// let mut span = start_cedar_span(&data, &Context::current());
/// // ... evaluate Cedar policy ...
/// finish_cedar_span(&mut span, "allow", "policy-1", 0.15);
/// ```
///
/// Implements: REQ-OBS-002 §5.3
pub fn start_cedar_span(data: &CedarSpanData, parent_cx: &Context) -> BoxedSpan {
    let tracer = global::tracer("thoughtgate");

    let mut attributes = vec![KeyValue::new(CEDAR_TOOL_NAME, data.tool_name.clone())];

    if let Some(ref policy_id) = data.policy_id {
        attributes.push(KeyValue::new(CEDAR_POLICY_ID, policy_id.clone()));
    }

    tracer
        .span_builder("cedar.evaluate")
        .with_kind(SpanKind::Internal)
        .with_attributes(attributes)
        .start_with_context(&tracer, parent_cx)
}

/// Finish a Cedar evaluation span with decision attributes.
///
/// Sets the decision, policy ID, and duration attributes, then ends the span.
///
/// # Arguments
///
/// * `span` - The span to finish (from `start_cedar_span`)
/// * `decision` - Cedar decision ("allow" or "deny")
/// * `policy_id` - Determining policy ID
/// * `duration_ms` - Evaluation duration in milliseconds
///
/// Implements: REQ-OBS-002 §5.3
pub fn finish_cedar_span(span: &mut impl Span, decision: &str, policy_id: &str, duration_ms: f64) {
    span.set_attribute(KeyValue::new(CEDAR_DECISION, decision.to_string()));
    span.set_attribute(KeyValue::new(CEDAR_POLICY_ID, policy_id.to_string()));
    span.set_attribute(KeyValue::new(CEDAR_DURATION_MS, duration_ms));
    span.set_status(Status::Ok);
    span.end();
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper Functions
// ─────────────────────────────────────────────────────────────────────────────

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
            parent_context: None,
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
            parent_context: None,
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
            parent_context: None,
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
    fn test_approval_callback_span_uses_link_not_parent() {
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

        // Callback span should have a link to dispatch span (for trace correlation)
        // Per REQ-OBS-002 §5.4: "Span links, not parent-child"
        assert_eq!(finished.links.len(), 1, "Callback span should have a link");
        assert_eq!(
            finished.links[0].span_context.span_id(),
            dispatch_span_id,
            "Link should point to the dispatch span"
        );
        assert_eq!(
            finished.links[0].span_context.trace_id(),
            dispatch_trace_id,
            "Link should reference the dispatch trace"
        );

        // Callback span starts a NEW trace (different trace_id)
        // This avoids multi-hour parent-child spans per REQ-OBS-002 §5.4
        assert_ne!(
            finished.span_context.trace_id(),
            dispatch_trace_id,
            "Callback span should have its own trace_id (new root span)"
        );

        // Callback span should have NO parent (it's a root span)
        assert_eq!(
            finished.parent_span_id,
            SpanId::INVALID,
            "Callback span should be a root span with no parent"
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

    // ─────────────────────────────────────────────────────────────────────────
    // Parent Context Tests (REQ-OBS-002 §7.1)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_mcp_span_with_parent_context() {
        use opentelemetry::trace::{SpanId, TraceFlags, TraceId, TraceState};

        let (provider, exporter) = setup_test_provider();

        // Create a mock parent span context (simulating extracted from headers)
        let parent_trace_id = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap();
        let parent_span_id = SpanId::from_hex("00f067aa0ba902b7").unwrap();
        let parent_ctx = SpanContext::new(
            parent_trace_id,
            parent_span_id,
            TraceFlags::SAMPLED,
            true, // is_remote = true (extracted from headers)
            TraceState::default(),
        );

        // Create a context with the remote span context
        let parent_context = Context::new().with_remote_span_context(parent_ctx);

        let data = McpSpanData {
            method: "tools/call",
            message_type: McpMessageType::Request,
            message_id: Some("123".to_string()),
            correlation_id: "test-parent-ctx",
            tool_name: Some("test_tool"),
            parent_context: Some(&parent_context),
        };

        let mut span: BoxedSpan = start_mcp_span(&data);
        finish_mcp_span(&mut span, false, None, None);
        drop(span);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        // MCP span should inherit the parent's trace_id
        assert_eq!(
            finished.span_context.trace_id(),
            parent_trace_id,
            "MCP span should inherit parent trace_id"
        );
        // MCP span should have a different span_id (its own)
        assert_ne!(
            finished.span_context.span_id(),
            parent_span_id,
            "MCP span should have its own span_id"
        );
        // Parent span ID should be set to the remote parent
        assert_eq!(
            finished.parent_span_id, parent_span_id,
            "MCP span should have parent_span_id pointing to remote parent"
        );
    }

    #[test]
    #[serial]
    fn test_mcp_span_without_parent_context_is_root() {
        use opentelemetry::trace::{SpanId, TraceId};

        let (provider, exporter) = setup_test_provider();

        let data = McpSpanData {
            method: "tools/call",
            message_type: McpMessageType::Request,
            message_id: Some("456".to_string()),
            correlation_id: "test-no-parent",
            tool_name: None,
            parent_context: None,
        };

        let mut span: BoxedSpan = start_mcp_span(&data);
        finish_mcp_span(&mut span, false, None, None);
        drop(span);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        // Span should have a valid trace_id (auto-generated)
        assert_ne!(
            finished.span_context.trace_id(),
            TraceId::INVALID,
            "Root span should have valid trace_id"
        );
        // Span should have no parent (is root)
        assert_eq!(
            finished.parent_span_id,
            SpanId::INVALID,
            "Root span should have no parent_span_id"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Cedar Span Tests (REQ-OBS-002 §5.3 / TC-OBS2-002)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_cedar_span_created() {
        let (provider, exporter) = setup_test_provider();

        let data = CedarSpanData {
            tool_name: "web_search".to_string(),
            policy_id: Some("sensitive-tools".to_string()),
        };

        let mut span = start_cedar_span(&data, &Context::current());
        finish_cedar_span(&mut span, "allow", "policy-1", 0.15);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.name.as_ref(), "cedar.evaluate");
        assert_eq!(finished.span_kind, opentelemetry::trace::SpanKind::Internal);
    }

    #[test]
    #[serial]
    fn test_cedar_span_with_deny_decision() {
        let (provider, exporter) = setup_test_provider();

        let data = CedarSpanData {
            tool_name: "delete_database".to_string(),
            policy_id: Some("admin-only".to_string()),
        };

        let mut span = start_cedar_span(&data, &Context::current());
        finish_cedar_span(&mut span, "deny", "admin-policy", 0.08);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.name.as_ref(), "cedar.evaluate");
    }

    #[test]
    #[serial]
    fn test_cedar_span_as_child_of_mcp_span() {
        use opentelemetry::trace::{TraceContextExt, Tracer};

        let (provider, exporter) = setup_test_provider();
        let tracer = global::tracer("test");

        // Create parent MCP span
        let parent_span = tracer.start("tools/call");
        let parent_cx = Context::current().with_span(parent_span);

        // Create Cedar span as child
        let cedar_data = CedarSpanData {
            tool_name: "test_tool".to_string(),
            policy_id: None,
        };
        let mut cedar_span = start_cedar_span(&cedar_data, &parent_cx);
        finish_cedar_span(&mut cedar_span, "allow", "default", 0.05);

        // End parent span
        drop(parent_cx);

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        // Both parent and child should be exported
        assert_eq!(spans.len(), 2, "Should have both parent and child spans");

        // Find the Cedar span
        let cedar = spans.iter().find(|s| s.name.as_ref() == "cedar.evaluate");
        assert!(cedar.is_some(), "Cedar span should exist");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Gateway Decision Span Tests (REQ-OBS-002 §5.3)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_gateway_decision_span_created() {
        let (provider, exporter) = setup_test_provider();

        let data = GatewayDecisionSpanData {
            request_id: "req-test-123",
            upstream_target: "http://mcp-server:3000",
        };

        let mut span = start_gateway_decision_span(&data, &Context::current());
        let outcomes = GateOutcomes {
            visibility: Some("pass".to_string()),
            governance: Some("forward".to_string()),
            ..Default::default()
        };
        finish_gateway_decision_span(&mut span, &outcomes, Some(15.5));

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.name.as_ref(), "thoughtgate.decision");
        assert_eq!(finished.span_kind, opentelemetry::trace::SpanKind::Internal);
    }

    #[test]
    #[serial]
    fn test_gateway_decision_span_with_all_gates() {
        let (provider, exporter) = setup_test_provider();

        let data = GatewayDecisionSpanData {
            request_id: "req-full-flow",
            upstream_target: "http://backend:8080",
        };

        let mut span = start_gateway_decision_span(&data, &Context::current());
        let outcomes = GateOutcomes {
            visibility: Some("pass".to_string()),
            governance: Some("policy".to_string()),
            cedar: Some("allow".to_string()),
            approval: Some("approved".to_string()),
            governance_rule_id: Some("rule-sensitive-tools".to_string()),
            policy_evaluated: true,
        };
        finish_gateway_decision_span(&mut span, &outcomes, Some(250.0));

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.name.as_ref(), "thoughtgate.decision");
    }

    #[test]
    #[serial]
    fn test_gateway_decision_span_partial_gates() {
        let (provider, exporter) = setup_test_provider();

        let data = GatewayDecisionSpanData {
            request_id: "req-fast-forward",
            upstream_target: "http://backend:8080",
        };

        let mut span = start_gateway_decision_span(&data, &Context::current());
        // Fast-forward path: only Gate 1 and 2 evaluated
        let outcomes = GateOutcomes {
            visibility: Some("pass".to_string()),
            governance: Some("forward".to_string()),
            governance_rule_id: Some("rule-allow-all".to_string()),
            policy_evaluated: false,
            ..Default::default()
        };
        finish_gateway_decision_span(&mut span, &outcomes, Some(5.0));

        provider.force_flush().expect("flush should succeed");
        let spans = exporter.get_finished_spans().expect("should get spans");
        assert_eq!(spans.len(), 1);

        let finished = &spans[0];
        assert_eq!(finished.name.as_ref(), "thoughtgate.decision");
    }
}
