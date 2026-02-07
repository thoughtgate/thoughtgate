//! Transport-agnostic governance evaluator (Gate 1–4).
//!
//! Implements: REQ-CORE-008/F-016 (Governance Integration)
//!
//! This module provides [`GovernanceEvaluator`], a concrete struct that
//! encapsulates the 4-gate governance logic without any upstream forwarding
//! concern. It operates on the wire types from [`super::api`] and returns
//! governance decisions that the transport layer (shim or proxy) acts on.
//!
//! ## Gate Flow
//!
//! ```text
//! ┌─ Gate 1: Visibility ──────────────────────┐
//! │  expose.is_visible(resource_name)?        │
//! │  Not visible → Deny                       │
//! └───────────────────────────────────────────┘
//!     │
//! ┌─ Gate 2: Governance Rules ────────────────┐
//! │  governance.evaluate(resource, source_id) │
//! │  Forward → Forward, Deny → Deny           │
//! │  Approve → Gate 4, Policy → Gate 3        │
//! └───────────────────────────────────────────┘
//!     │
//! ┌─ Gate 3: Cedar Policy ────────────────────┐
//! │  cedar_engine.evaluate_v2(request)        │
//! │  Permit → Gate 4, Forbid → Deny           │
//! └───────────────────────────────────────────┘
//!     │
//! ┌─ Gate 4: Approval Workflow ───────────────┐
//! │  Create task, post to Slack, return       │
//! │  PendingApproval with task_id             │
//! └───────────────────────────────────────────┘
//! ```

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::Utc;
use tracing::{debug, info, warn};

use crate::StreamDirection;
use crate::config::{Action, Config, ThoughtGateDefaults};
use crate::governance::api::{
    GovernanceDecision, GovernanceEvaluateRequest, GovernanceEvaluateResponse, MessageType,
};
use crate::governance::approval::{ApprovalRequest, PollingScheduler};
use crate::governance::engine::{ApprovalEngine, TimeoutAction};
use crate::governance::task::{Principal, TaskStatus, TaskStore, ToolCallRequest};
use crate::jsonrpc::JsonRpcId;
use crate::policy::engine::CedarEngine;
use crate::policy::types::{CedarContext, CedarDecision, CedarResource, TimeContext};
use crate::profile::Profile;
use crate::telemetry::{CedarSpanData, ThoughtGateMetrics, finish_cedar_span, start_cedar_span};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Default task TTL for approval workflows.
const DEFAULT_APPROVAL_TTL_SECS: u64 = 300;

// ─────────────────────────────────────────────────────────────────────────────
// Trace & Deny Types
// ─────────────────────────────────────────────────────────────────────────────

/// Gate-level trace data for telemetry (not serialized over the wire).
///
/// Returned alongside [`GovernanceEvaluateResponse`] so callers can populate
/// OTel span attributes accurately without lossy reconstruction.
///
/// Implements: REQ-OBS-002 §5.3 (Gateway Decision Span)
#[derive(Debug, Default)]
pub struct EvalTrace {
    /// Gate 1 (visibility) outcome: `"pass"` or `"block"`.
    pub gate1: Option<String>,
    /// Gate 2 (governance rules) outcome: `"forward"`, `"deny"`, `"approve"`, or `"policy"`.
    pub gate2: Option<String>,
    /// Gate 2 matched governance rule ID.
    pub gate2_rule_id: Option<String>,
    /// Gate 3 (Cedar policy) outcome: `"allow"` or `"deny"`.
    pub gate3: Option<String>,
    /// Cedar policy ID that was evaluated.
    pub gate3_policy_id: Option<String>,
    /// Cedar evaluation duration in milliseconds.
    pub gate3_duration_ms: Option<f64>,
    /// Gate 4 (approval) outcome: `"started"` if an approval was created.
    pub gate4: Option<String>,
}

/// Where a Deny originated (for correct error code mapping).
///
/// Callers (proxy, service) map this to the appropriate `ThoughtGateError`
/// variant so that JSON-RPC error codes are accurate per-gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenySource {
    /// Gate 1 → `-32015 ToolNotExposed`
    Visibility,
    /// Gate 2 → `-32014 GovernanceRuleDenied`
    GovernanceRule,
    /// Gate 3 → `-32003 PolicyDenied`
    CedarPolicy,
}

// ─────────────────────────────────────────────────────────────────────────────
// GovernanceEvaluator
// ─────────────────────────────────────────────────────────────────────────────

/// Transport-agnostic governance evaluator implementing the 4-gate model.
///
/// Evaluates [`GovernanceEvaluateRequest`] messages through Gates 1–4 and
/// returns a [`GovernanceEvaluateResponse`] without performing any upstream
/// forwarding. The transport layer (shim or proxy) is responsible for acting
/// on the decision.
///
/// Implements: REQ-CORE-008/F-016
pub struct GovernanceEvaluator {
    config: Arc<Config>,
    cedar_engine: Option<Arc<CedarEngine>>,
    task_store: Arc<TaskStore>,
    principal: Principal,
    profile: Profile,
    /// Legacy scheduler (used when no ApprovalEngine is configured).
    scheduler: OnceLock<Arc<PollingScheduler>>,
    tg_metrics: Option<Arc<ThoughtGateMetrics>>,
    /// When set, Gate 4 delegates to the engine (preferred path).
    /// When None, falls back to direct TaskStore + scheduler manipulation.
    approval_engine: Option<Arc<ApprovalEngine>>,
}

