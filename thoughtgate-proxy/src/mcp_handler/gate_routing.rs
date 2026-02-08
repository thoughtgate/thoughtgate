//! Gate routing: 4-gate model request routing and approval workflow initiation.
//!
//! Implements: REQ-CFG-001 Section 9 (Request Processing)
//! Implements: REQ-GOV-002/F-001, F-002 (Task creation and approval posting)

use thoughtgate_core::StreamDirection;
use thoughtgate_core::config::MatchResult;
use thoughtgate_core::error::ThoughtGateError;
use thoughtgate_core::governance::api::{GovernanceEvaluateRequest, MessageType};
use thoughtgate_core::governance::evaluator::DenySource;
use thoughtgate_core::governance::evaluator::{extract_arguments, extract_resource_name};
use thoughtgate_core::governance::{GovernanceDecision, Principal, ToolCallRequest};
use thoughtgate_core::policy::principal::{infer_principal_optional, infer_principal_or_error};
use thoughtgate_core::profile::Profile;
use thoughtgate_core::telemetry::{
    GateOutcomes, GatewayDecisionSpanData, finish_gateway_decision_span,
    start_gateway_decision_span,
};
use thoughtgate_core::transport::jsonrpc::{JsonRpcId, JsonRpcResponse, McpRequest};
use tracing::{debug, info};

use super::McpState;
use super::cedar_eval::get_source_id;

/// Result of routing a request through the 4-gate model.
///
/// Most paths resolve immediately. Blocking approval mode returns
/// `BlockingApproval` with the parameters needed to wait-and-execute
/// in a background task with HTTP keepalive.
pub(crate) enum GateResult {
    /// Immediate result (forward, deny, async approval, or completed blocking).
    Immediate(Result<JsonRpcResponse, ThoughtGateError>),
    /// Blocking approval: the caller should spawn a keepalive-streaming task.
    BlockingApproval {
        approval_engine: std::sync::Arc<thoughtgate_core::governance::ApprovalEngine>,
        task_id_parsed: thoughtgate_core::governance::TaskId,
        task_id_str: String,
        resource_name: String,
        request_id: Option<thoughtgate_core::transport::jsonrpc::JsonRpcId>,
        blocking_timeout: std::time::Duration,
        poll_interval: std::time::Duration,
        tg_metrics: Option<std::sync::Arc<thoughtgate_core::telemetry::ThoughtGateMetrics>>,
    },
}

