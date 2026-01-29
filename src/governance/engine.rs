//! Approval Engine - coordinator for the v0.2 approval workflow.
//!
//! Implements: REQ-GOV-002 (Approval Execution Pipeline)
//!
//! The ApprovalEngine coordinates the v0.2 simplified pipeline:
//! 1. Create task with stored request
//! 2. Post approval request to Slack
//! 3. Return task ID immediately
//! 4. Spawn background poller for decision
//! 5. Execute upstream on `tasks/result` call
//!
//! ## v0.2 Pipeline Flow
//!
//! ```text
//! tools/call (action: approve)
//!     │
//!     ▼
//! Create Task (status: pending)
//!     │
//!     ├──► Return TaskId immediately
//!     │
//!     └──► Spawn background:
//!             │
//!             ▼
//!          Post to Slack
//!             │
//!             ▼
//!          Poll for decision
//!             │
//!          ┌──┴──┐
//!          │     │
//!       approve reject
//!          │     │
//!          ▼     ▼
//!       task.state  task.state
//!       = Approved  = Failed(-32007)
//!
//! tasks/result (when called by agent):
//!     │
//!     ▼
//! If Approved → Execute upstream → Store result → Return
//! If Rejected → Return error (-32007)
//! If Pending  → Return "result not ready"
//! ```

use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::error::ThoughtGateError;
use crate::transport::UpstreamForwarder;

use super::approval::{ApprovalAdapter, ApprovalRequest, PollingConfig, PollingScheduler};
use super::pipeline::{ApprovalPipeline, ExecutionPipeline, PipelineConfig, PipelineResult};
use super::task::{FailureInfo, FailureStage, TaskStatus, ToolCallResult};
use super::{Principal, TaskError, TaskId, TaskStore, ToolCallRequest};

// ============================================================================
// Approval Engine Configuration
// ============================================================================

/// Configuration for the approval engine.
///
/// Implements: REQ-GOV-002/§5.1
#[derive(Debug, Clone)]
pub struct ApprovalEngineConfig {
    /// Timeout for approval workflow
    pub approval_timeout: Duration,
    /// Action on timeout: "deny" or "approve"
    pub on_timeout: TimeoutAction,
    /// Execution timeout for upstream calls
    pub execution_timeout: Duration,
}

impl Default for ApprovalEngineConfig {
    fn default() -> Self {
        Self {
            approval_timeout: Duration::from_secs(600), // 10 minutes
            on_timeout: TimeoutAction::Deny,
            execution_timeout: Duration::from_secs(30),
        }
    }
}

impl ApprovalEngineConfig {
    /// Load configuration from environment variables.
    ///
    /// Implements: REQ-GOV-002/§5.1 (Environment configuration)
    ///
    /// Handles: Reading env vars and defaulting timeouts
    ///
    /// # Environment Variables
    ///
    /// - `THOUGHTGATE_APPROVAL_TIMEOUT_SECS` - Approval timeout (default: 600)
    /// - `THOUGHTGATE_ON_TIMEOUT` - Action on timeout: "deny" or "approve" (default: deny)
    /// - `THOUGHTGATE_EXECUTION_TIMEOUT_SECS` - Execution timeout (default: 30)
    #[must_use]
    pub fn from_env() -> Self {
        let approval_timeout = std::env::var("THOUGHTGATE_APPROVAL_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(600));

        let on_timeout = std::env::var("THOUGHTGATE_ON_TIMEOUT")
            .ok()
            .map(|s| match s.to_lowercase().as_str() {
                "approve" => TimeoutAction::Approve,
                _ => TimeoutAction::Deny,
            })
            .unwrap_or(TimeoutAction::Deny);

        let execution_timeout = std::env::var("THOUGHTGATE_EXECUTION_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(30));

        Self {
            approval_timeout,
            on_timeout,
            execution_timeout,
        }
    }
}

/// Action to take when approval times out.
///
/// Implements: REQ-GOV-002/F-006.4
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeoutAction {
    /// Deny the request on timeout (return -32008)
    #[default]
    Deny,
    /// Auto-approve on timeout (dangerous, use with caution)
    Approve,
}

// ============================================================================
// Approval Engine Errors
// ============================================================================

/// Errors from the approval engine.
///
/// Implements: REQ-GOV-002/§6.4
#[derive(Debug, Clone)]
pub enum ApprovalEngineError {
    /// Failed to create task
    TaskCreation { details: String },
    /// Failed to post approval request
    PostFailed { details: String },
    /// Task not found
    TaskNotFound { task_id: TaskId },
    /// Task in unexpected state
    InvalidState { task_id: TaskId, status: TaskStatus },
    /// Upstream execution failed
    ExecutionFailed { details: String },
    /// Internal error
    Internal { details: String },
}

impl std::fmt::Display for ApprovalEngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TaskCreation { details } => write!(f, "Task creation failed: {details}"),
            Self::PostFailed { details } => write!(f, "Approval post failed: {details}"),
            Self::TaskNotFound { task_id } => write!(f, "Task not found: {task_id}"),
            Self::InvalidState { task_id, status } => {
                write!(f, "Task {task_id} in invalid state: {status}")
            }
            Self::ExecutionFailed { details } => write!(f, "Execution failed: {details}"),
            Self::Internal { details } => write!(f, "Internal error: {details}"),
        }
    }
}

