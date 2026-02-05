//! Prometheus metrics using prometheus-client crate.
//!
//! This module provides the `ThoughtGateMetrics` struct which registers and manages
//! all Prometheus metrics for the ThoughtGate proxy. Metrics are exported via the
//! `/metrics` endpoint on the admin port using OpenMetrics text format.
//!
//! # Performance Note
//!
//! Label fields use `Cow<'static, str>` to avoid heap allocations for static string
//! values (like method names "tools/call", status "success", etc.). This reduces
//! allocator pressure in hot paths. For dynamic values (like tool names from
//! cardinality limiting), `Cow::Owned` is used.
//!
//! # Traceability
//! - Implements: REQ-OBS-002 §6.1 (Counters)
//! - Implements: REQ-OBS-002 §6.2 (Histograms)
//! - Implements: REQ-OBS-002 §6.4 (Gauges)
//! - Implements: REQ-OBS-002 §6.5 (Cardinality Management)

use std::borrow::Cow;

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;

use super::cardinality::CardinalityLimiter;

// ─────────────────────────────────────────────────────────────────────────────
// Label Sets (prometheus-client requires #[derive(EncodeLabelSet)])
// ─────────────────────────────────────────────────────────────────────────────

/// Labels for request counters and duration histograms.
///
/// Uses `Cow<'static, str>` to avoid heap allocations for static label values.
///
/// Implements: REQ-OBS-002 §6.1/MC-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RequestLabels {
    /// JSON-RPC method name (e.g., "tools/call", "resources/read")
    pub method: Cow<'static, str>,
    /// Tool name for tools/call requests, "none" otherwise
    pub tool_name: Cow<'static, str>,
    /// Request outcome: "success" or "error"
    pub status: Cow<'static, str>,
}

/// Labels for gate decision counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-002
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DecisionLabels {
    /// Gate type (e.g., "cedar", "governance_rule")
    pub gate: Cow<'static, str>,
    /// Decision outcome (e.g., "allow", "deny", "approve")
    pub outcome: Cow<'static, str>,
}

/// Labels for error counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-003
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ErrorLabels {
    /// Error classification (e.g., "policy_denied", "upstream_timeout")
    pub error_type: Cow<'static, str>,
    /// JSON-RPC method that caused the error
    pub method: Cow<'static, str>,
}

/// Labels for Cedar evaluation counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-004
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct CedarEvalLabels {
    /// Cedar decision (e.g., "allow", "deny")
    pub decision: Cow<'static, str>,
    /// Determining policy ID
    pub policy_id: Cow<'static, str>,
}

/// Labels for approval request counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-005
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ApprovalLabels {
    /// Approval channel (e.g., "slack", "api")
    pub channel: Cow<'static, str>,
    /// Approval outcome (e.g., "approved", "rejected", "timeout")
    pub outcome: Cow<'static, str>,
}

/// Labels for upstream request counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-006
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct UpstreamLabels {
    /// Upstream target identifier
    pub target: Cow<'static, str>,
    /// HTTP status code as string
    pub status_code: Cow<'static, str>,
}

/// Labels for telemetry dropped counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-009
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct SignalLabels {
    /// Signal type (e.g., "span", "metric", "log")
    pub signal: Cow<'static, str>,
}

/// Labels for request duration histograms.
///
/// Implements: REQ-OBS-002 §6.2/MH-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DurationLabels {
    /// JSON-RPC method name
    pub method: Cow<'static, str>,
    /// Tool name for tools/call requests, "none" otherwise
    pub tool_name: Cow<'static, str>,
}

/// Labels for Cedar evaluation duration histograms.
///
/// Implements: REQ-OBS-002 §6.2/MH-002
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct CedarDurationLabels {
    /// Cedar decision (e.g., "allow", "deny")
    pub decision: Cow<'static, str>,
}

/// Labels for upstream duration histograms.
///
/// Implements: REQ-OBS-002 §6.2/MH-003
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TargetLabels {
    /// Upstream target identifier
    pub target: Cow<'static, str>,
}

/// Labels for payload size histograms.
///
/// Implements: REQ-OBS-002 §6.2/MH-005
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PayloadLabels {
    /// Direction: "request" or "response"
    pub direction: Cow<'static, str>,
    /// JSON-RPC method name
    pub method: Cow<'static, str>,
}

/// Labels for active connection gauges.
///
/// Implements: REQ-OBS-002 §6.4/MG-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TransportLabels {
    /// Transport type (e.g., "http", "stdio")
    pub transport: Cow<'static, str>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Green Path Labels (REQ-CORE-001)
// ─────────────────────────────────────────────────────────────────────────────

/// Labels for green path byte counters.
///
/// Implements: REQ-CORE-001 NFR-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct GreenPathLabels {
    /// Direction: "upload" or "download"
    pub direction: Cow<'static, str>,
}

/// Labels for green path stream outcome counters.
///
/// Implements: REQ-CORE-001 NFR-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StreamOutcomeLabels {
    /// Outcome: "success", "error", or "upgrade"
    pub outcome: Cow<'static, str>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Amber Path Labels (REQ-CORE-002)
// ─────────────────────────────────────────────────────────────────────────────

/// Labels for per-inspector duration histograms.
///
/// Implements: REQ-CORE-002 NFR-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct InspectorLabels {
    /// Inspector name (cardinality-limited)
    pub inspector_name: Cow<'static, str>,
}

/// Labels for amber path inspection decision counters.
///
/// Implements: REQ-CORE-002 NFR-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct AmberDecisionLabels {
    /// Decision: "approve", "modify", or "reject"
    pub decision: Cow<'static, str>,
}

/// Labels for amber path error counters.
///
/// Implements: REQ-CORE-002 NFR-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct AmberErrorLabels {
    /// Error type: "timeout", "limit", "semaphore", "panic", "compressed", "dropped"
    pub error_type: Cow<'static, str>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Stdio Transport Labels (REQ-CORE-008)
// ─────────────────────────────────────────────────────────────────────────────

/// Labels for stdio message counters.
///
/// Implements: REQ-CORE-008 NFR-002
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StdioMessageLabels {
    /// Server identifier (cardinality-limited)
    pub server_id: Cow<'static, str>,
    /// Direction: "agent_to_server" or "server_to_agent"
    pub direction: Cow<'static, str>,
    /// JSON-RPC method name
    pub method: Cow<'static, str>,
}

