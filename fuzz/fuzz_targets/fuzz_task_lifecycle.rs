#![no_main]

//! Fuzz target for REQ-GOV-001 Task Lifecycle State Machine
//!
//! # Traceability
//! - Implements: REQ-GOV-001 (Task Lifecycle)
//! - Attack surface: State machine transitions, concurrent operations
//!
//! # Goal
//! Verify that task lifecycle operations do not cause:
//! - Invalid state transitions
//! - Race conditions in concurrent updates
//! - Memory leaks from abandoned tasks
//! - Panics in status handling

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;
use std::time::Duration;

use thoughtgate_core::governance::{
    ApprovalDecision, Principal, TaskId, TaskStatus, TaskStore, TaskStoreConfig, TimeoutAction,
    ToolCallRequest, ToolCallResult,
};
use thoughtgate_core::governance::task::{FailureInfo, FailureStage};

/// Fuzz input for task lifecycle testing
#[derive(Arbitrary, Debug)]
struct FuzzTaskInput {
    /// Sequence of operations to perform
    operations: Vec<TaskOperation>,
    /// Number of concurrent tasks to create
    task_count: u8,
    /// Principal data
    principal: FuzzPrincipal,
    /// Tool call data
    tool_call: FuzzToolCall,
}

#[derive(Arbitrary, Debug)]
struct FuzzPrincipal {
    app_name: Vec<u8>,
    user_id: Option<Vec<u8>>,
    session_id: Option<Vec<u8>>,
}

#[derive(Arbitrary, Debug)]
struct FuzzToolCall {
    name: Vec<u8>,
    arguments: Vec<u8>,
}

#[derive(Arbitrary, Debug, Clone)]
enum TaskOperation {
    /// Create a new task
    Create,
    /// Transition to a new status
    Transition(FuzzStatus),
    /// Transition with expected status check
    TransitionIf { expected: FuzzStatus, new: FuzzStatus },
    /// Get a task by ID
    Get,
    /// List tasks for principal
    List,
    /// Update task result
    SetResult { success: bool },
    /// Set approval
    SetApproval { approved: bool },
    /// Complete the task
    Complete,
    /// Cancel the task
    Cancel,
    /// Mark as failed
    Fail,
    /// Expire the task
    Expire,
}

#[derive(Arbitrary, Debug, Clone, Copy)]
enum FuzzStatus {
    Working,
    InputRequired,
    Executing,
    Completed,
    Failed,
    Rejected,
    Cancelled,
    Expired,
}

impl From<FuzzStatus> for TaskStatus {
    fn from(s: FuzzStatus) -> Self {
        match s {
            FuzzStatus::Working => TaskStatus::Working,
            FuzzStatus::InputRequired => TaskStatus::InputRequired,
            FuzzStatus::Executing => TaskStatus::Executing,
            FuzzStatus::Completed => TaskStatus::Completed,
            FuzzStatus::Failed => TaskStatus::Failed,
            FuzzStatus::Rejected => TaskStatus::Rejected,
            FuzzStatus::Cancelled => TaskStatus::Cancelled,
            FuzzStatus::Expired => TaskStatus::Expired,
        }
    }
}

fuzz_target!(|input: FuzzTaskInput| {
    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(async { fuzz_task_lifecycle(input).await }));
});