impl std::error::Error for ApprovalEngineError {}

impl From<TaskError> for ApprovalEngineError {
    fn from(err: TaskError) -> Self {
        match err {
            TaskError::NotFound { task_id } => Self::TaskNotFound { task_id },
            TaskError::InvalidTransition { task_id, from, .. } => Self::InvalidState {
                task_id,
                status: from,
            },
            other => Self::Internal {
                details: other.to_string(),
            },
        }
    }
}

// ============================================================================
// Approval Start Result
// ============================================================================

/// Result of starting an approval workflow.
///
/// Implements: REQ-GOV-002/§6.2
#[derive(Debug, Clone)]
pub struct ApprovalStartResult {
    /// The created task ID
    pub task_id: TaskId,
    /// Task status (always InputRequired on success)
    pub status: TaskStatus,
    /// Poll interval hint for client
    pub poll_interval: Duration,
}

// ============================================================================
// Approval Engine
// ============================================================================

/// The approval engine coordinates the v0.2 approval workflow.
///
/// Implements: REQ-GOV-002
///
/// This is the main entry point for approval workflows. It:
/// 1. Creates tasks for approval-required requests
/// 2. Posts approval requests to external systems (via adapter)
/// 3. Manages background polling for decisions
/// 4. Executes approved requests on upstream
pub struct ApprovalEngine {
    /// Task store for managing task state
    task_store: Arc<TaskStore>,
    /// Polling scheduler for approval decisions
    scheduler: Arc<PollingScheduler>,
    /// Full pipeline for execution (includes pre/post amber phases, upstream forwarding)
    pipeline: Arc<ApprovalPipeline>,
    /// Engine configuration
    config: ApprovalEngineConfig,
    /// Tracks tasks currently being executed to prevent concurrent execution
    /// This ensures at-most-once semantics for upstream calls
    executing: dashmap::DashSet<TaskId>,
}

