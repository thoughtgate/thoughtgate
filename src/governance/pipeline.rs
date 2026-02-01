//! Approval execution pipeline for ThoughtGate.
//!
//! Implements: REQ-GOV-002 (Approval Execution Pipeline)
//!
//! This module provides the multi-phase execution pipeline for approval-required
//! requests. When a tool call requires human approval, it flows through:
//!
//! 1. **Pre-Approval Amber** - Transform/validate before showing to human
//! 2. **Approval Wait** - Human reviews and decides (REQ-GOV-003)
//! 3. **Post-Approval Amber** - Re-validate after approval (detect drift)
//! 4. **Execution** - Forward to upstream MCP server
//!
//! ## Key Design Decision
//!
//! Human approves the **transformed** request (not original). This ensures
//! the human sees exactly what will be executed.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::inspector::{Decision, InspectionContext, Inspector};
use crate::policy::engine::CedarEngine;
#[allow(deprecated)] // v0.1 types for backward compatibility
use crate::policy::{
    ApprovalGrant, PolicyAction, PolicyContext, PolicyRequest, Principal as PolicyPrincipal,
    Resource,
};
use crate::transport::{JsonRpcId, McpRequest, UpstreamForwarder};

use super::{
    ApprovalRecord, FailureStage, Principal, Task, TaskStatus, ToolCallRequest, ToolCallResult,
    hash_request,
};

/// Parse an environment variable with a warning on invalid values.
fn parse_env_warn<T: std::str::FromStr + std::fmt::Display>(name: &str, default: T) -> T {
    match std::env::var(name) {
        Ok(val) => match val.parse::<T>() {
            Ok(parsed) => parsed,
            Err(_) => {
                warn!(
                    env_var = name,
                    value = %val,
                    default = %default,
                    "Invalid value for environment variable, using default"
                );
                default
            }
        },
        Err(_) => default,
    }
}

// ============================================================================
// Pre-Approval Result
// ============================================================================

/// Result of the Pre-Approval Amber phase.
///
/// Implements: REQ-GOV-002/F-001
///
/// Contains the transformed request (after all inspectors have run) and
/// a hash for drift detection during post-approval.
#[derive(Debug, Clone)]
pub struct PreHitlResult {
    /// The transformed request (after all inspectors)
    pub transformed_request: ToolCallRequest,
    /// SHA256 hash of the transformed request for drift detection
    pub request_hash: String,
}

// ============================================================================
// Pipeline Result
// ============================================================================

/// Result of executing an approved task through the pipeline.
///
/// Implements: REQ-GOV-002/§6.2
#[derive(Debug, Clone)]
pub enum PipelineResult {
    /// Execution succeeded
    Success {
        /// Tool call result from upstream
        result: ToolCallResult,
    },
    /// Execution failed at some stage
    Failure {
        /// Which stage failed
        stage: FailureStage,
        /// Human-readable reason
        reason: String,
        /// Whether the operation can be retried
        retriable: bool,
    },
}

impl PipelineResult {
    /// Returns true if this is a success result.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    /// Returns true if this is a failure result.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failure { .. })
    }
}

// ============================================================================
// Pipeline Error
// ============================================================================

/// Pipeline-specific errors (for pre-approval phase).
///
/// Implements: REQ-GOV-002/§6.3
#[derive(Debug, Error, Clone)]
pub enum PipelineError {
    /// Inspector rejected the request
    #[error("Inspector '{inspector}' rejected: {reason}")]
    InspectionRejected {
        /// Name of the rejecting inspector
        inspector: String,
        /// Rejection reason
        reason: String,
    },

    /// Internal pipeline error
    #[error("Internal pipeline error: {details}")]
    InternalError {
        /// Error details
        details: String,
    },

    /// Inspector timed out
    #[error("Inspector '{inspector}' timed out after {timeout_secs}s")]
    InspectorTimeout {
        /// Name of the timed-out inspector
        inspector: String,
        /// Timeout duration in seconds
        timeout_secs: u64,
    },

    /// Request serialization/deserialization error
    #[error("Serialization error: {details}")]
    SerializationError {
        /// Error details
        details: String,
    },
}