/// Labels for stdio governance decision counters.
///
/// Implements: REQ-CORE-008 NFR-002
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StdioDecisionLabels {
    /// Server identifier (cardinality-limited)
    pub server_id: Cow<'static, str>,
    /// Decision: "forward", "deny", "pending_approval"
    pub decision: Cow<'static, str>,
    /// Profile name
    pub profile: Cow<'static, str>,
}

/// Labels for stdio framing error counters.
///
/// Implements: REQ-CORE-008 NFR-002
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StdioErrorLabels {
    /// Server identifier (cardinality-limited)
    pub server_id: Cow<'static, str>,
    /// Error type
    pub error_type: Cow<'static, str>,
}

/// Labels for stdio server-specific histograms.
///
/// Implements: REQ-CORE-008 NFR-002
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StdioServerLabels {
    /// Server identifier (cardinality-limited)
    pub server_id: Cow<'static, str>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Governance Pipeline Labels (REQ-GOV-002)
// ─────────────────────────────────────────────────────────────────────────────

/// Labels for governance pipeline failure counters.
///
/// Implements: REQ-GOV-002 NFR-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PipelineStageLabels {
    /// Pipeline stage that failed
    pub stage: Cow<'static, str>,
}

/// Labels for pending task gauges.
///
/// Implements: REQ-OBS-002 §6.4/MG-002
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TaskTypeLabels {
    /// Task type (e.g., "approval")
    pub task_type: Cow<'static, str>,
}

/// Labels for task created counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-007
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TaskCreatedLabels {
    /// Task type (e.g., "approval")
    pub task_type: Cow<'static, str>,
}