impl ApprovalEngine {
    /// Create a new approval engine.
    ///
    /// Implements: REQ-GOV-002/§10 (Engine instantiation)
    ///
    /// Handles: Creating engine state, scheduler, and pipeline
    ///
    /// # Arguments
    ///
    /// * `task_store` - Shared task store
    /// * `adapter` - Approval adapter (Slack, etc.)
    /// * `upstream` - Upstream client for executing approved requests
    /// * `config` - Engine configuration
    /// * `shutdown` - Cancellation token for graceful shutdown
    ///
    /// # Errors
    ///
    /// Returns error if Cedar policy engine fails to initialize.
    pub fn new(
        task_store: Arc<TaskStore>,
        adapter: Arc<dyn ApprovalAdapter>,
        upstream: Arc<dyn UpstreamForwarder>,
        config: ApprovalEngineConfig,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Result<Self, ApprovalEngineError> {
        // Create polling configuration from engine config
        let polling_config = PollingConfig {
            base_interval: Duration::from_secs(5),
            max_interval: Duration::from_secs(30),
            max_concurrent: 100,
            approval_valid_for: Duration::from_secs(60), // Approval validity window
            rate_limit_per_sec: 1.0,
        };

        let scheduler = Arc::new(PollingScheduler::new(
            adapter.clone(),
            task_store.clone(),
            polling_config,
            shutdown,
        ));

        // Create pipeline configuration
        let pipeline_config = PipelineConfig {
            approval_validity: Duration::from_secs(300),
            execution_timeout: config.execution_timeout,
            ..Default::default()
        };

        // Create Cedar policy engine for pipeline
        let cedar_engine = crate::policy::engine::CedarEngine::new().map_err(|e| {
            ApprovalEngineError::Internal {
                details: format!("Failed to create CedarEngine: {e}"),
            }
        })?;

        // Create pipeline with no inspectors for v0.2 (simplified)
        let pipeline = Arc::new(ApprovalPipeline::new(
            vec![], // No inspectors in v0.2
            Arc::new(cedar_engine),
            upstream.clone(),
            pipeline_config,
        ));

        Ok(Self {
            task_store,
            scheduler,
            pipeline,
            config,
            executing: dashmap::DashSet::new(),
        })
    }

    /// Spawn background tasks for the approval engine.
    ///
    /// Implements: REQ-GOV-003/F-002 (Background Polling), REQ-GOV-001/F-008 (TTL Enforcement)
    ///
    /// This must be called after creating the engine to start:
    /// - The polling scheduler loop that checks for approval decisions
    /// - Periodic expiration sweeps for overdue tasks
    ///
    /// The tasks will run until the shutdown token is cancelled.
    pub fn spawn_background_tasks(&self) {
        let scheduler = self.scheduler.clone();
        tokio::spawn(async move {
            scheduler.run().await;
        });
    }

    /// Start an approval workflow.
    ///
    /// Implements: REQ-GOV-002/F-001, F-002 (Task creation and approval posting)
    ///
    /// Handles: Starting approval workflow, creating task, posting to Slack
    ///
    /// Creates a task, posts the approval request, and returns immediately.
    /// Background polling for the decision is started automatically.
    ///
    /// # Arguments
    ///
    /// * `request` - The original tool call request
    /// * `principal` - Who is making the request
    /// * `workflow_timeout` - Optional workflow-specific timeout (overrides engine config)
    ///
    /// # Returns
    ///
    /// Task ID and status for immediate response to agent.
    ///
    /// # Errors
    ///
    /// Returns error if task creation or approval posting fails.
    pub async fn start_approval(
        &self,
        request: ToolCallRequest,
        principal: Principal,
        workflow_timeout: Option<Duration>,
    ) -> Result<ApprovalStartResult, ApprovalEngineError> {
        let correlation_id = Uuid::new_v4().to_string();

        info!(
            tool = %request.name,
            principal = %principal.app_name,
            correlation_id = %correlation_id,
            "Starting approval workflow"
        );

        // F-001.1: Run pre-approval amber phase (simplified for v0.2 - just hash)
        let pre_result = self
            .pipeline
            .pre_approval_amber(&request, &principal)
            .await
            .map_err(|e| ApprovalEngineError::Internal {
                details: format!("Pre-approval phase failed: {e}"),
            })?;

        // F-001.2: Create task with stored request
        // Use workflow-specific timeout if provided, otherwise fall back to engine config
        let timeout = workflow_timeout.unwrap_or(self.config.approval_timeout);
        let task = self
            .task_store
            .create(
                request.clone(),
                pre_result.transformed_request,
                principal.clone(),
                Some(timeout),
                self.config.on_timeout,
            )
            .map_err(|e| ApprovalEngineError::TaskCreation {
                details: e.to_string(),
            })?;

        // Store the request hash for later drift detection
        // (Already stored by create() in pre_approval_transformed)

        // F-001.3: Transition to InputRequired
        self.task_store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .map_err(|e| ApprovalEngineError::Internal {
                details: format!("Failed to transition task: {e}"),
            })?;

        // F-002.1: Post approval request to adapter
        let approval_request = ApprovalRequest {
            task_id: task.id.clone(),
            tool_name: request.name.clone(),
            tool_arguments: request.arguments.clone(),
            principal: principal.clone(),
            expires_at: task.expires_at,
            created_at: task.created_at,
            correlation_id: correlation_id.clone(),
        };

        // F-002.2: Submit to scheduler (posts to Slack and starts polling)
        self.scheduler.submit(approval_request).await.map_err(|e| {
            ApprovalEngineError::PostFailed {
                details: e.to_string(),
            }
        })?;

        info!(
            task_id = %task.id,
            tool = %request.name,
            correlation_id = %correlation_id,
            "Approval workflow started, task created"
        );

        // F-002.3: Return task ID immediately (scheduler polls in background)
        Ok(ApprovalStartResult {
            task_id: task.id,
            status: TaskStatus::InputRequired,
            poll_interval: task.poll_interval,
        })
    }

    /// Execute an approved task and return the result.
    ///
    /// Implements: REQ-GOV-002/F-005, F-006 (Result retrieval and execution)
    ///
    /// Handles: Executing approved requests on upstream, handling rejections/timeouts
    ///
    /// Called when the agent requests `tasks/result`. If the task is approved,
    /// forwards the request to upstream and returns the result. If rejected or
    /// timed out, returns the appropriate error.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The task ID to execute
    ///
    /// # Returns
    ///
    /// The result of executing the approved request, or an error.
    pub async fn execute_on_result(
        &self,
        task_id: &TaskId,
    ) -> Result<ToolCallResult, ThoughtGateError> {
        // Get the task
        let task = self.task_store.get(task_id).map_err(|e| match e {
            TaskError::NotFound { task_id } => ThoughtGateError::TaskNotFound {
                task_id: task_id.to_string(),
            },
            other => ThoughtGateError::ServiceUnavailable {
                reason: other.to_string(),
            },
        })?;

        // Check task status
        match task.status {
            TaskStatus::InputRequired => {
                // Still waiting for approval - return TaskResultNotReady per SEP-1686
                return Err(ThoughtGateError::TaskResultNotReady {
                    task_id: task_id.to_string(),
                });
            }
            TaskStatus::Executing => {
                // Already being executed (approval was recorded, now executing)
                // This is the "approved" state - continue below
            }
            TaskStatus::Rejected => {
                return Err(ThoughtGateError::ApprovalRejected {
                    tool: task.original_request.name.clone(),
                    rejected_by: task.approval.as_ref().map(|a| a.decided_by.clone()),
                    workflow: None, // v0.2: workflow not tracked
                });
            }
            TaskStatus::Expired => {
                // Handle timeout based on task's captured on_timeout (not current config)
                // This ensures "complete with state at decision time" semantics
                match task.on_timeout {
                    TimeoutAction::Deny => {
                        return Err(ThoughtGateError::ApprovalTimeout {
                            tool: task.original_request.name.clone(),
                            timeout_secs: task.ttl.as_secs(),
                            workflow: None, // v0.2: workflow not tracked
                        });
                    }
                    TimeoutAction::Approve => {
                        // Auto-approve: create synthetic approval and execute directly
                        // Note: Expired is terminal, so we can't transition. Instead,
                        // we create a synthetic approval record and execute the pipeline.

                        // Prevent concurrent execution - ensures at-most-once semantics
                        if !self.executing.insert(task_id.clone()) {
                            return Err(ThoughtGateError::ServiceUnavailable {
                                reason: "Task execution already in progress".to_string(),
                            });
                        }

                        warn!(
                            task_id = %task_id,
                            "Auto-approving timed-out task (on_timeout: approve)"
                        );

                        // Create synthetic approval record for pipeline execution
                        let now = chrono::Utc::now();
                        let synthetic_approval = super::ApprovalRecord {
                            decision: super::ApprovalDecision::Approved,
                            decided_by: "system:auto-approve".to_string(),
                            decided_at: now,
                            approval_valid_until: now
                                + chrono::Duration::from_std(self.config.execution_timeout)
                                    .unwrap_or(chrono::Duration::zero()),
                            metadata: Some(serde_json::json!({
                                "auto_approve": true,
                                "reason": "timeout with on_timeout: approve"
                            })),
                        };

                        // Clone task with Executing status so pipeline validation passes.
                        // The pipeline's validate_approval() requires TaskStatus::Executing,
                        // but auto-approved tasks are in Expired (terminal) state.
                        let mut auto_approved_task = task.clone();
                        auto_approved_task.status = TaskStatus::Executing;

                        // Execute the pipeline with the synthetic approval
                        let pipeline_result = self
                            .pipeline
                            .execute_approved(&auto_approved_task, &synthetic_approval)
                            .await;

                        // Remove from executing set now that pipeline is complete
                        self.executing.remove(task_id);

                        return match pipeline_result {
                            PipelineResult::Success { result } => {
                                // Note: We can't complete() the task since it's in Expired state
                                // Just return the result
                                info!(
                                    task_id = %task_id,
                                    "Auto-approved task executed successfully"
                                );
                                Ok(result)
                            }
                            PipelineResult::Failure { reason, .. } => {
                                Err(ThoughtGateError::ServiceUnavailable {
                                    reason: format!(
                                        "Auto-approved task execution failed: {reason}"
                                    ),
                                })
                            }
                        };
                    }
                }
            }
            TaskStatus::Failed => {
                return Err(ThoughtGateError::ServiceUnavailable {
                    reason: task
                        .failure
                        .as_ref()
                        .map(|f| f.reason.clone())
                        .unwrap_or_else(|| "Task failed".to_string()),
                });
            }
            TaskStatus::Cancelled => {
                return Err(ThoughtGateError::TaskCancelled {
                    task_id: task_id.to_string(),
                });
            }
            TaskStatus::Completed => {
                // Already completed - return cached result
                if let Some(result) = task.result {
                    return Ok(result);
                }
                return Err(ThoughtGateError::ServiceUnavailable {
                    reason: "Task completed but no result available".to_string(),
                });
            }
            TaskStatus::Working => {
                // Still in pre-approval phase - return TaskResultNotReady per SEP-1686
                return Err(ThoughtGateError::TaskResultNotReady {
                    task_id: task_id.to_string(),
                });
            }
        }

        // At this point, task is in Executing state (approval was recorded)
        // The task was already transitioned to Executing by record_approval()

        // Prevent concurrent execution - ensures at-most-once semantics
        // If another call is already executing this task, return "in progress" error
        if !self.executing.insert(task_id.clone()) {
            return Err(ThoughtGateError::ServiceUnavailable {
                reason: "Task execution already in progress".to_string(),
            });
        }

        // Execute the full pipeline (with approval record)
        // Use a guard to ensure we clean up the executing set on all exit paths
        let approval = task.approval.as_ref().ok_or_else(|| {
            self.executing.remove(task_id);
            ThoughtGateError::ServiceUnavailable {
                reason: "Task approved but no approval record".to_string(),
            }
        })?;

        let pipeline_result = self.pipeline.execute_approved(&task, approval).await;

        // Remove from executing set now that pipeline is complete
        self.executing.remove(task_id);

        // Handle pipeline result
        match pipeline_result {
            PipelineResult::Success { result } => {
                // Store result and mark complete
                if let Err(e) = self.task_store.complete(task_id, result.clone()) {
                    error!(task_id = %task_id, error = %e, "Failed to complete task");
                }
                Ok(result)
            }
            PipelineResult::Failure {
                stage,
                reason,
                retriable,
            } => {
                // Record failure (clone stage since we need it for error mapping)
                let failure = FailureInfo {
                    stage: stage.clone(),
                    reason: reason.clone(),
                    retriable,
                };
                if let Err(e) = self.task_store.fail(task_id, failure) {
                    error!(task_id = %task_id, error = %e, "Failed to record task failure");
                }

                // Map failure to appropriate error
                let tool_name = task.original_request.name.clone();
                match stage {
                    FailureStage::ApprovalTimeout => Err(ThoughtGateError::ApprovalTimeout {
                        tool: tool_name,
                        timeout_secs: self.config.approval_timeout.as_secs(),
                        workflow: None, // v0.2: workflow not tracked
                    }),
                    FailureStage::ApprovalRejected => Err(ThoughtGateError::ApprovalRejected {
                        tool: tool_name,
                        rejected_by: task.approval.as_ref().map(|a| a.decided_by.clone()),
                        workflow: None, // v0.2: workflow not tracked
                    }),
                    FailureStage::PolicyDrift => Err(ThoughtGateError::PolicyDenied {
                        tool: tool_name,
                        policy_id: None, // v0.2: policy_id not tracked
                        reason: Some(reason),
                    }),
                    FailureStage::TransformDrift => Err(ThoughtGateError::ServiceUnavailable {
                        reason: format!("Transform drift: {reason}"),
                    }),
                    FailureStage::UpstreamError => {
                        if reason.contains("timeout") || reason.contains("timed out") {
                            Err(ThoughtGateError::UpstreamTimeout {
                                url: "unknown".to_string(), // URL not available in failure info
                                timeout_secs: self.config.execution_timeout.as_secs(),
                            })
                        } else {
                            Err(ThoughtGateError::UpstreamError {
                                code: -32002,
                                message: reason,
                            })
                        }
                    }
                    _ => Err(ThoughtGateError::ServiceUnavailable { reason }),
                }
            }
        }
    }

    /// Returns the polling scheduler.
    ///
    /// Used to run the background polling loop.
    ///
    /// Implements: REQ-GOV-003/F-002 (Slack Polling)
    #[must_use]
    pub fn scheduler(&self) -> &PollingScheduler {
        &self.scheduler
    }

    /// Returns the task store.
    ///
    /// Implements: REQ-GOV-001/F-001 (Task Storage)
    #[must_use]
    pub fn task_store(&self) -> &TaskStore {
        &self.task_store
    }

    /// Returns the engine configuration.
    ///
    /// Implements: REQ-GOV-001/F-003 (Configuration Access)
    #[must_use]
    pub fn config(&self) -> &ApprovalEngineConfig {
        &self.config
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::ApprovalDecision;
    use crate::governance::approval::{AdapterError, ApprovalReference, PollResult};
    use crate::governance::task::JsonRpcId;
    use crate::transport::JsonRpcResponse;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    // ========================================================================
    // Unit Tests
    // ========================================================================

    #[test]
    fn test_config_defaults() {
        let config = ApprovalEngineConfig::default();

        assert_eq!(config.approval_timeout, Duration::from_secs(600));
        assert_eq!(config.on_timeout, TimeoutAction::Deny);
        assert_eq!(config.execution_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_timeout_action_default() {
        assert_eq!(TimeoutAction::default(), TimeoutAction::Deny);
    }

    #[test]
    fn test_approval_engine_error_display() {
        let err = ApprovalEngineError::TaskNotFound {
            task_id: TaskId::new(),
        };
        assert!(err.to_string().contains("Task not found"));

        let err = ApprovalEngineError::PostFailed {
            details: "connection refused".to_string(),
        };
        assert!(err.to_string().contains("connection refused"));
    }

    // ========================================================================
    // Integration Test Helpers
    // ========================================================================

    /// Mock approval adapter for testing
    struct MockApprovalAdapter {
        post_count: AtomicU32,
        poll_count: AtomicU32,
        poll_result: Mutex<Option<PollResult>>,
    }

    impl MockApprovalAdapter {
        fn new() -> Self {
            Self {
                post_count: AtomicU32::new(0),
                poll_count: AtomicU32::new(0),
                poll_result: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl super::super::approval::ApprovalAdapter for MockApprovalAdapter {
        async fn post_approval_request(
            &self,
            request: &super::super::approval::ApprovalRequest,
        ) -> Result<ApprovalReference, AdapterError> {
            self.post_count.fetch_add(1, Ordering::SeqCst);

            Ok(ApprovalReference {
                task_id: request.task_id.clone(),
                external_id: "mock-ts".to_string(),
                channel: "mock-channel".to_string(),
                posted_at: chrono::Utc::now(),
                next_poll_at: std::time::Instant::now() + Duration::from_millis(10),
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

    /// Mock upstream forwarder for testing
    struct MockUpstream {
        response: Mutex<Option<serde_json::Value>>,
        forward_count: AtomicU32,
    }

    impl MockUpstream {
        fn new() -> Self {
            Self {
                response: Mutex::new(Some(serde_json::json!({"success": true}))),
                forward_count: AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl crate::transport::UpstreamForwarder for MockUpstream {
        async fn forward(
            &self,
            _request: &crate::transport::McpRequest,
        ) -> Result<JsonRpcResponse, ThoughtGateError> {
            self.forward_count.fetch_add(1, Ordering::SeqCst);
            let result = self.response.lock().await.clone();
            Ok(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: Some(crate::transport::JsonRpcId::Number(1)),
                result,
                error: None,
            })
        }

        async fn forward_batch(
            &self,
            requests: &[crate::transport::McpRequest],
        ) -> Result<Vec<JsonRpcResponse>, ThoughtGateError> {
            let mut responses = Vec::with_capacity(requests.len());
            for req in requests {
                responses.push(self.forward(req).await?);
            }
            Ok(responses)
        }
    }

    fn test_request() -> ToolCallRequest {
        ToolCallRequest {
            method: "tools/call".to_string(),
            name: "delete_user".to_string(),
            arguments: serde_json::json!({"user_id": "123"}),
            mcp_request_id: JsonRpcId::Number(1),
        }
    }

    fn test_principal() -> Principal {
        Principal::new("test-app")
    }

    // ========================================================================
    // Integration Tests
    // ========================================================================

    /// Tests that start_approval creates task and returns immediately.
    ///
    /// Verifies: EC-PIP-001 (Task created and ID returned immediately)
    #[tokio::test]
    async fn test_start_approval_creates_task() {
        let task_store = Arc::new(TaskStore::with_defaults());
        let adapter = Arc::new(MockApprovalAdapter::new());
        let upstream = Arc::new(MockUpstream::new());
        let config = ApprovalEngineConfig::default();
        let shutdown = CancellationToken::new();

        let engine = ApprovalEngine::new(
            task_store.clone(),
            adapter.clone(),
            upstream,
            config,
            shutdown,
        )
        .expect("Failed to create engine");

        let result = engine
            .start_approval(test_request(), test_principal(), None)
            .await;

        assert!(result.is_ok(), "start_approval should succeed");
        let start_result = result.unwrap();

        // Task should be created
        let task = task_store.get(&start_result.task_id);
        assert!(task.is_ok(), "Task should exist");

        // Task should be in InputRequired state
        assert_eq!(start_result.status, TaskStatus::InputRequired);

        // Adapter should have been called
        assert_eq!(adapter.post_count.load(Ordering::SeqCst), 1);
    }

    /// Tests execute_on_result returns error when task not found.
    ///
    /// Verifies: EC-PIP-002 (Task not found)
    #[tokio::test]
    async fn test_execute_on_result_task_not_found() {
        let task_store = Arc::new(TaskStore::with_defaults());
        let adapter = Arc::new(MockApprovalAdapter::new());
        let upstream = Arc::new(MockUpstream::new());
        let config = ApprovalEngineConfig::default();
        let shutdown = CancellationToken::new();

        let engine = ApprovalEngine::new(task_store, adapter, upstream, config, shutdown)
            .expect("Failed to create engine");

        let result = engine.execute_on_result(&TaskId::new()).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ThoughtGateError::TaskNotFound { .. } => {}
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }

    /// Tests execute_on_result returns error when task is still pending.
    ///
    /// Verifies: EC-PIP-003 (Result not ready)
    #[tokio::test]
    async fn test_execute_on_result_still_pending() {
        let task_store = Arc::new(TaskStore::with_defaults());
        let adapter = Arc::new(MockApprovalAdapter::new());
        let upstream = Arc::new(MockUpstream::new());
        let config = ApprovalEngineConfig::default();
        let shutdown = CancellationToken::new();

        let engine = ApprovalEngine::new(task_store.clone(), adapter, upstream, config, shutdown)
            .expect("Failed to create engine");

        // Start approval
        let start_result = engine
            .start_approval(test_request(), test_principal(), None)
            .await
            .unwrap();

        // Try to get result immediately (still waiting for approval)
        let result = engine.execute_on_result(&start_result.task_id).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ThoughtGateError::TaskResultNotReady { task_id } => {
                // Per SEP-1686: pending tasks return TaskResultNotReady
                assert_eq!(task_id, start_result.task_id.to_string());
            }
            other => panic!("Expected TaskResultNotReady, got {:?}", other),
        }
    }

    /// Tests execute_on_result returns rejection error when task was rejected.
    ///
    /// Verifies: EC-PIP-004 (Approval rejected → -32007)
    #[tokio::test]
    async fn test_execute_on_result_rejected() {
        let task_store = Arc::new(TaskStore::with_defaults());
        let adapter = Arc::new(MockApprovalAdapter::new());
        let upstream = Arc::new(MockUpstream::new());
        let config = ApprovalEngineConfig::default();
        let shutdown = CancellationToken::new();

        let engine = ApprovalEngine::new(task_store.clone(), adapter, upstream, config, shutdown)
            .expect("Failed to create engine");

        // Start approval
        let start_result = engine
            .start_approval(test_request(), test_principal(), None)
            .await
            .unwrap();

        // Record rejection
        task_store
            .record_approval(
                &start_result.task_id,
                ApprovalDecision::Rejected {
                    reason: Some("Not authorized".to_string()),
                },
                "test-reviewer".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();

        // Try to get result
        let result = engine.execute_on_result(&start_result.task_id).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ThoughtGateError::ApprovalRejected {
                tool, rejected_by, ..
            } => {
                assert_eq!(tool, "delete_user");
                assert_eq!(rejected_by, Some("test-reviewer".to_string()));
            }
            other => panic!("Expected ApprovalRejected, got {:?}", other),
        }
    }

    /// Tests execute_on_result returns timeout error when task expired with on_timeout: deny.
    ///
    /// Verifies: EC-PIP-005 (Timeout with deny → -32008)
    #[tokio::test]
    async fn test_execute_on_result_timeout_deny() {
        let task_store = Arc::new(TaskStore::with_defaults());
        let adapter = Arc::new(MockApprovalAdapter::new());
        let upstream = Arc::new(MockUpstream::new());
        let config = ApprovalEngineConfig {
            on_timeout: TimeoutAction::Deny,
            ..Default::default()
        };
        let shutdown = CancellationToken::new();

        let engine = ApprovalEngine::new(task_store.clone(), adapter, upstream, config, shutdown)
            .expect("Failed to create engine");

        // Start approval
        let start_result = engine
            .start_approval(test_request(), test_principal(), None)
            .await
            .unwrap();

        // Manually expire the task
        task_store
            .transition(&start_result.task_id, TaskStatus::Expired, None)
            .unwrap();

        // Try to get result
        let result = engine.execute_on_result(&start_result.task_id).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ThoughtGateError::ApprovalTimeout { tool, .. } => {
                assert_eq!(tool, "delete_user");
            }
            other => panic!("Expected ApprovalTimeout, got {:?}", other),
        }
    }

    /// Tests execute_on_result returns error when task was cancelled.
    ///
    /// Verifies: EC-PIP-006 (Cancelled task)
    #[tokio::test]
    async fn test_execute_on_result_cancelled() {
        let task_store = Arc::new(TaskStore::with_defaults());
        let adapter = Arc::new(MockApprovalAdapter::new());
        let upstream = Arc::new(MockUpstream::new());
        let config = ApprovalEngineConfig::default();
        let shutdown = CancellationToken::new();

        let engine = ApprovalEngine::new(task_store.clone(), adapter, upstream, config, shutdown)
            .expect("Failed to create engine");

        // Start approval
        let start_result = engine
            .start_approval(test_request(), test_principal(), None)
            .await
            .unwrap();

        // Cancel the task
        task_store.cancel(&start_result.task_id).unwrap();

        // Try to get result
        let result = engine.execute_on_result(&start_result.task_id).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ThoughtGateError::TaskCancelled { task_id } => {
                assert_eq!(task_id, start_result.task_id.to_string());
            }
            other => panic!("Expected TaskCancelled, got {:?}", other),
        }
    }

    /// Tests that completed task returns cached result.
    ///
    /// Verifies: EC-PIP-007 (Completed task returns cached result)
    #[tokio::test]
    async fn test_execute_on_result_returns_cached() {
        let task_store = Arc::new(TaskStore::with_defaults());
        let adapter = Arc::new(MockApprovalAdapter::new());
        let upstream = Arc::new(MockUpstream::new());
        let config = ApprovalEngineConfig::default();
        let shutdown = CancellationToken::new();

        let engine = ApprovalEngine::new(task_store.clone(), adapter, upstream, config, shutdown)
            .expect("Failed to create engine");

        // Start approval
        let start_result = engine
            .start_approval(test_request(), test_principal(), None)
            .await
            .unwrap();

        // Approve the task
        task_store
            .record_approval(
                &start_result.task_id,
                ApprovalDecision::Approved,
                "test-reviewer".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();

        // Complete the task with a result
        task_store
            .complete(
                &start_result.task_id,
                super::super::ToolCallResult {
                    content: serde_json::json!({"cached": "result"}),
                    is_error: false,
                },
            )
            .unwrap();

        // Get result - should return cached result
        let result = engine.execute_on_result(&start_result.task_id).await;

        assert!(result.is_ok());
        let tool_result = result.unwrap();
        assert!(!tool_result.is_error);
        assert_eq!(tool_result.content["cached"], "result");
    }

    /// Tests poll interval is returned in start result.
    ///
    /// Verifies: EC-PIP-008 (Poll interval provided)
    #[tokio::test]
    async fn test_start_approval_returns_poll_interval() {
        let task_store = Arc::new(TaskStore::with_defaults());
        let adapter = Arc::new(MockApprovalAdapter::new());
        let upstream = Arc::new(MockUpstream::new());
        let config = ApprovalEngineConfig::default();
        let shutdown = CancellationToken::new();

        let engine = ApprovalEngine::new(task_store, adapter, upstream, config, shutdown)
            .expect("Failed to create engine");

        let result = engine
            .start_approval(test_request(), test_principal(), None)
            .await
            .unwrap();

        // Poll interval should be reasonable (between 1s and 30s)
        assert!(result.poll_interval >= Duration::from_secs(1));
        assert!(result.poll_interval <= Duration::from_secs(30));
    }

    /// Tests environment variable configuration loading.
    ///
    /// Verifies: REQ-GOV-002/§5.1 (Environment configuration)
    #[test]
    fn test_config_from_env_defaults() {
        // Without env vars, should use defaults
        let config = ApprovalEngineConfig::from_env();
        assert_eq!(config.approval_timeout, Duration::from_secs(600));
        assert_eq!(config.on_timeout, TimeoutAction::Deny);
    }

    /// Tests error conversion from TaskError.
    #[test]
    fn test_task_error_conversion() {
        let task_id = TaskId::new();
        let err = TaskError::NotFound {
            task_id: task_id.clone(),
        };
        let engine_err: ApprovalEngineError = err.into();

        match engine_err {
            ApprovalEngineError::TaskNotFound { task_id: id } => {
                assert_eq!(id, task_id);
            }
            _ => panic!("Expected TaskNotFound"),
        }
    }

    /// Tests that auto-approve on timeout executes the pipeline successfully.
    ///
    /// When on_timeout is Approve, expired tasks should be auto-approved
    /// with a synthetic approval record and executed through the pipeline.
    ///
    /// Verifies: EC-PIP-009 (Timeout with approve → auto-execute)
    #[tokio::test]
    async fn test_execute_on_result_timeout_approve() {
        let task_store = Arc::new(TaskStore::with_defaults());
        let adapter = Arc::new(MockApprovalAdapter::new());
        let upstream = Arc::new(MockUpstream::new());
        let config = ApprovalEngineConfig {
            on_timeout: TimeoutAction::Approve,
            execution_timeout: Duration::from_secs(30),
            ..Default::default()
        };
        let shutdown = CancellationToken::new();

        let engine = ApprovalEngine::new(
            task_store.clone(),
            adapter,
            upstream.clone(),
            config,
            shutdown,
        )
        .expect("Failed to create engine");

        // Start approval
        let start_result = engine
            .start_approval(test_request(), test_principal(), None)
            .await
            .unwrap();

        // Manually expire the task (simulating timeout)
        task_store
            .transition(&start_result.task_id, TaskStatus::Expired, None)
            .unwrap();

        // Execute should auto-approve and forward to upstream
        let result = engine.execute_on_result(&start_result.task_id).await;

        assert!(
            result.is_ok(),
            "Auto-approve on timeout should succeed, got: {:?}",
            result.err()
        );

        // Upstream should have received the forwarded request
        assert_eq!(
            upstream.forward_count.load(Ordering::SeqCst),
            1,
            "Upstream should have been called exactly once"
        );
    }
}