// ============================================================================
// Pipeline Configuration
// ============================================================================

/// Configuration for the execution pipeline.
///
/// Implements: REQ-GOV-002/§5.1
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// How long an approval is valid after being granted
    pub approval_validity: Duration,
    /// Timeout for upstream execution
    pub execution_timeout: Duration,
    /// Timeout for individual inspector execution
    pub inspector_timeout: Duration,
    /// Transform drift handling mode
    pub transform_drift_mode: TransformDriftMode,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            approval_validity: Duration::from_secs(300),
            execution_timeout: Duration::from_secs(30),
            inspector_timeout: Duration::from_secs(30),
            transform_drift_mode: TransformDriftMode::Strict,
        }
    }
}

impl PipelineConfig {
    /// Load configuration from environment variables.
    ///
    /// Implements: REQ-GOV-002/§5.1
    ///
    /// # Environment Variables
    ///
    /// - `THOUGHTGATE_APPROVAL_VALIDITY_SECS` (default: 300)
    /// - `THOUGHTGATE_EXECUTION_TIMEOUT_SECS` (default: 30)
    /// - `THOUGHTGATE_INSPECTOR_TIMEOUT_SECS` (default: 30)
    /// - `THOUGHTGATE_TRANSFORM_DRIFT_MODE` (default: strict)
    #[must_use]
    pub fn from_env() -> Self {
        let approval_validity =
            Duration::from_secs(parse_env_warn("THOUGHTGATE_APPROVAL_VALIDITY_SECS", 300u64));

        let execution_timeout =
            Duration::from_secs(parse_env_warn("THOUGHTGATE_EXECUTION_TIMEOUT_SECS", 30u64));

        let inspector_timeout =
            Duration::from_secs(parse_env_warn("THOUGHTGATE_INSPECTOR_TIMEOUT_SECS", 30u64));

        let transform_drift_mode = std::env::var("THOUGHTGATE_TRANSFORM_DRIFT_MODE")
            .ok()
            .map(|s| match s.to_lowercase().as_str() {
                "permissive" => TransformDriftMode::Permissive,
                _ => TransformDriftMode::Strict,
            })
            .unwrap_or(TransformDriftMode::Strict);

        Self {
            approval_validity,
            execution_timeout,
            inspector_timeout,
            transform_drift_mode,
        }
    }
}

/// How to handle transform drift between pre/post approval inspection.
///
/// Implements: REQ-GOV-002/§5.1
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransformDriftMode {
    /// Fail if post-approval transform differs from pre-approval
    #[default]
    Strict,
    /// Log warning and continue with new transform
    Permissive,
}

// ============================================================================
// Execution Pipeline Trait
// ============================================================================

/// The execution pipeline trait.
///
/// Implements: REQ-GOV-002/§6.3
#[async_trait]
pub trait ExecutionPipeline: Send + Sync {
    /// Run Pre-Approval Amber phase before task creation.
    ///
    /// Implements: REQ-GOV-002/F-001
    ///
    /// This runs inspectors on the original request to:
    /// 1. Validate the request is well-formed
    /// 2. Transform the request if needed (e.g., redact PII)
    /// 3. Compute hash for drift detection
    ///
    /// # Arguments
    ///
    /// * `request` - Original tool call request
    /// * `principal` - Who is making the request
    ///
    /// # Returns
    ///
    /// * `Ok(PreHitlResult)` - Transformed request and hash
    /// * `Err(PipelineError::InspectionRejected)` - Inspector rejected
    async fn pre_approval_amber(
        &self,
        request: &ToolCallRequest,
        principal: &Principal,
    ) -> Result<PreHitlResult, PipelineError>;

    /// Execute an approved task through the full pipeline.
    ///
    /// Implements: REQ-GOV-002/F-002
    ///
    /// Phases:
    /// 1. Validate approval (not expired, hash matches)
    /// 2. Re-evaluate policy with approval context
    /// 3. Run Post-Approval Amber (detect drift)
    /// 4. Forward to upstream
    ///
    /// # Arguments
    ///
    /// * `task` - The approved task
    /// * `approval` - The approval record
    ///
    /// # Returns
    ///
    /// `PipelineResult::Success` or `PipelineResult::Failure` with stage
    async fn execute_approved(&self, task: &Task, approval: &ApprovalRecord) -> PipelineResult;
}

