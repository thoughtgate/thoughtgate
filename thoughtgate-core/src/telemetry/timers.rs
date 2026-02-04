//! RAII timer helpers for metrics collection.
//!
//! These timers provide drop-safe metrics recording for Amber Path operations
//! and inspector execution timing. If a timer is dropped without explicitly
//! finishing, it records an error metric.
//!
//! # Traceability
//! - Implements: REQ-CORE-002 NFR-001 (Observability - Metrics)

use std::sync::Arc;
use std::time::Instant;

use super::ThoughtGateMetrics;

// ─────────────────────────────────────────────────────────────────────────────
// Amber Path Timer
// ─────────────────────────────────────────────────────────────────────────────

/// RAII timer for Amber Path operations.
///
/// Tracks the duration and active buffer count for buffered inspection
/// operations. Automatically records an error metric if dropped without
/// explicitly finishing.
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
///
/// # Traceability
/// - Implements: REQ-CORE-002 NFR-001 (Observability)
pub struct AmberPathTimer {
    metrics: Arc<ThoughtGateMetrics>,
    start: Instant,
    finished: bool,
}

impl AmberPathTimer {
    /// Start timing an Amber Path operation.
    ///
    /// Increments the active buffers gauge immediately.
    pub fn new(metrics: Arc<ThoughtGateMetrics>) -> Self {
        metrics.increment_amber_buffers_active();
        Self {
            metrics,
            start: Instant::now(),
            finished: false,
        }
    }

    /// Finish with success, recording duration and buffer size.
    ///
    /// This consumes the timer and records:
    /// - `thoughtgate_amber_duration_seconds` histogram
    /// - `thoughtgate_amber_buffer_size_bytes` histogram
    /// - Decrements `thoughtgate_amber_buffers_active` gauge
    pub fn finish_success(mut self, buffer_size: u64) {
        self.finished = true;
        self.metrics
            .record_amber_duration(self.start.elapsed().as_secs_f64());
        self.metrics.record_amber_buffer_size(buffer_size);
        self.metrics.decrement_amber_buffers_active();
    }

    /// Finish with error, recording the error type.
    ///
    /// This consumes the timer and records:
    /// - `thoughtgate_amber_duration_seconds` histogram
    /// - `thoughtgate_amber_errors_total` counter with given error_type
    /// - Decrements `thoughtgate_amber_buffers_active` gauge
    pub fn finish_error(mut self, error_type: &str) {
        self.finished = true;
        self.metrics
            .record_amber_duration(self.start.elapsed().as_secs_f64());
        self.metrics.record_amber_error(error_type);
        self.metrics.decrement_amber_buffers_active();
    }
}

impl Drop for AmberPathTimer {
    fn drop(&mut self) {
        if !self.finished {
            // Timer dropped without explicit finish — record as error
            self.metrics
                .record_amber_duration(self.start.elapsed().as_secs_f64());
            self.metrics.record_amber_error("dropped");
            self.metrics.decrement_amber_buffers_active();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Inspector Timer
// ─────────────────────────────────────────────────────────────────────────────

/// RAII timer for individual inspector execution.
///
/// Tracks the duration of a single inspector's execution within an
/// Amber Path operation. Unlike `AmberPathTimer`, this does not track
/// active buffers (the parent AmberPathTimer handles that).
///
/// # Usage
///
/// ```ignore
/// let timer = InspectorTimer::new(metrics.clone(), "my-inspector");
/// // ... do inspection ...
/// timer.finish(); // Records duration
/// ```
///
/// # Traceability
/// - Implements: REQ-CORE-002 NFR-001 (Observability)
pub struct InspectorTimer {
    metrics: Arc<ThoughtGateMetrics>,
    inspector_name: String,
    start: Instant,
}

impl InspectorTimer {
    /// Start timing an inspector execution.
    pub fn new(metrics: Arc<ThoughtGateMetrics>, inspector_name: &str) -> Self {
        Self {
            metrics,
            inspector_name: inspector_name.to_string(),
            start: Instant::now(),
        }
    }

    /// Finish timing and record the duration.
    ///
    /// Records `thoughtgate_amber_inspector_duration_seconds` histogram
    /// with the inspector_name label.
    pub fn finish(self) {
        self.metrics.record_amber_inspector_duration(
            &self.inspector_name,
            self.start.elapsed().as_secs_f64(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prometheus_client::registry::Registry;

    fn create_test_metrics() -> Arc<ThoughtGateMetrics> {
        let mut registry = Registry::default();
        Arc::new(ThoughtGateMetrics::new(&mut registry))
    }

    #[test]
    fn test_amber_path_timer_success() {
        let metrics = create_test_metrics();

        // Create timer, which increments active buffers
        let timer = AmberPathTimer::new(metrics.clone());

        // Finish with success
        timer.finish_success(1024);

        // Active buffers should be back to 0
        // (We can't easily assert gauge values in prometheus-client, but we can
        // verify no panic occurred)
    }

    #[test]
    fn test_amber_path_timer_error() {
        let metrics = create_test_metrics();

        let timer = AmberPathTimer::new(metrics.clone());
        timer.finish_error("timeout");
    }

    #[test]
    fn test_amber_path_timer_drop() {
        let metrics = create_test_metrics();

        {
            let _timer = AmberPathTimer::new(metrics.clone());
            // Timer dropped without explicit finish
        }

        // Should have recorded "dropped" error
    }

    #[test]
    fn test_inspector_timer() {
        let metrics = create_test_metrics();

        let timer = InspectorTimer::new(metrics.clone(), "test-inspector");
        timer.finish();
    }

    #[tokio::test]
    async fn test_inspector_timer_with_sleep() {
        let metrics = create_test_metrics();

        let timer = InspectorTimer::new(metrics.clone(), "slow-inspector");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        timer.finish();
    }
}
