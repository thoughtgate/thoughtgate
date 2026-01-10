//! Metrics and observability for ThoughtGate proxy.
//!
//! # Traffic Path Metrics
//!
//! - **Green Path (REQ-CORE-001):** Zero-copy streaming metrics
//! - **Amber Path (REQ-CORE-002):** Buffered inspection metrics
//!
//! # Traceability
//! - Implements: REQ-CORE-001 NFR-001 (Observability)
//! - Implements: REQ-CORE-002 NFR-001 (Observability)

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;

// ─────────────────────────────────────────────────────────────────────────────
// Green Path Metrics (REQ-CORE-001)
// ─────────────────────────────────────────────────────────────────────────────

/// Metrics collector for the Green Path (zero-copy streaming).
///
/// # Traceability
/// - Implements: REQ-CORE-001 NFR-001 (Observability - Metrics)
#[derive(Clone)]
pub struct GreenPathMetrics {
    /// Total bytes transferred (upload/download)
    pub bytes_total: Counter<u64>,
    /// Active streams (using atomic for gauge-like behavior)
    pub streams_active: Arc<AtomicI64>,
    /// Total streams counter (success/error/upgrade)
    pub streams_total: Counter<u64>,
    /// Time-to-first-byte histogram
    pub ttfb_seconds: Histogram<f64>,
    /// Chunk size histogram
    pub chunk_size_bytes: Histogram<u64>,
}

impl GreenPathMetrics {
    /// Create new metrics collector.
    pub fn new(meter: &Meter) -> Self {
        Self {
            bytes_total: meter
                .u64_counter("green_path_bytes_total")
                .with_description("Total bytes transferred through green path")
                .build(),
            streams_active: Arc::new(AtomicI64::new(0)),
            streams_total: meter
                .u64_counter("green_path_streams_total")
                .with_description("Total number of streams")
                .build(),
            ttfb_seconds: meter
                .f64_histogram("green_path_ttfb_seconds")
                .with_description("Time to first byte in seconds")
                .build(),
            chunk_size_bytes: meter
                .u64_histogram("green_path_chunk_size_bytes")
                .with_description("Size of chunks in bytes")
                .build(),
        }
    }

    /// Record bytes transferred.
    pub fn record_bytes(&self, direction: &str, bytes: u64) {
        self.bytes_total
            .add(bytes, &[KeyValue::new("direction", direction.to_string())]);
    }

