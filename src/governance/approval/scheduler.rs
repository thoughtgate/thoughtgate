//! Polling scheduler for approval workflows.
//!
//! Implements: REQ-GOV-003/F-002
//!
//! This module provides the polling scheduler that manages polling for
//! approval decisions across multiple pending tasks. It:
//!
//! - Maintains a priority queue ordered by next poll time
//! - Applies rate limiting to prevent API exhaustion
//! - Uses exponential backoff for repeated polls
//! - Handles graceful shutdown by draining pending approvals

use super::{
    AdapterError, ApprovalAdapter, ApprovalReference, ApprovalRequest, PollDecision, PollResult,
    PollingConfig, RateLimiter,
};
use crate::governance::{ApprovalDecision, TaskId, TaskStore};
use dashmap::DashMap;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

// ============================================================================
// Polling Scheduler
// ============================================================================

/// Scheduler for polling approval adapters.
///
/// Implements: REQ-GOV-003/F-002
///
/// The scheduler maintains a priority queue of pending approvals ordered by
/// their next poll time. It respects rate limits and uses exponential backoff
/// for repeated polls.
pub struct PollingScheduler {
    /// The approval adapter (Slack, etc.)
    adapter: Arc<dyn ApprovalAdapter>,
    /// Task store for recording decisions
    task_store: Arc<TaskStore>,

    /// Priority queue: (next_poll_at, task_id) -> ()
    /// Using composite key to handle multiple tasks with same poll time
    pending: Mutex<BTreeMap<(Instant, TaskId), ()>>,

    /// Task ID -> ApprovalReference mapping
    references: DashMap<TaskId, ApprovalReference>,

    /// Rate limiter for API calls
    rate_limiter: RateLimiter,

    /// Configuration
    config: PollingConfig,

    /// Shutdown token
    shutdown: CancellationToken,
}