/// Route a tools/call request through the 4-gate model.
///
/// Implements: REQ-CFG-001 Section 9 (Request Processing)
///
/// # Gate Flow
///
/// ```text
/// tools/call, resources/read, prompts/get
///     │
/// ┌─ Gate 1: Visibility ─────────────────────────┐
/// │  expose.is_visible(resource_name)?           │
/// │  Not visible → Error -32015                  │
/// └──────────────────────────────────────────────┘
///     │
/// ┌─ Gate 2: Governance Rules ───────────────────┐
/// │  governance.evaluate(resource_name, source)  │
/// │  Returns: MatchResult { action, policy_id }  │
/// └──────────────────────────────────────────────┘
///     │ (route by action)
/// ┌────────────────┬────────────────┬────────────────┬────────────────┐
/// │ Forward        │ Deny           │ Approve        │ Policy         │
/// │ → Upstream     │ → Error -32014 │ → Gate 4       │ → Gate 3       │
/// └────────────────┴────────────────┴────────────────┴────────────────┘
/// ```
pub(crate) async fn route_through_gates(state: &McpState, mut request: McpRequest) -> GateResult {
    let evaluator = match state.evaluator.as_ref() {
        Some(e) => e,
        None => {
            return GateResult::Immediate(Err(ThoughtGateError::ServiceUnavailable {
                reason: "Evaluator not configured".to_string(),
            }));
        }
    };

    let config = match state.config.as_ref() {
        Some(c) => c,
        None => {
            return GateResult::Immediate(Err(ThoughtGateError::ServiceUnavailable {
                reason: "Configuration not loaded".to_string(),
            }));
        }
    };

    // Extract resource name for SEP-1686 validation and logging
    let resource_name = match extract_resource_name(&request.method, request.params.as_deref()) {
        Some(name) => name,
        None => {
            let field = match request.method.as_str() {
                "tools/call" | "prompts/get" => "name",
                "resources/read" | "resources/subscribe" => "uri",
                _ => "identifier",
            };
            return GateResult::Immediate(Err(ThoughtGateError::InvalidParams {
                details: format!(
                    "Missing required field '{}' in {} params",
                    field, request.method
                ),
            }));
        }
    };
    let source_id = get_source_id(state);

    debug!(
        resource = %resource_name,
        method = %request.method,
        source = %source_id,
        "Routing through 4-gate model (via evaluator)"
    );

    // ========================================================================
    // SEP-1686: Task Metadata Validation (proxy-specific)
    // ========================================================================
    let match_result = config.governance.evaluate(&resource_name, source_id);
    let is_blocking_mode = match super::validate_task_metadata(
        &mut request,
        &match_result.action,
        &resource_name,
        state.capability_cache.upstream_supports_tasks(),
        state.approval_engine.is_some(),
    ) {
        Ok(b) => b,
        Err(e) => return GateResult::Immediate(Err(e)),
    };

    // ========================================================================
    // Start Gateway Decision Span (REQ-OBS-002 §5.3)
    // ========================================================================
    let correlation_id_str = request.correlation_id.to_string();
    let decision_span_data = GatewayDecisionSpanData {
        request_id: &correlation_id_str,
        upstream_target: "upstream",
    };
    let mut decision_span =
        start_gateway_decision_span(&decision_span_data, &opentelemetry::Context::current());

    // ========================================================================
    // Infer per-request principal (K8s service account detection)
    // ========================================================================
    let principal = infer_principal_optional().await.map(|p| {
        Principal::from_policy(
            &p.app_name,
            &p.namespace,
            &p.service_account,
            p.roles.clone(),
        )
    });

    // ========================================================================
    // Build GovernanceEvaluateRequest and delegate to evaluator
    // ========================================================================
    let gov_req = GovernanceEvaluateRequest {
        server_id: source_id.to_string(),
        direction: StreamDirection::AgentToServer,
        method: request.method.clone(),
        id: request.id.clone(),
        params: request.params.as_deref().cloned(),
        message_type: MessageType::Request,
        profile: Profile::Production,
    };

    let (resp, trace) = evaluator
        .evaluate_with_principal(&gov_req, principal.as_ref())
        .await;

    // ========================================================================
    // Map EvalTrace → GateOutcomes for the gateway decision span
    // ========================================================================
    let gate_outcomes = GateOutcomes {
        visibility: trace.gate1,
        governance: trace.gate2,
        cedar: trace.gate3,
        approval: trace.gate4,
        governance_rule_id: trace.gate2_rule_id,
        policy_evaluated: trace.gate3_policy_id.is_some(),
    };

    // ========================================================================
    // Map GovernanceDecision to proxy actions
    // ========================================================================
    let result = match resp.decision {
        GovernanceDecision::Forward => {
            let upstream_start = std::time::Instant::now();
            let result = state.upstream.forward(&request).await;
            let upstream_latency_ms = Some(upstream_start.elapsed().as_secs_f64() * 1000.0);
            finish_gateway_decision_span(&mut decision_span, &gate_outcomes, upstream_latency_ms);
            return GateResult::Immediate(result);
        }
        GovernanceDecision::Deny => Err(match resp.deny_source {
            Some(DenySource::Visibility) => ThoughtGateError::ToolNotExposed {
                tool: resource_name,
                source_id: source_id.to_string(),
            },
            Some(DenySource::GovernanceRule) => ThoughtGateError::GovernanceRuleDenied {
                tool: resource_name,
                rule: resp.reason.clone(),
            },
            Some(DenySource::CedarPolicy) => ThoughtGateError::PolicyDenied {
                tool: resource_name,
                policy_id: resp.policy_id.clone(),
                reason: resp.reason.clone(),
            },
            None => ThoughtGateError::PolicyDenied {
                tool: resource_name,
                policy_id: resp.policy_id.clone(),
                reason: resp.reason.clone(),
            },
        }),
        GovernanceDecision::PendingApproval => {
            let task_id = resp.task_id.clone().unwrap_or_default();
            let poll_interval_ms = resp.poll_interval_ms.unwrap_or(1000);

            if is_blocking_mode {
                // Blocking mode: return BlockingApproval so the caller can spawn
                // a keepalive-streaming task instead of holding the connection idle.
                let approval_engine = match state.approval_engine.as_ref() {
                    Some(engine) => engine,
                    None => {
                        return GateResult::Immediate(Err(ThoughtGateError::ServiceUnavailable {
                            reason: "Approval engine not available for blocking mode".to_string(),
                        }));
                    }
                };

                // Resolve blocking timeout: workflow config → handler config → 300s default
                let blocking_timeout = match_result
                    .approval_workflow
                    .as_ref()
                    .and_then(|name| state.config.as_ref().and_then(|c| c.get_workflow(name)))
                    .map(|w| w.blocking_timeout_or_default())
                    .unwrap_or_else(|| {
                        std::time::Duration::from_secs(state.blocking_approval_timeout_secs)
                    });

                let task_id_parsed = thoughtgate_core::governance::TaskId::from_raw(&task_id);

                info!(
                    task_id = %task_id,
                    tool = %resource_name,
                    timeout_secs = blocking_timeout.as_secs(),
                    "Blocking approval mode: returning streaming response with keepalive"
                );

                finish_gateway_decision_span(&mut decision_span, &gate_outcomes, None);
                return GateResult::BlockingApproval {
                    approval_engine: approval_engine.clone(),
                    task_id_parsed,
                    task_id_str: task_id,
                    resource_name,
                    request_id: request.id.clone(),
                    blocking_timeout,
                    poll_interval: std::time::Duration::from_millis(poll_interval_ms),
                    tg_metrics: state.tg_metrics.clone(),
                };
            } else {
                // Async SEP-1686 mode: return task ID immediately
                Ok(JsonRpcResponse::task_created(
                    request.id.clone(),
                    task_id,
                    "working".to_string(),
                    std::time::Duration::from_millis(poll_interval_ms),
                ))
            }
        }
    };

    finish_gateway_decision_span(&mut decision_span, &gate_outcomes, None);
    GateResult::Immediate(result)
}

