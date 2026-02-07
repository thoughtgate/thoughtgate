//! In-memory task store with concurrent access support.
//!
//! Implements: REQ-GOV-001/§10

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

use super::error::TaskError;
use super::status::TaskStatus;
use super::types::*;
use crate::governance::engine::TimeoutAction;

// ============================================================================
// Task Store Configuration
// ============================================================================

/// Configuration for the task store.
///
/// Implements: REQ-GOV-001/§5.2
#[derive(Debug, Clone)]
pub struct TaskStoreConfig {
    /// Default TTL for new tasks
    pub default_ttl: Duration,
    /// Maximum TTL allowed
    pub max_ttl: Duration,
    /// Minimum TTL allowed
    pub min_ttl: Duration,
    /// How often to run cleanup
    pub cleanup_interval: Duration,
    /// Maximum pending tasks per principal
    pub max_pending_per_principal: usize,
    /// Maximum pending tasks globally
    pub max_pending_global: usize,
    /// Grace period after terminal before removal
    pub terminal_grace_period: Duration,
}

impl Default for TaskStoreConfig {
    fn default() -> Self {
        Self {
            default_ttl: Duration::from_secs(600),     // 10 minutes
            max_ttl: Duration::from_secs(86400),       // 24 hours
            min_ttl: Duration::from_secs(60),          // 1 minute
            cleanup_interval: Duration::from_secs(60), // 1 minute
            max_pending_per_principal: 10,
            max_pending_global: 1000,
            terminal_grace_period: Duration::from_secs(3600), // 1 hour
        }
    }
}

// ============================================================================
// Task Store
// ============================================================================

/// Internal task entry with metadata for cleanup.
///
/// Stores `Arc<Task>` to avoid deep clones on reads. Mutations use
/// `Arc::make_mut` which clones only when other references exist
/// (copy-on-write semantics).
#[derive(Debug)]
struct TaskEntry {
    /// The task itself (Arc for cheap reads, make_mut for writes)
    task: Arc<Task>,
    /// When the task became terminal (for grace period cleanup)
    terminal_at: Option<DateTime<Utc>>,
    /// Notifier for waiters on this task
    notify: Arc<Notify>,
}

/// In-memory task store with concurrent access support.
///
/// Implements: REQ-GOV-001/§10
///
/// Uses DashMap for lock-free concurrent access to tasks.
pub struct TaskStore {
    /// Task storage keyed by TaskId
    tasks: DashMap<TaskId, TaskEntry>,
    /// Index of task IDs by principal for rate limiting and listing
    by_principal: DashMap<String, Vec<TaskId>>,
    /// Configuration
    config: TaskStoreConfig,
    /// Counter for pending (non-terminal) tasks
    pending_count: AtomicUsize,
    /// Atomic counter of pending tasks per principal (keyed by rate_limit_key).
    /// Prevents TOCTOU race between count check and insert in `create()`.
    pending_by_principal: DashMap<String, AtomicUsize>,
    /// Optional Prometheus metrics for gauge updates (REQ-OBS-002 §6.4/MG-002).
    /// When set, the tasks_pending gauge is updated on task creation/completion.
    tg_metrics: Option<Arc<crate::telemetry::ThoughtGateMetrics>>,
}

impl std::fmt::Debug for TaskStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskStore")
            .field("tasks_count", &self.tasks.len())
            .field("config", &self.config)
            .field("pending_count", &self.pending_count.load(Ordering::Acquire))
            .field("has_metrics", &self.tg_metrics.is_some())
            .finish()
    }
}

impl TaskStore {
    /// Creates a new task store with the given configuration.
    ///
    /// Implements: REQ-GOV-001/§10
    #[must_use]
    pub fn new(config: TaskStoreConfig) -> Self {
        Self {
            tasks: DashMap::new(),
            by_principal: DashMap::new(),
            config,
            pending_count: AtomicUsize::new(0),
            pending_by_principal: DashMap::new(),
            tg_metrics: None,
        }
    }

