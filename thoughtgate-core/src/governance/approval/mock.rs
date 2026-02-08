//! Mock approval adapter for testing.
//!
//! Implements: REQ-GOV-003/T-001 (Testability)
//!
//! This adapter provides a mock implementation of the approval workflow
//! for testing and development purposes. It auto-approves requests after
//! a configurable delay without requiring actual Slack integration.
//!
//! ## Configuration
//!
//! - `THOUGHTGATE_MOCK_APPROVAL_DELAY_SECS` - Delay before auto-approving (default: 5)
//! - `THOUGHTGATE_MOCK_AUTO_APPROVE` - Set to "false" to auto-reject (default: true)
//!
//! ## Usage
//!
//! Set `THOUGHTGATE_APPROVAL_ADAPTER=mock` to use this adapter.

use async_trait::async_trait;
use chrono::Utc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, info};

use super::{
    AdapterError, ApprovalAdapter, ApprovalReference, ApprovalRequest, DecisionMethod,
    PollDecision, PollResult,
};

/// Mock approval adapter for testing.
///
/// This adapter auto-approves (or auto-rejects) requests after a configurable
/// delay. Useful for:
/// - Unit and integration tests
/// - Local development without Slack
/// - CI pipelines
///
/// Implements: REQ-GOV-003/T-001
pub struct MockAdapter {
    /// Delay before returning a decision
    delay: Duration,
    /// Whether to auto-approve (true) or auto-reject (false)
    auto_approve: bool,
    /// Counter for posted requests (for testing)
    post_count: AtomicU32,
    /// Counter for poll attempts (for testing)
    poll_count: AtomicU32,
}

impl MockAdapter {
    /// Create a new mock adapter with specified settings.
    ///
    /// # Arguments
    ///
    /// * `delay` - How long to wait before returning a decision
    /// * `auto_approve` - Whether to approve (true) or reject (false)
    #[must_use]
    pub fn new(delay: Duration, auto_approve: bool) -> Self {
        Self {
            delay,
            auto_approve,
            post_count: AtomicU32::new(0),
            poll_count: AtomicU32::new(0),
        }
    }

