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
pub mod timers;
pub mod trace_context;

// Re-export span types and functions (stable public API).
// Span attribute constants are pub(crate) implementation details.
pub use spans::{
    // Approval Span Types and Functions (REQ-OBS-002 §5.4)
    ApprovalCallbackData,
    ApprovalDispatchData,
    // Cedar Span Types (REQ-OBS-002 §5.3)
    CedarSpanData,
    // Gateway Decision Span Types (REQ-OBS-002 §5.3)
    GateOutcomes,
    GatewayDecisionSpanData,
    // MCP Span Types
    McpMessageType,
    McpSpanData,
    // Span Functions (stable API)
    current_span_context,
    finish_approval_callback_span,
    finish_cedar_span,
    finish_gateway_decision_span,
    finish_mcp_span,
    start_approval_callback_span,
    start_approval_dispatch_span,
    start_cedar_span,
    start_gateway_decision_span,
    start_mcp_span,
};

// Re-export trace context types (REQ-OBS-002 §7.4)
pub use trace_context::{
    DeserializedContext, SerializedTraceContext, deserialize_span_context, serialize_span_context,
};

// Re-export propagation utilities (REQ-OBS-002 §7.1, §7.2, §7.3)
pub use propagation::{
    // Stdio transport (§7.3)
    StdioTraceContext,
    // HTTP transport (§7.1, §7.2)
    extract_context_from_headers,
    extract_context_from_meta,
    inject_context_into_headers,
    inject_context_into_meta,
};

// Re-export prometheus-client metrics
pub use cardinality::CardinalityLimiter;
pub use prom_metrics::ThoughtGateMetrics;

// Re-export RAII timer helpers (REQ-CORE-002 NFR-001)
pub use timers::{AmberPathTimer, InspectorTimer};

// Re-export BoxedSpan for convenience (callers need to annotate span type)
pub use opentelemetry::global::BoxedSpan;

use opentelemetry::global;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
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

    /// Additional resource attributes.
    pub resource_attributes: std::collections::HashMap<String, String>,

    /// Custom HTTP headers for OTLP export (e.g., Authorization tokens).
    pub otlp_headers: std::collections::HashMap<String, String>,

    /// Head sampling rate (0.0 to 1.0).
    pub sample_rate: f64,

    /// Batch processor settings.
    pub batch: BatchSettings,
}

/// Batch span processor settings.
///
/// Implements: REQ-OBS-002 §8.2 (Batch Configuration)
#[derive(Debug, Clone)]
pub struct BatchSettings {
    /// Maximum spans in the export queue.
    pub max_queue_size: usize,

    /// Maximum spans per export batch.
    pub max_export_batch_size: usize,

    /// Delay between scheduled exports.
    pub scheduled_delay: std::time::Duration,

    /// Export timeout.
    pub export_timeout: std::time::Duration,
}

