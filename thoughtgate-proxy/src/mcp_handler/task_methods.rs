//! SEP-1686 task method handlers.
//!
//! Implements: REQ-GOV-001/F-003 through F-006

use thoughtgate_core::error::ThoughtGateError;
use thoughtgate_core::governance::Principal;
use thoughtgate_core::policy::principal::{infer_principal, infer_principal_or_error};
use thoughtgate_core::protocol::{
    TasksCancelRequest, TasksGetRequest, TasksListRequest, TasksResultRequest,
};
use thoughtgate_core::transport::jsonrpc::{JsonRpcResponse, McpRequest};
use thoughtgate_core::transport::router::TaskMethod;
use tracing::warn;

use super::McpState;

/// Handle SEP-1686 task method requests.
///
/// Implements: REQ-GOV-001/F-003 through F-006
///
/// For tasks/result, integrates with the ApprovalEngine to execute approved
/// requests on the upstream server.
pub(crate) async fn handle_task_method(
    state: &McpState,
    method: TaskMethod,
    request: &McpRequest,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    // Extract params, defaulting to empty object
    let params = request
        .params
        .as_deref()
        .cloned()
        .unwrap_or(serde_json::json!({}));

    match method {
        TaskMethod::Get => {
            // Implements: REQ-GOV-001/F-003 (tasks/get)
            let req: TasksGetRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/get params: {}", e),
                })?;

            // Verify caller owns this task (returns TaskNotFound on mismatch)
            verify_task_principal(state, &req.task_id).await?;

            let result = state
                .task_handler
                .handle_tasks_get(req)
                .map_err(task_error_to_thoughtgate)?;

            Ok(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::to_value(result).map_err(|e| ThoughtGateError::ServiceUnavailable {
                    reason: format!("Failed to serialize tasks/get response: {}", e),
                })?,
            ))
        }

        TaskMethod::Result => {
            // Implements: REQ-GOV-001/F-004 (tasks/result)
            // Implements: REQ-GOV-002/F-005 (Result retrieval with execution)
            let req: TasksResultRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/result params: {}", e),
                })?;

            // Verify caller owns this task (returns TaskNotFound on mismatch)
            verify_task_principal(state, &req.task_id).await?;

            // If we have an approval engine, use it to execute approved tasks
            if let Some(approval_engine) = &state.approval_engine {
                // req.task_id is already a Sep1686TaskId (aliased as TaskId)
                let task_id = req.task_id.clone();

                let tool_result = approval_engine.execute_on_result(&task_id).await?;

                Ok(JsonRpcResponse::success(
                    request.id.clone(),
                    serde_json::to_value(tool_result).map_err(|e| {
                        ThoughtGateError::ServiceUnavailable {
                            reason: format!("Failed to serialize tasks/result response: {}", e),
                        }
                    })?,
                ))
            } else {
                // Fallback to basic task handler (no execution)
                let result = state
                    .task_handler
                    .handle_tasks_result(req)
                    .map_err(task_error_to_thoughtgate)?;

                Ok(JsonRpcResponse::success(
                    request.id.clone(),
                    serde_json::to_value(result).map_err(|e| {
                        ThoughtGateError::ServiceUnavailable {
                            reason: format!("Failed to serialize tasks/result response: {}", e),
                        }
                    })?,
                ))
            }
        }

        TaskMethod::List => {
            // Implements: REQ-GOV-001/F-005 (tasks/list)
            let req: TasksListRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/list params: {}", e),
                })?;

            // Infer principal from environment (sidecar mode).
            // TODO(gateway): In gateway/multi-tenant mode, extract principal from
            // request headers (e.g. X-Forwarded-User, mTLS client cert CN) instead
            // of the sidecar's own K8s identity. Without this, all tasks/list calls
            // in gateway mode would return the same principal's tasks.
            let policy_principal = infer_principal_or_error().await?;
            let principal = Principal::from_policy(
                &policy_principal.app_name,
                &policy_principal.namespace,
                &policy_principal.service_account,
                policy_principal.roles.clone(),
            );

            let result = state.task_handler.handle_tasks_list(req, &principal);

            Ok(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::to_value(result).map_err(|e| ThoughtGateError::ServiceUnavailable {
                    reason: format!("Failed to serialize tasks/list response: {}", e),
                })?,
            ))
        }

        TaskMethod::Cancel => {
            // Implements: REQ-GOV-001/F-006 (tasks/cancel)
            let req: TasksCancelRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/cancel params: {}", e),
                })?;

            // Verify caller owns this task (returns TaskNotFound on mismatch)
            verify_task_principal(state, &req.task_id).await?;

            let result = state
                .task_handler
                .handle_tasks_cancel(req)
                .map_err(task_error_to_thoughtgate)?;

            Ok(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::to_value(result).map_err(|e| ThoughtGateError::ServiceUnavailable {
                    reason: format!("Failed to serialize tasks/cancel response: {}", e),
                })?,
            ))
        }
    }
}