/// Labels for task completed counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-008
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TaskCompletedLabels {
    /// Task type (e.g., "approval")
    pub task_type: Cow<'static, str>,
    /// Task outcome (e.g., "completed", "failed", "expired", "cancelled")
    pub outcome: Cow<'static, str>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Histogram Bucket Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Request duration buckets in milliseconds.
const REQUEST_DURATION_BUCKETS: &[f64] = &[
    1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 5000.0,
];

/// Cedar evaluation duration buckets in milliseconds.
const CEDAR_BUCKETS: &[f64] = &[0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0, 10.0];

/// Upstream call duration buckets in milliseconds.
const UPSTREAM_BUCKETS: &[f64] = &[
    1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 5000.0, 10000.0,
];

/// Approval wait duration buckets in seconds.
const APPROVAL_BUCKETS: &[f64] = &[1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 900.0, 1800.0, 3600.0];

/// Payload size buckets in bytes.
const PAYLOAD_BUCKETS: &[f64] = &[
    128.0, 512.0, 1024.0, 4096.0, 16384.0, 65536.0, 262144.0, 1048576.0,
];

/// Green path TTFB buckets in seconds (sub-millisecond to 1 second).
const GREEN_TTFB_BUCKETS: &[f64] = &[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0];

/// Green path chunk size buckets in bytes.
const GREEN_CHUNK_BUCKETS: &[f64] = &[64.0, 256.0, 1024.0, 4096.0, 16384.0, 65536.0];

/// Amber path buffer size buckets in bytes (same as payload buckets).
const AMBER_BUFFER_BUCKETS: &[f64] = &[
    128.0, 512.0, 1024.0, 4096.0, 16384.0, 65536.0, 262144.0, 1048576.0,
];

/// Amber path duration buckets in seconds (sub-millisecond to 5 seconds).
const AMBER_DURATION_BUCKETS: &[f64] = &[0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.5, 1.0, 5.0];

/// Stdio approval latency buckets in seconds (1 second to 1 hour).
const STDIO_APPROVAL_BUCKETS: &[f64] = &[1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 900.0, 1800.0, 3600.0];

// ─────────────────────────────────────────────────────────────────────────────
// ThoughtGateMetrics
// ─────────────────────────────────────────────────────────────────────────────

/// Prometheus metrics for ThoughtGate.
///
/// All metric names use "thoughtgate_" prefix per Prometheus naming conventions.
/// This struct manages all counters, histograms, and gauges defined in REQ-OBS-002 §6.
///
/// # Cardinality Protection
///
/// The `tool_name` label is cardinality-limited to 200 distinct values. When
/// exceeded, new values are mapped to `"__other__"` to prevent unbounded time
/// series growth.
///
/// # Gauge Wiring
///
/// Gauges are wired to runtime state in their respective subsystems:
/// - `connections_active`: proxy main.rs connection accept/drop lifecycle
/// - `tasks_pending`: task.rs TaskStore create/decrement_pending_counters
/// - `cedar_policies_loaded`: engine.rs CedarEngine set_metrics/reload
/// - `uptime_seconds`: proxy main.rs background tick (15s interval)
/// - `config_reload_timestamp`: proxy main.rs on config load
///
/// Implements: REQ-OBS-002 §6.1-6.5
pub struct ThoughtGateMetrics {
    // ─────────────────────────────────────────────────────────────────────────
    // Counters (§6.1)
    // ─────────────────────────────────────────────────────────────────────────
    /// Total MCP requests processed.
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-001
    pub requests_total: Family<RequestLabels, Counter>,

    /// Decision counts per gate (cedar, governance_rule).
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-002
    pub decisions_total: Family<DecisionLabels, Counter>,

    /// Error counts by type and method.
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-003
    pub errors_total: Family<ErrorLabels, Counter>,

    /// Cedar policy evaluation counts by decision and policy_id.
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-004
    pub cedar_evaluations_total: Family<CedarEvalLabels, Counter>,

    /// Approval request counts by channel and outcome.
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-005
    pub approval_requests_total: Family<ApprovalLabels, Counter>,

    /// Upstream MCP server call counts by target and status_code.
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-006
    pub upstream_requests_total: Family<UpstreamLabels, Counter>,

    /// Telemetry items dropped due to full export queue.
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-009
    pub telemetry_dropped_total: Family<SignalLabels, Counter>,

    /// SEP-1686 tasks created by type.
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-007
    pub tasks_created_total: Family<TaskCreatedLabels, Counter>,

    /// SEP-1686 tasks completed by type and outcome.
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-008
    pub tasks_completed_total: Family<TaskCompletedLabels, Counter>,

    // ─────────────────────────────────────────────────────────────────────────
    // Histograms (§6.2)
    // ─────────────────────────────────────────────────────────────────────────
    /// End-to-end request latency in milliseconds.
    ///
    /// Implements: REQ-OBS-002 §6.2/MH-001
    pub request_duration_ms: Family<DurationLabels, Histogram>,

    /// Cedar policy evaluation latency in milliseconds.
    ///
    /// Implements: REQ-OBS-002 §6.2/MH-002
    pub cedar_evaluation_duration_ms: Family<CedarDurationLabels, Histogram>,

    /// Upstream MCP server call latency in milliseconds.
    ///
    /// Implements: REQ-OBS-002 §6.2/MH-003
    pub upstream_duration_ms: Family<TargetLabels, Histogram>,

    /// Approval wait time in seconds (from dispatch to callback).
    ///
    /// Implements: REQ-OBS-002 §6.2/MH-004
    pub approval_wait_duration_s: Family<ApprovalLabels, Histogram>,

    /// Request/response payload sizes in bytes.
    ///
    /// Implements: REQ-OBS-002 §6.2/MH-005
    pub request_payload_size_bytes: Family<PayloadLabels, Histogram>,

    // ─────────────────────────────────────────────────────────────────────────
    // Gauges (§6.4)
    // ─────────────────────────────────────────────────────────────────────────
    /// Active MCP connections by transport type.
    ///
    /// Wired: main.rs connection lifecycle hooks.
    ///
    /// Implements: REQ-OBS-002 §6.4/MG-001
    pub connections_active: Family<TransportLabels, Gauge>,

    /// Currently pending approval tasks by task_type.
    ///
    /// Wired: task.rs TaskStore add/remove.
    ///
    /// Implements: REQ-OBS-002 §6.4/MG-002
    pub tasks_pending: Family<TaskTypeLabels, Gauge>,

    /// Number of loaded Cedar policies.
    ///
    /// Wired: engine.rs PolicyEngine initialization.
    ///
    /// Implements: REQ-OBS-002 §6.4/MG-003
    pub cedar_policies_loaded: Gauge,

    /// Process uptime in seconds.
    ///
    /// Wired: main.rs startup instant tracking.
    ///
    /// Implements: REQ-OBS-002 §6.4/MG-004
    pub uptime_seconds: Gauge,

    /// Unix timestamp of last configuration reload.
    ///
    /// Implements: REQ-OBS-002 §6.4/MG-005
    pub config_reload_timestamp: Gauge,

    // ─────────────────────────────────────────────────────────────────────────
    // Lifecycle Metrics (REQ-CORE-005 NFR-001)
    // ─────────────────────────────────────────────────────────────────────────
    /// Startup duration in seconds (set once when service becomes ready).
    ///
    /// Wired: main.rs on mark_ready().
    ///
    /// Implements: REQ-CORE-005/NFR-001
    pub startup_duration_seconds: Gauge,

    /// Currently active requests being processed.
    ///
    /// Wired: main.rs connection accept/complete lifecycle.
    ///
    /// Implements: REQ-CORE-005/NFR-001
    pub active_requests: Gauge,

    /// Total number of drain timeouts (counter).
    ///
    /// Wired: main.rs shutdown sequence on DrainResult::Timeout.
    ///
    /// Implements: REQ-CORE-005/NFR-001
    pub drain_timeout_total: Counter,

    // ─────────────────────────────────────────────────────────────────────────
    // Green Path Metrics (REQ-CORE-001)
    // ─────────────────────────────────────────────────────────────────────────
    /// Total bytes transferred through green path.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub green_bytes_total: Family<GreenPathLabels, Counter>,

    /// Active green path streams.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub green_streams_active: Gauge,

    /// Total green path streams by outcome.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub green_streams_total: Family<StreamOutcomeLabels, Counter>,

    /// Time-to-first-byte for green path streams in seconds.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub green_ttfb_seconds: Histogram,

    /// Chunk sizes for green path streams in bytes.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub green_chunk_size_bytes: Histogram,

    // ─────────────────────────────────────────────────────────────────────────
    // Amber Path Metrics (REQ-CORE-002)
    // ─────────────────────────────────────────────────────────────────────────
    /// Buffered payload sizes in bytes.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub amber_buffer_size_bytes: Histogram,

    /// Total amber path operation duration in seconds.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub amber_duration_seconds: Histogram,

    /// Per-inspector execution duration in seconds.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub amber_inspector_duration_seconds: Family<InspectorLabels, Histogram>,

    /// Inspection decisions by decision type.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub amber_inspections_total: Family<AmberDecisionLabels, Counter>,

    /// Amber path errors by error type.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub amber_errors_total: Family<AmberErrorLabels, Counter>,

    /// Currently active amber path buffers.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub amber_buffers_active: Gauge,

    // ─────────────────────────────────────────────────────────────────────────
    // Stdio Transport Metrics (REQ-CORE-008)
    // ─────────────────────────────────────────────────────────────────────────
    /// Total stdio messages processed.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub stdio_messages_total: Family<StdioMessageLabels, Counter>,

    /// Governance decisions for stdio messages.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub stdio_governance_decisions_total: Family<StdioDecisionLabels, Counter>,

    /// Framing errors for stdio transport.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub stdio_framing_errors_total: Family<StdioErrorLabels, Counter>,

    /// Currently active stdio-managed servers.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub stdio_active_servers: Gauge,

    /// Approval latency for stdio requests in seconds.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub stdio_approval_latency_seconds: Family<StdioServerLabels, Histogram>,

    // ─────────────────────────────────────────────────────────────────────────
    // Governance Pipeline Metrics (REQ-GOV-002)
    // ─────────────────────────────────────────────────────────────────────────
    /// Pipeline execution failures by stage.
    ///
    /// Implements: REQ-GOV-002 NFR-001
    pub governance_pipeline_failures_total: Family<PipelineStageLabels, Counter>,

    /// Total scheduler poll operations.
    ///
    /// Implements: REQ-GOV-002 NFR-001
    pub governance_scheduler_polls_total: Counter,

    // ─────────────────────────────────────────────────────────────────────────
    // Internal State
    // ─────────────────────────────────────────────────────────────────────────
    /// Cardinality limiter for tool_name label (max 200 distinct values).
    tool_name_limiter: CardinalityLimiter,

    /// Cardinality limiter for server_id labels (max 50 distinct values).
    server_id_limiter: CardinalityLimiter,

    /// Cardinality limiter for inspector_name labels (max 50 distinct values).
    inspector_name_limiter: CardinalityLimiter,
}

impl ThoughtGateMetrics {
    /// Create and register all metrics with the given registry.
    ///
    /// This registers all counters, histograms, and gauges with the provided
    /// `prometheus_client::registry::Registry`. The registry can then be used
    /// to encode metrics for the `/metrics` endpoint.
    ///
    /// Implements: REQ-OBS-002 §6.1-6.4
    pub fn new(registry: &mut Registry) -> Self {
        // ─────────────────────────────────────────────────────────────────────
        // Counters
        // ─────────────────────────────────────────────────────────────────────

        let requests_total = Family::<RequestLabels, Counter>::default();
        registry.register(
            "thoughtgate_requests_total",
            "Total MCP requests processed",
            requests_total.clone(),
        );

        let decisions_total = Family::<DecisionLabels, Counter>::default();
        registry.register(
            "thoughtgate_decisions_total",
            "Decision counts per gate",
            decisions_total.clone(),
        );

        let errors_total = Family::<ErrorLabels, Counter>::default();
        registry.register(
            "thoughtgate_errors_total",
            "Error counts by type",
            errors_total.clone(),
        );

        let cedar_evaluations_total = Family::<CedarEvalLabels, Counter>::default();
        registry.register(
            "thoughtgate_cedar_evaluations_total",
            "Cedar policy evaluation counts",
            cedar_evaluations_total.clone(),
        );

        let approval_requests_total = Family::<ApprovalLabels, Counter>::default();
        registry.register(
            "thoughtgate_approval_requests_total",
            "Approval requests by channel and outcome",
            approval_requests_total.clone(),
        );

        let upstream_requests_total = Family::<UpstreamLabels, Counter>::default();
        registry.register(
            "thoughtgate_upstream_requests_total",
            "Upstream MCP server call counts",
            upstream_requests_total.clone(),
        );

        let telemetry_dropped_total = Family::<SignalLabels, Counter>::default();
        registry.register(
            "thoughtgate_telemetry_dropped_total",
            "Telemetry items dropped due to full export queue",
            telemetry_dropped_total.clone(),
        );

        let tasks_created_total = Family::<TaskCreatedLabels, Counter>::default();
        registry.register(
            "thoughtgate_tasks_created_total",
            "SEP-1686 tasks created by type",
            tasks_created_total.clone(),
        );

        let tasks_completed_total = Family::<TaskCompletedLabels, Counter>::default();
        registry.register(
            "thoughtgate_tasks_completed_total",
            "SEP-1686 tasks completed by type and outcome",
            tasks_completed_total.clone(),
        );

        // ─────────────────────────────────────────────────────────────────────
        // Histograms (with static bucket slices for efficiency)
        // ─────────────────────────────────────────────────────────────────────

        let request_duration_ms = Family::<DurationLabels, Histogram>::new_with_constructor(|| {
            Histogram::new(REQUEST_DURATION_BUCKETS.iter().copied())
        });
        registry.register(
            "thoughtgate_request_duration_ms",
            "End-to-end request latency in milliseconds",
            request_duration_ms.clone(),
        );

        let cedar_evaluation_duration_ms =
            Family::<CedarDurationLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(CEDAR_BUCKETS.iter().copied())
            });
        registry.register(
            "thoughtgate_cedar_evaluation_duration_ms",
            "Cedar policy evaluation latency in milliseconds",
            cedar_evaluation_duration_ms.clone(),
        );

        let upstream_duration_ms = Family::<TargetLabels, Histogram>::new_with_constructor(|| {
            Histogram::new(UPSTREAM_BUCKETS.iter().copied())
        });
        registry.register(
            "thoughtgate_upstream_duration_ms",
            "Upstream MCP server call latency in milliseconds",
            upstream_duration_ms.clone(),
        );

        let approval_wait_duration_s =
            Family::<ApprovalLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(APPROVAL_BUCKETS.iter().copied())
            });
        registry.register(
            "thoughtgate_approval_wait_duration_s",
            "Approval wait time in seconds",
            approval_wait_duration_s.clone(),
        );

        let request_payload_size_bytes =
            Family::<PayloadLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(PAYLOAD_BUCKETS.iter().copied())
            });
        registry.register(
            "thoughtgate_request_payload_size_bytes",
            "Request/response payload sizes in bytes",
            request_payload_size_bytes.clone(),
        );

        // ─────────────────────────────────────────────────────────────────────
        // Gauges (§6.4 — wired in respective subsystems, see struct docs)
        // ─────────────────────────────────────────────────────────────────────

        let connections_active = Family::<TransportLabels, Gauge>::default();
        registry.register(
            "thoughtgate_connections_active",
            "Active MCP connections",
            connections_active.clone(),
        );

        let tasks_pending = Family::<TaskTypeLabels, Gauge>::default();
        registry.register(
            "thoughtgate_tasks_pending",
            "Currently pending approval tasks",
            tasks_pending.clone(),
        );

        let cedar_policies_loaded = Gauge::default();
        registry.register(
            "thoughtgate_cedar_policies_loaded",
            "Number of loaded Cedar policies",
            cedar_policies_loaded.clone(),
        );

        let uptime_seconds = Gauge::default();
        registry.register(
            "thoughtgate_uptime_seconds",
            "Process uptime in seconds",
            uptime_seconds.clone(),
        );

        let config_reload_timestamp = Gauge::default();
        registry.register(
            "thoughtgate_config_reload_timestamp",
            "Unix timestamp of last configuration reload",
            config_reload_timestamp.clone(),
        );

        // ─────────────────────────────────────────────────────────────────────
        // Lifecycle Metrics (REQ-CORE-005 NFR-001)
        // ─────────────────────────────────────────────────────────────────────

        let startup_duration_seconds = Gauge::default();
        registry.register(
            "thoughtgate_startup_duration_seconds",
            "Time from process start to ready state in seconds",
            startup_duration_seconds.clone(),
        );

        let active_requests = Gauge::default();
        registry.register(
            "thoughtgate_active_requests",
            "Currently active requests being processed",
            active_requests.clone(),
        );

        let drain_timeout_total = Counter::default();
        registry.register(
            "thoughtgate_drain_timeout_total",
            "Total drain timeouts during shutdown",
            drain_timeout_total.clone(),
        );

        // ─────────────────────────────────────────────────────────────────────
        // Green Path Metrics (REQ-CORE-001)
        // ─────────────────────────────────────────────────────────────────────

        let green_bytes_total = Family::<GreenPathLabels, Counter>::default();
        registry.register(
            "thoughtgate_green_bytes_total",
            "Total bytes transferred through green path",
            green_bytes_total.clone(),
        );

        let green_streams_active = Gauge::default();
        registry.register(
            "thoughtgate_green_streams_active",
            "Active green path streams",
            green_streams_active.clone(),
        );

        let green_streams_total = Family::<StreamOutcomeLabels, Counter>::default();
        registry.register(
            "thoughtgate_green_streams_total",
            "Total green path streams by outcome",
            green_streams_total.clone(),
        );

        let green_ttfb_seconds = Histogram::new(GREEN_TTFB_BUCKETS.iter().copied());
        registry.register(
            "thoughtgate_green_ttfb_seconds",
            "Time-to-first-byte for green path streams",
            green_ttfb_seconds.clone(),
        );

        let green_chunk_size_bytes = Histogram::new(GREEN_CHUNK_BUCKETS.iter().copied());
        registry.register(
            "thoughtgate_green_chunk_size_bytes",
            "Chunk sizes for green path streams",
            green_chunk_size_bytes.clone(),
        );

        // ─────────────────────────────────────────────────────────────────────
        // Amber Path Metrics (REQ-CORE-002)
        // ─────────────────────────────────────────────────────────────────────

        let amber_buffer_size_bytes = Histogram::new(AMBER_BUFFER_BUCKETS.iter().copied());
        registry.register(
            "thoughtgate_amber_buffer_size_bytes",
            "Buffered payload sizes in bytes",
            amber_buffer_size_bytes.clone(),
        );

        let amber_duration_seconds = Histogram::new(AMBER_DURATION_BUCKETS.iter().copied());
        registry.register(
            "thoughtgate_amber_duration_seconds",
            "Total amber path operation duration",
            amber_duration_seconds.clone(),
        );

        let amber_inspector_duration_seconds =
            Family::<InspectorLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(AMBER_DURATION_BUCKETS.iter().copied())
            });
        registry.register(
            "thoughtgate_amber_inspector_duration_seconds",
            "Per-inspector execution duration",
            amber_inspector_duration_seconds.clone(),
        );

        let amber_inspections_total = Family::<AmberDecisionLabels, Counter>::default();
        registry.register(
            "thoughtgate_amber_inspections_total",
            "Inspection decisions by decision type",
            amber_inspections_total.clone(),
        );

        let amber_errors_total = Family::<AmberErrorLabels, Counter>::default();
        registry.register(
            "thoughtgate_amber_errors_total",
            "Amber path errors by error type",
            amber_errors_total.clone(),
        );

        let amber_buffers_active = Gauge::default();
        registry.register(
            "thoughtgate_amber_buffers_active",
            "Currently active amber path buffers",
            amber_buffers_active.clone(),
        );

        // ─────────────────────────────────────────────────────────────────────
        // Stdio Transport Metrics (REQ-CORE-008)
        // ─────────────────────────────────────────────────────────────────────

        let stdio_messages_total = Family::<StdioMessageLabels, Counter>::default();
        registry.register(
            "thoughtgate_stdio_messages_total",
            "Total stdio messages processed",
            stdio_messages_total.clone(),
        );

        let stdio_governance_decisions_total = Family::<StdioDecisionLabels, Counter>::default();
        registry.register(
            "thoughtgate_stdio_governance_decisions_total",
            "Governance decisions for stdio messages",
            stdio_governance_decisions_total.clone(),
        );

        let stdio_framing_errors_total = Family::<StdioErrorLabels, Counter>::default();
        registry.register(
            "thoughtgate_stdio_framing_errors_total",
            "Framing errors for stdio transport",
            stdio_framing_errors_total.clone(),
        );

        let stdio_active_servers = Gauge::default();
        registry.register(
            "thoughtgate_stdio_active_servers",
            "Currently active stdio-managed servers",
            stdio_active_servers.clone(),
        );

        let stdio_approval_latency_seconds =
            Family::<StdioServerLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(STDIO_APPROVAL_BUCKETS.iter().copied())
            });
        registry.register(
            "thoughtgate_stdio_approval_latency_seconds",
            "Approval latency for stdio requests",
            stdio_approval_latency_seconds.clone(),
        );

        // ─────────────────────────────────────────────────────────────────────
        // Governance Pipeline Metrics (REQ-GOV-002)
        // ─────────────────────────────────────────────────────────────────────

        let governance_pipeline_failures_total = Family::<PipelineStageLabels, Counter>::default();
        registry.register(
            "thoughtgate_governance_pipeline_failures_total",
            "Pipeline execution failures by stage",
            governance_pipeline_failures_total.clone(),
        );

        let governance_scheduler_polls_total = Counter::default();
        registry.register(
            "thoughtgate_governance_scheduler_polls_total",
            "Total scheduler poll operations",
            governance_scheduler_polls_total.clone(),
        );

        Self {
            requests_total,
            decisions_total,
            errors_total,
            cedar_evaluations_total,
            approval_requests_total,
            upstream_requests_total,
            telemetry_dropped_total,
            tasks_created_total,
            tasks_completed_total,
            request_duration_ms,
            cedar_evaluation_duration_ms,
            upstream_duration_ms,
            approval_wait_duration_s,
            request_payload_size_bytes,
            connections_active,
            tasks_pending,
            cedar_policies_loaded,
            uptime_seconds,
            config_reload_timestamp,
            // Lifecycle
            startup_duration_seconds,
            active_requests,
            drain_timeout_total,
            // Green Path
            green_bytes_total,
            green_streams_active,
            green_streams_total,
            green_ttfb_seconds,
            green_chunk_size_bytes,
            // Amber Path
            amber_buffer_size_bytes,
            amber_duration_seconds,
            amber_inspector_duration_seconds,
            amber_inspections_total,
            amber_errors_total,
            amber_buffers_active,
            // Stdio
            stdio_messages_total,
            stdio_governance_decisions_total,
            stdio_framing_errors_total,
            stdio_active_servers,
            stdio_approval_latency_seconds,
            // Governance
            governance_pipeline_failures_total,
            governance_scheduler_polls_total,
            // Cardinality limiters
            tool_name_limiter: CardinalityLimiter::new(200),
            server_id_limiter: CardinalityLimiter::new(50),
            inspector_name_limiter: CardinalityLimiter::new(50),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Convenience Methods
    // ─────────────────────────────────────────────────────────────────────────

    /// Record a request with cardinality-limited tool_name.
    ///
    /// # Arguments
    ///
    /// * `method` - JSON-RPC method name (e.g., "tools/call")
    /// * `tool_name` - Tool name for tools/call requests, None otherwise
    /// * `status` - Request outcome: "success" or "error"
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-001, §6.5
    pub fn record_request(&self, method: &str, tool_name: Option<&str>, status: &str) {
        let tool = tool_name
            .map(|t| self.tool_name_limiter.resolve(t))
            .unwrap_or("none");

        self.requests_total
            .get_or_create(&RequestLabels {
                method: Cow::Owned(method.to_string()),
                tool_name: Cow::Owned(tool.to_string()),
                status: Cow::Owned(status.to_string()),
            })
            .inc();
    }

    /// Record request duration in milliseconds.
    ///
    /// # Arguments
    ///
    /// * `method` - JSON-RPC method name
    /// * `tool_name` - Tool name for tools/call requests, None otherwise
    /// * `duration_ms` - Duration in milliseconds
    ///
    /// Implements: REQ-OBS-002 §6.2/MH-001
    pub fn record_request_duration(&self, method: &str, tool_name: Option<&str>, duration_ms: f64) {
        let tool = tool_name
            .map(|t| self.tool_name_limiter.resolve(t))
            .unwrap_or("none");

        self.request_duration_ms
            .get_or_create(&DurationLabels {
                method: Cow::Owned(method.to_string()),
                tool_name: Cow::Owned(tool.to_string()),
            })
            .observe(duration_ms);
    }

    /// Record a Cedar policy evaluation.
    ///
    /// # Arguments
    ///
    /// * `decision` - Cedar decision (e.g., "allow", "deny")
    /// * `policy_id` - Determining policy ID
    /// * `duration_ms` - Evaluation duration in milliseconds
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-004, §6.2/MH-002
    pub fn record_cedar_eval(&self, decision: &str, policy_id: &str, duration_ms: f64) {
        self.cedar_evaluations_total
            .get_or_create(&CedarEvalLabels {
                decision: Cow::Owned(decision.to_string()),
                policy_id: Cow::Owned(policy_id.to_string()),
            })
            .inc();

        self.cedar_evaluation_duration_ms
            .get_or_create(&CedarDurationLabels {
                decision: Cow::Owned(decision.to_string()),
            })
            .observe(duration_ms);
    }

    /// Record a gate decision.
    ///
    /// # Arguments
    ///
    /// * `gate` - Gate type (e.g., "cedar", "governance_rule")
    /// * `outcome` - Decision outcome (e.g., "allow", "deny", "approve")
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-002
    pub fn record_gate_decision(&self, gate: &str, outcome: &str) {
        self.decisions_total
            .get_or_create(&DecisionLabels {
                gate: Cow::Owned(gate.to_string()),
                outcome: Cow::Owned(outcome.to_string()),
            })
            .inc();
    }

    /// Record an error.
    ///
    /// # Arguments
    ///
    /// * `error_type` - Error classification (e.g., "policy_denied", "upstream_timeout")
    /// * `method` - JSON-RPC method that caused the error
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-003
    pub fn record_error(&self, error_type: &str, method: &str) {
        self.errors_total
            .get_or_create(&ErrorLabels {
                error_type: Cow::Owned(error_type.to_string()),
                method: Cow::Owned(method.to_string()),
            })
            .inc();
    }

    /// Record an upstream request.
    ///
    /// # Arguments
    ///
    /// * `target` - Upstream target identifier
    /// * `status_code` - HTTP status code as string
    /// * `duration_ms` - Request duration in milliseconds
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-006, §6.2/MH-003
    pub fn record_upstream_request(&self, target: &str, status_code: &str, duration_ms: f64) {
        self.upstream_requests_total
            .get_or_create(&UpstreamLabels {
                target: Cow::Owned(target.to_string()),
                status_code: Cow::Owned(status_code.to_string()),
            })
            .inc();

        self.upstream_duration_ms
            .get_or_create(&TargetLabels {
                target: Cow::Owned(target.to_string()),
            })
            .observe(duration_ms);
    }

    /// Record payload size.
    ///
    /// # Arguments
    ///
    /// * `direction` - "request" or "response"
    /// * `method` - JSON-RPC method name
    /// * `size_bytes` - Payload size in bytes
    ///
    /// Implements: REQ-OBS-002 §6.2/MH-005
    pub fn record_payload_size(&self, direction: &str, method: &str, size_bytes: f64) {
        self.request_payload_size_bytes
            .get_or_create(&PayloadLabels {
                direction: Cow::Owned(direction.to_string()),
                method: Cow::Owned(method.to_string()),
            })
            .observe(size_bytes);
    }

    /// Record an approval request.
    ///
    /// Increments the approval request counter for the given channel and outcome.
    ///
    /// # Arguments
    ///
    /// * `channel` - Approval channel (e.g., "slack", "webhook")
    /// * `outcome` - Approval outcome (e.g., "approved", "rejected", "timeout", "pending")
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-005
    pub fn record_approval_request(&self, channel: &str, outcome: &str) {
        self.approval_requests_total
            .get_or_create(&ApprovalLabels {
                channel: Cow::Owned(channel.to_string()),
                outcome: Cow::Owned(outcome.to_string()),
            })
            .inc();
    }

    /// Record approval wait duration.
    ///
    /// Records how long an approval took from dispatch to callback.
    ///
    /// # Arguments
    ///
    /// * `channel` - Approval channel (e.g., "slack", "webhook")
    /// * `outcome` - Approval outcome (e.g., "approved", "rejected", "timeout")
    /// * `duration_secs` - Wall-clock time from dispatch to callback in seconds
    ///
    /// Implements: REQ-OBS-002 §6.2/MH-004
    pub fn record_approval_wait_duration(&self, channel: &str, outcome: &str, duration_secs: f64) {
        self.approval_wait_duration_s
            .get_or_create(&ApprovalLabels {
                channel: Cow::Owned(channel.to_string()),
                outcome: Cow::Owned(outcome.to_string()),
            })
            .observe(duration_secs);
    }

    /// Record a SEP-1686 task creation.
    ///
    /// # Arguments
    ///
    /// * `task_type` - Type of task (e.g., "approval")
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-007
    pub fn record_task_created(&self, task_type: &str) {
        self.tasks_created_total
            .get_or_create(&TaskCreatedLabels {
                task_type: Cow::Owned(task_type.to_string()),
            })
            .inc();
    }

    /// Record a SEP-1686 task completion.
    ///
    /// # Arguments
    ///
    /// * `task_type` - Type of task (e.g., "approval")
    /// * `outcome` - Task outcome (e.g., "completed", "failed", "expired", "cancelled")
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-008
    pub fn record_task_completed(&self, task_type: &str, outcome: &str) {
        self.tasks_completed_total
            .get_or_create(&TaskCompletedLabels {
                task_type: Cow::Owned(task_type.to_string()),
                outcome: Cow::Owned(outcome.to_string()),
            })
            .inc();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Gauge Update Methods (§6.4)
    // ─────────────────────────────────────────────────────────────────────────

    /// Set the number of loaded Cedar policies.
    ///
    /// Call after initial policy load and each reload.
    ///
    /// Implements: REQ-OBS-002 §6.4/MG-003
    pub fn set_cedar_policies_loaded(&self, count: i64) {
        self.cedar_policies_loaded.set(count);
    }

    /// Set the process uptime in seconds.
    ///
    /// Called periodically or on scrape to update the uptime gauge.
    ///
    /// Implements: REQ-OBS-002 §6.4/MG-004
    pub fn set_uptime_seconds(&self, seconds: i64) {
        self.uptime_seconds.set(seconds);
    }

    /// Record configuration reload timestamp.
    ///
    /// Updates the config_reload_timestamp gauge to the current Unix timestamp.
    /// Call this whenever configuration is reloaded (startup or hot-reload).
    ///
    /// Implements: REQ-OBS-002 §6.4/MG-005
    pub fn record_config_reload(&self) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.config_reload_timestamp.set(timestamp);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Lifecycle Methods (REQ-CORE-005 NFR-001)
    // ─────────────────────────────────────────────────────────────────────────

    /// Record startup duration in seconds.
    ///
    /// Call once when the service transitions to Ready state.
    ///
    /// Implements: REQ-CORE-005/NFR-001
    pub fn record_startup_duration(&self, seconds: f64) {
        self.startup_duration_seconds.set(seconds as i64);
    }

    /// Set the active requests gauge.
    ///
    /// Implements: REQ-CORE-005/NFR-001
    pub fn set_active_requests(&self, count: i64) {
        self.active_requests.set(count);
    }

    /// Increment drain timeout counter.
    ///
    /// Call when a shutdown drain exceeds its timeout.
    ///
    /// Implements: REQ-CORE-005/NFR-001
    pub fn record_drain_timeout(&self) {
        self.drain_timeout_total.inc();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Green Path Methods (REQ-CORE-001)
    // ─────────────────────────────────────────────────────────────────────────

    /// Record bytes transferred through green path.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub fn record_green_bytes(&self, direction: &str, bytes: u64) {
        self.green_bytes_total
            .get_or_create(&GreenPathLabels {
                direction: Cow::Owned(direction.to_string()),
            })
            .inc_by(bytes);
    }

    /// Increment active green path streams.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub fn increment_green_streams_active(&self) {
        self.green_streams_active.inc();
    }

    /// Decrement active green path streams.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub fn decrement_green_streams_active(&self) {
        self.green_streams_active.dec();
    }

    /// Record a green path stream completion.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub fn record_green_stream(&self, outcome: &str) {
        self.green_streams_total
            .get_or_create(&StreamOutcomeLabels {
                outcome: Cow::Owned(outcome.to_string()),
            })
            .inc();
    }

    /// Record time-to-first-byte for green path stream.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub fn record_green_ttfb(&self, seconds: f64) {
        self.green_ttfb_seconds.observe(seconds);
    }

    /// Record chunk size for green path stream.
    ///
    /// Implements: REQ-CORE-001 NFR-001
    pub fn record_green_chunk_size(&self, bytes: u64) {
        self.green_chunk_size_bytes.observe(bytes as f64);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Amber Path Methods (REQ-CORE-002)
    // ─────────────────────────────────────────────────────────────────────────

    /// Record amber path buffer size.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub fn record_amber_buffer_size(&self, bytes: u64) {
        self.amber_buffer_size_bytes.observe(bytes as f64);
    }

    /// Record amber path operation duration.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub fn record_amber_duration(&self, seconds: f64) {
        self.amber_duration_seconds.observe(seconds);
    }

    /// Record per-inspector execution duration.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub fn record_amber_inspector_duration(&self, inspector_name: &str, seconds: f64) {
        let limited_name = self.inspector_name_limiter.resolve(inspector_name);
        self.amber_inspector_duration_seconds
            .get_or_create(&InspectorLabels {
                inspector_name: Cow::Owned(limited_name.to_string()),
            })
            .observe(seconds);
    }

    /// Record an inspection decision.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub fn record_amber_inspection(&self, decision: &str) {
        self.amber_inspections_total
            .get_or_create(&AmberDecisionLabels {
                decision: Cow::Owned(decision.to_string()),
            })
            .inc();
    }

    /// Record an amber path error.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub fn record_amber_error(&self, error_type: &str) {
        self.amber_errors_total
            .get_or_create(&AmberErrorLabels {
                error_type: Cow::Owned(error_type.to_string()),
            })
            .inc();
    }

    /// Increment active amber path buffers.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub fn increment_amber_buffers_active(&self) {
        self.amber_buffers_active.inc();
    }

    /// Decrement active amber path buffers.
    ///
    /// Implements: REQ-CORE-002 NFR-001
    pub fn decrement_amber_buffers_active(&self) {
        self.amber_buffers_active.dec();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Stdio Transport Methods (REQ-CORE-008)
    // ─────────────────────────────────────────────────────────────────────────

    /// Record a stdio message.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub fn record_stdio_message(&self, server_id: &str, direction: &str, method: &str) {
        let limited_id = self.server_id_limiter.resolve(server_id);
        self.stdio_messages_total
            .get_or_create(&StdioMessageLabels {
                server_id: Cow::Owned(limited_id.to_string()),
                direction: Cow::Owned(direction.to_string()),
                method: Cow::Owned(method.to_string()),
            })
            .inc();
    }

    /// Record a governance decision for stdio transport.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub fn record_stdio_governance_decision(&self, server_id: &str, decision: &str, profile: &str) {
        let limited_id = self.server_id_limiter.resolve(server_id);
        self.stdio_governance_decisions_total
            .get_or_create(&StdioDecisionLabels {
                server_id: Cow::Owned(limited_id.to_string()),
                decision: Cow::Owned(decision.to_string()),
                profile: Cow::Owned(profile.to_string()),
            })
            .inc();
    }

    /// Record a framing error for stdio transport.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub fn record_stdio_framing_error(&self, server_id: &str, error_type: &str) {
        let limited_id = self.server_id_limiter.resolve(server_id);
        self.stdio_framing_errors_total
            .get_or_create(&StdioErrorLabels {
                server_id: Cow::Owned(limited_id.to_string()),
                error_type: Cow::Owned(error_type.to_string()),
            })
            .inc();
    }

    /// Increment active stdio servers.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub fn increment_stdio_active_servers(&self) {
        self.stdio_active_servers.inc();
    }

    /// Decrement active stdio servers.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub fn decrement_stdio_active_servers(&self) {
        self.stdio_active_servers.dec();
    }

    /// Record approval latency for stdio transport.
    ///
    /// Implements: REQ-CORE-008 NFR-002
    pub fn record_stdio_approval_latency(&self, server_id: &str, duration: std::time::Duration) {
        let limited_id = self.server_id_limiter.resolve(server_id);
        self.stdio_approval_latency_seconds
            .get_or_create(&StdioServerLabels {
                server_id: Cow::Owned(limited_id.to_string()),
            })
            .observe(duration.as_secs_f64());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Governance Pipeline Methods (REQ-GOV-002)
    // ─────────────────────────────────────────────────────────────────────────

    /// Record a governance pipeline failure.
    ///
    /// Implements: REQ-GOV-002 NFR-001
    pub fn record_pipeline_failure(&self, stage: &str) {
        self.governance_pipeline_failures_total
            .get_or_create(&PipelineStageLabels {
                stage: Cow::Owned(stage.to_string()),
            })
            .inc();
    }

    /// Record a scheduler poll operation.
    ///
    /// Implements: REQ-GOV-002 NFR-001
    pub fn record_scheduler_poll(&self) {
        self.governance_scheduler_polls_total.inc();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_registration() {
        let mut registry = Registry::default();
        let metrics = ThoughtGateMetrics::new(&mut registry);

        // Record some metrics
        metrics.record_request("tools/call", Some("web_search"), "success");
        metrics.record_request_duration("tools/call", Some("web_search"), 42.5);
        metrics.record_cedar_eval("allow", "policy-1", 0.15);
        metrics.record_gate_decision("cedar", "allow");
        metrics.record_error("policy_denied", "tools/call");

        // Encode to verify registration worked
        let mut buffer = String::new();
        prometheus_client::encoding::text::encode(&mut buffer, &registry)
            .expect("encoding should succeed");

        assert!(buffer.contains("thoughtgate_requests_total"));
        assert!(buffer.contains("thoughtgate_request_duration_ms"));
        assert!(buffer.contains("thoughtgate_cedar_evaluations_total"));
    }

    #[test]
    fn test_cardinality_limiting() {
        let mut registry = Registry::default();
        let metrics = ThoughtGateMetrics::new(&mut registry);

        // Record 201 unique tool names (limit is 200)
        for i in 0..201 {
            metrics.record_request("tools/call", Some(&format!("tool_{}", i)), "success");
        }

        // The 201st should be mapped to __other__
        let mut buffer = String::new();
        prometheus_client::encoding::text::encode(&mut buffer, &registry)
            .expect("encoding should succeed");

        assert!(buffer.contains("__other__"));
    }

    #[test]
    fn test_histogram_buckets() {
        let mut registry = Registry::default();
        let metrics = ThoughtGateMetrics::new(&mut registry);

        // Record values that will fall into different buckets
        metrics.record_request_duration("tools/call", None, 0.5); // < 1ms
        metrics.record_request_duration("tools/call", None, 50.0); // 50ms bucket
        metrics.record_request_duration("tools/call", None, 2000.0); // > 1000ms

        let mut buffer = String::new();
        prometheus_client::encoding::text::encode(&mut buffer, &registry)
            .expect("encoding should succeed");

        // Verify histogram is present with bucket boundaries
        assert!(buffer.contains("thoughtgate_request_duration_ms_bucket"));
    }

    #[test]
    fn test_upstream_recording() {
        let mut registry = Registry::default();
        let metrics = ThoughtGateMetrics::new(&mut registry);

        metrics.record_upstream_request("upstream-1", "200", 150.0);
        metrics.record_upstream_request("upstream-1", "500", 50.0);

        let mut buffer = String::new();
        prometheus_client::encoding::text::encode(&mut buffer, &registry)
            .expect("encoding should succeed");

        assert!(buffer.contains("thoughtgate_upstream_requests_total"));
        assert!(buffer.contains("thoughtgate_upstream_duration_ms"));
    }

    #[test]
    fn test_payload_size_recording() {
        let mut registry = Registry::default();
        let metrics = ThoughtGateMetrics::new(&mut registry);

        metrics.record_payload_size("request", "tools/call", 1024.0);
        metrics.record_payload_size("response", "tools/call", 4096.0);

        let mut buffer = String::new();
        prometheus_client::encoding::text::encode(&mut buffer, &registry)
            .expect("encoding should succeed");

        assert!(buffer.contains("thoughtgate_request_payload_size_bytes"));
    }

    #[test]
    fn test_task_counters() {
        let mut registry = Registry::default();
        let metrics = ThoughtGateMetrics::new(&mut registry);

        // Record task lifecycle events
        metrics.record_task_created("approval");
        metrics.record_task_created("approval");
        metrics.record_task_completed("approval", "completed");
        metrics.record_task_completed("approval", "failed");
        metrics.record_task_completed("approval", "expired");

        let mut buffer = String::new();
        prometheus_client::encoding::text::encode(&mut buffer, &registry)
            .expect("encoding should succeed");

        assert!(buffer.contains("thoughtgate_tasks_created_total"));
        assert!(buffer.contains("thoughtgate_tasks_completed_total"));
        assert!(buffer.contains("task_type=\"approval\""));
        assert!(buffer.contains("outcome=\"completed\""));
        assert!(buffer.contains("outcome=\"failed\""));
        assert!(buffer.contains("outcome=\"expired\""));
    }
}