impl Default for BatchSettings {
    fn default() -> Self {
        Self {
            max_queue_size: 2048,
            max_export_batch_size: 512,
            scheduled_delay: std::time::Duration::from_millis(5000),
            export_timeout: std::time::Duration::from_millis(30000),
        }
    }
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: None,
            service_name: "thoughtgate".to_string(),
            resource_attributes: std::collections::HashMap::new(),
            otlp_headers: std::collections::HashMap::new(),
            sample_rate: 1.0,
            batch: BatchSettings::default(),
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
            resource_attributes: std::collections::HashMap::new(),
            otlp_headers: std::collections::HashMap::new(),
            sample_rate: 1.0,
            batch: BatchSettings::default(),
        }
    }

    /// Create configuration from YAML config with env var overrides.
    ///
    /// Priority: OTEL_EXPORTER_OTLP_ENDPOINT env var > YAML > defaults
    ///
    /// Implements: REQ-OBS-002 §8.2, §8.5
    pub fn from_yaml_config(yaml: Option<&crate::config::TelemetryYamlConfig>) -> Self {
        let yaml = yaml.cloned().unwrap_or_default();

        // OTEL_EXPORTER_OTLP_ENDPOINT env var overrides YAML
        let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .or_else(|| yaml.otlp.as_ref().map(|o| o.endpoint.clone()));

        // Warn if gRPC protocol requested (v0.2: HTTP/protobuf only)
        if let Some(ref otlp) = yaml.otlp {
            if otlp.protocol == "grpc" {
                tracing::warn!(
                    "OTLP gRPC protocol not supported in v0.2, falling back to http/protobuf"
                );
            }
        }

        // Warn if tail sampling requested (v0.2: head only)
        let sample_rate = if let Some(ref sampling) = yaml.sampling {
            if sampling.strategy == "tail" {
                tracing::warn!(
                    sample_rate = sampling.success_sample_rate,
                    "Tail sampling not yet implemented in v0.2, falling back to head sampling"
                );
            }
            sampling.success_sample_rate
        } else {
            1.0
        };

        // Build resource attributes from config + K8s env vars
        let mut resource_attributes = yaml.resource.unwrap_or_default();
        for (env_var, attr_name) in [
            ("K8S_NAMESPACE", "k8s.namespace.name"),
            ("K8S_POD_NAME", "k8s.pod.name"),
            ("K8S_NODE_NAME", "k8s.node.name"),
            ("DEPLOY_ENV", "deployment.environment"),
        ] {
            if let Ok(val) = std::env::var(env_var) {
                resource_attributes.insert(attr_name.to_string(), val);
            }
        }

        // Service name: config > OTEL_SERVICE_NAME > default
        let service_name = resource_attributes
            .get("service.name")
            .cloned()
            .or_else(|| std::env::var("OTEL_SERVICE_NAME").ok())
            .unwrap_or_else(|| "thoughtgate".to_string());

        // OTLP headers from config (e.g., Authorization tokens)
        let otlp_headers = yaml
            .otlp
            .as_ref()
            .map(|o| o.headers.clone())
            .unwrap_or_default();

        // Batch settings
        let batch = yaml
            .batch
            .as_ref()
            .map(|b| BatchSettings {
                max_queue_size: b.max_queue_size,
                max_export_batch_size: b.max_export_batch_size,
                scheduled_delay: std::time::Duration::from_millis(b.scheduled_delay_ms),
                export_timeout: std::time::Duration::from_millis(b.export_timeout_ms),
            })
            .unwrap_or_default();

        Self {
            enabled: yaml.enabled,
            otlp_endpoint,
            service_name,
            resource_attributes,
            otlp_headers,
            sample_rate,
            batch,
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
/// - Wraps it in a `BatchSpanProcessor` with configurable batch settings
/// - Configures head sampling via `TraceIdRatioBased` sampler
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
/// Implements: REQ-OBS-002 §8.5 (Head Sampling)
/// Implements: REQ-OBS-002 §12/B-OBS2-003 (Zero Overhead When Disabled)
pub fn init_telemetry(config: &TelemetryConfig) -> Result<TelemetryGuard, TelemetryError> {
    use opentelemetry::KeyValue;
    use opentelemetry_sdk::trace::{BatchConfigBuilder, BatchSpanProcessor, Sampler};

    // Install W3C TraceContext as global text map propagator
    // This enables automatic extraction/injection of traceparent and tracestate headers
    // Implements: REQ-OBS-002 §7.1, §7.2
    global::set_text_map_propagator(TraceContextPropagator::new());

    // Build resource with service name and additional attributes
    let mut resource_builder = Resource::builder().with_service_name(config.service_name.clone());

    for (key, value) in &config.resource_attributes {
        resource_builder =
            resource_builder.with_attribute(KeyValue::new(key.clone(), value.clone()));
    }
    let resource = resource_builder.build();

    let provider = if config.enabled {
        // Build OTLP HTTP/protobuf exporter
        // Note: gRPC is not supported in v0.2 — B-CFG-TEL-003 logs a warning
        // and falls back to HTTP/protobuf in from_yaml_config().
        let mut exporter_builder = opentelemetry_otlp::SpanExporter::builder().with_http();

        if let Some(ref endpoint) = config.otlp_endpoint {
            exporter_builder = exporter_builder.with_endpoint(endpoint);
        }

        if !config.otlp_headers.is_empty() {
            exporter_builder = exporter_builder.with_headers(config.otlp_headers.clone());
        }

        let exporter = exporter_builder
            .build()
            .map_err(|e| TelemetryError::ExporterBuild {
                reason: e.to_string(),
            })?;

        // Configure batch processor (B-OBS2-002: queue overflow drops newest)
        // Note: max_export_timeout requires experimental feature flag, using SDK defaults
        let batch_config = BatchConfigBuilder::default()
            .with_max_queue_size(config.batch.max_queue_size)
            .with_max_export_batch_size(config.batch.max_export_batch_size)
            .with_scheduled_delay(config.batch.scheduled_delay)
            .build();

        let batch_processor = BatchSpanProcessor::builder(exporter)
            .with_batch_config(batch_config)
            .build();

        // Configure head sampler (TraceIdRatioBased for consistency)
        // Implements: REQ-OBS-002 §8.5
        let sampler = if config.sample_rate >= 1.0 {
            Sampler::AlwaysOn
        } else if config.sample_rate <= 0.0 {
            Sampler::AlwaysOff
        } else {
            Sampler::TraceIdRatioBased(config.sample_rate)
        };

        SdkTracerProvider::builder()
            .with_resource(resource)
            .with_sampler(sampler)
            .with_span_processor(batch_processor)
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
        assert_eq!(config.sample_rate, 1.0);
        assert_eq!(config.batch.max_queue_size, 2048);
    }

    #[test]
    fn test_noop_when_disabled() {
        let config = TelemetryConfig {
            enabled: false,
            otlp_endpoint: None,
            service_name: "test-noop".to_string(),
            resource_attributes: std::collections::HashMap::new(),
            otlp_headers: std::collections::HashMap::new(),
            sample_rate: 1.0,
            batch: BatchSettings::default(),
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

    #[test]
    fn test_telemetry_disabled_no_export() {
        let config = TelemetryConfig {
            enabled: false,
            otlp_endpoint: Some("http://should-not-be-called:4318".to_string()),
            service_name: "test".to_string(),
            resource_attributes: std::collections::HashMap::new(),
            otlp_headers: std::collections::HashMap::new(),
            sample_rate: 1.0,
            batch: BatchSettings::default(),
        };

        let guard = init_telemetry(&config).expect("init should succeed");
        let tracer = guard.provider().tracer("test");
        let span = tracer.start("test-span");
        drop(span);
        guard.shutdown().expect("shutdown should succeed");
        // Provider has no exporters, so no network calls made
    }

    #[test]
    fn test_batch_settings_default() {
        let batch = BatchSettings::default();
        assert_eq!(batch.max_queue_size, 2048);
        assert_eq!(batch.max_export_batch_size, 512);
        assert_eq!(
            batch.scheduled_delay,
            std::time::Duration::from_millis(5000)
        );
        assert_eq!(
            batch.export_timeout,
            std::time::Duration::from_millis(30000)
        );
    }

    #[test]
    fn test_from_yaml_config_none() {
        // When no YAML config is provided, should use defaults
        let config = TelemetryConfig::from_yaml_config(None);
        assert!(!config.enabled);
        assert!(config.otlp_endpoint.is_none());
        assert_eq!(config.sample_rate, 1.0);
    }

    #[test]
    fn test_from_yaml_config_with_values() {
        use crate::config::{BatchConfig, OtlpConfig, SamplingConfig, TelemetryYamlConfig};

        let yaml_config = TelemetryYamlConfig {
            enabled: true,
            otlp: Some(OtlpConfig {
                endpoint: "http://test-collector:4318".to_string(),
                protocol: "http/protobuf".to_string(),
                headers: std::collections::HashMap::new(),
            }),
            sampling: Some(SamplingConfig {
                strategy: "head".to_string(),
                success_sample_rate: 0.5,
            }),
            batch: Some(BatchConfig {
                max_queue_size: 4096,
                max_export_batch_size: 1024,
                scheduled_delay_ms: 10000,
                export_timeout_ms: 60000,
            }),
            resource: Some({
                let mut map = std::collections::HashMap::new();
                map.insert("service.name".to_string(), "test-service".to_string());
                map
            }),
            stdio: None,
        };

        let config = TelemetryConfig::from_yaml_config(Some(&yaml_config));
        assert!(config.enabled);
        assert_eq!(
            config.otlp_endpoint,
            Some("http://test-collector:4318".to_string())
        );
        assert_eq!(config.sample_rate, 0.5);
        assert_eq!(config.service_name, "test-service");
        assert_eq!(config.batch.max_queue_size, 4096);
        assert_eq!(config.batch.max_export_batch_size, 1024);
    }

    #[test]
    fn test_head_sampling_rate() {
        use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler};

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .with_sampler(Sampler::TraceIdRatioBased(0.5))
            .build();

        let tracer = provider.tracer("test");
        for i in 0..1000 {
            let span = tracer.start(format!("span-{}", i));
            drop(span);
        }

        provider.force_flush().expect("flush should succeed");
        let spans = exporter
            .get_finished_spans()
            .expect("get spans should succeed");
        let count = spans.len();
        // 50% sampling should give ~500 spans (allow 40-60%)
        assert!(
            count > 400 && count < 600,
            "Expected ~500 spans, got {}",
            count
        );
    }
}