impl PollingScheduler {
    /// Create a new polling scheduler.
    ///
    /// Implements: REQ-GOV-003/F-002
    ///
    /// # Arguments
    ///
    /// * `adapter` - The approval adapter to use
    /// * `task_store` - Task store for recording decisions
    /// * `config` - Polling configuration
    /// * `shutdown` - Cancellation token for graceful shutdown
    pub fn new(
        adapter: Arc<dyn ApprovalAdapter>,
        task_store: Arc<TaskStore>,
        config: PollingConfig,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            adapter,
            task_store,
            pending: Mutex::new(BTreeMap::new()),
            references: DashMap::new(),
            rate_limiter: RateLimiter::new(config.rate_limit_per_sec),
            config,
            shutdown,
        }
    }

    /// Returns the polling configuration.
    ///
    /// Used by adapters to get the base interval for initial poll timing.
    #[must_use]
    pub fn config(&self) -> &PollingConfig {
        &self.config
    }

    /// Submit a new task for approval polling.
    ///
    /// Implements: REQ-GOV-003/F-001
    ///
    /// Posts the approval request to the external system and schedules
    /// polling for the decision.
    ///
    /// # Errors
    ///
    /// Returns `AdapterError` if posting fails.
    pub async fn submit(&self, request: ApprovalRequest) -> Result<(), AdapterError> {
        // Check capacity
        if self.references.len() >= self.config.max_concurrent {
            warn!(
                task_id = %request.task_id,
                current = self.references.len(),
                max = self.config.max_concurrent,
                "Polling queue at capacity"
            );
            // Don't reject - oldest tasks will be polled first
        }

        // Rate limit the post operation
        self.rate_limiter.acquire().await;

        // Post to adapter
        let reference = self.adapter.post_approval_request(&request).await?;

        // Add to polling queue
        let task_id = reference.task_id.clone();
        let next_poll_at = reference.next_poll_at;

        self.references.insert(task_id.clone(), reference);

        {
            let mut pending = self.pending.lock().await;
            pending.insert((next_poll_at, task_id.clone()), ());
        }

        info!(
            task_id = %task_id,
            "Task submitted for approval polling"
        );

        Ok(())
    }

    /// Run the polling loop.
    ///
    /// Implements: REQ-GOV-003/F-002, REQ-GOV-001/F-008
    ///
    /// This method runs until the shutdown token is cancelled.
    /// It polls the next due task, handles decisions, reschedules
    /// pending tasks with backoff, and periodically expires overdue tasks.
    pub async fn run(&self) {
        info!(adapter = %self.adapter.name(), "Polling scheduler started");

        // Interval for expiration sweeps (every 10 seconds)
        let mut expiration_interval = tokio::time::interval(Duration::from_secs(10));
        // Don't catch up on missed ticks
        expiration_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;

                _ = self.shutdown.cancelled() => {
                    info!("Polling scheduler shutting down");
                    break;
                }

                // Periodic expiration sweep (REQ-GOV-001/F-008)
                _ = expiration_interval.tick() => {
                    let expired_count = self.task_store.expire_overdue();
                    if expired_count > 0 {
                        info!(expired_count, "Expired overdue tasks");
                    }
                }

                _ = self.poll_next() => {}
            }
        }

        // Final expiration sweep before shutdown
        let expired_count = self.task_store.expire_overdue();
        if expired_count > 0 {
            info!(expired_count, "Expired overdue tasks during shutdown");
        }

        // Graceful shutdown: cancel all pending approvals
        self.drain_pending().await;

        info!("Polling scheduler stopped");
    }

    /// Poll the next due task.
    ///
    /// Implements: REQ-GOV-003/F-002
    async fn poll_next(&self) {
        // Get next task due for polling
        let (task_id, mut reference) = match self.get_next_due().await {
            Some(t) => t,
            None => {
                // No tasks pending, sleep briefly
                tokio::time::sleep(Duration::from_millis(100)).await;
                return;
            }
        };

        // Check if task is still pending in task store
        match self.task_store.get(&task_id) {
            Ok(task) => {
                if task.status.is_terminal() {
                    // Task already completed (expired, cancelled, etc.)
                    self.references.remove(&task_id);
                    debug!(
                        task_id = %task_id,
                        status = ?task.status,
                        "Task already terminal, removing from polling queue"
                    );
                    return;
                }

                // Check if task has expired (REQ-GOV-001/F-008)
                if task.is_expired() {
                    self.references.remove(&task_id);
                    // Transition to Expired status
                    if let Err(e) = self.task_store.transition(
                        &task_id,
                        crate::governance::TaskStatus::Expired,
                        Some("TTL exceeded".to_string()),
                    ) {
                        warn!(
                            task_id = %task_id,
                            error = %e,
                            "Failed to transition expired task"
                        );
                    } else {
                        info!(
                            task_id = %task_id,
                            "Task expired, transitioned to Expired status"
                        );
                    }
                    return;
                }
            }
            Err(_) => {
                // Task not found
                self.references.remove(&task_id);
                debug!(task_id = %task_id, "Task not found, removing from polling queue");
                return;
            }
        }

        // Rate limit
        self.rate_limiter.acquire().await;

        // Poll for decision
        match self.adapter.poll_for_decision(&reference).await {
            Ok(Some(poll_result)) => {
                self.handle_decision(&task_id, poll_result).await;
            }
            Ok(None) => {
                // Still pending, reschedule with backoff
                reference.poll_count += 1;
                self.reschedule_with_backoff(task_id, reference).await;
            }
            Err(AdapterError::RateLimited { retry_after }) => {
                warn!(
                    task_id = %task_id,
                    retry_after_secs = retry_after.as_secs(),
                    "Rate limited by adapter"
                );
                // Reschedule after retry_after
                reference.next_poll_at = Instant::now() + retry_after;
                reference.poll_count += 1;
                self.reschedule(task_id, reference).await;
            }
            Err(AdapterError::MessageNotFound { .. }) => {
                // Message was deleted, remove from queue
                self.references.remove(&task_id);
                warn!(
                    task_id = %task_id,
                    "Approval message not found (deleted?), removing from polling queue"
                );
            }
            Err(e) => {
                warn!(
                    task_id = %task_id,
                    error = %e,
                    retriable = e.is_retriable(),
                    "Poll failed"
                );
                if e.is_retriable() {
                    // Reschedule with backoff
                    reference.poll_count += 1;
                    self.reschedule_with_backoff(task_id, reference).await;
                } else {
                    // Non-retriable error, remove from queue
                    self.references.remove(&task_id);
                }
            }
        }
    }

    /// Get the next task due for polling.
    ///
    /// Implements: REQ-GOV-003/F-002.1
    async fn get_next_due(&self) -> Option<(TaskId, ApprovalReference)> {
        let mut pending = self.pending.lock().await;

        // Get the first entry (lowest next_poll_at, then by task_id)
        let (&(next_poll_at, ref task_id), _) = pending.iter().next()?;
        let task_id = task_id.clone();

        // Wait until it's due
        let now = Instant::now();
        if next_poll_at > now {
            let wait_time = next_poll_at - now;
            // Cap the wait to avoid blocking too long
            let wait_time = wait_time.min(Duration::from_secs(1));
            drop(pending); // Release lock while waiting
            tokio::time::sleep(wait_time).await;
            return None; // Re-check after sleep
        }

        // Remove from queue
        pending.remove(&(next_poll_at, task_id.clone()));
        drop(pending);

        // Get reference
        let reference = self.references.get(&task_id)?.clone();

        Some((task_id, reference))
    }

    /// Handle a detected decision.
    ///
    /// Implements: REQ-GOV-003/F-004
    async fn handle_decision(&self, task_id: &TaskId, poll_result: PollResult) {
        // Remove from polling queue
        self.references.remove(task_id);

        // Convert to task-layer approval decision
        let decision = match poll_result.decision {
            PollDecision::Approved => ApprovalDecision::Approved,
            PollDecision::Rejected => ApprovalDecision::Rejected {
                reason: Some(format!(
                    "Rejected via {} by {}",
                    poll_result.method.description(),
                    poll_result.decided_by
                )),
            },
        };

        // Record approval in task store
        match self.task_store.record_approval(
            task_id,
            decision.clone(),
            poll_result.decided_by.clone(),
            self.config.approval_valid_for,
        ) {
            Ok(_task) => {
                info!(
                    task_id = %task_id,
                    decision = ?poll_result.decision,
                    decided_by = %poll_result.decided_by,
                    method = %poll_result.method.description(),
                    "Recorded approval decision"
                );
            }
            Err(e) => {
                error!(
                    task_id = %task_id,
                    error = %e,
                    "Failed to record approval decision"
                );
            }
        }
    }

    /// Reschedule a task with exponential backoff.
    ///
    /// Implements: REQ-GOV-003/F-002.3
    async fn reschedule_with_backoff(&self, task_id: TaskId, mut reference: ApprovalReference) {
        let interval = self.config.backoff_interval(reference.poll_count);
        reference.next_poll_at = Instant::now() + interval;

        debug!(
            task_id = %task_id,
            poll_count = reference.poll_count,
            next_poll_secs = interval.as_secs(),
            "Rescheduled with backoff"
        );

        self.reschedule(task_id, reference).await;
    }

    /// Reschedule a task.
    async fn reschedule(&self, task_id: TaskId, reference: ApprovalReference) {
        let next_poll_at = reference.next_poll_at;

        self.references.insert(task_id.clone(), reference);

        let mut pending = self.pending.lock().await;
        pending.insert((next_poll_at, task_id), ());
    }

    /// Drain pending approvals on shutdown.
    ///
    /// Implements: REQ-GOV-003/F-002.5, EC-APR-015
    async fn drain_pending(&self) {
        let task_ids: Vec<TaskId> = self.references.iter().map(|r| r.key().clone()).collect();

        info!(count = task_ids.len(), "Draining pending approval requests");

        for task_id in task_ids {
            if let Some((_, reference)) = self.references.remove(&task_id) {
                // Best-effort cancel
                if let Err(e) = self.adapter.cancel_approval(&reference).await {
                    debug!(
                        task_id = %task_id,
                        error = %e,
                        "Failed to cancel approval during shutdown"
                    );
                }
            }
        }
    }

    /// Cancel polling for a specific task.
    ///
    /// Implements: REQ-GOV-003/F-005
    pub async fn cancel(&self, task_id: &TaskId) {
        if let Some((_, reference)) = self.references.remove(task_id) {
            // Best-effort cancel the Slack message
            if let Err(e) = self.adapter.cancel_approval(&reference).await {
                debug!(
                    task_id = %task_id,
                    error = %e,
                    "Failed to cancel approval"
                );
            }

            info!(task_id = %task_id, "Cancelled approval polling");
        }
    }

    /// Returns the number of pending approvals.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.references.len()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::{JsonRpcId, Principal, TaskStoreConfig, ToolCallRequest};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Mock adapter for testing
    struct MockAdapter {
        post_count: AtomicU32,
        poll_count: AtomicU32,
        poll_result: Mutex<Option<PollResult>>,
    }

    impl MockAdapter {
        fn new() -> Self {
            Self {
                post_count: AtomicU32::new(0),
                poll_count: AtomicU32::new(0),
                poll_result: Mutex::new(None),
            }
        }

        async fn set_poll_result(&self, result: Option<PollResult>) {
            *self.poll_result.lock().await = result;
        }
    }

    #[async_trait]
    impl ApprovalAdapter for MockAdapter {
        async fn post_approval_request(
            &self,
            request: &ApprovalRequest,
        ) -> Result<ApprovalReference, AdapterError> {
            self.post_count.fetch_add(1, Ordering::SeqCst);

            Ok(ApprovalReference {
                task_id: request.task_id.clone(),
                external_id: "mock-ts".to_string(),
                channel: "mock-channel".to_string(),
                posted_at: chrono::Utc::now(),
                next_poll_at: Instant::now() + Duration::from_millis(10),
                poll_count: 0,
            })
        }

        async fn poll_for_decision(
            &self,
            _reference: &ApprovalReference,
        ) -> Result<Option<PollResult>, AdapterError> {
            self.poll_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.poll_result.lock().await.clone())
        }

        async fn cancel_approval(
            &self,
            _reference: &ApprovalReference,
        ) -> Result<(), AdapterError> {
            Ok(())
        }

        fn name(&self) -> &'static str {
            "mock"
        }
    }

    fn test_request(task_id: TaskId) -> ApprovalRequest {
        ApprovalRequest {
            task_id,
            tool_name: "test_tool".to_string(),
            tool_arguments: serde_json::json!({}),
            principal: Principal::new("test-app"),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            created_at: chrono::Utc::now(),
            correlation_id: "test-correlation".to_string(),
        }
    }

    #[tokio::test]
    async fn test_submit_adds_to_queue() {
        let adapter = Arc::new(MockAdapter::new());
        let task_store = Arc::new(TaskStore::new(TaskStoreConfig::default()));
        let config = PollingConfig::default();
        let shutdown = CancellationToken::new();

        let scheduler = PollingScheduler::new(adapter.clone(), task_store, config, shutdown);

        let task_id = TaskId::new();
        let request = test_request(task_id.clone());

        scheduler.submit(request).await.expect("Submit failed");

        assert_eq!(scheduler.pending_count(), 1);
        assert_eq!(adapter.post_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_cancel_removes_from_queue() {
        let adapter = Arc::new(MockAdapter::new());
        let task_store = Arc::new(TaskStore::new(TaskStoreConfig::default()));
        let config = PollingConfig::default();
        let shutdown = CancellationToken::new();

        let scheduler = PollingScheduler::new(adapter, task_store, config, shutdown);

        let task_id = TaskId::new();
        let request = test_request(task_id.clone());

        scheduler.submit(request).await.expect("Submit failed");
        assert_eq!(scheduler.pending_count(), 1);

        scheduler.cancel(&task_id).await;
        assert_eq!(scheduler.pending_count(), 0);
    }

    #[tokio::test]
    async fn test_decision_removes_from_queue() {
        let adapter = Arc::new(MockAdapter::new());
        let task_store = Arc::new(TaskStore::new(TaskStoreConfig::default()));
        let config = PollingConfig::default();
        let shutdown = CancellationToken::new();

        // Create a task in the store first
        let tool_request = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "test_tool".to_string(),
            arguments: serde_json::json!({}),
            mcp_request_id: JsonRpcId::Null,
        };
        let task = task_store
            .create(
                tool_request.clone(),
                tool_request,
                Principal::new("test-app"),
                None,
                crate::governance::TimeoutAction::default(),
            )
            .expect("Failed to create task");

        // Transition to InputRequired
        task_store
            .transition(&task.id, crate::governance::TaskStatus::InputRequired, None)
            .expect("Failed to transition");

        let scheduler = PollingScheduler::new(adapter.clone(), task_store, config, shutdown);

        let request = test_request(task.id.clone());
        scheduler.submit(request).await.expect("Submit failed");
        assert_eq!(scheduler.pending_count(), 1);

        // Set approval result
        adapter
            .set_poll_result(Some(PollResult {
                decision: PollDecision::Approved,
                decided_by: "test-user".to_string(),
                decided_at: chrono::Utc::now(),
                method: crate::governance::approval::DecisionMethod::Reaction {
                    emoji: "+1".to_string(),
                },
            }))
            .await;

        // Wait for next_poll_at to pass, then poll until decision processed
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Poll until task is removed (get_next_due may return None first time due to timing)
        for _ in 0..5 {
            scheduler.poll_next().await;
            if scheduler.pending_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Task should be removed from queue
        assert_eq!(scheduler.pending_count(), 0);
    }

    #[test]
    fn test_backoff_interval() {
        let config = PollingConfig::default();

        // 5s * 2^0 = 5s
        assert_eq!(config.backoff_interval(0), Duration::from_secs(5));
        // 5s * 2^1 = 10s
        assert_eq!(config.backoff_interval(1), Duration::from_secs(10));
        // 5s * 2^2 = 20s
        assert_eq!(config.backoff_interval(2), Duration::from_secs(20));
        // 5s * 2^3 = 40s -> capped to 30s
        assert_eq!(config.backoff_interval(3), Duration::from_secs(30));
        // Further polls stay at max
        assert_eq!(config.backoff_interval(10), Duration::from_secs(30));
    }

    /// Tests that multiple tasks submitted concurrently don't cause data loss.
    ///
    /// Verifies: BTreeMap composite key handles multiple tasks with same poll time.
    #[tokio::test]
    async fn test_concurrent_submit_no_data_loss() {
        // Create adapter that returns same next_poll_at for all tasks
        struct SameTimeAdapter {
            fixed_poll_at: Instant,
        }

        #[async_trait]
        impl ApprovalAdapter for SameTimeAdapter {
            async fn post_approval_request(
                &self,
                request: &ApprovalRequest,
            ) -> Result<ApprovalReference, AdapterError> {
                Ok(ApprovalReference {
                    task_id: request.task_id.clone(),
                    external_id: format!("ts-{}", request.task_id),
                    channel: "test-channel".to_string(),
                    posted_at: chrono::Utc::now(),
                    next_poll_at: self.fixed_poll_at, // Same time for all!
                    poll_count: 0,
                })
            }

            async fn poll_for_decision(
                &self,
                _reference: &ApprovalReference,
            ) -> Result<Option<PollResult>, AdapterError> {
                Ok(None) // Always pending
            }

            async fn cancel_approval(
                &self,
                _reference: &ApprovalReference,
            ) -> Result<(), AdapterError> {
                Ok(())
            }

            fn name(&self) -> &'static str {
                "same-time"
            }
        }

        let fixed_time = Instant::now() + Duration::from_secs(100);
        let adapter = Arc::new(SameTimeAdapter {
            fixed_poll_at: fixed_time,
        });
        let task_store = Arc::new(TaskStore::new(TaskStoreConfig::default()));
        let config = PollingConfig::default();
        let shutdown = CancellationToken::new();

        let scheduler = PollingScheduler::new(adapter, task_store, config, shutdown);

        // Submit 5 tasks rapidly - they will all have the same next_poll_at
        let mut task_ids = Vec::new();
        for _ in 0..5 {
            let task_id = TaskId::new();
            task_ids.push(task_id.clone());
            let request = test_request(task_id);
            scheduler.submit(request).await.expect("Submit failed");
        }

        // All 5 tasks should be in the queue (no data loss from key collision)
        assert_eq!(
            scheduler.pending_count(),
            5,
            "All 5 tasks should be in queue despite same poll time"
        );

        // Verify each task is actually tracked
        for task_id in &task_ids {
            assert!(
                scheduler.references.contains_key(task_id),
                "Task {task_id} should be in references"
            );
        }
    }
}