/// Start an approval workflow (Gate 4).
///
/// Implements: REQ-GOV-002/F-001, F-002 (Task creation and approval posting)
///
/// Creates a task, posts the approval request, and returns a task ID response.
/// The agent will poll for the result using tasks/get and tasks/result.
pub(crate) async fn start_approval_flow(
    state: &McpState,
    request: McpRequest,
    tool_name: &str,
    match_result: &MatchResult,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    let approval_engine =
        state
            .approval_engine
            .as_ref()
            .ok_or_else(|| ThoughtGateError::ServiceUnavailable {
                reason: "Approval engine not configured".to_string(),
            })?;

    // Infer principal from environment (uses spawn_blocking for file I/O)
    let policy_principal = infer_principal_or_error().await?;

    // Create ToolCallRequest for the approval engine
    // Transport and governance now share the same JsonRpcId type
    let mcp_request_id = request.id.clone().unwrap_or(JsonRpcId::Null);

    let tool_request = ToolCallRequest {
        method: request.method.clone(),
        name: tool_name.to_string(),
        arguments: extract_arguments(request.params.as_deref()),
        mcp_request_id,
    };

    // Create Principal for governance (preserve full K8s identity for re-evaluation)
    let principal = Principal::from_policy(
        &policy_principal.app_name,
        &policy_principal.namespace,
        &policy_principal.service_account,
        policy_principal.roles.clone(),
    );

    // Look up workflow-specific timeout from config
    let workflow_timeout = match_result
        .approval_workflow
        .as_ref()
        .and_then(|workflow_name| {
            state
                .config
                .as_ref()
                .and_then(|c| c.get_workflow(workflow_name))
                .map(|w| w.timeout_or_default())
        });

    // Resolve workflow-specific on_timeout action
    let workflow_on_timeout = match_result
        .approval_workflow
        .as_ref()
        .and_then(|workflow_name| {
            state
                .config
                .as_ref()
                .and_then(|c| c.get_workflow(workflow_name))
                .map(|w| w.on_timeout_or_default())
        });

    // Extract redact_fields from workflow config (H-001 mitigation)
    let redact_fields = match_result
        .approval_workflow
        .as_ref()
        .and_then(|workflow_name| {
            state
                .config
                .as_ref()
                .and_then(|c| c.get_workflow(workflow_name))
                .and_then(|w| w.redact_fields.clone())
        })
        .unwrap_or_default();

    // Start the approval workflow with workflow-specific timeout and on_timeout
    let result = approval_engine
        .start_approval(
            tool_request,
            principal,
            workflow_timeout,
            workflow_on_timeout,
            redact_fields,
        )
        .await
        .map_err(|e| ThoughtGateError::ServiceUnavailable {
            reason: format!("Failed to start approval: {}", e),
        })?;

    // Record gate 4 started metric (REQ-OBS-002 §6.1/MC-002)
    // Also record approval request pending (REQ-OBS-002 §6.1/MC-005)
    if let Some(ref metrics) = state.tg_metrics {
        metrics.record_gate_decision("gate4", "started");
        metrics.record_approval_request("slack", "pending");
    }

    info!(
        task_id = %result.task_id,
        tool = %tool_name,
        workflow = ?match_result.approval_workflow,
        timeout_secs = ?workflow_timeout.map(|d| d.as_secs()),
        "Gate 4: Approval workflow started"
    );

    // Return SEP-1686 task response
    Ok(JsonRpcResponse::task_created(
        request.id.clone(),
        result.task_id.to_string(),
        result.status.to_string(),
        result.poll_interval,
    ))
}
