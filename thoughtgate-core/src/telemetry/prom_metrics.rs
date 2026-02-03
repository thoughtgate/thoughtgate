//! Prometheus metrics using prometheus-client crate.
//!
//! This module provides the `ThoughtGateMetrics` struct which registers and manages
//! all Prometheus metrics for the ThoughtGate proxy. Metrics are exported via the
//! `/metrics` endpoint on the admin port using OpenMetrics text format.
//!
//! # Traceability
//! - Implements: REQ-OBS-002 §6.1 (Counters)
//! - Implements: REQ-OBS-002 §6.2 (Histograms)
//! - Implements: REQ-OBS-002 §6.4 (Gauges)
//! - Implements: REQ-OBS-002 §6.5 (Cardinality Management)

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
/// Implements: REQ-OBS-002 §6.1/MC-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RequestLabels {
    /// JSON-RPC method name (e.g., "tools/call", "resources/read")
    pub method: String,
    /// Tool name for tools/call requests, "none" otherwise
    pub tool_name: String,
    /// Request outcome: "success" or "error"
    pub status: String,
}

/// Labels for gate decision counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-002
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DecisionLabels {
    /// Gate type (e.g., "cedar", "governance_rule")
    pub gate: String,
    /// Decision outcome (e.g., "allow", "deny", "approve")
    pub outcome: String,
}

/// Labels for error counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-003
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ErrorLabels {
    /// Error classification (e.g., "policy_denied", "upstream_timeout")
    pub error_type: String,
    /// JSON-RPC method that caused the error
    pub method: String,
}

/// Labels for Cedar evaluation counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-004
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct CedarEvalLabels {
    /// Cedar decision (e.g., "allow", "deny")
    pub decision: String,
    /// Determining policy ID
    pub policy_id: String,
}

/// Labels for approval request counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-005
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ApprovalLabels {
    /// Approval channel (e.g., "slack", "api")
    pub channel: String,
    /// Approval outcome (e.g., "approved", "rejected", "timeout")
    pub outcome: String,
}

/// Labels for upstream request counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-006
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct UpstreamLabels {
    /// Upstream target identifier
    pub target: String,
    /// HTTP status code as string
    pub status_code: String,
}

/// Labels for telemetry dropped counters.
///
/// Implements: REQ-OBS-002 §6.1/MC-009
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct SignalLabels {
    /// Signal type (e.g., "span", "metric", "log")
    pub signal: String,
}

/// Labels for request duration histograms.
///
/// Implements: REQ-OBS-002 §6.2/MH-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DurationLabels {
    /// JSON-RPC method name
    pub method: String,
    /// Tool name for tools/call requests, "none" otherwise
    pub tool_name: String,
}

/// Labels for Cedar evaluation duration histograms.
///
/// Implements: REQ-OBS-002 §6.2/MH-002
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct CedarDurationLabels {
    /// Cedar decision (e.g., "allow", "deny")
    pub decision: String,
}

/// Labels for upstream duration histograms.
///
/// Implements: REQ-OBS-002 §6.2/MH-003
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TargetLabels {
    /// Upstream target identifier
    pub target: String,
}

/// Labels for payload size histograms.
///
/// Implements: REQ-OBS-002 §6.2/MH-005
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PayloadLabels {
    /// Direction: "request" or "response"
    pub direction: String,
    /// JSON-RPC method name
    pub method: String,
}

/// Labels for active connection gauges.
///
/// Implements: REQ-OBS-002 §6.4/MG-001
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TransportLabels {
    /// Transport type (e.g., "http", "stdio")
    pub transport: String,
}

