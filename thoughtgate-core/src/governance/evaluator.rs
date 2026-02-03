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
use crate::config::{Action, Config};
use crate::governance::api::{
    GovernanceDecision, GovernanceEvaluateRequest, GovernanceEvaluateResponse, MessageType,
};
use crate::governance::approval::{ApprovalRequest, PollingScheduler};
use crate::governance::engine::TimeoutAction;
use crate::governance::task::{Principal, TaskStatus, TaskStore, ToolCallRequest};
use crate::jsonrpc::JsonRpcId;
use crate::policy::engine::CedarEngine;
use crate::policy::types::{CedarContext, CedarDecision, CedarResource, TimeContext};
use crate::profile::Profile;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Default poll interval hint returned to shims (milliseconds).
const DEFAULT_POLL_INTERVAL_MS: u64 = 5000;

/// Default task TTL for approval workflows.
const DEFAULT_APPROVAL_TTL_SECS: u64 = 300;

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
    scheduler: OnceLock<Arc<PollingScheduler>>,
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
        }
    }

    /// Set the polling scheduler (after construction).
    ///
    /// The scheduler is set after construction because it may depend on
    /// whether approval workflows are needed (determined after config load).
    pub fn set_scheduler(&self, scheduler: Arc<PollingScheduler>) {
        let _ = self.scheduler.set(scheduler);
    }

    /// Evaluate a governance request through Gates 1–4.
    ///
    /// Returns a [`GovernanceEvaluateResponse`] with the governance decision.
    /// The `shutdown` field is NOT set here — the caller (service.rs) sets it.
    ///
    /// Implements: REQ-CORE-008/F-016
    pub async fn evaluate(&self, req: &GovernanceEvaluateRequest) -> GovernanceEvaluateResponse {
        // ── Fast-path: only evaluate agent→server requests ───────────────
        if req.message_type != MessageType::Request
            || req.direction != StreamDirection::AgentToServer
        {
            return self.forward(None);
        }

        // ── Fast-path: skip non-governable methods ──────────────────────
        if !method_requires_gates(&req.method) {
            return self.forward(None);
        }

        // ── Extract resource name from params ───────────────────────────
        let resource_name = match extract_resource_name(&req.method, &req.params) {
            Some(name) => name,
            None => {
                // Governable method without a resource identifier — forward.
                // This handles list methods (tools/list, etc.) that don't
                // reference a specific tool/resource.
                return self.forward(None);
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
                return self.deny(
                    Some(format!(
                        "Resource '{}' not exposed on source '{}'",
                        resource_name, req.server_id
                    )),
                    None,
                );
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
            return self.deny(
                Some(format!(
                    "Source '{}' not found in configuration",
                    req.server_id
                )),
                None,
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

        match match_result.action {
            Action::Forward => {
                debug!(resource = %resource_name, "Gate 2: forwarding directly");
                self.forward(None)
            }
            Action::Deny => {
                info!(resource = %resource_name, "Gate 2: denied by governance rule");
                self.deny(
                    Some(format!("Denied by governance rule for '{}'", resource_name)),
                    match_result.policy_id.as_deref(),
                )
            }
            Action::Policy => {
                // Gate 3: Cedar policy evaluation
                debug!(resource = %resource_name, "Gate 2 → Gate 3: evaluating Cedar policy");
                self.evaluate_cedar(
                    req,
                    &resource_name,
                    &match_result.policy_id,
                    &match_result.approval_workflow,
                )
                .await
            }
            Action::Approve => {
                // Gate 4: Approval workflow
                debug!(resource = %resource_name, "Gate 2 → Gate 4: starting approval workflow");
                self.start_approval(req, &resource_name, match_result.policy_id.as_deref())
                    .await
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
                return self.deny(Some("Cedar engine not configured".to_string()), None);
            }
        };

        let pid = policy_id.clone().unwrap_or_else(|| "default".to_string());

        let cedar_resource = if req.method == "tools/call" {
            CedarResource::ToolCall {
                name: resource_name.to_string(),
                server: req.server_id.clone(),
                arguments: extract_arguments(&req.params),
            }
        } else {
            CedarResource::McpMethod {
                method: req.method.clone(),
                server: req.server_id.clone(),
            }
        };

        let cedar_request = crate::policy::types::CedarRequest {
            principal: crate::policy::Principal {
                app_name: self.principal.app_name.clone(),
                namespace: self.principal.namespace.clone(),
                service_account: self.principal.service_account.clone(),
                roles: self.principal.roles.clone(),
            },
            resource: cedar_resource,
            context: CedarContext {
                policy_id: pid.clone(),
                source_id: req.server_id.clone(),
                time: TimeContext::now(),
            },
        };

        match engine.evaluate_v2(&cedar_request) {
            CedarDecision::Permit { .. } => {
                // Cedar permit → Gate 4 (approval workflow)
                debug!(
                    resource = %resource_name,
                    policy_id = %pid,
                    "Gate 3: Cedar permit → Gate 4 approval"
                );
                self.start_approval(req, resource_name, Some(&pid)).await
            }
            CedarDecision::Forbid { reason, .. } => {
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
                self.deny(Some(reason), Some(&pid))
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Gate 4: Approval Workflow
    // ─────────────────────────────────────────────────────────────────────

    /// Start an approval workflow (Gate 4).
    ///
    /// In development mode, logs the would-be approval and returns Forward.
    /// In production, creates a task, transitions to InputRequired, submits
    /// to the polling scheduler, and returns PendingApproval.
    ///
    /// Implements: REQ-CORE-008/F-016, REQ-GOV-001
    async fn start_approval(
        &self,
        req: &GovernanceEvaluateRequest,
        resource_name: &str,
        policy_id: Option<&str>,
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

        // ── Production: create task + submit to scheduler ───────────────
        let mcp_request_id = req.id.clone().unwrap_or(JsonRpcId::Null);

        let tool_request = ToolCallRequest {
            method: req.method.clone(),
            name: resource_name.to_string(),
            arguments: extract_arguments(&req.params),
            mcp_request_id,
        };

        // Create task in TaskStore.
        let task = match self.task_store.create(
            tool_request.clone(),
            tool_request,
            self.principal.clone(),
            Some(Duration::from_secs(DEFAULT_APPROVAL_TTL_SECS)),
            TimeoutAction::Deny,
        ) {
            Ok(task) => task,
            Err(e) => {
                warn!(error = %e, "Gate 4: failed to create approval task — denying");
                return self.deny(
                    Some(format!("Failed to create approval task: {e}")),
                    policy_id,
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
            return self.deny(Some(format!("Task transition failed: {e}")), policy_id);
        }

        // Submit to scheduler (if available) for Slack posting + polling.
        if let Some(scheduler) = self.scheduler.get() {
            let approval_req = ApprovalRequest {
                task_id: task_id.clone(),
                tool_name: resource_name.to_string(),
                tool_arguments: extract_arguments(&req.params),
                principal: self.principal.clone(),
                expires_at: task.expires_at,
                created_at: Utc::now(),
                correlation_id: task_id.to_string(),
                request_span_context: None, // TODO: Wire span context from request handler
            };

            if let Err(e) = scheduler.submit(approval_req).await {
                warn!(
                    task_id = %task_id,
                    error = %e,
                    "Gate 4: failed to submit to scheduler — denying"
                );
                // Best-effort: cancel the task we just created.
                let _ = self.task_store.transition(
                    &task_id,
                    TaskStatus::Failed,
                    Some(format!("Scheduler submission failed: {e}")),
                );
                return self.deny(Some(format!("Approval submission failed: {e}")), policy_id);
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

        GovernanceEvaluateResponse {
            decision: GovernanceDecision::PendingApproval,
            task_id: Some(task_id.to_string()),
            policy_id: policy_id.map(String::from),
            reason: None,
            poll_interval_ms: Some(DEFAULT_POLL_INTERVAL_MS),
            shutdown: false,
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
            shutdown: false,
        }
    }

    fn deny(&self, reason: Option<String>, policy_id: Option<&str>) -> GovernanceEvaluateResponse {
        GovernanceEvaluateResponse {
            decision: GovernanceDecision::Deny,
            task_id: None,
            policy_id: policy_id.map(String::from),
            reason,
            poll_interval_ms: None,
            shutdown: false,
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
fn method_requires_gates(method: &str) -> bool {
    matches!(
        method,
        "tools/call" | "resources/read" | "resources/subscribe" | "prompts/get"
    )
}

/// Extract the governable resource name from JSON-RPC params.
///
/// Ported from `mcp_handler.rs::extract_governable_name`, adapted to
/// operate on `&Option<Value>` instead of `&McpRequest`.
///
/// Implements: REQ-CORE-008/F-016
fn extract_resource_name(method: &str, params: &Option<serde_json::Value>) -> Option<String> {
    let params = params.as_ref()?;

    match method {
        "tools/call" => params.get("name")?.as_str().map(String::from),
        "resources/read" | "resources/subscribe" => params.get("uri")?.as_str().map(String::from),
        "prompts/get" => params.get("name")?.as_str().map(String::from),
        _ => None,
    }
}

/// Extract tool arguments from JSON-RPC params.
///
/// Ported from `mcp_handler.rs::extract_tool_arguments`.
fn extract_arguments(params: &Option<serde_json::Value>) -> serde_json::Value {
    params
        .as_ref()
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
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    #[tokio::test]
    async fn test_forward_for_non_governable_method() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = make_request("initialize", "test-server", None);
        let resp = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    #[tokio::test]
    async fn test_forward_for_ping() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = make_request("ping", "test-server", None);
        let resp = eval.evaluate(&req).await;
        assert_eq!(resp.decision, GovernanceDecision::Forward);
    }

    #[tokio::test]
    async fn test_forward_for_tools_list() {
        let eval = test_evaluator(default_config(), Profile::Production);
        let req = make_request("tools/list", "test-server", None);
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
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
        let resp = eval.evaluate(&req).await;
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
            extract_resource_name("tools/call", &params),
            Some("read_file".to_string())
        );
    }

    #[test]
    fn test_extract_resource_name_resources_read() {
        let params = Some(serde_json::json!({"uri": "file:///tmp/data.txt"}));
        assert_eq!(
            extract_resource_name("resources/read", &params),
            Some("file:///tmp/data.txt".to_string())
        );
    }

    #[test]
    fn test_extract_resource_name_prompts_get() {
        let params = Some(serde_json::json!({"name": "code_review"}));
        assert_eq!(
            extract_resource_name("prompts/get", &params),
            Some("code_review".to_string())
        );
    }

    #[test]
    fn test_extract_resource_name_none_for_list() {
        assert_eq!(extract_resource_name("tools/list", &None), None);
    }

    #[test]
    fn test_extract_resource_name_none_for_missing_params() {
        assert_eq!(extract_resource_name("tools/call", &None), None);
    }

    #[test]
    fn test_extract_arguments() {
        let params = Some(serde_json::json!({"name": "x", "arguments": {"path": "/tmp"}}));
        assert_eq!(
            extract_arguments(&params),
            serde_json::json!({"path": "/tmp"})
        );
    }

    #[test]
    fn test_extract_arguments_missing() {
        let params = Some(serde_json::json!({"name": "x"}));
        assert_eq!(extract_arguments(&params), serde_json::json!({}));
    }
}
