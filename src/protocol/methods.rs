//! SEP-1686 Task Method Request/Response Types.
//!
//! Implements: REQ-CORE-007/§6.6 (Task Response Formats)
//!
//! This module defines the request and response types for `tasks/*` methods:
//! - `tasks/get`: Retrieve task status
//! - `tasks/result`: Retrieve completed task result
//! - `tasks/list`: List tasks with pagination
//! - `tasks/cancel`: Cancel a pending task

use serde::{Deserialize, Serialize};

use super::{Sep1686TaskId, Sep1686TaskListEntry, Sep1686TaskMetadata, Sep1686TaskResult};

// ============================================================================
// tasks/get
// ============================================================================

/// Request parameters for `tasks/get`.
///
/// Implements: REQ-CORE-007/§6.6
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksGetRequest {
    /// The task ID to retrieve.
    pub task_id: Sep1686TaskId,
}

impl TasksGetRequest {
    /// Creates a new tasks/get request.
    #[must_use]
    pub fn new(task_id: Sep1686TaskId) -> Self {
        Self { task_id }
    }
}

/// Response for `tasks/get`.
///
/// Implements: REQ-CORE-007/§6.6
///
/// Returns the same format as `tools/call` task response.
pub type TasksGetResponse = Sep1686TaskMetadata;

// ============================================================================
// tasks/result
// ============================================================================

/// Request parameters for `tasks/result`.
///
/// Implements: REQ-CORE-007/§6.6
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksResultRequest {
    /// The task ID to retrieve the result for.
    pub task_id: Sep1686TaskId,
}

impl TasksResultRequest {
    /// Creates a new tasks/result request.
    #[must_use]
    pub fn new(task_id: Sep1686TaskId) -> Self {
        Self { task_id }
    }
}

/// Response for `tasks/result`.
///
/// Implements: REQ-CORE-007/§6.6
///
/// Returns the actual tool result (same format as sync `tools/call` response).
pub type TasksResultResponse = Sep1686TaskResult;

// ============================================================================
// tasks/list
// ============================================================================

/// Request parameters for `tasks/list`.
///
/// Implements: REQ-CORE-007/§6.6
///
/// Note: Per MCP Tasks Specification, pagination uses cursor-only model.
/// The server controls page size (no client-specified `limit` parameter).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TasksListRequest {
    /// Cursor for pagination (opaque string from previous response).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl TasksListRequest {
    /// Creates a new tasks/list request.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the pagination cursor.
    #[must_use]
    pub fn with_cursor(mut self, cursor: impl Into<String>) -> Self {
        self.cursor = Some(cursor.into());
        self
    }
}

/// Response for `tasks/list`.
///
/// Implements: REQ-CORE-007/§6.6
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksListResponse {
    /// List of task entries.
    pub tasks: Vec<Sep1686TaskListEntry>,

    /// Cursor for the next page, if more results exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl TasksListResponse {
    /// Creates a new tasks/list response.
    #[must_use]
    pub fn new(tasks: Vec<Sep1686TaskListEntry>) -> Self {
        Self {
            tasks,
            next_cursor: None,
        }
    }

    /// Sets the next page cursor.
    #[must_use]
    pub fn with_next_cursor(mut self, cursor: impl Into<String>) -> Self {
        self.next_cursor = Some(cursor.into());
        self
    }

    /// Returns true if there are more pages.
    #[must_use]
    pub fn has_more(&self) -> bool {
        self.next_cursor.is_some()
    }
}

// ============================================================================
// tasks/cancel
// ============================================================================

/// Request parameters for `tasks/cancel`.
///
/// Implements: REQ-CORE-007/§6.6
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksCancelRequest {
    /// The task ID to cancel.
    pub task_id: Sep1686TaskId,
}

impl TasksCancelRequest {
    /// Creates a new tasks/cancel request.
    #[must_use]
    pub fn new(task_id: Sep1686TaskId) -> Self {
        Self { task_id }
    }
}

/// Response for `tasks/cancel`.
///
/// Implements: REQ-CORE-007/§6.6
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksCancelResponse {
    /// The cancelled task ID.
    pub task_id: Sep1686TaskId,

    /// The new status (should be "cancelled").
    pub status: super::Sep1686Status,
}

impl TasksCancelResponse {
    /// Creates a new tasks/cancel response.
    #[must_use]
    pub fn new(task_id: Sep1686TaskId) -> Self {
        Self {
            task_id,
            status: super::Sep1686Status::Cancelled,
        }
    }
}

// ============================================================================
// Tools/Call with Task Support
// ============================================================================

/// Tool call request with optional task metadata.
///
/// Implements: REQ-CORE-007/§5.1
///
/// When the `task` field is present, the client is opting into async mode.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCallRequest {
    /// Name of the tool to call.
    pub name: String,

    /// Arguments to pass to the tool.
    #[serde(default)]
    pub arguments: serde_json::Value,

    /// Optional task metadata for async execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<super::Sep1686TaskRequest>,
}

/// Custom Debug implementation that redacts arguments to prevent PII leakage.
impl std::fmt::Debug for ToolsCallRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolsCallRequest")
            .field("name", &self.name)
            .field("arguments", &"<redacted>")
            .field("task", &self.task)
            .finish()
    }
}