/// Verify that the caller's principal matches the task's principal.
///
/// Returns TaskNotFound (not "access denied") to prevent information leakage
/// about tasks owned by other principals.
///
/// Fails closed: if principal inference fails for any reason (I/O error,
/// misconfiguration, spawn_blocking panic), access is denied rather than
/// silently allowed.
///
/// Implements: REQ-GOV-001/F-011 (Principal isolation)
async fn verify_task_principal(
    state: &McpState,
    task_id: &thoughtgate_core::governance::task::TaskId,
) -> Result<(), ThoughtGateError> {
    // Fail closed: if identity cannot be established, deny access.
    // This prevents unauthorized access to tasks when identity inference
    // fails due to I/O errors, misconfiguration, or spawn_blocking panics.
    let caller_principal = match tokio::task::spawn_blocking(infer_principal).await {
        Ok(Ok(p)) => Principal::new(&p.app_name),
        Ok(Err(e)) => {
            warn!(error = %e, "Principal inference failed, denying task access");
            return Err(ThoughtGateError::TaskNotFound {
                task_id: task_id.to_string(),
            });
        }
        Err(e) => {
            warn!(error = %e, "Principal inference task panicked, denying task access");
            return Err(ThoughtGateError::TaskNotFound {
                task_id: task_id.to_string(),
            });
        }
    };

    let task = state
        .task_handler
        .store()
        .get(task_id)
        .map_err(task_error_to_thoughtgate)?;

    if task.principal != caller_principal {
        return Err(ThoughtGateError::TaskNotFound {
            task_id: task_id.to_string(),
        });
    }

    Ok(())
}

/// Convert TaskError to ThoughtGateError.
///
/// Implements: REQ-CORE-004 (Error Handling)
fn task_error_to_thoughtgate(error: thoughtgate_core::governance::TaskError) -> ThoughtGateError {
    use thoughtgate_core::governance::TaskError;

    match error {
        TaskError::NotFound { task_id } => ThoughtGateError::TaskNotFound {
            task_id: task_id.to_string(),
        },
        TaskError::Expired { task_id } => ThoughtGateError::TaskExpired {
            task_id: task_id.to_string(),
        },
        TaskError::AlreadyTerminal { task_id, status } => ThoughtGateError::InvalidParams {
            details: format!(
                "Cannot cancel task {}: already in terminal status '{}'",
                task_id, status
            ),
        },
        TaskError::InvalidTransition { task_id, from, to } => ThoughtGateError::InvalidRequest {
            details: format!(
                "Invalid task transition for {}: {} -> {}",
                task_id, from, to
            ),
        },
        TaskError::ConcurrentModification {
            task_id,
            expected,
            actual,
        } => ThoughtGateError::InvalidRequest {
            details: format!(
                "Concurrent modification of task {}: expected {}, found {}",
                task_id, expected, actual
            ),
        },
        TaskError::RateLimited { retry_after, .. } => ThoughtGateError::RateLimited {
            retry_after_secs: Some(retry_after.as_secs()),
        },
        TaskError::ResultNotReady { task_id } => ThoughtGateError::TaskResultNotReady {
            task_id: task_id.to_string(),
        },
        TaskError::CapacityExceeded => ThoughtGateError::ServiceUnavailable {
            reason: "Task capacity exceeded".to_string(),
        },
        TaskError::Internal { details } => ThoughtGateError::ServiceUnavailable {
            reason: format!("Internal task error: {}", details),
        },
    }
}