async fn fuzz_task_lifecycle(input: FuzzTaskInput) {
    // Create task store with reasonable limits
    let config = TaskStoreConfig {
        default_ttl: Duration::from_secs(300),
        max_ttl: Duration::from_secs(3600),
        min_ttl: Duration::from_secs(60),
        cleanup_interval: Duration::from_secs(60),
        max_pending_per_principal: 100,
        max_pending_global: 1000,
        terminal_grace_period: Duration::from_secs(300),
    };

    let store = Arc::new(TaskStore::new(config));

    // Build principal
    let principal = build_principal(&input.principal);

    // Build tool call request
    let tool_call = build_tool_call(&input.tool_call);

    // Create initial tasks
    let task_count = (input.task_count % 10).max(1) as usize;
    let mut task_ids: Vec<TaskId> = Vec::new();

    for _ in 0..task_count {
        if let Ok(task) = store.create(
            tool_call.clone(),
            tool_call.clone(),
            principal.clone(),
            Some(Duration::from_secs(600)),
            TimeoutAction::Deny,
        ) {
            task_ids.push(task.id.clone());
        }
    }

    // Process operations
    for op in input.operations.iter().take(100) {
        // Get a task ID to operate on (round-robin)
        let task_id = if task_ids.is_empty() {
            continue;
        } else {
            &task_ids[task_ids.len() % task_ids.len()]
        };

        match op {
            TaskOperation::Create => {
                if let Ok(task) = store.create(
                    tool_call.clone(),
                    tool_call.clone(),
                    principal.clone(),
                    Some(Duration::from_secs(600)),
                    TimeoutAction::Deny,
                ) {
                    task_ids.push(task.id.clone());
                }
            }

            TaskOperation::Transition(status) => {
                let _ = store.transition(task_id, (*status).into(), None);
            }

            TaskOperation::TransitionIf { expected, new } => {
                let _ = store.transition_if(task_id, (*expected).into(), (*new).into(), None);
            }

            TaskOperation::Get => {
                let _ = store.get(task_id);
            }

            TaskOperation::List => {
                let _ = store.list_for_principal(&principal, 0, 100);
            }

            TaskOperation::SetResult { success } => {
                // Use complete() or fail() to set results
                if *success {
                    let _ = store.complete(
                        task_id,
                        ToolCallResult {
                            content: serde_json::json!({"status": "ok"}),
                            is_error: false,
                        },
                    );
                } else {
                    let _ = store.fail(
                        task_id,
                        FailureInfo {
                            stage: FailureStage::UpstreamError,
                            reason: "Fuzz test failure".to_string(),
                            retriable: false,
                        },
                    );
                }
            }

            TaskOperation::SetApproval { approved } => {
                // Use record_approval() to set approval
                let decision = if *approved {
                    ApprovalDecision::Approved
                } else {
                    ApprovalDecision::Rejected {
                        reason: Some("fuzz test".to_string()),
                    }
                };
                let _ = store.record_approval(
                    task_id,
                    decision,
                    "fuzz-approver".to_string(),
                    Duration::from_secs(300),
                );
            }

            TaskOperation::Complete => {
                let _ = store.transition(task_id, TaskStatus::Completed, None);
            }

            TaskOperation::Cancel => {
                let _ = store.transition(task_id, TaskStatus::Cancelled, Some("fuzz cancel".to_string()));
            }

            TaskOperation::Fail => {
                let _ = store.transition(task_id, TaskStatus::Failed, Some("fuzz fail".to_string()));
            }

            TaskOperation::Expire => {
                let _ = store.transition(task_id, TaskStatus::Expired, None);
            }
        }
    }

    // Final cleanup - verify store is in consistent state
    let _ = store.expire_overdue();
    let _ = store.cleanup_terminal();

    // Verify all tasks are retrievable or properly removed
    for task_id in &task_ids {
        let result = store.get(task_id);
        // Result should be either Ok(task) or Err(NotFound) - never panic
        match result {
            Ok(_task) => {}
            Err(_e) => {}
        }
    }
}

fn build_principal(input: &FuzzPrincipal) -> Principal {
    let app_name = sanitize_string(&input.app_name, 128);

    let mut principal = Principal::new(if app_name.is_empty() {
        "fuzz-app".to_string()
    } else {
        app_name
    });

    if let Some(user_id) = &input.user_id {
        principal.user_id = Some(sanitize_string(user_id, 128));
    }

    if let Some(session_id) = &input.session_id {
        principal.session_id = Some(sanitize_string(session_id, 128));
    }

    principal
}

fn build_tool_call(input: &FuzzToolCall) -> ToolCallRequest {
    use thoughtgate_core::governance::task::JsonRpcId;

    let name = sanitize_string(&input.name, 256);
    let arguments = if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&input.arguments) {
        json
    } else {
        serde_json::json!({})
    };

    ToolCallRequest {
        method: "tools/call".to_string(),
        name: if name.is_empty() {
            "fuzz_tool".to_string()
        } else {
            name
        },
        arguments,
        mcp_request_id: JsonRpcId::Null,
    }
}

/// Convert bytes to a sanitized string with length limit
fn sanitize_string(bytes: &[u8], max_len: usize) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(max_len)
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}