impl GovernanceEvaluator {
    /// Create a new governance evaluator.
    ///
    /// The `cedar_engine` is optional — if `None`, Gate 3 (Cedar policy)
    /// is skipped and `action: policy` rules fall through to Forward (dev)
    /// or Deny (prod).
    ///
    /// Implements: REQ-CORE-008/F-016
    pub fn new(
        config: Arc<Config>,
        cedar_engine: Option<Arc<CedarEngine>>,
        task_store: Arc<TaskStore>,
        principal: Principal,
        profile: Profile,
    ) -> Self {
        Self {
            config,
            cedar_engine,
            task_store,
            principal,
            profile,
            scheduler: OnceLock::new(),
            tg_metrics: None,
            approval_engine: None,
        }
    }

    /// Builder-style method to set Prometheus metrics.
    ///
    /// When set, the evaluator records per-gate decisions and Cedar
    /// evaluation timing as Prometheus counters/histograms.
    pub fn with_metrics(mut self, metrics: Arc<ThoughtGateMetrics>) -> Self {
        self.tg_metrics = Some(metrics);
        self
    }

    /// Builder-style method to set the approval engine.
    ///
    /// When set, Gate 4 delegates to the engine (which handles
    /// pre-approval pipeline, task creation, Slack posting, and polling).
    /// When unset, Gate 4 falls back to direct TaskStore + scheduler.
    pub fn with_approval_engine(mut self, engine: Arc<ApprovalEngine>) -> Self {
        self.approval_engine = Some(engine);
        self
    }

    /// Set the polling scheduler (after construction).
    ///
    /// The scheduler is set after construction because it may depend on
    /// whether approval workflows are needed (determined after config load).
    pub fn set_scheduler(&self, scheduler: Arc<PollingScheduler>) {
        if self.scheduler.set(scheduler).is_err() {
            debug!("Scheduler already initialized, ignoring duplicate set_scheduler call");
        }
    }

    /// Evaluate a governance request through Gates 1–4.
    ///
    /// Uses the evaluator's default principal (set at construction).
    /// For per-request principal override, use [`evaluate_with_principal`].
    ///
    /// Returns a [`GovernanceEvaluateResponse`] with the governance decision
    /// and an [`EvalTrace`] carrying per-gate outcome data for telemetry.
    /// The `shutdown` field is NOT set here — the caller (service.rs) sets it.
    ///
    /// Implements: REQ-CORE-008/F-016
    pub async fn evaluate(
        &self,
        req: &GovernanceEvaluateRequest,
    ) -> (GovernanceEvaluateResponse, EvalTrace) {
        self.evaluate_with_principal(req, None).await
    }