    /// Creates a new task store with default configuration.
    ///
    /// Implements: REQ-GOV-001/§10
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(TaskStoreConfig::default())
    }

    /// Sets the Prometheus metrics for gauge updates.
    ///
    /// When set, the `tasks_pending` gauge is updated whenever tasks
    /// are created or transition to terminal states.
    ///
    /// Implements: REQ-OBS-002 §6.4/MG-002
    pub fn set_metrics(&mut self, metrics: Arc<crate::telemetry::ThoughtGateMetrics>) {
        self.tg_metrics = Some(metrics);
    }

    /// Creates a new task store with metrics wired for gauge updates.
    ///
    /// Implements: REQ-GOV-001/§10, REQ-OBS-002 §6.4/MG-002
    #[must_use]
    pub fn with_metrics(
        config: TaskStoreConfig,
        metrics: Arc<crate::telemetry::ThoughtGateMetrics>,
    ) -> Self {
        Self {
            tasks: DashMap::new(),
            by_principal: DashMap::new(),
            config,
            pending_count: AtomicUsize::new(0),
            pending_by_principal: DashMap::new(),
            tg_metrics: Some(metrics),
        }
    }

    /// Returns the store configuration.
    ///
    /// Implements: REQ-GOV-001/§5.2
    #[must_use]
    pub fn config(&self) -> &TaskStoreConfig {
        &self.config
    }

    /// Decrements both global and per-principal pending counters.
    ///
    /// Must be called exactly once when a task transitions from non-terminal
    /// to terminal. Caller is responsible for ensuring the task was previously
    /// non-terminal (i.e., `was_terminal == false`).
    ///
    /// Also updates the Prometheus gauge if metrics are configured
    /// (REQ-OBS-002 §6.4/MG-002).
    fn decrement_pending_counters(&self, task: &Task) {
        let new_count = self.pending_count.fetch_sub(1, Ordering::Release) - 1;

        // Update Prometheus gauge (REQ-OBS-002 §6.4/MG-002)
        if let Some(ref metrics) = self.tg_metrics {
            metrics
                .tasks_pending
                .get_or_create(&crate::telemetry::prom_metrics::TaskTypeLabels {
                    task_type: std::borrow::Cow::Borrowed("approval"),
                })
                .set(new_count as i64);
        }

        let principal_key = task.principal.rate_limit_key();
        if let Some(counter) = self.pending_by_principal.get(&principal_key) {
            let prev = counter.fetch_sub(1, Ordering::Release);
            drop(counter);
            // Clean up entry when count reaches zero to prevent unbounded growth
            // of the DashMap with stale zero-valued entries.
            if prev == 1 {
                self.pending_by_principal
                    .remove_if(&principal_key, |_, v| v.load(Ordering::Acquire) == 0);
            }
        }
    }

    /// Returns the number of pending (non-terminal) tasks.
    ///
    /// Implements: REQ-GOV-001/F-009.3
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending_count.load(Ordering::Acquire)
    }

    /// Returns the total number of tasks (including terminal).
    ///
    /// Implements: REQ-GOV-001/§10
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.tasks.len()
    }

    /// Returns true if the principal index contains the given key.
    ///
    /// Exposed for testing principal index cleanup behavior.
    #[cfg(test)]
    pub(super) fn has_principal_entry(&self, key: &str) -> bool {
        self.by_principal.contains_key(key)
    }

    /// Creates and inserts a new task.
    ///
    /// Implements: REQ-GOV-001/F-002, F-009
    ///
    /// Checks rate limits and capacity before insertion.
    ///
    /// # Concurrency Note
    ///
    /// Global capacity uses a compare_exchange loop for atomic reservation.
    /// The slot is reserved before insertion and rolled back if the per-principal
    /// check fails, ensuring strict enforcement under concurrent access.
    pub fn create(
        &self,
        original_request: ToolCallRequest,
        pre_approval_transformed: ToolCallRequest,
        principal: Principal,
        ttl: Option<Duration>,
        on_timeout: TimeoutAction,
    ) -> Result<Arc<Task>, TaskError> {
        // F-009.3, F-009.4: Atomically reserve a global capacity slot
        loop {
            let current = self.pending_count.load(Ordering::Acquire);
            if current >= self.config.max_pending_global {
                return Err(TaskError::CapacityExceeded);
            }
            if self
                .pending_count
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
            // CAS failed — another thread changed pending_count, retry
        }

        // F-009.1, F-009.2: Atomically check and reserve per-principal slot.
        // Uses atomic increment + check to prevent TOCTOU race between
        // count check and insert under concurrent access.
        let principal_key = principal.rate_limit_key();
        let principal_counter = self
            .pending_by_principal
            .entry(principal_key.clone())
            .or_insert_with(|| AtomicUsize::new(0));
        let prev = principal_counter.fetch_add(1, Ordering::AcqRel);
        if prev >= self.config.max_pending_per_principal {
            // Rollback both counters
            principal_counter.fetch_sub(1, Ordering::Release);
            self.pending_count.fetch_sub(1, Ordering::Release);
            return Err(TaskError::RateLimited {
                principal: principal_key,
                retry_after: Duration::from_secs(60),
            });
        }

        // F-002.4: Apply TTL bounds
        let ttl = ttl.unwrap_or(self.config.default_ttl);
        let ttl = ttl.clamp(self.config.min_ttl, self.config.max_ttl);

        // Create task with on_timeout captured at creation time
        let task = Task::new(
            original_request,
            pre_approval_transformed,
            principal,
            ttl,
            on_timeout,
        )
        .map_err(|e| {
            // Rollback counters reserved above (same pattern as rate-limit rollback)
            principal_counter.fetch_sub(1, Ordering::Release);
            self.pending_count.fetch_sub(1, Ordering::Release);
            TaskError::Internal { details: e }
        })?;
        let task_id = task.id.clone();
        let task_arc = Arc::new(task);

        // Insert into store
        let entry = TaskEntry {
            task: task_arc.clone(),
            terminal_at: None,
            notify: Arc::new(Notify::new()),
        };
        self.tasks.insert(task_id.clone(), entry);

        // Update principal index
        self.by_principal
            .entry(principal_key)
            .or_default()
            .push(task_id);

        // Update Prometheus gauge (REQ-OBS-002 §6.4/MG-002)
        // Note: pending_count was already incremented via compare_exchange above
        if let Some(ref metrics) = self.tg_metrics {
            metrics
                .tasks_pending
                .get_or_create(&crate::telemetry::prom_metrics::TaskTypeLabels {
                    task_type: std::borrow::Cow::Borrowed("approval"),
                })
                .set(self.pending_count.load(Ordering::Acquire) as i64);
        }

        Ok(task_arc)
    }

    /// Gets a task by ID.
    ///
    /// Implements: REQ-GOV-001/F-003
    pub fn get(&self, task_id: &TaskId) -> Result<Arc<Task>, TaskError> {
        self.tasks
            .get(task_id)
            .map(|entry| entry.task.clone())
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })
    }

    /// Transitions a task to a new status.
    ///
    /// Implements: REQ-GOV-001/F-001, F-007
    pub fn transition(
        &self,
        task_id: &TaskId,
        new_status: TaskStatus,
        reason: Option<String>,
    ) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        let was_terminal = entry.task.status.is_terminal();
        Arc::make_mut(&mut entry.task).transition(new_status, reason)?;

        // Track when task became terminal
        if !was_terminal && entry.task.status.is_terminal() {
            entry.terminal_at = Some(Utc::now());
            self.decrement_pending_counters(&entry.task);
            entry.notify.notify_waiters();
        }

        Ok(entry.task.clone())
    }

    /// Transitions a task with optimistic locking.
    ///
    /// Implements: REQ-GOV-001/F-007
    pub fn transition_if(
        &self,
        task_id: &TaskId,
        expected_status: TaskStatus,
        new_status: TaskStatus,
        reason: Option<String>,
    ) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        let was_terminal = entry.task.status.is_terminal();
        Arc::make_mut(&mut entry.task).transition_if(expected_status, new_status, reason)?;

        // Track when task became terminal
        if !was_terminal && entry.task.status.is_terminal() {
            entry.terminal_at = Some(Utc::now());
            self.decrement_pending_counters(&entry.task);
            entry.notify.notify_waiters();
        }

        Ok(entry.task.clone())
    }

    /// Records an approval decision on a task.
    ///
    /// Implements: REQ-GOV-001 (called by REQ-GOV-003)
    pub fn record_approval(
        &self,
        task_id: &TaskId,
        decision: ApprovalDecision,
        decided_by: String,
        approval_valid_for: Duration,
    ) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        // Must be in InputRequired state
        if entry.task.status != TaskStatus::InputRequired {
            return Err(TaskError::ConcurrentModification {
                task_id: task_id.clone(),
                expected: TaskStatus::InputRequired,
                actual: entry.task.status,
            });
        }

        let now = Utc::now();
        let approval_valid_until = now
            + chrono::Duration::from_std(approval_valid_for).unwrap_or(chrono::Duration::zero());

        // Transition based on decision
        let (new_status, reason) = match &decision {
            ApprovalDecision::Approved => (TaskStatus::Executing, Some("Approved".to_string())),
            ApprovalDecision::Rejected { reason } => (
                TaskStatus::Rejected,
                reason.clone().or_else(|| Some("Rejected".to_string())),
            ),
        };

        let was_terminal = entry.task.status.is_terminal();
        {
            let task = Arc::make_mut(&mut entry.task);
            task.approval = Some(ApprovalRecord {
                decision,
                decided_by,
                decided_at: now,
                approval_valid_until,
                metadata: None,
            });
            task.transition(new_status, reason)?;
        }

        if !was_terminal && entry.task.status.is_terminal() {
            entry.terminal_at = Some(Utc::now());
            self.decrement_pending_counters(&entry.task);
            entry.notify.notify_waiters();
        }

        Ok(entry.task.clone())
    }

    /// Marks a task as completed with a result.
    ///
    /// Implements: REQ-GOV-001 (called by REQ-GOV-002)
    pub fn complete(
        &self,
        task_id: &TaskId,
        result: ToolCallResult,
    ) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        // Must be in Executing state
        if entry.task.status != TaskStatus::Executing {
            return Err(TaskError::ConcurrentModification {
                task_id: task_id.clone(),
                expected: TaskStatus::Executing,
                actual: entry.task.status,
            });
        }

        {
            let task = Arc::make_mut(&mut entry.task);
            task.result = Some(result);
            task.transition(
                TaskStatus::Completed,
                Some("Execution completed".to_string()),
            )?;
        }
        entry.terminal_at = Some(Utc::now());
        self.decrement_pending_counters(&entry.task);
        entry.notify.notify_waiters();

        Ok(entry.task.clone())
    }

    /// Records an execution result on an auto-approved expired task.
    ///
    /// Unlike `complete()`, this does not transition the task status (it stays
    /// `Expired`). It records the result and approval for audit purposes so
    /// that the task shows evidence of execution after auto-approval.
    pub fn record_auto_approve_result(
        &self,
        task_id: &TaskId,
        result: ToolCallResult,
        approval: ApprovalRecord,
    ) -> Result<(), TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        let task = Arc::make_mut(&mut entry.task);
        task.result = Some(result);
        task.approval = Some(approval);
        task.transitions.push(TaskTransition {
            from: TaskStatus::Expired,
            to: TaskStatus::Expired, // Status doesn't change (terminal)
            at: Utc::now(),
            reason: Some("Auto-approved after expiration; executed successfully".to_string()),
        });
        Ok(())
    }

    /// Marks a task as failed with failure info.
    ///
    /// Implements: REQ-GOV-001 (called by REQ-GOV-002)
    pub fn fail(&self, task_id: &TaskId, failure: FailureInfo) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        let was_terminal = entry.task.status.is_terminal();
        if was_terminal {
            return Err(TaskError::AlreadyTerminal {
                task_id: task_id.clone(),
                status: entry.task.status,
            });
        }

        {
            let task = Arc::make_mut(&mut entry.task);
            task.failure = Some(failure.clone());
            task.transition(TaskStatus::Failed, Some(failure.reason))?;
        }
        entry.terminal_at = Some(Utc::now());

        if !was_terminal {
            self.decrement_pending_counters(&entry.task);
        }
        entry.notify.notify_waiters();

        Ok(entry.task.clone())
    }

    /// Cancels a task (only from InputRequired state).
    ///
    /// Implements: REQ-GOV-001/F-006
    ///
    /// This operation is idempotent: cancelling an already-cancelled task
    /// returns success per SEP-1686 spec requirements.
    pub fn cancel(&self, task_id: &TaskId) -> Result<Arc<Task>, TaskError> {
        let mut entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| TaskError::NotFound {
                task_id: task_id.clone(),
            })?;

        // F-006.1: Only cancel from InputRequired
        if entry.task.status != TaskStatus::InputRequired {
            if entry.task.status.is_terminal() {
                // F-006.2: Idempotency - already cancelled returns success
                if entry.task.status == TaskStatus::Cancelled {
                    return Ok(entry.task.clone());
                }
                // F-006.2b: Other terminal states cannot be cancelled
                return Err(TaskError::AlreadyTerminal {
                    task_id: task_id.clone(),
                    status: entry.task.status,
                });
            }
            // F-006.3: Cannot cancel from non-InputRequired, non-terminal state (e.g., Executing)
            return Err(TaskError::InvalidTransition {
                task_id: task_id.clone(),
                from: entry.task.status,
                to: TaskStatus::Cancelled,
            });
        }

        Arc::make_mut(&mut entry.task).transition(
            TaskStatus::Cancelled,
            Some("Cancelled by agent".to_string()),
        )?;
        entry.terminal_at = Some(Utc::now());
        self.decrement_pending_counters(&entry.task);
        entry.notify.notify_waiters();

        Ok(entry.task.clone())
    }

    /// Expires non-terminal tasks that have exceeded their TTL.
    ///
    /// Implements: REQ-GOV-001/F-008
    ///
    /// Returns the number of tasks expired.
    pub fn expire_overdue(&self) -> usize {
        let now = Utc::now();
        let mut expired = 0;

        // Collect task IDs to expire (to avoid holding locks while modifying)
        let to_expire: Vec<TaskId> = self
            .tasks
            .iter()
            .filter_map(|entry| {
                let task = &entry.task;
                if !task.status.is_terminal() && now > task.expires_at {
                    Some(task.id.clone())
                } else {
                    None
                }
            })
            .collect();

        // Expire each task
        for task_id in to_expire {
            if let Some(mut entry) = self.tasks.get_mut(&task_id) {
                if !entry.task.status.is_terminal()
                    && now > entry.task.expires_at
                    && Arc::make_mut(&mut entry.task)
                        .transition(TaskStatus::Expired, Some("TTL exceeded".to_string()))
                        .is_ok()
                {
                    entry.terminal_at = Some(now);
                    self.decrement_pending_counters(&entry.task);
                    // Record MC-008: tasks_completed_total (prometheus-client) with outcome=expired
                    if let Some(ref metrics) = self.tg_metrics {
                        metrics.record_task_completed("approval", "expired");
                    }
                    entry.notify.notify_waiters();
                    expired += 1;
                    tracing::warn!(
                        task_id = %task_id,
                        tool = %entry.task.original_request.name,
                        age_seconds = (now - entry.task.created_at).num_seconds(),
                        "Task expired"
                    );
                }
            }
        }

        expired
    }

    /// Removes terminal tasks that have exceeded the grace period.
    ///
    /// Implements: REQ-GOV-001/F-008.4
    ///
    /// Returns the number of tasks removed.
    pub fn cleanup_terminal(&self) -> usize {
        let now = Utc::now();
        let grace_period =
            chrono::Duration::from_std(self.config.terminal_grace_period).unwrap_or_default();

        // Collect task IDs to remove
        let to_remove: Vec<TaskId> = self
            .tasks
            .iter()
            .filter_map(|entry| {
                if let Some(terminal_at) = entry.terminal_at
                    && now - terminal_at > grace_period
                {
                    return Some(entry.task.id.clone());
                }
                None
            })
            .collect();

        let count = to_remove.len();

        // Remove tasks
        for task_id in to_remove {
            if let Some((_, entry)) = self.tasks.remove(&task_id) {
                // Clean up principal index
                let principal_key = entry.task.principal.rate_limit_key();
                if let Some(mut ids) = self.by_principal.get_mut(&principal_key) {
                    ids.retain(|id| id != &task_id);
                    if ids.is_empty() {
                        // Drop the mutable reference before removing to avoid deadlock
                        drop(ids);
                        // Use remove_if to avoid TOCTOU race (another thread may have
                        // added a new ID between the drop and this remove)
                        self.by_principal
                            .remove_if(&principal_key, |_, v| v.is_empty());
                    }
                }
            }
        }

        count
    }

    /// Lists tasks for a principal with pagination.
    ///
    /// Implements: REQ-GOV-001/F-005
    pub fn list_for_principal(
        &self,
        principal: &Principal,
        offset: usize,
        limit: usize,
    ) -> Vec<Arc<Task>> {
        let principal_key = principal.rate_limit_key();
        let task_ids = match self.by_principal.get(&principal_key) {
            Some(ids) => ids.clone(),
            None => return Vec::new(),
        };

        // Collect tasks, sorted by creation time (newest first)
        let mut tasks: Vec<Arc<Task>> = task_ids
            .iter()
            .filter_map(|id| self.tasks.get(id).map(|e| e.task.clone()))
            .collect();

        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        // Apply pagination
        tasks.into_iter().skip(offset).take(limit).collect()
    }

    /// Waits for a task to reach a terminal state.
    ///
    /// Implements: REQ-GOV-001/§10
    ///
    /// Returns the task if it reaches a terminal state within the timeout,
    /// or an error if the timeout is exceeded.
    ///
    /// Uses a loop to handle the race between checking terminal status and
    /// registering for notifications. The Notify::notified() future is created
    /// before checking the status to avoid missing notifications.
    pub async fn wait_for_terminal(
        &self,
        task_id: &TaskId,
        timeout: Duration,
    ) -> Result<Arc<Task>, TaskError> {
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            // Get the notify handle first
            let notify = {
                let entry = self.tasks.get(task_id).ok_or_else(|| TaskError::NotFound {
                    task_id: task_id.clone(),
                })?;
                entry.notify.clone()
            };

            // Create the notified future BEFORE checking status to avoid race
            let notified = notify.notified();

            // Now check if already terminal (after creating notified future)
            {
                let entry = self.tasks.get(task_id).ok_or_else(|| TaskError::NotFound {
                    task_id: task_id.clone(),
                })?;

                if entry.task.status.is_terminal() {
                    return Ok(entry.task.clone());
                }
            }

            // Calculate remaining time
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(TaskError::ResultNotReady {
                    task_id: task_id.clone(),
                });
            }
            let remaining = deadline - now;

            // Wait for notification or timeout
            if tokio::time::timeout(remaining, notified).await.is_err() {
                // Timeout - do one final check
                let entry = self.tasks.get(task_id).ok_or_else(|| TaskError::NotFound {
                    task_id: task_id.clone(),
                })?;

                if entry.task.status.is_terminal() {
                    return Ok(entry.task.clone());
                }
                return Err(TaskError::ResultNotReady {
                    task_id: task_id.clone(),
                });
            }

            // Notified - loop back to check status (handles spurious wakeups)
        }
    }

    /// Marks all pending tasks as failed due to service shutdown.
    ///
    /// Implements: EC-TASK-016
    pub fn fail_all_pending(&self, reason: &str) -> usize {
        let failure = FailureInfo {
            stage: FailureStage::ServiceShutdown,
            reason: reason.to_string(),
            retriable: true,
        };

        let pending_ids: Vec<TaskId> = self
            .tasks
            .iter()
            .filter_map(|entry| {
                if !entry.task.status.is_terminal() {
                    Some(entry.task.id.clone())
                } else {
                    None
                }
            })
            .collect();

        let mut failed = 0;
        for task_id in pending_ids {
            if self.fail(&task_id, failure.clone()).is_ok() {
                failed += 1;
            }
        }

        failed
    }
}
