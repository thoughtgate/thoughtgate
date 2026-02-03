//! OpenTelemetry tracing initialization and lifecycle management.
//!
//! This module provides the foundation for distributed tracing in ThoughtGate.
//! It initializes the OpenTelemetry tracer provider with OTLP HTTP/protobuf export
//! and manages the provider lifecycle (startup and graceful shutdown).
//!
//! # Submodules
//!
//! - [`spans`] - MCP span instrumentation with OTel GenAI semantic conventions
//! - [`prom_metrics`] - Prometheus metrics using prometheus-client crate
//! - [`cardinality`] - Cardinality limiter for metric labels
//!
//! # Traceability
//! - Implements: REQ-OBS-002 §11.1 (Crate Selection)
//! - Implements: REQ-OBS-002 §12/B-OBS2-001 (Telemetry Disabled by Default)
//! - Implements: REQ-OBS-002 §12/B-OBS2-002 (Graceful Degradation on Export Failure)
//! - Implements: REQ-OBS-002 §12/B-OBS2-003 (Zero Overhead When Disabled)
//! - Implements: REQ-OBS-002 §6.1-6.5 (Prometheus Metrics)

pub mod cardinality;
pub mod prom_metrics;
pub mod propagation;
pub mod spans;
pub mod trace_context;

pub use spans::{
    // Approval Span Types and Functions (REQ-OBS-002 §5.4)
    ApprovalCallbackData,
    ApprovalDispatchData,
    // MCP Span Constants
    ERROR_TYPE,
    GENAI_OPERATION_NAME,
    GENAI_TOOL_CALL_ID,
    GENAI_TOOL_NAME,
    MCP_ERROR_CODE,
    MCP_MESSAGE_ID,
    MCP_MESSAGE_TYPE,
    MCP_METHOD_NAME,
    MCP_RESULT_IS_ERROR,
    MCP_SESSION_ID,
    // MCP Span Types and Functions
    McpMessageType,
    McpSpanData,
    // Approval Span Constants (REQ-OBS-002 §5.4)
    THOUGHTGATE_APPROVAL_CHANNEL,
    THOUGHTGATE_APPROVAL_DECISION,
    THOUGHTGATE_APPROVAL_LATENCY_S,
    THOUGHTGATE_APPROVAL_TARGET,
    THOUGHTGATE_APPROVAL_TIMEOUT_S,
    THOUGHTGATE_APPROVAL_USER,
    THOUGHTGATE_REQUEST_ID,
    THOUGHTGATE_TASK_ID,
    THOUGHTGATE_TRACE_CONTEXT_RECOVERED,
    current_span_context,
    finish_approval_callback_span,
    finish_mcp_span,
    start_approval_callback_span,
    start_approval_dispatch_span,
    start_mcp_span,
};

// Re-export trace context types (REQ-OBS-002 §7.4)
pub use trace_context::{
    DeserializedContext, SerializedTraceContext, deserialize_span_context, serialize_span_context,
};

// Re-export propagation utilities (REQ-OBS-002 §7.1, §7.2)
pub use propagation::{extract_context_from_headers, inject_context_into_headers};

// Re-export prometheus-client metrics
pub use cardinality::CardinalityLimiter;
pub use prom_metrics::ThoughtGateMetrics;

// Re-export BoxedSpan for convenience (callers need to annotate span type)
pub use opentelemetry::global::BoxedSpan;

use opentelemetry::global;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;

/// Configuration for OpenTelemetry tracing initialization.
///
/// When `enabled` is `false`, a provider with no exporters is created,
/// resulting in zero overhead (no network calls, no allocations for spans).
///
/// Implements: REQ-OBS-002 §8.2 (OTLP Export Configuration)
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Master enable/disable switch for trace export.
    /// When false, a provider with no processors is created (zero overhead).
    /// Default: false (B-OBS2-001: disabled by default in development)
    pub enabled: bool,

    /// OTLP HTTP endpoint for trace export.
    /// Example: "http://otel-collector:4318"
    /// If None, uses the OTel SDK default.
    pub otlp_endpoint: Option<String>,

    /// OTel resource `service.name` attribute.
    /// Default: "thoughtgate"
    pub service_name: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: None,
            service_name: "thoughtgate".to_string(),
        }
    }
}

impl TelemetryConfig {
    /// Create configuration from environment variables.
    ///
    /// | Variable | Default | Description |
    /// |----------|---------|-------------|
    /// | `THOUGHTGATE_TELEMETRY_ENABLED` | `false` | Enable OTLP trace export |
    /// | `OTEL_EXPORTER_OTLP_ENDPOINT` | OTel SDK default | OTLP collector endpoint |
    /// | `OTEL_SERVICE_NAME` | `"thoughtgate"` | Resource service.name |
    ///
    /// Implements: REQ-OBS-002 §8.2
    pub fn from_env() -> Self {
        let enabled = std::env::var("THOUGHTGATE_TELEMETRY_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();

        let service_name =
            std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "thoughtgate".to_string());

        Self {
            enabled,
            otlp_endpoint,
            service_name,
        }
    }
}

/// Errors that can occur during telemetry initialization or shutdown.
///
/// Implements: REQ-OBS-002 §13.1 (Telemetry Pipeline Errors)
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// Failed to build the OTLP span exporter.
    #[error("failed to build OTLP exporter: {reason}")]
    ExporterBuild { reason: String },

    /// Shutdown of the tracer provider failed.
    #[error("tracer provider shutdown failed: {reason}")]
    Shutdown { reason: String },
}