    /// Create a mock adapter from environment variables.
    ///
    /// # Environment Variables
    ///
    /// - `THOUGHTGATE_MOCK_APPROVAL_DELAY_SECS` - Delay in seconds (default: 5)
    /// - `THOUGHTGATE_MOCK_AUTO_APPROVE` - "true" or "false" (default: true)
    #[must_use]
    pub fn from_env() -> Self {
        let delay_secs = std::env::var("THOUGHTGATE_MOCK_APPROVAL_DELAY_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);

        let auto_approve = std::env::var("THOUGHTGATE_MOCK_AUTO_APPROVE")
            .map(|s| s.to_lowercase() != "false")
            .unwrap_or(true);

        info!(
            delay_secs = delay_secs,
            auto_approve = auto_approve,
            "MockAdapter initialized from environment"
        );

        Self::new(Duration::from_secs(delay_secs), auto_approve)
    }

    /// Create a mock adapter that approves instantly (for fast tests).
    #[must_use]
    pub fn instant_approve() -> Self {
        Self::new(Duration::ZERO, true)
    }

    /// Create a mock adapter that rejects instantly (for fast tests).
    #[must_use]
    pub fn instant_reject() -> Self {
        Self::new(Duration::ZERO, false)
    }

    /// Get the number of approval requests posted.
    #[must_use]
    pub fn post_count(&self) -> u32 {
        self.post_count.load(Ordering::Relaxed)
    }

    /// Get the number of poll attempts made.
    #[must_use]
    pub fn poll_count(&self) -> u32 {
        self.poll_count.load(Ordering::Relaxed)
    }

    /// Reset counters (for testing).
    pub fn reset_counters(&self) {
        self.post_count.store(0, Ordering::Relaxed);
        self.poll_count.store(0, Ordering::Relaxed);
    }
}

impl Default for MockAdapter {
    fn default() -> Self {
        Self::from_env()
    }
}

#[async_trait]
impl ApprovalAdapter for MockAdapter {
    /// Post an approval request (mock implementation).
    ///
    /// This immediately returns a mock reference. The actual "approval"
    /// happens during polling after the configured delay.
    async fn post_approval_request(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalReference, AdapterError> {
        self.post_count.fetch_add(1, Ordering::Relaxed);

        debug!(
            task_id = %request.task_id,
            tool = %request.tool_name,
            delay_ms = %self.delay.as_millis(),
            auto_approve = %self.auto_approve,
            "MockAdapter: Posted approval request"
        );

        // Generate a mock external ID
        let external_id = format!("mock_{}", request.task_id);

        Ok(ApprovalReference {
            task_id: request.task_id.clone(),
            external_id,
            channel: "mock_channel".to_string(),
            posted_at: Utc::now(),
            next_poll_at: Instant::now() + self.delay,
            poll_count: 0,
            dispatch_trace_context: None, // Mock adapter doesn't track trace context
        })
    }

    /// Poll for a decision (mock implementation).
    ///
    /// Returns the configured decision (approve/reject) after the delay
    /// has elapsed from when the request was posted.
    async fn poll_for_decision(
        &self,
        reference: &ApprovalReference,
    ) -> Result<Option<PollResult>, AdapterError> {
        self.poll_count.fetch_add(1, Ordering::Relaxed);

        // Check if enough time has passed
        let now = Instant::now();
        if now < reference.next_poll_at {
            debug!(
                task_id = %reference.task_id,
                remaining_ms = %(reference.next_poll_at - now).as_millis(),
                "MockAdapter: Still waiting for approval delay"
            );
            return Ok(None);
        }

        // Delay has passed, return the configured decision
        let decision = if self.auto_approve {
            PollDecision::Approved
        } else {
            PollDecision::Rejected
        };

        info!(
            task_id = %reference.task_id,
            decision = ?decision,
            "MockAdapter: Returning mock decision"
        );

        Ok(Some(PollResult {
            decision,
            decided_by: "mock:auto".to_string(),
            decided_at: Utc::now(),
            method: DecisionMethod::Reaction {
                emoji: if self.auto_approve {
                    "+1".to_string()
                } else {
                    "-1".to_string()
                },
            },
        }))
    }

    /// Cancel a pending approval (mock implementation).
    ///
    /// This is a no-op for the mock adapter.
    async fn cancel_approval(&self, reference: &ApprovalReference) -> Result<(), AdapterError> {
        debug!(
            task_id = %reference.task_id,
            "MockAdapter: Cancelled approval (no-op)"
        );
        Ok(())
    }

    /// Returns the adapter name.
    fn name(&self) -> &'static str {
        "mock"
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::{Principal, TaskId};
    use chrono::Utc;

    fn make_test_request() -> ApprovalRequest {
        ApprovalRequest {
            task_id: TaskId::new(),
            tool_name: "test_tool".to_string(),
            tool_arguments: serde_json::json!({"key": "value"}),
            principal: Principal::new("test_app"),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            created_at: Utc::now(),
            correlation_id: "test-123".to_string(),
            request_span_context: None,
            redact_fields: Vec::new(),
        }
    }

    #[tokio::test]
    async fn test_instant_approve() {
        let adapter = MockAdapter::instant_approve();
        let request = make_test_request();

        // Post request
        let reference = adapter.post_approval_request(&request).await.unwrap();
        assert_eq!(reference.task_id, request.task_id);
        assert_eq!(adapter.post_count(), 1);

        // Poll immediately - should get approval
        let result = adapter.poll_for_decision(&reference).await.unwrap();
        assert!(result.is_some());
        let poll_result = result.unwrap();
        assert_eq!(poll_result.decision, PollDecision::Approved);
        assert_eq!(adapter.poll_count(), 1);
    }

    #[tokio::test]
    async fn test_instant_reject() {
        let adapter = MockAdapter::instant_reject();
        let request = make_test_request();

        let reference = adapter.post_approval_request(&request).await.unwrap();
        let result = adapter.poll_for_decision(&reference).await.unwrap();

        assert!(result.is_some());
        assert_eq!(result.unwrap().decision, PollDecision::Rejected);
    }

    #[tokio::test]
    async fn test_delayed_approval() {
        let adapter = MockAdapter::new(Duration::from_millis(50), true);
        let request = make_test_request();

        let reference = adapter.post_approval_request(&request).await.unwrap();

        // Poll immediately - should still be pending
        let result = adapter.poll_for_decision(&reference).await.unwrap();
        assert!(result.is_none());

        // Wait for delay
        tokio::time::sleep(Duration::from_millis(60)).await;

        // Poll again - should now be approved
        let result = adapter.poll_for_decision(&reference).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().decision, PollDecision::Approved);
    }

    #[test]
    fn test_adapter_name() {
        let adapter = MockAdapter::instant_approve();
        assert_eq!(adapter.name(), "mock");
    }

    #[tokio::test]
    async fn test_cancel_is_noop() {
        let adapter = MockAdapter::instant_approve();
        let request = make_test_request();

        let reference = adapter.post_approval_request(&request).await.unwrap();

        // Cancel should succeed (no-op)
        let result = adapter.cancel_approval(&reference).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_reset_counters() {
        let adapter = MockAdapter::instant_approve();
        adapter.post_count.store(10, Ordering::Relaxed);
        adapter.poll_count.store(20, Ordering::Relaxed);

        adapter.reset_counters();

        assert_eq!(adapter.post_count(), 0);
        assert_eq!(adapter.poll_count(), 0);
    }
}