// ============================================================================
// Approval Pipeline Implementation
// ============================================================================

/// The approval execution pipeline implementation.
///
/// Implements: REQ-GOV-002
pub struct ApprovalPipeline {
    /// Registered inspectors (executed in order)
    inspectors: Arc<Vec<Arc<dyn Inspector>>>,
    /// Policy engine for re-evaluation
    policy_engine: Arc<CedarEngine>,
    /// Upstream client for forwarding
    upstream_client: Arc<dyn UpstreamForwarder>,
    /// Pipeline configuration
    config: PipelineConfig,
}

impl ApprovalPipeline {
    /// Create a new pipeline with dependencies.
    ///
    /// Implements: REQ-GOV-002/§10
    pub fn new(
        inspectors: Vec<Arc<dyn Inspector>>,
        policy_engine: Arc<CedarEngine>,
        upstream_client: Arc<dyn UpstreamForwarder>,
        config: PipelineConfig,
    ) -> Self {
        Self {
            inspectors: Arc::new(inspectors),
            policy_engine,
            upstream_client,
            config,
        }
    }

    /// Create pipeline with configuration from environment.
    ///
    /// Implements: REQ-GOV-002/§5.1
    pub fn from_env(
        inspectors: Vec<Arc<dyn Inspector>>,
        policy_engine: Arc<CedarEngine>,
        upstream_client: Arc<dyn UpstreamForwarder>,
    ) -> Self {
        Self::new(
            inspectors,
            policy_engine,
            upstream_client,
            PipelineConfig::from_env(),
        )
    }

    /// Returns the pipeline configuration.
    #[must_use]
    pub fn config(&self) -> &PipelineConfig {
        &self.config
    }

    /// Run inspector chain on a ToolCallRequest.
    ///
    /// Implements: REQ-GOV-002/F-001.1, F-001.2
    async fn run_inspector_chain(
        &self,
        request: &ToolCallRequest,
    ) -> Result<ToolCallRequest, PipelineError> {
        if self.inspectors.is_empty() {
            return Ok(request.clone());
        }

        let mut current = request.clone();

        for inspector in self.inspectors.iter() {
            let inspector_name = inspector.name();
            debug!(inspector = inspector_name, "Running inspector");

            // Serialize current request to bytes (canonical form: name + arguments)
            let bytes = serde_json::to_vec(&serde_json::json!({
                "name": current.name,
                "arguments": current.arguments,
            }))
            .map_err(|e| PipelineError::SerializationError {
                details: e.to_string(),
            })?;

            // Build synthetic request parts for InspectionContext
            // Use the actual method from the request for proper context
            let uri = format!("/{}", current.method);
            let fake_req = http::Request::builder()
                .method("POST")
                .uri(&uri)
                .header("content-type", "application/json")
                .body(())
                .map_err(|e| PipelineError::InternalError {
                    details: e.to_string(),
                })?;
            let (parts, _) = fake_req.into_parts();
            let ctx = InspectionContext::Request(&parts);

            // Run inspector with timeout to prevent hung inspectors from
            // blocking the pipeline indefinitely.
            let decision = tokio::time::timeout(
                self.config.inspector_timeout,
                inspector.inspect(&bytes, ctx),
            )
            .await
            .map_err(|_| PipelineError::InspectorTimeout {
                inspector: inspector_name.to_string(),
                timeout_secs: self.config.inspector_timeout.as_secs(),
            })?
            .map_err(|e| PipelineError::InternalError {
                details: format!("Inspector '{}' error: {}", inspector_name, e),
            })?;

            match decision {
                Decision::Approve => {
                    // Continue with current request
                }
                Decision::Modify(new_bytes) => {
                    // Parse modified bytes back to request
                    #[derive(serde::Deserialize)]
                    struct PartialRequest {
                        name: String,
                        arguments: serde_json::Value,
                    }

                    let parsed: PartialRequest =
                        serde_json::from_slice(&new_bytes).map_err(|e| {
                            PipelineError::SerializationError {
                                details: format!(
                                    "Inspector '{}' returned invalid JSON: {}",
                                    inspector_name, e
                                ),
                            }
                        })?;

                    current = ToolCallRequest {
                        method: current.method.clone(),
                        name: parsed.name,
                        arguments: parsed.arguments,
                        mcp_request_id: current.mcp_request_id,
                    };
                }
                Decision::Reject(status) => {
                    return Err(PipelineError::InspectionRejected {
                        inspector: inspector_name.to_string(),
                        reason: format!("Rejected with status {}", status),
                    });
                }
            }
        }

        Ok(current)
    }