    /// Increment active streams.
    pub fn increment_active(&self) {
        self.streams_active.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement active streams.
    pub fn decrement_active(&self) {
        self.streams_active.fetch_sub(1, Ordering::Relaxed);
    }

    /// Get active stream count.
    pub fn active_count(&self) -> i64 {
        self.streams_active.load(Ordering::Relaxed)
    }

    /// Record stream completion.
    pub fn record_stream(&self, outcome: &str) {
        self.streams_total
            .add(1, &[KeyValue::new("outcome", outcome.to_string())]);
    }

    /// Record TTFB.
    pub fn record_ttfb(&self, seconds: f64) {
        self.ttfb_seconds.record(seconds, &[]);
    }

    /// Record chunk size.
    pub fn record_chunk_size(&self, bytes: u64) {
        self.chunk_size_bytes.record(bytes, &[]);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Amber Path Metrics (REQ-CORE-002)
// ─────────────────────────────────────────────────────────────────────────────

/// Metrics collector for the Amber Path (buffered inspection).
///
/// # Metrics
///
/// - `amber_path_buffer_size_bytes`: Histogram of buffered payload sizes
/// - `amber_path_duration_seconds`: Histogram of total Amber Path operation duration
/// - `amber_inspector_duration_seconds`: Per-inspector duration histogram
/// - `amber_path_inspections_total`: Counter by decision type
/// - `amber_path_errors_total`: Counter by error type
///
/// # Traceability
/// - Implements: REQ-CORE-002 NFR-001 (Observability - Metrics)
#[derive(Clone)]
pub struct AmberPathMetrics {
    /// Buffer size histogram
    pub buffer_size_bytes: Histogram<u64>,
    /// Total operation duration histogram
    pub duration_seconds: Histogram<f64>,
    /// Per-inspector duration histogram
    pub inspector_duration_seconds: Histogram<f64>,
    /// Inspection decisions counter
    pub inspections_total: Counter<u64>,
    /// Error counter
    pub errors_total: Counter<u64>,
    /// Active buffered connections (using atomic for gauge-like behavior)
    pub buffers_active: Arc<AtomicI64>,
}

impl AmberPathMetrics {
    /// Create new Amber Path metrics collector.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 NFR-001 (Observability)
    pub fn new(meter: &Meter) -> Self {
        Self {
            buffer_size_bytes: meter
                .u64_histogram("amber_path_buffer_size_bytes")
                .with_description("Size of buffered payloads in bytes")
                .build(),
            duration_seconds: meter
                .f64_histogram("amber_path_duration_seconds")
                .with_description("Total duration of Amber Path operations in seconds")
                .build(),
            inspector_duration_seconds: meter
                .f64_histogram("amber_inspector_duration_seconds")
                .with_description("Duration of individual inspector executions in seconds")
                .build(),
            inspections_total: meter
                .u64_counter("amber_path_inspections_total")
                .with_description("Total number of inspections by decision type")
                .build(),
            errors_total: meter
                .u64_counter("amber_path_errors_total")
                .with_description("Total number of Amber Path errors by type")
                .build(),
            buffers_active: Arc::new(AtomicI64::new(0)),
        }
    }

    /// Record buffer size.
    pub fn record_buffer_size(&self, bytes: u64) {
        self.buffer_size_bytes.record(bytes, &[]);
    }

    /// Record total operation duration.
    pub fn record_duration(&self, seconds: f64) {
        self.duration_seconds.record(seconds, &[]);
    }

    /// Record individual inspector duration.
    pub fn record_inspector_duration(&self, inspector_name: &str, seconds: f64) {
        self.inspector_duration_seconds.record(
            seconds,
            &[KeyValue::new("inspector_name", inspector_name.to_string())],
        );
    }

    /// Record inspection decision.
    ///
    /// # Arguments
    ///
    /// * `decision` - One of: "approve", "modify", "reject"
    pub fn record_inspection(&self, decision: &str) {
        self.inspections_total
            .add(1, &[KeyValue::new("decision", decision.to_string())]);
    }

    /// Record an error.
    ///
    /// # Arguments
    ///
    /// * `error_type` - One of: "timeout", "limit", "semaphore", "panic", "compressed", "error"
    pub fn record_error(&self, error_type: &str) {
        self.errors_total
            .add(1, &[KeyValue::new("type", error_type.to_string())]);
    }

    /// Increment active buffers.
    pub fn increment_active(&self) {
        self.buffers_active.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement active buffers.
    pub fn decrement_active(&self) {
        self.buffers_active.fetch_sub(1, Ordering::Relaxed);
    }

    /// Get active buffer count.
    pub fn active_count(&self) -> i64 {
        self.buffers_active.load(Ordering::Relaxed)
    }
}

/// Helper to track inspector timing.
///
/// # Usage
///
/// ```ignore
/// let timer = InspectorTimer::new(metrics.clone(), "my-inspector");
/// // ... do inspection ...
/// timer.finish(); // Records duration
/// ```
pub struct InspectorTimer {
    metrics: Arc<AmberPathMetrics>,
    inspector_name: String,
    start: Instant,
}

impl InspectorTimer {
    /// Start timing an inspector.
    pub fn new(metrics: Arc<AmberPathMetrics>, inspector_name: &str) -> Self {
        Self {
            metrics,
            inspector_name: inspector_name.to_string(),
            start: Instant::now(),
        }
    }

    /// Finish timing and record the duration.
    pub fn finish(self) {
        let duration = self.start.elapsed();
        self.metrics
            .record_inspector_duration(&self.inspector_name, duration.as_secs_f64());
    }
}

/// Helper to track Amber Path operation timing.
///
/// Uses `Arc<AmberPathMetrics>` to avoid lifetime issues with async code.
///
/// # Usage
///
/// ```ignore
/// let timer = AmberPathTimer::new(metrics.clone());
/// // ... do buffering and inspection ...
/// timer.finish_success(buffer_size); // Records duration and buffer size
/// // OR
/// timer.finish_error("timeout"); // Records error
/// ```
pub struct AmberPathTimer {
    metrics: Arc<AmberPathMetrics>,
    start: Instant,
    finished: bool,
}

impl AmberPathTimer {
    /// Start timing an Amber Path operation.
    pub fn new(metrics: Arc<AmberPathMetrics>) -> Self {
        metrics.increment_active();
        Self {
            metrics,
            start: Instant::now(),
            finished: false,
        }
    }

    /// Finish with success, recording duration and buffer size.
    pub fn finish_success(mut self, buffer_size: u64) {
        self.finished = true;
        let duration = self.start.elapsed();
        self.metrics.record_duration(duration.as_secs_f64());
        self.metrics.record_buffer_size(buffer_size);
        self.metrics.decrement_active();
    }

    /// Finish with error, recording the error type.
    pub fn finish_error(mut self, error_type: &str) {
        self.finished = true;
        let duration = self.start.elapsed();
        self.metrics.record_duration(duration.as_secs_f64());
        self.metrics.record_error(error_type);
        self.metrics.decrement_active();
    }
}

impl Drop for AmberPathTimer {
    fn drop(&mut self) {
        if !self.finished {
            // If not explicitly finished, record as an error
            let duration = self.start.elapsed();
            self.metrics.record_duration(duration.as_secs_f64());
            self.metrics.record_error("dropped");
            self.metrics.decrement_active();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Global Metrics
// ─────────────────────────────────────────────────────────────────────────────

/// Combined metrics for both traffic paths.
#[derive(Clone)]
pub struct ProxyMetrics {
    pub green_path: GreenPathMetrics,
    pub amber_path: AmberPathMetrics,
}

impl ProxyMetrics {
    /// Create new proxy metrics.
    pub fn new(meter: &Meter) -> Self {
        Self {
            green_path: GreenPathMetrics::new(meter),
            amber_path: AmberPathMetrics::new(meter),
        }
    }
}

/// Global metrics instance (Green Path only for backwards compatibility).
static GREEN_METRICS: once_cell::sync::OnceCell<Arc<GreenPathMetrics>> =
    once_cell::sync::OnceCell::new();

/// Global Amber Path metrics instance.
static AMBER_METRICS: once_cell::sync::OnceCell<Arc<AmberPathMetrics>> =
    once_cell::sync::OnceCell::new();

/// Initialize global metrics.
pub fn init_metrics(meter: &Meter) {
    let green_metrics = Arc::new(GreenPathMetrics::new(meter));
    let amber_metrics = Arc::new(AmberPathMetrics::new(meter));
    let _ = GREEN_METRICS.set(green_metrics);
    let _ = AMBER_METRICS.set(amber_metrics);
}

/// Get global Green Path metrics instance.
pub fn get_metrics() -> Option<Arc<GreenPathMetrics>> {
    GREEN_METRICS.get().cloned()
}

/// Get global Amber Path metrics instance.
///
/// # Traceability
/// - Implements: REQ-CORE-002 NFR-001 (Observability)
pub fn get_amber_metrics() -> Option<Arc<AmberPathMetrics>> {
    AMBER_METRICS.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::global;

    #[test]
    fn test_amber_path_metrics_creation() {
        let meter = global::meter("test");
        let metrics = AmberPathMetrics::new(&meter);

        // Initial state
        assert_eq!(metrics.active_count(), 0);

        // Record some metrics
        metrics.record_buffer_size(1024);
        metrics.record_duration(0.5);
        metrics.record_inspector_duration("test-inspector", 0.1);
        metrics.record_inspection("approve");
        metrics.record_error("timeout");

        // Active buffer tracking
        metrics.increment_active();
        assert_eq!(metrics.active_count(), 1);
        metrics.decrement_active();
        assert_eq!(metrics.active_count(), 0);
    }

    #[test]
    fn test_amber_path_timer() {
        let meter = global::meter("test");
        let metrics = Arc::new(AmberPathMetrics::new(&meter));

        // Test success path
        {
            let timer = AmberPathTimer::new(metrics.clone());
            assert_eq!(metrics.active_count(), 1);
            timer.finish_success(1024);
            assert_eq!(metrics.active_count(), 0);
        }

        // Test error path
        {
            let timer = AmberPathTimer::new(metrics.clone());
            assert_eq!(metrics.active_count(), 1);
            timer.finish_error("timeout");
            assert_eq!(metrics.active_count(), 0);
        }

        // Test drop path (implicit error)
        {
            let _timer = AmberPathTimer::new(metrics.clone());
            assert_eq!(metrics.active_count(), 1);
            // Timer dropped without explicit finish
        }
        assert_eq!(metrics.active_count(), 0);
    }

    #[test]
    fn test_inspector_timer() {
        let meter = global::meter("test");
        let metrics = Arc::new(AmberPathMetrics::new(&meter));

        let timer = InspectorTimer::new(metrics.clone(), "test-inspector");
        std::thread::sleep(std::time::Duration::from_millis(10));
        timer.finish();
        // Duration recorded (can't easily assert the value)
    }
}