    /// Evaluate with a request-specific principal (overrides default).
    ///
    /// Used by the proxy where principal is inferred per-request (e.g.,
    /// K8s service account detection). Falls back to `self.principal`
    /// when `principal_override` is `None`.
    ///
    /// Implements: REQ-CORE-008/F-016
    pub async fn evaluate_with_principal(
        &self,
        req: &GovernanceEvaluateRequest,
        principal_override: Option<&Principal>,
    ) -> (GovernanceEvaluateResponse, EvalTrace) {
        let effective_principal = principal_override.unwrap_or(&self.principal);
        let mut trace = EvalTrace::default();

        // ── Fast-path: only evaluate agent→server requests ───────────────
        if req.message_type != MessageType::Request
            || req.direction != StreamDirection::AgentToServer
        {
            return (self.forward(None), trace);
        }

        // ── Fast-path: skip non-governable methods ──────────────────────
        if !method_requires_gates(&req.method) {
            return (self.forward(None), trace);
        }

        // ── Extract resource name from params ───────────────────────────
        let resource_name = match extract_resource_name(&req.method, req.params.as_ref()) {
            Some(name) => name,
            None => {
                // Governable method without a resource identifier — forward.
                // This handles list methods (tools/list, etc.) that don't
                // reference a specific tool/resource.
                return (self.forward(None), trace);
            }
        };

        // ── Gate 1: Visibility check ────────────────────────────────────
        if let Some(source) = self.config.get_source(&req.server_id) {
            let expose = source.expose();
            if !expose.is_visible(&resource_name) {
                info!(
                    resource = %resource_name,
                    server_id = %req.server_id,
                    "Gate 1: resource not exposed — denying"
                );
                trace.gate1 = Some("block".to_string());
                if let Some(ref m) = self.tg_metrics {
                    m.record_gate_decision("gate1", "block");
                }
                return (
                    self.deny(
                        Some(format!(
                            "Resource '{}' not exposed on source '{}'",
                            resource_name, req.server_id
                        )),
                        None,
                        DenySource::Visibility,
                    ),
                    trace,
                );
            }
            trace.gate1 = Some("pass".to_string());
            if let Some(ref m) = self.tg_metrics {
                m.record_gate_decision("gate1", "pass");
            }
            debug!(resource = %resource_name, "Gate 1 passed: resource is visible");
        } else {
            // Source not in config — fail closed regardless of profile.
            // This is a hard boundary: if the source_id isn't configured,
            // we can't evaluate anything.
            warn!(
                server_id = %req.server_id,
                "Gate 1: source not found in config — denying"
            );
            trace.gate1 = Some("block".to_string());
            if let Some(ref m) = self.tg_metrics {
                m.record_gate_decision("gate1", "block");
            }
            return (
                self.deny(
                    Some(format!(
                        "Source '{}' not found in configuration",
                        req.server_id
                    )),
                    None,
                    DenySource::Visibility,
                ),
                trace,
            );
        }

        // ── Gate 2: Governance rules ────────────────────────────────────
        let match_result = self
            .config
            .governance
            .evaluate(&resource_name, &req.server_id);

        info!(
            resource = %resource_name,
            action = ?match_result.action,
            matched_rule = ?match_result.matched_rule,
            "Gate 2: governance rule matched"
        );

        trace.gate2_rule_id = match_result.matched_rule.clone();

        let gate2_outcome = match match_result.action {
            Action::Forward => "forward",
            Action::Deny => "deny",
            Action::Policy => "policy",
            Action::Approve => "approve",
        };
        trace.gate2 = Some(gate2_outcome.to_string());
        if let Some(ref m) = self.tg_metrics {
            m.record_gate_decision("gate2", gate2_outcome);
        }

        match match_result.action {
            Action::Forward => {
                debug!(resource = %resource_name, "Gate 2: forwarding directly");
                (self.forward(None), trace)
            }
            Action::Deny => {
                info!(resource = %resource_name, "Gate 2: denied by governance rule");
                (
                    self.deny(
                        Some(format!("Denied by governance rule for '{}'", resource_name)),
                        match_result.policy_id.as_deref(),
                        DenySource::GovernanceRule,
                    ),
                    trace,
                )
            }
            Action::Policy => {
                // Gate 3: Cedar policy evaluation
                debug!(resource = %resource_name, "Gate 2 → Gate 3: evaluating Cedar policy");
                let resp = self
                    .evaluate_cedar(
                        req,
                        &resource_name,
                        &match_result.policy_id,
                        &match_result.approval_workflow,
                        effective_principal,
                        &mut trace,
                    )
                    .await;
                (resp, trace)
            }
            Action::Approve => {
                // Gate 4: Approval workflow
                debug!(resource = %resource_name, "Gate 2 → Gate 4: starting approval workflow");
                let resp = self
                    .start_approval(
                        req,
                        &resource_name,
                        match_result.policy_id.as_deref(),
                        effective_principal,
                        &mut trace,
                    )
                    .await;
                (resp, trace)
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Gate 3: Cedar Policy
    // ─────────────────────────────────────────────────────────────────────

    /// Evaluate Cedar policy (Gate 3) and route to Gate 4 on Permit.
    ///
    /// Implements: REQ-CORE-008/F-016, REQ-POL-001
    async fn evaluate_cedar(
        &self,
        req: &GovernanceEvaluateRequest,
        resource_name: &str,
        policy_id: &Option<String>,
        approval_workflow: &Option<String>,
        principal: &Principal,
        trace: &mut EvalTrace,
    ) -> GovernanceEvaluateResponse {
        let engine = match &self.cedar_engine {
            Some(engine) => engine,
            None => {
                // No Cedar engine — profile-dependent fallback.
                if self.profile == Profile::Development {
                    info!(
                        resource = %resource_name,
                        "Gate 3: no Cedar engine, auto-forwarding in dev mode"
                    );
                    return self.forward(Some(
                        "No Cedar engine configured — forwarding in dev mode".to_string(),
                    ));
                }
                warn!(
                    resource = %resource_name,
                    "Gate 3: no Cedar engine — denying in production"
                );
                return self.deny(
                    Some("Cedar engine not configured".to_string()),
                    None,
                    DenySource::CedarPolicy,
                );
            }
        };

        let pid = policy_id.clone().unwrap_or_else(|| "default".to_string());
        trace.gate3_policy_id = Some(pid.clone());

        let cedar_resource = if req.method == "tools/call" {
            CedarResource::ToolCall {
                name: resource_name.to_string(),
                server: req.server_id.clone(),
                arguments: extract_arguments(req.params.as_ref()),
            }
        } else {
            CedarResource::McpMethod {
                method: req.method.clone(),
                server: req.server_id.clone(),
            }
        };

        let resource_type = match &cedar_resource {
            CedarResource::ToolCall { .. } => "ThoughtGate::ToolCall",
            CedarResource::McpMethod { .. } => "ThoughtGate::McpMethod",
        };

        let cedar_request = crate::policy::types::CedarRequest {
            principal: crate::policy::Principal {
                app_name: principal.app_name.clone(),
                namespace: principal.namespace.clone(),
                service_account: principal.service_account.clone(),
                roles: principal.roles.clone(),
            },
            resource: cedar_resource,
            context: CedarContext {
                policy_id: pid.clone(),
                source_id: req.server_id.clone(),
                time: TimeContext::now(),
            },
        };

        // Create Cedar OTel span (REQ-OBS-002 §5.2)
        let cedar_span_data = CedarSpanData {
            tool_name: resource_name.to_string(),
            policy_id: Some(pid.clone()),
            principal_type: "ThoughtGate::App".to_string(),
            principal_id: principal.app_name.clone(),
            action: req.method.clone(),
            resource_type: resource_type.to_string(),
            resource_id: resource_name.to_string(),
        };
        let mut cedar_span = start_cedar_span(&cedar_span_data, &opentelemetry::Context::current());

        let eval_start = std::time::Instant::now();
        let cedar_result = engine.evaluate_v2(&cedar_request);
        let eval_duration_ms = eval_start.elapsed().as_secs_f64() * 1000.0;
        trace.gate3_duration_ms = Some(eval_duration_ms);

        match cedar_result {
            CedarDecision::Permit { .. } => {
                trace.gate3 = Some("allow".to_string());

                // Finish Cedar span + record metrics
                finish_cedar_span(&mut cedar_span, "allow", &pid, eval_duration_ms, None);
                if let Some(ref m) = self.tg_metrics {
                    m.record_cedar_eval("allow", &pid, eval_duration_ms);
                    m.record_gate_decision("gate3", "allow");
                }

                // Cedar permit → Gate 4 (approval workflow)
                debug!(
                    resource = %resource_name,
                    policy_id = %pid,
                    "Gate 3: Cedar permit → Gate 4 approval"
                );
                self.start_approval(req, resource_name, Some(&pid), principal, trace)
                    .await
            }
            CedarDecision::Forbid { reason, .. } => {
                trace.gate3 = Some("deny".to_string());

                // Finish Cedar span + record metrics
                finish_cedar_span(
                    &mut cedar_span,
                    "deny",
                    &pid,
                    eval_duration_ms,
                    Some(&reason),
                );
                if let Some(ref m) = self.tg_metrics {
                    m.record_cedar_eval("deny", &pid, eval_duration_ms);
                    m.record_gate_decision("gate3", "deny");
                }

                if self.profile == Profile::Development {
                    info!(
                        resource = %resource_name,
                        reason = %reason,
                        "Gate 3: WOULD_DENY (Cedar forbid) — auto-forwarding in dev mode"
                    );
                    return self.forward(Some(format!("WOULD_DENY: Cedar forbid — {}", reason)));
                }
                info!(
                    resource = %resource_name,
                    reason = %reason,
                    "Gate 3: Cedar forbid — denying"
                );
                let _ = approval_workflow; // used in future for workflow selection
                self.deny(Some(reason), Some(&pid), DenySource::CedarPolicy)
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Gate 4: Approval Workflow
    // ─────────────────────────────────────────────────────────────────────

    /// Start an approval workflow (Gate 4).
    ///
    /// In development mode, logs the would-be approval and returns Forward.
    /// In production, delegates to `ApprovalEngine` when available (preferred),
    /// falling back to direct TaskStore + scheduler manipulation (legacy).
    ///
    /// Implements: REQ-CORE-008/F-016, REQ-GOV-001
    async fn start_approval(
        &self,
        req: &GovernanceEvaluateRequest,
        resource_name: &str,
        policy_id: Option<&str>,
        principal: &Principal,
        trace: &mut EvalTrace,
    ) -> GovernanceEvaluateResponse {
        // ── Development mode: skip approval, auto-forward ───────────────
        if self.profile == Profile::Development {
            info!(
                method = %req.method,
                resource = %resource_name,
                "WOULD_REQUIRE_APPROVAL — auto-forwarding in dev mode"
            );
            return self.forward(Some(format!(
                "WOULD_REQUIRE_APPROVAL for '{}' — auto-forwarding in dev mode",
                resource_name
            )));
        }

        let mcp_request_id = req.id.clone().unwrap_or(JsonRpcId::Null);

        let tool_request = ToolCallRequest {
            method: req.method.clone(),
            name: resource_name.to_string(),
            arguments: extract_arguments(req.params.as_ref()),
            mcp_request_id,
        };

        // ── Preferred path: delegate to ApprovalEngine ──────────────────
        if let Some(ref engine) = self.approval_engine {
            match engine
                .start_approval(tool_request, principal.clone(), None)
                .await
            {
                Ok(result) => {
                    let poll_interval_ms = result.poll_interval.as_millis() as u64;

                    // Look up task TTL for the shim's timeout hint.
                    let timeout_secs = self
                        .task_store
                        .get(&result.task_id)
                        .ok()
                        .map(|task| task.ttl.as_secs());

                    trace.gate4 = Some("started".to_string());
                    if let Some(ref m) = self.tg_metrics {
                        m.record_gate_decision("gate4", "started");
                        m.record_approval_request("slack", "pending");
                    }

                    return GovernanceEvaluateResponse {
                        decision: GovernanceDecision::PendingApproval,
                        task_id: Some(result.task_id.to_string()),
                        policy_id: policy_id.map(String::from),
                        reason: None,
                        poll_interval_ms: Some(poll_interval_ms),
                        timeout_secs,
                        shutdown: false,
                        deny_source: None,
                    };
                }
                Err(e) => {
                    warn!(error = %e, "Gate 4: ApprovalEngine failed — denying");
                    return self.deny(
                        Some(format!("Approval workflow failed: {e}")),
                        policy_id,
                        DenySource::GovernanceRule,
                    );
                }
            }
        }

        // ── Legacy path: direct TaskStore + scheduler ───────────────────

        // Create task in TaskStore.
        let task = match self.task_store.create(
            tool_request.clone(),
            tool_request,
            principal.clone(),
            Some(Duration::from_secs(DEFAULT_APPROVAL_TTL_SECS)),
            TimeoutAction::Deny,
        ) {
            Ok(task) => task,
            Err(e) => {
                warn!(error = %e, "Gate 4: failed to create approval task — denying");
                return self.deny(
                    Some(format!("Failed to create approval task: {e}")),
                    policy_id,
                    DenySource::GovernanceRule,
                );
            }
        };

        let task_id = task.id.clone();

        // Transition to InputRequired (waiting for human decision).
        if let Err(e) = self
            .task_store
            .transition(&task_id, TaskStatus::InputRequired, None)
        {
            warn!(
                task_id = %task_id,
                error = %e,
                "Gate 4: failed to transition task to InputRequired"
            );
            return self.deny(
                Some(format!("Task transition failed: {e}")),
                policy_id,
                DenySource::GovernanceRule,
            );
        }

        // Submit to scheduler (if available) for Slack posting + polling.
        if let Some(scheduler) = self.scheduler.get() {
            let approval_req = ApprovalRequest {
                task_id: task_id.clone(),
                tool_name: resource_name.to_string(),
                tool_arguments: extract_arguments(req.params.as_ref()),
                principal: principal.clone(),
                expires_at: task.expires_at,
                created_at: Utc::now(),
                correlation_id: task_id.to_string(),
                request_span_context: None,
            };

            if let Err(e) = scheduler.submit(approval_req).await {
                warn!(
                    task_id = %task_id,
                    error = %e,
                    "Gate 4: failed to submit to scheduler — denying"
                );
                if let Err(te) = self.task_store.transition(
                    &task_id,
                    TaskStatus::Failed,
                    Some(format!("Scheduler submission failed: {e}")),
                ) {
                    warn!(task_id = %task_id, error = %te, "Failed to transition task to Failed after scheduler submission error");
                }
                return self.deny(
                    Some(format!("Approval submission failed: {e}")),
                    policy_id,
                    DenySource::GovernanceRule,
                );
            }

            info!(
                task_id = %task_id,
                resource = %resource_name,
                "Gate 4: approval workflow started"
            );
        } else {
            warn!(
                task_id = %task_id,
                "Gate 4: no scheduler configured — task created but no Slack post"
            );
        }

        // Use centralized default for poll interval (REQ-CFG-001 §5.6)
        let poll_interval_ms = ThoughtGateDefaults::default()
            .approval_poll_interval
            .as_millis() as u64;

        trace.gate4 = Some("started".to_string());
        if let Some(ref m) = self.tg_metrics {
            m.record_gate_decision("gate4", "started");
            m.record_approval_request("slack", "pending");
        }

        GovernanceEvaluateResponse {
            decision: GovernanceDecision::PendingApproval,
            task_id: Some(task_id.to_string()),
            policy_id: policy_id.map(String::from),
            reason: None,
            poll_interval_ms: Some(poll_interval_ms),
            timeout_secs: Some(DEFAULT_APPROVAL_TTL_SECS),
            shutdown: false,
            deny_source: None,
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Response Builders
    // ─────────────────────────────────────────────────────────────────────

    fn forward(&self, reason: Option<String>) -> GovernanceEvaluateResponse {
        GovernanceEvaluateResponse {
            decision: GovernanceDecision::Forward,
            task_id: None,
            policy_id: None,
            reason,
            poll_interval_ms: None,
            timeout_secs: None,
            shutdown: false,
            deny_source: None,
        }
    }

    fn deny(
        &self,
        reason: Option<String>,
        policy_id: Option<&str>,
        source: DenySource,
    ) -> GovernanceEvaluateResponse {
        GovernanceEvaluateResponse {
            decision: GovernanceDecision::Deny,
            task_id: None,
            policy_id: policy_id.map(String::from),
            reason,
            poll_interval_ms: None,
            timeout_secs: None,
            shutdown: false,
            deny_source: Some(source),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Private Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Check if a JSON-RPC method requires Gate 1–4 evaluation.
///
/// Only certain MCP methods have governable resources:
/// - `tools/call` — tool invocations
/// - `resources/read` — resource access
/// - `resources/subscribe` — resource subscriptions
/// - `prompts/get` — prompt retrieval
///
/// All other methods (list methods, initialize, notifications, etc.)
/// are forwarded without governance evaluation.
pub fn method_requires_gates(method: &str) -> bool {
    matches!(
        method,
        "tools/call" | "resources/read" | "resources/subscribe" | "prompts/get"
    )
}

/// Extract the governable resource name from JSON-RPC params.
///
/// Returns the tool name, resource URI, or prompt name depending on the method.
///
/// Implements: REQ-CORE-008/F-016
pub fn extract_resource_name(method: &str, params: Option<&serde_json::Value>) -> Option<String> {
    let params = params?;

    match method {
        "tools/call" => params.get("name")?.as_str().map(String::from),
        "resources/read" | "resources/subscribe" => params.get("uri")?.as_str().map(String::from),
        "prompts/get" => params.get("name")?.as_str().map(String::from),
        _ => None,
    }
}

/// Extract tool arguments from JSON-RPC params.
///
/// Returns `params.arguments` if present, otherwise an empty JSON object.
///
/// Implements: REQ-CORE-008/F-016
pub fn extract_arguments(params: Option<&serde_json::Value>) -> serde_json::Value {
    params
        .and_then(|p| p.get("arguments").cloned())
        .unwrap_or(serde_json::json!({}))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::api::MessageType;

    // ── Helper: minimal config with one source ──────────────────────────

    fn test_config(yaml: &str) -> Arc<Config> {
        Arc::new(serde_saphyr::from_str(yaml).expect("test config should parse"))
    }

    fn default_config() -> Arc<Config> {
        test_config(
            r#"
schema: 1
sources:
  - id: test-server
    kind: mcp
    url: http://localhost:3000
governance:
  defaults:
    action: forward
"#,
        )
    }

    fn deny_config() -> Arc<Config> {
        test_config(
            r##"
schema: 1
sources:
  - id: test-server
    kind: mcp
    url: http://localhost:3000
governance:
  defaults:
    action: forward
  rules:
    - match: "dangerous_*"
      action: deny
    - match: "needs_approval_*"
      action: approve
      approval: default
    - match: "cedar_*"
      action: policy
      policy_id: default
approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
    timeout: 5m
    on_timeout: deny
"##,
        )
    }

    fn blocklist_config() -> Arc<Config> {
        test_config(
            r#"
schema: 1
sources:
  - id: test-server
    kind: mcp
    url: http://localhost:3000
    expose:
      mode: blocklist
      tools:
        - "hidden_*"
governance:
  defaults:
    action: forward
"#,
        )
    }

    fn test_principal() -> Principal {
        Principal::new("test-app")
    }

    fn test_evaluator(config: Arc<Config>, profile: Profile) -> GovernanceEvaluator {
        GovernanceEvaluator::new(
            config,
            None, // no Cedar engine for most tests
            Arc::new(TaskStore::with_defaults()),
            test_principal(),
            profile,
        )
    }

    fn make_request(
        method: &str,
        server_id: &str,
        params: Option<serde_json::Value>,
    ) -> GovernanceEvaluateRequest {
        GovernanceEvaluateRequest {
            server_id: server_id.to_string(),
            direction: StreamDirection::AgentToServer,
            method: method.to_string(),
            id: Some(JsonRpcId::Number(1)),
            params,
            message_type: MessageType::Request,
            profile: Profile::Production,
        }
    }

    // ── Fast-path tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_forward_for_notification() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = GovernanceEvaluateRequest {
            server_id: "test-server".to_string(),
            direction: StreamDirection::AgentToServer,
            method: "tools/call".to_string(),
            id: None,
            params: Some(serde_json::json!({"name": "read_file"})),
            message_type: MessageType::Notification,
            profile: Profile::Production,
        };
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    #[tokio::test]
    async fn test_forward_for_response() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = GovernanceEvaluateRequest {
            server_id: "test-server".to_string(),
            direction: StreamDirection::AgentToServer,
            method: "tools/call".to_string(),
            id: Some(JsonRpcId::Number(1)),
            params: None,
            message_type: MessageType::Response,
            profile: Profile::Production,
        };
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    #[tokio::test]
    async fn test_forward_for_server_to_agent() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = GovernanceEvaluateRequest {
            server_id: "test-server".to_string(),
            direction: StreamDirection::ServerToAgent,
            method: "tools/call".to_string(),
            id: Some(JsonRpcId::Number(1)),
            params: Some(serde_json::json!({"name": "read_file"})),
            message_type: MessageType::Request,
            profile: Profile::Production,
        };
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    #[tokio::test]
    async fn test_forward_for_non_governable_method() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = make_request("initialize", "test-server", None);
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    #[tokio::test]
    async fn test_forward_for_ping() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = make_request("ping", "test-server", None);
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    #[tokio::test]
    async fn test_forward_for_tools_list() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = make_request("tools/list", "test-server", None);
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    // ── Gate 1: Visibility tests ────────────────────────────────────────

    #[tokio::test]
    async fn test_deny_hidden_tool_gate1() {
        let eval = test_evaluator(blocklist_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "hidden_admin"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Deny);
        assert!(resp.reason.as_deref().unwrap_or("").contains("not exposed"));
    }

    #[tokio::test]
    async fn test_forward_visible_tool_gate1() {
        let eval = test_evaluator(blocklist_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "read_file"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    #[tokio::test]
    async fn test_deny_unknown_source_gate1() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "nonexistent-server",
            Some(serde_json::json!({"name": "read_file"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Deny);
        assert!(
            resp.reason.as_deref().unwrap_or("").contains("not found"),
            "reason should mention source not found, got: {:?}",
            resp.reason
        );
    }

    // ── Gate 2: Governance rules tests ──────────────────────────────────

    #[tokio::test]
    async fn test_deny_by_governance_rule_gate2() {
        let eval = test_evaluator(deny_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "dangerous_delete"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Deny);
        assert!(
            resp.reason
                .as_deref()
                .unwrap_or("")
                .contains("governance rule")
        );
    }

    #[tokio::test]
    async fn test_forward_by_governance_default_gate2() {
        let eval = test_evaluator(deny_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "safe_read"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    // ── Gate 4: Approval tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_pending_approval_production_gate4() {
        let config = deny_config();
        let task_store = Arc::new(TaskStore::with_defaults());
        let eval = GovernanceEvaluator::new(
            config,
            None,
            task_store.clone(),
            test_principal(),
            Profile::Production,
        );
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "needs_approval_deploy"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::PendingApproval);
        assert!(resp.task_id.is_some());
        assert!(resp.poll_interval_ms.is_some());
    }

    #[tokio::test]
    async fn test_auto_forward_approval_in_dev_gate4() {
        let eval = test_evaluator(deny_config(), Profile::Development);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "needs_approval_deploy"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
        assert!(
            resp.reason
                .as_deref()
                .unwrap_or("")
                .contains("WOULD_REQUIRE_APPROVAL")
        );
    }

    // ── Gate 3: Cedar (no engine) tests ─────────────────────────────────

    #[tokio::test]
    async fn test_cedar_no_engine_dev_forwards() {
        let eval = test_evaluator(deny_config(), Profile::Development);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "cedar_protected"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    #[tokio::test]
    async fn test_cedar_no_engine_prod_denies() {
        let eval = test_evaluator(deny_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "cedar_protected"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Deny);
    }

    // ── Helper function tests ───────────────────────────────────────────

    #[test]
    fn test_method_requires_gates() {
        assert!(method_requires_gates("tools/call"));
        assert!(method_requires_gates("resources/read"));
        assert!(method_requires_gates("resources/subscribe"));
        assert!(method_requires_gates("prompts/get"));
        assert!(!method_requires_gates("tools/list"));
        assert!(!method_requires_gates("resources/list"));
        assert!(!method_requires_gates("initialize"));
        assert!(!method_requires_gates("ping"));
    }

    #[test]
    fn test_extract_resource_name_tools_call() {
        let params = Some(serde_json::json!({"name": "read_file", "arguments": {}}));
        assert_eq!(
            extract_resource_name("tools/call", params.as_ref()),
            Some("read_file".to_string())
        );
    }

    #[test]
    fn test_extract_resource_name_resources_read() {
        let params = Some(serde_json::json!({"uri": "file:///tmp/data.txt"}));
        assert_eq!(
            extract_resource_name("resources/read", params.as_ref()),
            Some("file:///tmp/data.txt".to_string())
        );
    }

    #[test]
    fn test_extract_resource_name_prompts_get() {
        let params = Some(serde_json::json!({"name": "code_review"}));
        assert_eq!(
            extract_resource_name("prompts/get", params.as_ref()),
            Some("code_review".to_string())
        );
    }

    #[test]
    fn test_extract_resource_name_none_for_list() {
        assert_eq!(extract_resource_name("tools/list", None), None);
    }

    #[test]
    fn test_extract_resource_name_none_for_missing_params() {
        assert_eq!(extract_resource_name("tools/call", None), None);
    }

    #[test]
    fn test_extract_arguments() {
        let params = Some(serde_json::json!({"name": "x", "arguments": {"path": "/tmp"}}));
        assert_eq!(
            extract_arguments(params.as_ref()),
            serde_json::json!({"path": "/tmp"})
        );
    }

    #[test]
    fn test_extract_arguments_missing() {
        let params = Some(serde_json::json!({"name": "x"}));
        assert_eq!(extract_arguments(params.as_ref()), serde_json::json!({}));
    }

    // ── EvalTrace + DenySource tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_trace_gate1_block_sets_deny_source_visibility() {
        let eval = test_evaluator(blocklist_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "hidden_admin"})),
        );
        let (resp, trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Deny);
        assert_eq!(resp.deny_source, Some(DenySource::Visibility));
        assert_eq!(trace.gate1.as_deref(), Some("block"));
        assert!(trace.gate2.is_none());
    }

    #[tokio::test]
    async fn test_trace_gate2_deny_sets_deny_source_governance() {
        let eval = test_evaluator(deny_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "dangerous_delete"})),
        );
        let (resp, trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Deny);
        assert_eq!(resp.deny_source, Some(DenySource::GovernanceRule));
        assert_eq!(trace.gate1.as_deref(), Some("pass"));
        assert_eq!(trace.gate2.as_deref(), Some("deny"));
    }

    #[tokio::test]
    async fn test_trace_gate2_forward_has_no_deny_source() {
        let eval = test_evaluator(deny_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "safe_read"})),
        );
        let (resp, trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
        assert!(resp.deny_source.is_none());
        assert_eq!(trace.gate1.as_deref(), Some("pass"));
        assert_eq!(trace.gate2.as_deref(), Some("forward"));
    }

    #[tokio::test]
    async fn test_trace_gate3_no_engine_prod_sets_deny_source_cedar() {
        let eval = test_evaluator(deny_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "cedar_protected"})),
        );
        let (resp, trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Deny);
        assert_eq!(resp.deny_source, Some(DenySource::CedarPolicy));
        assert_eq!(trace.gate2.as_deref(), Some("policy"));
    }

    #[tokio::test]
    async fn test_trace_gate4_approval_started() {
        let config = deny_config();
        let task_store = Arc::new(TaskStore::with_defaults());
        let eval = GovernanceEvaluator::new(
            config,
            None,
            task_store.clone(),
            test_principal(),
            Profile::Production,
        );
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "needs_approval_deploy"})),
        );
        let (resp, trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::PendingApproval);
        assert!(resp.deny_source.is_none());
        assert_eq!(trace.gate1.as_deref(), Some("pass"));
        assert_eq!(trace.gate2.as_deref(), Some("approve"));
        assert_eq!(trace.gate4.as_deref(), Some("started"));
    }

    // ── Metrics recording tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_evaluate_records_gate1_pass_metric() {
        use crate::telemetry::prom_metrics::DecisionLabels;
        use std::borrow::Cow;

        let mut registry = prometheus_client::registry::Registry::default();
        let metrics = Arc::new(ThoughtGateMetrics::new(&mut registry));

        let eval = GovernanceEvaluator::new(
            default_config(),
            None,
            Arc::new(TaskStore::with_defaults()),
            test_principal(),
            Profile::Production,
        )
        .with_metrics(metrics.clone());

        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "safe_read"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);

        // Verify gate1 "pass" counter was incremented
        let count = metrics
            .decisions_total
            .get_or_create(&DecisionLabels {
                gate: Cow::Borrowed("gate1"),
                outcome: Cow::Borrowed("pass"),
            })
            .get();
        assert_eq!(count, 1, "gate1 pass counter should be 1");
    }

    #[tokio::test]
    async fn test_evaluate_records_gate1_block_metric() {
        use crate::telemetry::prom_metrics::DecisionLabels;
        use std::borrow::Cow;

        let mut registry = prometheus_client::registry::Registry::default();
        let metrics = Arc::new(ThoughtGateMetrics::new(&mut registry));

        let eval = GovernanceEvaluator::new(
            blocklist_config(),
            None,
            Arc::new(TaskStore::with_defaults()),
            test_principal(),
            Profile::Production,
        )
        .with_metrics(metrics.clone());

        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "hidden_admin"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Deny);

        // Verify gate1 "block" counter was incremented
        let count = metrics
            .decisions_total
            .get_or_create(&DecisionLabels {
                gate: Cow::Borrowed("gate1"),
                outcome: Cow::Borrowed("block"),
            })
            .get();
        assert_eq!(count, 1, "gate1 block counter should be 1");
    }

    #[tokio::test]
    async fn test_evaluate_records_gate2_deny_metric() {
        use crate::telemetry::prom_metrics::DecisionLabels;
        use std::borrow::Cow;

        let mut registry = prometheus_client::registry::Registry::default();
        let metrics = Arc::new(ThoughtGateMetrics::new(&mut registry));

        let eval = GovernanceEvaluator::new(
            deny_config(),
            None,
            Arc::new(TaskStore::with_defaults()),
            test_principal(),
            Profile::Production,
        )
        .with_metrics(metrics.clone());

        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "dangerous_delete"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Deny);

        // Gate 1 should pass, Gate 2 should deny
        let gate1 = metrics
            .decisions_total
            .get_or_create(&DecisionLabels {
                gate: Cow::Borrowed("gate1"),
                outcome: Cow::Borrowed("pass"),
            })
            .get();
        assert_eq!(gate1, 1, "gate1 should have recorded pass");

        let gate2 = metrics
            .decisions_total
            .get_or_create(&DecisionLabels {
                gate: Cow::Borrowed("gate2"),
                outcome: Cow::Borrowed("deny"),
            })
            .get();
        assert_eq!(gate2, 1, "gate2 should have recorded deny");
    }

    #[tokio::test]
    async fn test_evaluate_records_gate4_metric() {
        use crate::telemetry::prom_metrics::DecisionLabels;
        use std::borrow::Cow;

        let mut registry = prometheus_client::registry::Registry::default();
        let metrics = Arc::new(ThoughtGateMetrics::new(&mut registry));

        let eval = GovernanceEvaluator::new(
            deny_config(),
            None,
            Arc::new(TaskStore::with_defaults()),
            test_principal(),
            Profile::Production,
        )
        .with_metrics(metrics.clone());

        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "needs_approval_deploy"})),
        );
        let (resp, _trace) = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::PendingApproval);

        let gate4 = metrics
            .decisions_total
            .get_or_create(&DecisionLabels {
                gate: Cow::Borrowed("gate4"),
                outcome: Cow::Borrowed("started"),
            })
            .get();
        assert_eq!(gate4, 1, "gate4 should have recorded started");
    }

    // ── Per-request principal tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_evaluate_with_override_principal() {
        let config = deny_config();
        let task_store = Arc::new(TaskStore::with_defaults());
        let eval = GovernanceEvaluator::new(
            config,
            None,
            task_store.clone(),
            test_principal(), // default: "test-app"
            Profile::Production,
        );

        // Override principal
        let override_principal = Principal::new("k8s-service-account");

        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "needs_approval_deploy"})),
        );
        let (resp, trace) = eval
            .evaluate_with_principal(&req, Some(&override_principal))
            .await;

        // Should still get PendingApproval (principal doesn't change the gate routing)
        assert_eq!(resp.decision, GovernanceDecision::PendingApproval);
        assert_eq!(trace.gate4.as_deref(), Some("started"));

        // Verify the task was created with the override principal
        let task_id_str = resp.task_id.as_deref().expect("should have task_id");
        let task_id: crate::governance::TaskId = task_id_str.parse().unwrap();
        let task = task_store.get(&task_id).expect("task should exist");
        assert_eq!(
            task.principal.app_name.as_str(),
            "k8s-service-account",
            "task should use override principal, not default"
        );
    }

    #[tokio::test]
    async fn test_evaluate_with_principal_none_uses_default() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = make_request(
            "tools/call",
            "test-server",
            Some(serde_json::json!({"name": "safe_read"})),
        );

        // None principal should behave identically to evaluate()
        let (resp1, _) = eval.evaluate(&req).await;
        let (resp2, _) = eval.evaluate_with_principal(&req, None).await;
        assert_eq!(resp1.decision, resp2.decision);
    }
}