    /// Validate approval is still valid.
    ///
    /// Implements: REQ-GOV-002/F-003
    fn validate_approval(
        &self,
        task: &Task,
        approval: &ApprovalRecord,
    ) -> Result<(), PipelineResult> {
        // F-003.1: Check approval validity window
        let now = chrono::Utc::now();
        if now > approval.approval_valid_until {
            warn!(
                task_id = %task.id,
                expired_at = %approval.approval_valid_until,
                "Approval expired"
            );
            return Err(PipelineResult::Failure {
                stage: FailureStage::ApprovalTimeout,
                reason: "Approval has expired".to_string(),
                retriable: false,
            });
        }

        // F-003.2: Verify task is in correct state (should be Executing)
        if task.status != TaskStatus::Executing {
            warn!(
                task_id = %task.id,
                status = %task.status,
                "Task in unexpected state for execution"
            );
            return Err(PipelineResult::Failure {
                stage: FailureStage::InvalidTaskState,
                reason: format!("Task in invalid state: {}", task.status),
                retriable: false,
            });
        }

        Ok(())
    }

    /// Re-evaluate policy with approval context.
    ///
    /// Implements: REQ-GOV-002/F-004
    #[allow(deprecated)] // Using v0.1 PolicyAction API
    fn reevaluate_policy(
        &self,
        task: &Task,
        approval: &ApprovalRecord,
    ) -> Result<(), PipelineResult> {
        // Build approval grant for policy context
        let approval_grant = ApprovalGrant {
            task_id: task.id.to_string(),
            approved_by: approval.decided_by.clone(),
            approved_at: approval.decided_at.timestamp(),
        };

        // Build policy request with approval context
        let policy_request = build_policy_request(
            &task.pre_approval_transformed,
            &task.principal,
            Some(approval_grant),
        );

        // Evaluate policy
        let action = self.policy_engine.evaluate(&policy_request);

        // F-004.2: Both Forward and Approve are valid post-approval outcomes.
        //
        // Forward means "this tool no longer requires approval" — a policy
        // relaxation. This is safe because:
        // 1. The human already approved the request
        // 2. Policy relaxation (Approve→Forward) means the tool could now
        //    proceed without approval, which is strictly less restrictive
        // 3. Only Reject constitutes policy drift that should block execution,
        //    since it means the tool is now forbidden entirely
        match action {
            PolicyAction::Forward | PolicyAction::Approve { .. } => {
                debug!(
                    task_id = %task.id,
                    action = ?action,
                    "Policy permits execution"
                );
                Ok(())
            }
            PolicyAction::Reject { reason } => {
                // F-004.3: Policy drift
                warn!(
                    task_id = %task.id,
                    reason = %reason,
                    "Policy drift detected - no longer permitted"
                );
                Err(PipelineResult::Failure {
                    stage: FailureStage::PolicyDrift,
                    reason: "Policy changed - request no longer permitted".to_string(),
                    retriable: false,
                })
            }
        }
    }

