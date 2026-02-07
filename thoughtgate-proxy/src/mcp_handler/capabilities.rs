//! Initialize and list method handlers with capability injection.
//!
//! Implements: REQ-CORE-007/F-001 (Capability Injection)
//! Implements: SEP-1686 Section 3.2 (Tool Advertisement)

use thoughtgate_core::error::ThoughtGateError;
use thoughtgate_core::protocol::{
    extract_upstream_sse_support, extract_upstream_task_support, inject_task_capability,
    strip_sse_capability,
};
use thoughtgate_core::transport::jsonrpc::{JsonRpcResponse, McpRequest};
use tracing::{debug, info, warn};

use super::McpState;

/// Handle the `initialize` method with capability injection.
///
/// Implements: REQ-CORE-007/F-001 (Capability Injection)
///
/// This function:
/// 1. Forwards the `initialize` request to upstream
/// 2. Extracts and caches upstream capability detection
/// 3. Injects ThoughtGate's task capability (always)
/// 4. Conditionally advertises SSE (only if upstream supports it)
///
/// # Arguments
///
/// * `state` - MCP handler state with capability cache and upstream client
/// * `request` - The initialize request
///
/// # Returns
///
/// Modified initialize response with injected capabilities.
pub(super) async fn handle_initialize_method(
    state: &McpState,
    request: McpRequest,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    // Forward to upstream to get the raw initialize response
    let response = state.upstream.forward(&request).await?;

    // If error response, return as-is (don't inject capabilities on error)
    if response.error.is_some() {
        return Ok(response);
    }

    // Extract result, return as-is if missing
    let result = match &response.result {
        Some(r) => r.clone(),
        None => return Ok(response),
    };

    // Extract and cache upstream capabilities
    let upstream_tasks = extract_upstream_task_support(&result);
    let upstream_sse = extract_upstream_sse_support(&result);

    state
        .capability_cache
        .set_upstream_supports_tasks(upstream_tasks);
    state
        .capability_cache
        .set_upstream_supports_task_sse(upstream_sse);

    info!(
        upstream_tasks = upstream_tasks,
        upstream_sse = upstream_sse,
        "Detected upstream capabilities"
    );

    // Inject ThoughtGate's capabilities
    let mut new_result = result;

    // Always inject task capability (ThoughtGate supports tasks)
    inject_task_capability(&mut new_result);

    // v0.2: Strip SSE capability entirely
    // ThoughtGate does not yet implement SSE endpoints, so we must not
    // advertise the capability - even if upstream supports it. Clients would
    // attempt to subscribe and fail. Detection is preserved in CapabilityCache
    // for future use in v0.3+.
    // See: REQ-GOV-004 (Upstream Task Orchestration) - DEFERRED to v0.3+
    strip_sse_capability(&mut new_result);

    debug!(
        injected_tasks = true,
        upstream_sse_detected = upstream_sse,
        sse_stripped = true,
        "Injected capabilities into initialize response (SSE stripped for v0.2)"
    );

    Ok(JsonRpcResponse::success(response.id, new_result))
}

/// Handle list methods (tools/list, resources/list, prompts/list) with response filtering.
///
/// Implements: SEP-1686 Section 3.2 (Tool Advertisement)
///
/// This function:
/// 1. Forwards the request to upstream
/// 2. Parses the tool list from the response
/// 3. Applies Gate 1: Filters tools by visibility (ExposeConfig)
/// 4. Applies Gate 2: Annotates taskSupport based on governance rules
///
/// # Arguments
///
/// * `state` - MCP handler state with config and upstream client
/// * `request` - The list request (e.g., tools/list)
///
/// # Returns
///
/// Modified response with filtered tools and taskSupport annotations.
pub(super) async fn handle_list_method(
    state: &McpState,
    request: McpRequest,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    // Forward to upstream to get the raw tool list
    let response = state.upstream.forward(&request).await?;

    // If error response, return as-is
    if response.error.is_some() {
        return Ok(response);
    }

    // If no config, return response as-is (transparent proxy mode)
    let config = match &state.config {
        Some(c) => c,
        None => return Ok(response),
    };

    // Extract result object from response
    let result = match &response.result {
        Some(r) => r,
        None => return Ok(response),
    };

    let source_id = super::cedar_eval::get_source_id(state);

    use thoughtgate_core::governance::list_filtering;

    // Gate 1+2: Filter by visibility and annotate taskSupport.
    // Delegates to core so both proxy and CLI share identical filtering.
    // Per REQ-CORE-003/F-003: Gate 1 applies to tools, resources, and prompts.
    match request.method.as_str() {
        "tools/list" => {
            let mut new_result = result.clone();
            let tools_arr = match new_result
                .as_object_mut()
                .and_then(|obj| obj.get_mut("tools"))
                .and_then(|v| v.as_array_mut())
            {
                Some(arr) => arr,
                None => return Ok(response),
            };
            list_filtering::filter_and_annotate_tools(tools_arr, config, source_id);
            Ok(JsonRpcResponse::success(response.id, new_result))
        }
        "resources/list" => {
            let mut new_result = result.clone();
            let resources_arr = match new_result
                .as_object_mut()
                .and_then(|obj| obj.get_mut("resources"))
                .and_then(|v| v.as_array_mut())
            {
                Some(arr) => arr,
                None => return Ok(response),
            };
            list_filtering::filter_resources_by_visibility(resources_arr, config, source_id);
            Ok(JsonRpcResponse::success(response.id, new_result))
        }
        "prompts/list" => {
            let mut new_result = result.clone();
            let prompts_arr = match new_result
                .as_object_mut()
                .and_then(|obj| obj.get_mut("prompts"))
                .and_then(|v| v.as_array_mut())
            {
                Some(arr) => arr,
                None => return Ok(response),
            };
            list_filtering::filter_prompts_by_visibility(prompts_arr, config, source_id);
            Ok(JsonRpcResponse::success(response.id, new_result))
        }
        _ => {
            // Not a list method we handle, return as-is
            Ok(response)
        }
    }
}

/// Validate task metadata presence for SEP-1686 compliance.
///
/// Implements: SEP-1686 Section 3.3 (TaskRequired/TaskForbidden)
///
/// - For `action: approve` or `action: policy`: client MUST send `params.task`
/// - For `action: forward` or `action: deny`: if client sent task metadata but
///   upstream doesn't support tasks, return TaskForbidden error
pub(super) fn validate_task_metadata(
    request: &mut McpRequest,
    action: &thoughtgate_core::config::Action,
    tool_name: &str,
    upstream_supports_tasks: bool,
) -> Result<(), ThoughtGateError> {
    let has_task_metadata = request.is_task_augmented();

    match action {
        thoughtgate_core::config::Action::Approve | thoughtgate_core::config::Action::Policy => {
            // Task-required: client MUST send params.task
            if !has_task_metadata {
                return Err(ThoughtGateError::TaskRequired {
                    tool: tool_name.to_string(),
                    hint: "Include params.task per tools/list taskSupport annotation".to_string(),
                });
            }
        }
        thoughtgate_core::config::Action::Forward | thoughtgate_core::config::Action::Deny => {
            // If client sent task metadata but upstream doesn't support tasks,
            // strip the metadata and forward anyway. This avoids breaking forward
            // compatibility as upstreams gradually add task support.
            if has_task_metadata && !upstream_supports_tasks {
                warn!(
                    tool = %tool_name,
                    "Stripping task metadata: upstream does not support tasks"
                );
                request.task_metadata = None;
            }
        }
    }

    Ok(())
}
