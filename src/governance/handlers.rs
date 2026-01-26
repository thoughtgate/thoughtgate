//! SEP-1686 Task Method Handlers.
//!
//! Implements: REQ-GOV-001/F-003 through F-006, REQ-CORE-007/§6.6
//!
//! This module provides handlers for the `tasks/*` JSON-RPC methods:
//! - `tasks/get` - Retrieve task status (F-003)
//! - `tasks/result` - Retrieve completed task result (F-004)
//! - `tasks/list` - List tasks with pagination (F-005)
//! - `tasks/cancel` - Cancel a pending task (F-006)

use std::sync::Arc;

use crate::protocol::{
    Sep1686TaskListEntry, Sep1686TaskMetadata, Sep1686TaskResult, TasksCancelRequest,
    TasksCancelResponse, TasksGetRequest, TasksListRequest, TasksListResponse, TasksResultRequest,
};

use super::{Principal, Task, TaskError, TaskStore};

// ============================================================================
// Task Handler
// ============================================================================

/// Handler for SEP-1686 task methods.
///
/// Implements: REQ-GOV-001/F-003 through F-006
///
/// Wraps a TaskStore and provides method handlers that convert between
/// internal task representation and SEP-1686 wire format.
#[derive(Debug, Clone)]
pub struct TaskHandler {
    store: Arc<TaskStore>,
}

impl TaskHandler {
    /// Creates a new task handler wrapping the given store.
    #[must_use]
    pub fn new(store: Arc<TaskStore>) -> Self {
        Self { store }
    }

    /// Returns a reference to the underlying task store.
    ///
    /// Use this when you need temporary read access to the store.
    /// For shared ownership, use [`shared_store()`](Self::shared_store) instead.
    #[must_use]
    pub fn store(&self) -> &TaskStore {
        &self.store
    }

    /// Returns a clone of the underlying task store Arc for shared ownership.
    ///
    /// This is a cheap operation (Arc clone) and is safe to call frequently.
    /// The returned Arc shares ownership with the internal store.
    ///
    /// Use this when another component needs to share access to the same
    /// TaskStore (e.g., ApprovalEngine needs shared access).
    #[must_use]
    pub fn shared_store(&self) -> Arc<TaskStore> {
        self.store.clone()
    }

    // ========================================================================
    // tasks/get
    // ========================================================================

    /// Handles `tasks/get` request.
    ///
    /// Implements: REQ-GOV-001/F-003
    ///
    /// Returns the current status and metadata of a task.
    ///
    /// # Errors
    ///
    /// Returns `TaskError::NotFound` if the task does not exist.
    pub fn handle_tasks_get(&self, req: TasksGetRequest) -> Result<Sep1686TaskMetadata, TaskError> {
        let task = self.store.get(&req.task_id)?;
        Ok(task_to_metadata(&task))
    }

    // ========================================================================
    // tasks/result
    // ========================================================================

    /// Handles `tasks/result` request.
    ///
    /// Implements: REQ-GOV-001/F-004
    ///
    /// Returns the result of a completed task.
    ///
    /// # Errors
    ///
    /// - `TaskError::NotFound` if the task does not exist
    /// - `TaskError::ResultNotReady` if the task is not in a terminal state
    pub fn handle_tasks_result(
        &self,
        req: TasksResultRequest,
    ) -> Result<Sep1686TaskResult, TaskError> {
        let task = self.store.get(&req.task_id)?;

        if !task.status.is_terminal() {
            return Err(TaskError::ResultNotReady {
                task_id: req.task_id,
            });
        }

        // Return result if completed successfully
        if let Some(result) = task.result {
            return Ok(Sep1686TaskResult {
                content: result.content,
                is_error: result.is_error,
            });
        }

        // For failed/rejected/cancelled/expired tasks, construct error result
        let error_content = if let Some(failure) = task.failure {
            serde_json::json!({
                "error": failure.reason,
                "stage": format!("{:?}", failure.stage),
                "retriable": failure.retriable,
            })
        } else if let Some(approval) = task.approval {
            match approval.decision {
                super::ApprovalDecision::Rejected { reason } => {
                    serde_json::json!({
                        "error": reason.unwrap_or_else(|| "Request rejected".to_string()),
                        "decided_by": approval.decided_by,
                    })
                }
                _ => serde_json::json!({ "error": "Task did not complete successfully" }),
            }
        } else {
            serde_json::json!({ "error": format!("Task ended in {} state", task.status) })
        };

        Ok(Sep1686TaskResult {
            content: error_content,
            is_error: true,
        })
    }

    // ========================================================================
    // tasks/list
    // ========================================================================