    /// Run post-approval amber phase.
    ///
    /// Implements: REQ-GOV-002/F-005
    ///
    /// Drift detection works by re-running inspectors on the original request
    /// and comparing the output hash to the stored hash. This verifies that
    /// inspector behavior hasn't changed between approval and execution.
    ///
    /// Flow: Inspect(original_request) → new_transformed → hash(new_transformed)
    /// Compare: hash(new_transformed) == task.request_hash (which is hash(pre_approval_transformed))
    async fn post_approval_amber(&self, task: &Task) -> Result<ToolCallRequest, PipelineResult> {
        // F-005.1: Run same inspector chain on ORIGINAL request
        // This produces a fresh transform that should match pre_approval_transformed
        let transformed = match self.run_inspector_chain(&task.original_request).await {
            Ok(req) => req,
            Err(PipelineError::InspectionRejected { inspector, reason }) => {
                // F-005.5: Rejection in post-approval fails the task
                warn!(
                    task_id = %task.id,
                    inspector = %inspector,
                    reason = %reason,
                    "Post-approval inspection rejected"
                );
                return Err(PipelineResult::Failure {
                    stage: FailureStage::PostHitlInspection,
                    reason: format!("Inspector '{}' rejected: {}", inspector, reason),
                    retriable: false,
                });
            }
            Err(e) => {
                return Err(PipelineResult::Failure {
                    stage: FailureStage::PostHitlInspection,
                    reason: e.to_string(),
                    retriable: true,
                });
            }
        };

        // F-005.2: Compare output hash to stored hash
        let new_hash = hash_request(&transformed);
        if new_hash != task.request_hash {
            // Transform drift detected
            match self.config.transform_drift_mode {
                TransformDriftMode::Strict => {
                    // F-005.3: Strict mode fails on drift
                    warn!(
                        task_id = %task.id,
                        stored_hash = %task.request_hash,
                        new_hash = %new_hash,
                        "Transform drift detected (strict mode)"
                    );
                    return Err(PipelineResult::Failure {
                        stage: FailureStage::TransformDrift,
                        reason: "Request transformed differently than at approval time".to_string(),
                        retriable: false,
                    });
                }
                TransformDriftMode::Permissive => {
                    // F-005.4: Permissive mode logs but uses the ORIGINAL
                    // approved transformation, not the drifted one. This
                    // preserves approval integrity: the human approved
                    // pre_approval_transformed, so that's what gets executed.
                    warn!(
                        task_id = %task.id,
                        stored_hash = %task.request_hash,
                        new_hash = %new_hash,
                        "Transform drift detected (permissive mode) - \
                         using original approved request"
                    );
                    return Ok(task.pre_approval_transformed.clone());
                }
            }
        }

        Ok(transformed)
    }

    /// Forward request to upstream MCP server.
    ///
    /// Implements: REQ-GOV-002/F-006
    async fn forward_to_upstream(&self, request: &ToolCallRequest, task: &Task) -> PipelineResult {
        // Convert to McpRequest
        let mcp_request = to_mcp_request(request);

        // F-006.1: Apply execution timeout
        let result = tokio::time::timeout(
            self.config.execution_timeout,
            self.upstream_client.forward(&mcp_request),
        )
        .await;

        match result {
            Ok(Ok(response)) => {
                // F-006.3: Success
                if let Some(error) = response.error {
                    // Upstream returned JSON-RPC error
                    warn!(
                        task_id = %task.id,
                        error_code = error.code,
                        error_message = %error.message,
                        "Upstream returned error"
                    );
                    PipelineResult::Failure {
                        stage: FailureStage::UpstreamError,
                        reason: error.message,
                        retriable: is_retriable_error(error.code),
                    }
                } else {
                    // Success
                    let content = match response.result {
                        Some(value) => value,
                        None => {
                            warn!(
                                task_id = %task.id,
                                "Upstream response has neither result nor error — treating as null"
                            );
                            serde_json::Value::Null
                        }
                    };
                    let result = ToolCallResult {
                        content,
                        is_error: false,
                    };
                    info!(
                        task_id = %task.id,
                        "Execution completed successfully"
                    );
                    PipelineResult::Success { result }
                }
            }
            Ok(Err(e)) => {
                // F-006.2: Handle upstream errors
                warn!(
                    task_id = %task.id,
                    error = %e,
                    "Upstream request failed"
                );
                let (retriable, reason) = classify_upstream_error(&e);
                PipelineResult::Failure {
                    stage: FailureStage::UpstreamError,
                    reason,
                    retriable,
                }
            }
            Err(_) => {
                // Timeout
                warn!(
                    task_id = %task.id,
                    timeout_secs = self.config.execution_timeout.as_secs(),
                    "Upstream request timed out"
                );
                PipelineResult::Failure {
                    stage: FailureStage::UpstreamError,
                    reason: "Upstream request timed out".to_string(),
                    retriable: true,
                }
            }
        }
    }
}