/// RAII guard holding the `SdkTracerProvider`.
///
/// Holds ownership of the tracer provider for the lifetime of the application.
/// Call `shutdown()` during graceful shutdown to flush pending spans.
/// If dropped without calling `shutdown()`, the provider's `Drop` impl
/// will still attempt to flush, but explicit shutdown is preferred.
///
/// # Test Isolation (DI Pattern)
///
/// Tests should use `provider()` to obtain a reference to the tracer provider
/// and create tracers from it directly, rather than using the global provider.
/// This avoids race conditions when tests run in parallel.
///
/// Implements: REQ-OBS-002 §12/B-OBS2-002 (Graceful Degradation)
pub struct TelemetryGuard {
    provider: SdkTracerProvider,
}

impl TelemetryGuard {
    /// Returns a reference to the underlying `SdkTracerProvider`.
    ///
    /// Useful for tests that need to create tracers from the provider
    /// without touching the global state.
    pub fn provider(&self) -> &SdkTracerProvider {
        &self.provider
    }

    /// Explicitly shuts down the tracer provider, flushing all pending spans.
    ///
    /// This should be called during the graceful shutdown sequence, after
    /// request draining completes but before process exit.
    ///
    /// Implements: REQ-OBS-002 §12/B-OBS2-002
    pub fn shutdown(&self) -> Result<(), TelemetryError> {
        self.provider
            .shutdown()
            .map_err(|e| TelemetryError::Shutdown {
                reason: e.to_string(),
            })
    }
}

/// Initializes OpenTelemetry tracing infrastructure.
///
/// When `config.enabled` is `true`:
/// - Creates an OTLP HTTP/protobuf span exporter
/// - Wraps it in a `BatchSpanProcessor`
/// - Builds an `SdkTracerProvider` with the processor
/// - Sets it as the global tracer provider
///
/// When `config.enabled` is `false`:
/// - Creates an `SdkTracerProvider` with no processors (zero overhead)
/// - Still sets it as the global tracer provider (so `global::tracer()` works as noop)
///
/// Returns a `TelemetryGuard` that must be held for the application lifetime.
/// Call `guard.shutdown()` during graceful shutdown.
///
/// # Example
///
/// ```ignore
/// let config = TelemetryConfig::from_env();
/// let guard = init_telemetry(&config)?;
///
/// // Application runs...
///
/// // During shutdown:
/// guard.shutdown()?;
/// ```
///
/// Implements: REQ-OBS-002 §8.1 (Export Pipeline Architecture)
/// Implements: REQ-OBS-002 §7.1-7.2 (W3C Trace Context Propagation)
/// Implements: REQ-OBS-002 §12/B-OBS2-003 (Zero Overhead When Disabled)
pub fn init_telemetry(config: &TelemetryConfig) -> Result<TelemetryGuard, TelemetryError> {
    // Install W3C TraceContext as global text map propagator
    // This enables automatic extraction/injection of traceparent and tracestate headers
    // Implements: REQ-OBS-002 §7.1, §7.2
    global::set_text_map_propagator(TraceContextPropagator::new());

    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .build();

    let provider = if config.enabled {
        // Build OTLP HTTP/protobuf exporter
        let mut exporter_builder = opentelemetry_otlp::SpanExporter::builder().with_http();

        if let Some(ref endpoint) = config.otlp_endpoint {
            exporter_builder = exporter_builder.with_endpoint(endpoint);
        }

        let exporter = exporter_builder
            .build()
            .map_err(|e| TelemetryError::ExporterBuild {
                reason: e.to_string(),
            })?;

        SdkTracerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(exporter)
            .build()
    } else {
        // No processors = noop provider. Tracers from this provider
        // produce spans that are never exported.
        // Implements: REQ-OBS-002/B-OBS2-003 (Zero Overhead When Disabled)
        SdkTracerProvider::builder().with_resource(resource).build()
    };

    // Set as global so that `opentelemetry::global::tracer("name")` works
    opentelemetry::global::set_tracer_provider(provider.clone());

    Ok(TelemetryGuard { provider })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::{Tracer, TracerProvider};

    #[test]
    fn test_config_default() {
        let config = TelemetryConfig::default();
        assert!(!config.enabled);
        assert!(config.otlp_endpoint.is_none());
        assert_eq!(config.service_name, "thoughtgate");
    }

    #[test]
    fn test_noop_when_disabled() {
        let config = TelemetryConfig {
            enabled: false,
            otlp_endpoint: None,
            service_name: "test-noop".to_string(),
        };
        let guard = init_telemetry(&config).expect("init should succeed when disabled");

        // Create a tracer from the returned provider (not global) — DI pattern
        let tracer = guard.provider().tracer("test");

        // Create a span — this should be a noop span (no processor to receive it)
        let span = tracer.start("test-span");
        drop(span);

        // Shutdown should succeed
        guard.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn test_init_with_stdout_exporter() {
        // Build a provider with stdout exporter for test verification
        // This tests that the provider machinery works without needing OTLP
        let exporter = opentelemetry_stdout::SpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter)
            .build();

        let tracer = provider.tracer("test-stdout");
        let span = tracer.start("test-span");
        drop(span);

        // Shutdown flushes the span to stdout
        provider.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn test_guard_provides_access_to_provider() {
        let config = TelemetryConfig::default();
        let guard = init_telemetry(&config).expect("init should succeed");

        // provider() returns a reference we can use
        let _tracer = guard.provider().tracer("test-di");

        guard.shutdown().expect("shutdown should succeed");
    }
}