    /// Handles `tasks/list` request.
    ///
    /// Implements: REQ-GOV-001/F-005
    ///
    /// Returns a paginated list of tasks for the given principal.
    /// The cursor is an opaque string encoding the offset.
    ///
    /// Note: Per MCP Tasks Specification, page size is server-controlled.
    /// Clients use cursor-only pagination without specifying limit.
    pub fn handle_tasks_list(
        &self,
        req: TasksListRequest,
        principal: &Principal,
    ) -> TasksListResponse {
        // Fixed page size per MCP spec (server-controlled, no client limit)
        const PAGE_SIZE: usize = 20;

        // Parse cursor as offset (simple numeric cursor for now)
        let offset = req
            .cursor
            .as_ref()
            .and_then(|c| c.parse::<usize>().ok())
            .unwrap_or(0);

        let tasks = self
            .store
            .list_for_principal(principal, offset, PAGE_SIZE + 1);

        // Check if there are more results
        let has_more = tasks.len() > PAGE_SIZE;
        let tasks: Vec<_> = tasks.into_iter().take(PAGE_SIZE).collect();

        // Build response entries
        let entries: Vec<Sep1686TaskListEntry> = tasks
            .iter()
            .map(|task| {
                Sep1686TaskListEntry::new(
                    task.id.clone(),
                    task.status.into(),
                    task.created_at,
                    &task.original_request.name,
                )
            })
            .collect();

        let mut response = TasksListResponse::new(entries);

        if has_more {
            response = response.with_next_cursor((offset + PAGE_SIZE).to_string());
        }

        response
    }

    // ========================================================================
    // tasks/cancel
    // ========================================================================