#[async_trait]
impl ExecutionPipeline for ApprovalPipeline {
    /// Implements: REQ-GOV-002/F-001 (Pre-Approval Amber Phase)
    #[tracing::instrument(skip(self, request, _principal), fields(tool = %request.name))]
    async fn pre_approval_amber(
        &self,
        request: &ToolCallRequest,
        _principal: &Principal,
    ) -> Result<PreHitlResult, PipelineError> {
        info!(
            tool = %request.name,
            "Starting pre-approval amber phase"
        );

        // Run inspector chain
        let transformed = self.run_inspector_chain(request).await?;

        // Compute hash of transformed request
        let request_hash = hash_request(&transformed);

        info!(
            tool = %request.name,
            hash = %request_hash,
            "Pre-approval amber phase complete"
        );

        Ok(PreHitlResult {
            transformed_request: transformed,
            request_hash,
        })
    }

    /// Implements: REQ-GOV-002/F-002 (Execution Pipeline)
    #[tracing::instrument(skip(self, task, approval), fields(task_id = %task.id))]
    async fn execute_approved(&self, task: &Task, approval: &ApprovalRecord) -> PipelineResult {
        let tool_name = &task.pre_approval_transformed.name;

        info!(
            task_id = %task.id,
            tool = %tool_name,
            "Starting execution pipeline"
        );

        // Phase 1: Validate approval
        // Implements: REQ-GOV-002/F-003
        if let Err(failure) = self.validate_approval(task, approval) {
            return failure;
        }

        // Phase 2: Policy re-evaluation
        // Implements: REQ-GOV-002/F-004
        if let Err(failure) = self.reevaluate_policy(task, approval) {
            return failure;
        }

        // Phase 3: Post-approval amber
        // Implements: REQ-GOV-002/F-005
        let final_request = match self.post_approval_amber(task).await {
            Ok(req) => req,
            Err(failure) => return failure,
        };

        // Phase 4: Upstream forward
        // Implements: REQ-GOV-002/F-006
        self.forward_to_upstream(&final_request, task).await
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

// NOTE: hash_request is imported from super (task.rs) to ensure consistent
// hashing between task creation and pipeline execution.

/// Convert ToolCallRequest to McpRequest for upstream.
///
/// Reconstructs the correct params structure based on the original method:
/// - `tools/call` → `{ name, arguments }`
/// - `resources/read` → `{ uri }`
/// - `resources/subscribe` → `{ uri }`
/// - `prompts/get` → `{ name, arguments }`
fn to_mcp_request(request: &ToolCallRequest) -> McpRequest {
    let id = match &request.mcp_request_id {
        super::JsonRpcId::Number(n) => Some(JsonRpcId::Number(*n)),
        super::JsonRpcId::String(s) => Some(JsonRpcId::String(s.clone())),
        super::JsonRpcId::Null => None,
    };

    // Build method-specific params structure
    let params = match request.method.as_str() {
        "tools/call" => Some(serde_json::json!({
            "name": request.name,
            "arguments": request.arguments,
        })),
        "resources/read" | "resources/subscribe" => Some(serde_json::json!({
            "uri": request.name,
        })),
        "prompts/get" => {
            // prompts/get can have optional arguments
            if request.arguments.is_null() || request.arguments == serde_json::json!({}) {
                Some(serde_json::json!({
                    "name": request.name,
                }))
            } else {
                Some(serde_json::json!({
                    "name": request.name,
                    "arguments": request.arguments,
                }))
            }
        }
        // Fallback for any other methods
        _ => Some(serde_json::json!({
            "name": request.name,
            "arguments": request.arguments,
        })),
    };

    McpRequest {
        id,
        method: request.method.clone(),
        params: params.map(std::sync::Arc::new),
        task_metadata: None,
        received_at: Instant::now(),
        correlation_id: crate::transport::jsonrpc::fast_correlation_id(),
    }
}

/// Build PolicyRequest from task components.
///
/// Uses the real K8s identity stored on the governance Principal (populated
/// via [`Principal::from_policy`] at Gate 4) to ensure post-approval policy
/// re-evaluation sees the correct namespace, service_account, and roles.
fn build_policy_request(
    request: &ToolCallRequest,
    principal: &Principal,
    approval_grant: Option<ApprovalGrant>,
) -> PolicyRequest {
    PolicyRequest {
        principal: PolicyPrincipal {
            app_name: principal.app_name.clone(),
            namespace: principal.namespace.clone(),
            service_account: principal.service_account.clone(),
            roles: principal.roles.clone(),
        },
        resource: Resource::ToolCall {
            name: request.name.clone(),
            server: "default".to_string(),
        },
        context: approval_grant.map(|g| PolicyContext {
            approval_grant: Some(g),
        }),
    }
}

/// Classify upstream errors for retriability.
fn classify_upstream_error(error: &crate::error::ThoughtGateError) -> (bool, String) {
    use crate::error::ThoughtGateError;

    match error {
        ThoughtGateError::UpstreamTimeout { .. } => (true, "Upstream timed out".to_string()),
        ThoughtGateError::UpstreamConnectionFailed { reason, .. } => {
            (true, format!("Connection failed: {}", reason))
        }
        ThoughtGateError::UpstreamError { message, .. } => (false, message.clone()),
        _ => (false, error.to_string()),
    }
}

/// Check if JSON-RPC error code is retriable.
fn is_retriable_error(code: i32) -> bool {
    // -32000: Connection failed, -32001: Timeout, -32013: Service unavailable
    matches!(code, -32000 | -32001 | -32013)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::ApprovalDecision;
    use chrono::Utc;

    // ─────────────────────────────────────────────────────────────────────────
    // Hash computation tests
    // ─────────────────────────────────────────────────────────────────────────

    /// Tests that hash computation is deterministic.
    ///
    /// Verifies: EC-PIP-008
    #[test]
    fn test_hash_request_deterministic() {
        let request = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "delete_user".to_string(),
            arguments: serde_json::json!({"user_id": 123}),
            mcp_request_id: super::super::JsonRpcId::Number(1),
        };

        let hash1 = hash_request(&request);
        let hash2 = hash_request(&request);

        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // SHA256 produces 64 hex chars
    }

    /// Tests that hash ignores mcp_request_id.
    ///
    /// Verifies: Request integrity is based on content, not metadata.
    #[test]
    fn test_hash_request_ignores_mcp_request_id() {
        let request1 = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "delete_user".to_string(),
            arguments: serde_json::json!({"user_id": 123}),
            mcp_request_id: super::super::JsonRpcId::Number(1),
        };

        let request2 = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "delete_user".to_string(),
            arguments: serde_json::json!({"user_id": 123}),
            mcp_request_id: super::super::JsonRpcId::Number(999),
        };