/// Labels for pending task gauges.
///
/// Implements: REQ-OBS-002 §6.4/MG-002
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TaskTypeLabels {
    /// Task type (e.g., "approval")
    pub task_type: String,
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
/// # Gauge Updates
///
/// The gauges (connections_active, tasks_pending, cedar_policies_loaded, uptime_seconds)
/// are registered but NOT wired to update in this prompt. They require hooks into
/// lifecycle, connection tracking, and policy loading that span multiple subsystems.
/// Updates will be wired in Prompt 5 (Audit logging integration) or later.
///
/// Implements: REQ-OBS-002 §6.1-6.5
pub struct ThoughtGateMetrics {
    // ─────────────────────────────────────────────────────────────────────────
    // Counters (§6.1)
    // ─────────────────────────────────────────────────────────────────────────
    /// MC-001: Total MCP requests processed.
    pub requests_total: Family<RequestLabels, Counter>,

    /// MC-002: Decision counts per gate.
    pub decisions_total: Family<DecisionLabels, Counter>,

    /// MC-003: Error counts by type.
    pub errors_total: Family<ErrorLabels, Counter>,

    /// MC-004: Cedar policy evaluation counts.
    pub cedar_evaluations_total: Family<CedarEvalLabels, Counter>,

    /// MC-005: Approval request counts by channel and outcome.
    pub approval_requests_total: Family<ApprovalLabels, Counter>,

    /// MC-006: Upstream MCP server call counts.
    pub upstream_requests_total: Family<UpstreamLabels, Counter>,

    /// MC-009: Telemetry items dropped due to full export queue.
    pub telemetry_dropped_total: Family<SignalLabels, Counter>,

    // ─────────────────────────────────────────────────────────────────────────
    // Histograms (§6.2)
    // ─────────────────────────────────────────────────────────────────────────
    /// MH-001: End-to-end request latency in milliseconds.
    pub request_duration_ms: Family<DurationLabels, Histogram>,

    /// MH-002: Cedar policy evaluation latency in milliseconds.
    pub cedar_evaluation_duration_ms: Family<CedarDurationLabels, Histogram>,

    /// MH-003: Upstream MCP server call latency in milliseconds.
    pub upstream_duration_ms: Family<TargetLabels, Histogram>,

    /// MH-004: Approval wait time in seconds.
    pub approval_wait_duration_s: Family<ApprovalLabels, Histogram>,

    /// MH-005: Request/response payload sizes in bytes.
    pub request_payload_size_bytes: Family<PayloadLabels, Histogram>,

    // ─────────────────────────────────────────────────────────────────────────
    // Gauges (§6.4)
    // ─────────────────────────────────────────────────────────────────────────
    /// MG-001: Active MCP connections.
    /// TODO(Prompt 5+): Wire to connection lifecycle hooks.
    pub connections_active: Family<TransportLabels, Gauge>,

    /// MG-002: Currently pending approval tasks.
    /// TODO(Prompt 5+): Wire to TaskStore add/remove.
    pub tasks_pending: Family<TaskTypeLabels, Gauge>,

    /// MG-003: Number of loaded Cedar policies.
    /// TODO(Prompt 5+): Wire to PolicyEngine reload.
    pub cedar_policies_loaded: Gauge,

    /// MG-004: Process uptime in seconds.
    /// TODO(Prompt 5+): Wire to tokio::time::Instant at startup.
    pub uptime_seconds: Gauge,

    // ─────────────────────────────────────────────────────────────────────────
    // Internal State
    // ─────────────────────────────────────────────────────────────────────────
    /// Cardinality limiter for tool_name label (max 200 distinct values).
    tool_name_limiter: CardinalityLimiter,
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
        // Gauges (registered but NOT wired in this prompt - see struct docs)
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

        Self {
            requests_total,
            decisions_total,
            errors_total,
            cedar_evaluations_total,
            approval_requests_total,
            upstream_requests_total,
            telemetry_dropped_total,
            request_duration_ms,
            cedar_evaluation_duration_ms,
            upstream_duration_ms,
            approval_wait_duration_s,
            request_payload_size_bytes,
            connections_active,
            tasks_pending,
            cedar_policies_loaded,
            uptime_seconds,
            tool_name_limiter: CardinalityLimiter::new(200),
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
                method: method.to_string(),
                tool_name: tool.to_string(),
                status: status.to_string(),
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
                method: method.to_string(),
                tool_name: tool.to_string(),
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
                decision: decision.to_string(),
                policy_id: policy_id.to_string(),
            })
            .inc();

        self.cedar_evaluation_duration_ms
            .get_or_create(&CedarDurationLabels {
                decision: decision.to_string(),
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
                gate: gate.to_string(),
                outcome: outcome.to_string(),
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
                error_type: error_type.to_string(),
                method: method.to_string(),
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
                target: target.to_string(),
                status_code: status_code.to_string(),
            })
            .inc();

        self.upstream_duration_ms
            .get_or_create(&TargetLabels {
                target: target.to_string(),
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
                direction: direction.to_string(),
                method: method.to_string(),
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
                channel: channel.to_string(),
                outcome: outcome.to_string(),
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
                channel: channel.to_string(),
                outcome: outcome.to_string(),
            })
            .observe(duration_secs);
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
}