    /// Handles `tasks/cancel` request.
    ///
    /// Implements: REQ-GOV-001/F-006
    ///
    /// Cancels a task that is in the `InputRequired` state.
    ///
    /// # Errors
    ///
    /// - `TaskError::NotFound` if the task does not exist
    /// - `TaskError::AlreadyTerminal` if the task is already in a terminal state
    /// - `TaskError::InvalidTransition` if the task cannot be cancelled from its current state
    pub fn handle_tasks_cancel(
        &self,
        req: TasksCancelRequest,
    ) -> Result<TasksCancelResponse, TaskError> {
        self.store.cancel(&req.task_id)?;
        Ok(TasksCancelResponse::new(req.task_id))
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Converts a Task to SEP-1686 task metadata.
fn task_to_metadata(task: &Task) -> Sep1686TaskMetadata {
    let mut metadata = Sep1686TaskMetadata::new(task.id.clone(), task.status.into());

    // Add poll interval for non-terminal tasks
    if !task.status.is_terminal() {
        metadata = metadata.with_poll_interval(task.poll_interval.as_millis() as u64);
    }

    // Add status message if present
    if let Some(msg) = &task.status_message {
        metadata = metadata.with_message(msg.clone());
    }

    metadata
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::task::JsonRpcId;
    use crate::governance::{TaskId, TaskStatus, TimeoutAction, ToolCallRequest};
    use crate::protocol::Sep1686Status;
    use std::time::Duration;

    fn test_store() -> Arc<TaskStore> {
        Arc::new(TaskStore::with_defaults())
    }

    fn test_principal() -> Principal {
        Principal::new("test-app")
    }

    fn test_request() -> ToolCallRequest {
        ToolCallRequest {
            method: "tools/call".to_string(),
            name: "delete_user".to_string(),
            arguments: serde_json::json!({"user_id": "123"}),
            mcp_request_id: JsonRpcId::Number(1),
        }
    }

    // ========================================================================
    // tasks/get Tests
    // ========================================================================

    /// Tests tasks/get returns correct metadata.
    ///
    /// Verifies: REQ-GOV-001/F-003
    #[test]
    fn test_tasks_get_returns_metadata() {
        let store = test_store();
        let handler = TaskHandler::new(store.clone());

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        let req = TasksGetRequest::new(task.id.clone());
        let response = handler.handle_tasks_get(req).unwrap();

        assert_eq!(response.task_id, task.id);
        assert_eq!(response.status, Sep1686Status::Working);
        assert!(response.poll_interval.is_some());
    }

    /// Tests tasks/get returns not found for missing task.
    #[test]
    fn test_tasks_get_not_found() {
        let handler = TaskHandler::new(test_store());

        let req = TasksGetRequest::new(TaskId::new());
        let result = handler.handle_tasks_get(req);

        assert!(matches!(result, Err(TaskError::NotFound { .. })));
    }

    // ========================================================================
    // tasks/result Tests
    // ========================================================================

    /// Tests tasks/result returns not ready for non-terminal task.
    ///
    /// Verifies: REQ-GOV-001/F-004
    #[test]
    fn test_tasks_result_not_ready() {
        let store = test_store();
        let handler = TaskHandler::new(store.clone());

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        let req = TasksResultRequest::new(task.id.clone());
        let result = handler.handle_tasks_result(req);

        assert!(matches!(result, Err(TaskError::ResultNotReady { .. })));
    }

    /// Tests tasks/result returns result for completed task.
    #[test]
    fn test_tasks_result_completed() {
        let store = test_store();
        let handler = TaskHandler::new(store.clone());

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Complete the task
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();
        store
            .record_approval(
                &task.id,
                super::super::ApprovalDecision::Approved,
                "tester".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();
        store
            .complete(
                &task.id,
                super::super::ToolCallResult {
                    content: serde_json::json!({"success": true}),
                    is_error: false,
                },
            )
            .unwrap();

        let req = TasksResultRequest::new(task.id);
        let result = handler.handle_tasks_result(req).unwrap();

        assert!(!result.is_error);
        assert_eq!(result.content["success"], true);
    }

    // ========================================================================
    // tasks/list Tests
    // ========================================================================

    /// Tests tasks/list returns tasks for principal.
    ///
    /// Verifies: REQ-GOV-001/F-005
    #[test]
    fn test_tasks_list_returns_tasks() {
        let store = test_store();
        let handler = TaskHandler::new(store.clone());
        let principal = test_principal();

        // Create 3 tasks
        for _ in 0..3 {
            store
                .create(
                    test_request(),
                    test_request(),
                    principal.clone(),
                    None,
                    TimeoutAction::default(),
                )
                .unwrap();
        }

        let req = TasksListRequest::new();
        let response = handler.handle_tasks_list(req, &principal);

        assert_eq!(response.tasks.len(), 3);
        assert!(!response.has_more());
    }

    /// Tests tasks/list pagination with fixed server-controlled page size.
    ///
    /// Note: Per MCP spec, page size is server-controlled (PAGE_SIZE = 20).
    /// This test creates enough tasks to test pagination behavior.
    #[test]
    fn test_tasks_list_pagination() {
        use crate::governance::TaskStoreConfig;

        // Use a custom store with higher limit for pagination testing
        let config = TaskStoreConfig {
            max_pending_per_principal: 30, // Allow enough for 25 tasks
            ..Default::default()
        };
        let store = Arc::new(TaskStore::new(config));
        let handler = TaskHandler::new(store.clone());
        let principal = test_principal();

        // Create 25 tasks to exceed one page (PAGE_SIZE = 20)
        for _ in 0..25 {
            store
                .create(
                    test_request(),
                    test_request(),
                    principal.clone(),
                    None,
                    TimeoutAction::default(),
                )
                .unwrap();
        }

        // Get first page (should return 20 tasks with cursor for more)
        let req = TasksListRequest::new();
        let page1 = handler.handle_tasks_list(req, &principal);
        assert_eq!(page1.tasks.len(), 20);
        assert!(page1.has_more());

        // Get second page using cursor (should return remaining 5 tasks)
        let req = TasksListRequest::new().with_cursor(page1.next_cursor.unwrap());
        let page2 = handler.handle_tasks_list(req, &principal);
        assert_eq!(page2.tasks.len(), 5);
        assert!(!page2.has_more());
    }

    /// Tests tasks/list returns empty for different principal.
    #[test]
    fn test_tasks_list_empty_for_other_principal() {
        let store = test_store();
        let handler = TaskHandler::new(store.clone());

        // Create task for one principal
        store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // List for different principal
        let other = Principal::new("other-app");
        let req = TasksListRequest::new();
        let response = handler.handle_tasks_list(req, &other);

        assert!(response.tasks.is_empty());
    }

    // ========================================================================
    // tasks/cancel Tests
    // ========================================================================

    /// Tests tasks/cancel cancels a pending task.
    ///
    /// Verifies: REQ-GOV-001/F-006
    #[test]
    fn test_tasks_cancel_success() {
        let store = test_store();
        let handler = TaskHandler::new(store.clone());

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Move to InputRequired (cancellable state)
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        let req = TasksCancelRequest::new(task.id.clone());
        let response = handler.handle_tasks_cancel(req).unwrap();

        assert_eq!(response.task_id, task.id);
        assert_eq!(response.status, Sep1686Status::Cancelled);

        // Verify task is cancelled
        let task = store.get(&response.task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Cancelled);
    }

    /// Tests tasks/cancel fails for task not in InputRequired.
    #[test]
    fn test_tasks_cancel_invalid_state() {
        let store = test_store();
        let handler = TaskHandler::new(store.clone());

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Task is in Working state, not cancellable
        let req = TasksCancelRequest::new(task.id);
        let result = handler.handle_tasks_cancel(req);

        assert!(matches!(result, Err(TaskError::InvalidTransition { .. })));
    }

    /// Tests task ID format is SEP-1686 compliant.
    ///
    /// Verifies: REQ-CORE-007/§6.5
    #[test]
    fn test_task_id_sep1686_format() {
        let store = test_store();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Task ID should start with "tg_"
        assert!(
            task.id.as_str().starts_with("tg_"),
            "Task ID should have tg_ prefix: {}",
            task.id
        );

        // Task ID should be owned by ThoughtGate
        assert!(task.id.is_thoughtgate_owned());
    }
}