        assert_eq!(hash_request(&request1), hash_request(&request2));
    }

    /// Tests that different content produces different hashes.
    #[test]
    fn test_hash_request_different_content() {
        let request1 = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "delete_user".to_string(),
            arguments: serde_json::json!({"user_id": 123}),
            mcp_request_id: super::super::JsonRpcId::Number(1),
        };

        let request2 = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "delete_user".to_string(),
            arguments: serde_json::json!({"user_id": 456}),
            mcp_request_id: super::super::JsonRpcId::Number(1),
        };

        assert_ne!(hash_request(&request1), hash_request(&request2));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Configuration tests
    // ─────────────────────────────────────────────────────────────────────────

    /// Tests default configuration values.
    ///
    /// Verifies: REQ-GOV-002/§5.1
    #[test]
    fn test_config_defaults() {
        let config = PipelineConfig::default();

        assert_eq!(config.approval_validity, Duration::from_secs(300));
        assert_eq!(config.execution_timeout, Duration::from_secs(30));
        assert_eq!(config.transform_drift_mode, TransformDriftMode::Strict);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Pipeline result tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_pipeline_result_success() {
        let result = PipelineResult::Success {
            result: ToolCallResult {
                content: serde_json::json!({"status": "ok"}),
                is_error: false,
            },
        };

        assert!(result.is_success());
        assert!(!result.is_failure());
    }

    #[test]
    fn test_pipeline_result_failure() {
        let result = PipelineResult::Failure {
            stage: FailureStage::ApprovalTimeout,
            reason: "Approval expired".to_string(),
            retriable: false,
        };

        assert!(!result.is_success());
        assert!(result.is_failure());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Error classification tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_is_retriable_error() {
        // Retriable codes
        assert!(is_retriable_error(-32000)); // Connection failed
        assert!(is_retriable_error(-32001)); // Timeout
        assert!(is_retriable_error(-32013)); // Service unavailable

        // Non-retriable codes
        assert!(!is_retriable_error(-32600)); // Invalid request
        assert!(!is_retriable_error(-32601)); // Method not found
        assert!(!is_retriable_error(-32003)); // Policy denied
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Validation tests
    // ─────────────────────────────────────────────────────────────────────────

    /// Helper to create a test task
    fn test_task(status: TaskStatus) -> Task {
        Task {
            id: super::super::TaskId::new(),
            original_request: ToolCallRequest {
                method: "tools/call".to_string(),
                name: "test_tool".to_string(),
                arguments: serde_json::json!({}),
                mcp_request_id: super::super::JsonRpcId::Number(1),
            },
            pre_approval_transformed: ToolCallRequest {
                method: "tools/call".to_string(),
                name: "test_tool".to_string(),
                arguments: serde_json::json!({}),
                mcp_request_id: super::super::JsonRpcId::Number(1),
            },
            request_hash: "abc123".to_string(),
            principal: Principal::new("test-app"),
            created_at: Utc::now(),
            ttl: Duration::from_secs(3600),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            poll_interval: Duration::from_secs(5),
            status,
            status_message: None,
            transitions: vec![],
            approval: None,
            result: None,
            failure: None,
            on_timeout: crate::governance::TimeoutAction::default(),
        }
    }

    /// Helper to create a valid approval
    fn valid_approval() -> ApprovalRecord {
        ApprovalRecord {
            decision: ApprovalDecision::Approved,
            decided_by: "test-user".to_string(),
            decided_at: Utc::now(),
            approval_valid_until: Utc::now() + chrono::Duration::hours(1),
            metadata: None,
        }
    }

    /// Helper to create an expired approval
    fn expired_approval() -> ApprovalRecord {
        ApprovalRecord {
            decision: ApprovalDecision::Approved,
            decided_by: "test-user".to_string(),
            decided_at: Utc::now() - chrono::Duration::hours(2),
            approval_valid_until: Utc::now() - chrono::Duration::hours(1),
            metadata: None,
        }
    }

    /// Tests that expired approval is detected.
    ///
    /// Verifies: EC-PIP-004
    #[test]
    fn test_approval_expired() {
        let approval = expired_approval();

        // Manually validate (since we can't create full ApprovalPipeline in unit test easily)
        let now = chrono::Utc::now();
        let is_expired = now > approval.approval_valid_until;
        assert!(is_expired, "Approval should be detected as expired");
    }

    /// Tests that valid approval passes validation.
    ///
    /// Verifies: EC-PIP-003
    #[test]
    fn test_approval_valid() {
        let approval = valid_approval();
        let now = chrono::Utc::now();
        let is_valid = now <= approval.approval_valid_until;
        assert!(is_valid, "Approval should be valid");
    }

    /// Tests task state validation.
    #[test]
    fn test_task_state_validation() {
        let task_executing = test_task(TaskStatus::Executing);
        assert_eq!(task_executing.status, TaskStatus::Executing);

        let task_input_required = test_task(TaskStatus::InputRequired);
        assert_ne!(task_input_required.status, TaskStatus::Executing);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Transform drift mode tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_transform_drift_mode_strict_default() {
        let mode = TransformDriftMode::default();
        assert_eq!(mode, TransformDriftMode::Strict);
    }
}
