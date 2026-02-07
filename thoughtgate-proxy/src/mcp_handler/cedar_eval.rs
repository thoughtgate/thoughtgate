//! Cedar policy evaluation (Gate 3).
//!
//! Implements: REQ-POL-001/F-001 (Policy Evaluation)

use thoughtgate_core::config::MatchResult;
use thoughtgate_core::error::ThoughtGateError;
use thoughtgate_core::governance::evaluator::{
    extract_arguments, extract_resource_name, method_requires_gates,
};
use thoughtgate_core::policy::principal::infer_principal_or_error;
use thoughtgate_core::policy::{
    CedarContext, CedarDecision, CedarRequest, CedarResource, TimeContext,
};
use thoughtgate_core::telemetry::{
    CedarSpanData, GateOutcomes, finish_cedar_span, start_cedar_span,
};
use thoughtgate_core::transport::jsonrpc::{JsonRpcResponse, McpRequest};
use tracing::{debug, warn};

use super::McpState;

/// Get source ID for the request.
///
/// v0.2: Single upstream, hardcoded to "upstream".
pub(crate) fn get_source_id(_state: &McpState) -> &'static str {
    "upstream"
}

/// Evaluate a request against Cedar policy engine (Gate 3).
///
/// Implements: REQ-POL-001/F-001 (Policy Evaluation)
///
/// Evaluates Cedar policy for governable MCP methods:
/// - `tools/call` → uses CedarResource::ToolCall with name and arguments
/// - `resources/read`, `resources/subscribe`, `prompts/get` → uses CedarResource::McpMethod
///
/// List methods (`tools/list`, `resources/list`, `prompts/list`) are forwarded to
/// upstream without Cedar evaluation since they don't reference a specific resource.
///
/// Cedar decisions:
/// - Permit → forward to upstream (or to Gate 4 if approval workflow specified)
/// - Forbid → return PolicyDenied error
pub(crate) async fn evaluate_with_cedar(
    state: &McpState,
    request: McpRequest,
    match_result: Option<&MatchResult>,
    gate_outcomes: Option<&mut GateOutcomes>,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    // List methods bypass Cedar - they don't reference a specific resource
    if !method_requires_gates(request.method.as_str()) {
        debug!(
            method = %request.method,
            "Gate 3: Bypassing Cedar for list method - forwarding to upstream"
        );
        return state.upstream.forward(&request).await;
    }

    // Extract resource name for all governable methods
    let resource_name = match extract_resource_name(&request.method, request.params.as_deref()) {
        Some(name) => name,
        None => {
            // Governable method without required identifier is invalid params
            let field = match request.method.as_str() {
                "tools/call" | "prompts/get" => "name",
                "resources/read" | "resources/subscribe" => "uri",
                _ => "identifier",
            };
            return Err(ThoughtGateError::InvalidParams {
                details: format!(
                    "Missing required field '{}' in {} params",
                    field, request.method
                ),
            });
        }
    };

    // Infer principal from environment (uses spawn_blocking for file I/O)
    let policy_principal = infer_principal_or_error().await?;

    // Get policy_id and source_id from Gate 2 result, or use defaults
    let policy_id = match_result
        .and_then(|m| m.policy_id.clone())
        .unwrap_or_else(|| "default".to_string());
    let source_id = get_source_id(state).to_string();

    // Build Cedar resource based on method type
    let cedar_resource = if request.method == "tools/call" {
        // tools/call includes arguments for fine-grained policy checks
        CedarResource::ToolCall {
            name: resource_name.clone(),
            server: source_id.clone(),
            arguments: extract_arguments(request.params.as_deref()),
        }
    } else {
        // resources/read, resources/subscribe, prompts/get use McpMethod
        CedarResource::McpMethod {
            method: request.method.clone(),
            server: source_id.clone(),
        }
    };

    // Build Cedar request
    let cedar_request = CedarRequest {
        principal: policy_principal,
        resource: cedar_resource,
        context: CedarContext {
            policy_id: policy_id.clone(),
            source_id,
            time: TimeContext::now(),
        },
    };

    // ========================================================================
    // Cedar Policy Evaluation with Telemetry (REQ-OBS-002 §5.3, §6.1/MC-004)
    // ========================================================================

    // Derive Cedar entity metadata for span attributes
    let (cedar_action, cedar_resource_type) = match &cedar_request.resource {
        CedarResource::ToolCall { .. } => ("tools/call", "ThoughtGate::ToolCall"),
        CedarResource::McpMethod { .. } => ("mcp/method", "ThoughtGate::McpMethod"),
    };

    // Create Cedar span data
    let cedar_span_data = CedarSpanData {
        tool_name: resource_name.clone(),
        policy_id: Some(policy_id.clone()),
        principal_type: "ThoughtGate::App".to_string(),
        principal_id: cedar_request.principal.app_name.clone(),
        action: cedar_action.to_string(),
        resource_type: cedar_resource_type.to_string(),
        resource_id: cedar_request.resource.name().to_string(),
    };

    // Start Cedar span as child of current context
    let mut cedar_span = start_cedar_span(&cedar_span_data, &opentelemetry::Context::current());

    // Time the evaluation
    let eval_start = std::time::Instant::now();
    let cedar_result = state.cedar_engine.evaluate_v2(&cedar_request);
    let eval_duration_ms = eval_start.elapsed().as_secs_f64() * 1000.0;

    // Determine decision string and denial reason for telemetry
    let (decision_str, denial_reason) = match &cedar_result {
        CedarDecision::Permit { .. } => ("allow", None),
        CedarDecision::Forbid { reason, .. } => ("deny", Some(reason.as_str())),
    };

    // Finish Cedar span with attributes (records cedar.denial event on deny)
    finish_cedar_span(
        &mut cedar_span,
        decision_str,
        &policy_id,
        eval_duration_ms,
        denial_reason,
    );

    // Record Cedar metrics (REQ-OBS-002 §6.1/MC-004, §6.2/MH-002)
    if let Some(ref metrics) = state.tg_metrics {
        metrics.record_cedar_eval(decision_str, &policy_id, eval_duration_ms);
        // Record gate 3 decision
        metrics.record_gate_decision("gate3", decision_str);
    }

    // Populate gate outcome for gateway decision span (REQ-OBS-002 §5.3)
    if let Some(outcomes) = gate_outcomes {
        outcomes.cedar = Some(decision_str.to_string());
    }

    // Process the Cedar decision
    match cedar_result {
        CedarDecision::Permit { .. } => {
            // For action: policy, Cedar permit → Gate 4 (approval workflow)
            // Per REQ-CORE-003 Section 8: policy action always goes to Gate 4 after permit
            if let Some(match_result) = match_result {
                // Use specified workflow or default to "default"
                let workflow = match_result
                    .approval_workflow
                    .clone()
                    .unwrap_or_else(|| "default".to_string());
                debug!(
                    resource = %resource_name,
                    method = %request.method,
                    workflow = %workflow,
                    policy_id = %policy_id,
                    "Gate 3: Cedar permit → Gate 4 approval"
                );
                return super::gate_routing::start_approval_flow(
                    state,
                    request,
                    &resource_name,
                    match_result,
                )
                .await;
            }

            // TODO(v0.5): Deprecate config-less mode; route all evaluation through GovernanceEvaluator
            // Legacy mode (no Gate 2 result): Cedar permit → forward to upstream
            debug!(
                resource = %resource_name,
                method = %request.method,
                policy_id = %policy_id,
                "Gate 3: Cedar permit (legacy mode) - forwarding to upstream"
            );
            state.upstream.forward(&request).await
        }
        CedarDecision::Forbid { reason, .. } => {
            // Cedar forbid → return PolicyDenied error
            warn!(
                resource = %resource_name,
                method = %request.method,
                policy_id = %policy_id,
                reason = %reason,
                "Gate 3: Cedar forbid - denying request"
            );
            Err(ThoughtGateError::PolicyDenied {
                tool: resource_name,
                policy_id: Some(policy_id),
                reason: Some(reason),
            })
        }
    }
}