impl ToolsCallRequest {
    /// Creates a new synchronous tool call request.
    #[must_use]
    pub fn new(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            arguments,
            task: None,
        }
    }

    /// Creates a new async tool call request with task metadata.
    #[must_use]
    pub fn new_async(
        name: impl Into<String>,
        arguments: serde_json::Value,
        task: super::Sep1686TaskRequest,
    ) -> Self {
        Self {
            name: name.into(),
            arguments,
            task: Some(task),
        }
    }

    /// Returns true if this request opts into async mode.
    #[must_use]
    pub fn is_async(&self) -> bool {
        self.task.is_some()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // tasks/get Tests
    // ========================================================================

    #[test]
    fn test_tasks_get_request_serialization() {
        let req = TasksGetRequest::new(Sep1686TaskId::from_raw("tg_test123"));
        let json = serde_json::to_string(&req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["taskId"], "tg_test123");
    }

    #[test]
    fn test_tasks_get_request_deserialization() {
        let json = r#"{"taskId": "tg_abc123456789012345"}"#;
        let req: TasksGetRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.task_id.as_str(), "tg_abc123456789012345");
    }

    // ========================================================================
    // tasks/result Tests
    // ========================================================================

    #[test]
    fn test_tasks_result_request_serialization() {
        let req = TasksResultRequest::new(Sep1686TaskId::from_raw("tg_xyz"));
        let json = serde_json::to_string(&req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["taskId"], "tg_xyz");
    }

    // ========================================================================
    // tasks/list Tests
    // ========================================================================

    #[test]
    fn test_tasks_list_request_empty() {
        let req = TasksListRequest::new();
        let json = serde_json::to_string(&req).unwrap();

        // Should be empty object when no fields set
        assert_eq!(json, "{}");
    }

    #[test]
    fn test_tasks_list_request_with_pagination() {
        let req = TasksListRequest::new().with_cursor("abc123");

        let json = serde_json::to_string(&req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["cursor"], "abc123");
        // Note: No limit parameter per MCP spec - server controls page size
    }

    #[test]
    fn test_tasks_list_response_serialization() {
        let tasks = vec![Sep1686TaskListEntry::new(
            Sep1686TaskId::from_raw("tg_test"),
            super::super::Sep1686Status::InputRequired,
            chrono::Utc::now(),
            "delete_user",
        )];

        let response = TasksListResponse::new(tasks).with_next_cursor("next123");

        let json = serde_json::to_string(&response).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed["tasks"].is_array());
        assert_eq!(parsed["tasks"][0]["taskId"], "tg_test");
        assert_eq!(parsed["nextCursor"], "next123");
    }

    #[test]
    fn test_tasks_list_response_no_more_pages() {
        let response = TasksListResponse::new(vec![]);
        assert!(!response.has_more());

        let response_with_cursor = TasksListResponse::new(vec![]).with_next_cursor("next");
        assert!(response_with_cursor.has_more());
    }

    // ========================================================================
    // tasks/cancel Tests
    // ========================================================================

    #[test]
    fn test_tasks_cancel_request_serialization() {
        let req = TasksCancelRequest::new(Sep1686TaskId::from_raw("tg_cancel_me"));
        let json = serde_json::to_string(&req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["taskId"], "tg_cancel_me");
    }

    #[test]
    fn test_tasks_cancel_response_serialization() {
        let response = TasksCancelResponse::new(Sep1686TaskId::from_raw("tg_cancelled"));

        let json = serde_json::to_string(&response).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["taskId"], "tg_cancelled");
        assert_eq!(parsed["status"], "cancelled");
    }

    // ========================================================================
    // ToolsCallRequest Tests
    // ========================================================================

    #[test]
    fn test_tools_call_sync_request() {
        let req = ToolsCallRequest::new("read_file", serde_json::json!({"path": "/etc/hosts"}));

        assert!(!req.is_async());

        let json = serde_json::to_string(&req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["name"], "read_file");
        assert_eq!(parsed["arguments"]["path"], "/etc/hosts");
        assert!(parsed.get("task").is_none());
    }

    #[test]
    fn test_tools_call_async_request() {
        let req = ToolsCallRequest::new_async(
            "delete_user",
            serde_json::json!({"user_id": "123"}),
            super::super::Sep1686TaskRequest::with_ttl(600000),
        );

        assert!(req.is_async());

        let json = serde_json::to_string(&req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["name"], "delete_user");
        // Arguments are passed through unchanged - user_id stays as user_id (snake_case)
        // The camelCase serialization only applies to the request struct fields, not argument contents
        assert_eq!(parsed["arguments"]["user_id"], "123");
        assert_eq!(parsed["task"]["ttl"], 600000);
    }

    #[test]
    fn test_tools_call_request_deserialization() {
        // Sync request
        let json = r#"{"name": "test_tool", "arguments": {}}"#;
        let req: ToolsCallRequest = serde_json::from_str(json).unwrap();
        assert!(!req.is_async());

        // Async request
        let json = r#"{"name": "test_tool", "arguments": {}, "task": {"ttl": 60000}}"#;
        let req: ToolsCallRequest = serde_json::from_str(json).unwrap();
        assert!(req.is_async());
        assert_eq!(req.task.unwrap().ttl, Some(60000));
    }
}
