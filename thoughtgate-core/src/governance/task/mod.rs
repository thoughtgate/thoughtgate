//! Task lifecycle management for ThoughtGate approval workflows.
//!
//! Implements: REQ-GOV-001 (Task Lifecycle)
//!
//! This module provides:
//! - Task data structures for tracking approval state
//! - TaskStatus state machine with valid transitions
//! - In-memory TaskStore with concurrent access support
//! - TTL enforcement and expiration cleanup
//! - Rate limiting per principal
//!
//! ## v0.2 Updates
//!
//! - **SEP-1686 Task IDs** - Tasks use `tg_<nanoid>` format for SEP-1686 compliance
//! - **Task method handlers** - `tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel`
//! - **Status conversion** - `TaskStatus` ↔ `Sep1686Status`
//!
//! ## Constraints
//!
//! - **In-memory storage** - Tasks are lost on restart

pub mod error;
pub mod helpers;
pub mod status;
pub mod store;
pub mod types;

// Re-export all public types so that `governance::task::Foo` paths continue to work.
pub use error::TaskError;
pub use helpers::hash_request;
pub use status::TaskStatus;
pub use store::{TaskStore, TaskStoreConfig};
pub use types::{
    ApprovalDecision, ApprovalRecord, FailureInfo, FailureStage, JsonRpcId, Principal, Task,
    TaskId, TaskTransition, ToolCallRequest, ToolCallResult,
};

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::engine::TimeoutAction;
    use std::time::Duration;

    fn test_request() -> ToolCallRequest {
        ToolCallRequest {
            method: "tools/call".to_string(),
            name: "delete_user".to_string(),
            arguments: serde_json::json!({"user_id": "123"}),
            mcp_request_id: JsonRpcId::Number(1),
        }
    }

    fn test_principal() -> Principal {
        Principal::new("test-app")
    }

    // ========================================================================
    // TaskStatus Tests
    // ========================================================================

    /// Tests that is_terminal correctly identifies terminal states.
    ///
    /// Verifies: REQ-GOV-001/§5.1
    #[test]
    fn test_task_status_is_terminal() {
        assert!(!TaskStatus::Working.is_terminal());
        assert!(!TaskStatus::InputRequired.is_terminal());
        assert!(!TaskStatus::Executing.is_terminal());
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Rejected.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(TaskStatus::Expired.is_terminal());
    }

    /// Tests that is_agent_visible correctly hides internal states.
    ///
    /// Verifies: REQ-GOV-001/§6.2
    #[test]
    fn test_task_status_is_agent_visible() {
        assert!(TaskStatus::Working.is_agent_visible());
        assert!(TaskStatus::InputRequired.is_agent_visible());
        assert!(!TaskStatus::Executing.is_agent_visible()); // Internal only
        assert!(TaskStatus::Completed.is_agent_visible());
        assert!(TaskStatus::Failed.is_agent_visible());
        assert!(TaskStatus::Rejected.is_agent_visible());
        assert!(TaskStatus::Cancelled.is_agent_visible());
        assert!(TaskStatus::Expired.is_agent_visible());
    }

    /// Tests SEP-1686 status mapping.
    ///
    /// Verifies: REQ-GOV-001/§6.2
    #[test]
    fn test_task_status_to_sep1686() {
        assert_eq!(TaskStatus::Working.to_sep1686(), "working");
        assert_eq!(TaskStatus::InputRequired.to_sep1686(), "input_required");
        assert_eq!(TaskStatus::Executing.to_sep1686(), "working"); // Maps to working
        assert_eq!(TaskStatus::Completed.to_sep1686(), "completed");
        assert_eq!(TaskStatus::Failed.to_sep1686(), "failed");
        assert_eq!(TaskStatus::Rejected.to_sep1686(), "failed"); // Maps to failed
        assert_eq!(TaskStatus::Cancelled.to_sep1686(), "cancelled");
        assert_eq!(TaskStatus::Expired.to_sep1686(), "failed"); // Maps to failed
    }

    /// Tests valid state transitions.
    ///
    /// Verifies: REQ-GOV-001/F-001.1
    #[test]
    fn test_state_machine_valid_transitions() {
        // From Working
        assert!(TaskStatus::Working.can_transition_to(TaskStatus::InputRequired));
        assert!(TaskStatus::Working.can_transition_to(TaskStatus::Failed));
        assert!(TaskStatus::Working.can_transition_to(TaskStatus::Expired));

        // From InputRequired
        assert!(TaskStatus::InputRequired.can_transition_to(TaskStatus::Executing));
        assert!(TaskStatus::InputRequired.can_transition_to(TaskStatus::Rejected));
        assert!(TaskStatus::InputRequired.can_transition_to(TaskStatus::Cancelled));
        assert!(TaskStatus::InputRequired.can_transition_to(TaskStatus::Expired));

        // From Executing
        assert!(TaskStatus::Executing.can_transition_to(TaskStatus::Completed));
        assert!(TaskStatus::Executing.can_transition_to(TaskStatus::Failed));
    }

    /// Tests invalid state transitions.
    ///
    /// Verifies: REQ-GOV-001/F-001.2
    #[test]
    fn test_state_machine_invalid_transitions() {
        // Can't go backwards
        assert!(!TaskStatus::InputRequired.can_transition_to(TaskStatus::Working));
        assert!(!TaskStatus::Executing.can_transition_to(TaskStatus::InputRequired));
        assert!(!TaskStatus::Completed.can_transition_to(TaskStatus::Executing));

        // Can't skip states
        assert!(!TaskStatus::Working.can_transition_to(TaskStatus::Executing));
        assert!(!TaskStatus::Working.can_transition_to(TaskStatus::Completed));

        // Terminal states can't transition
        assert!(!TaskStatus::Completed.can_transition_to(TaskStatus::Failed));
        assert!(!TaskStatus::Failed.can_transition_to(TaskStatus::Completed));
        assert!(!TaskStatus::Rejected.can_transition_to(TaskStatus::Cancelled));
    }

    // ========================================================================
    // Task Tests
    // ========================================================================

    /// Tests task creation.
    ///
    /// Verifies: EC-TASK-001
    #[test]
    fn test_task_creation() {
        let task = Task::new(
            test_request(),
            test_request(),
            test_principal(),
            Duration::from_secs(600),
            TimeoutAction::default(),
        )
        .unwrap();

        assert_eq!(task.status, TaskStatus::Working);
        assert!(
            task.id.is_thoughtgate_owned(),
            "Task ID should be ThoughtGate-owned"
        );
        assert!(task.transitions.is_empty());
        assert!(task.approval.is_none());
        assert!(task.result.is_none());
        assert!(task.failure.is_none());
        assert_eq!(task.on_timeout, TimeoutAction::Deny);
    }

    /// Tests task transition records audit trail.
    ///
    /// Verifies: REQ-GOV-001/F-001.3
    #[test]
    fn test_task_transition_audit_trail() {
        let mut task = Task::new(
            test_request(),
            test_request(),
            test_principal(),
            Duration::from_secs(600),
            TimeoutAction::default(),
        )
        .unwrap();

        task.transition(
            TaskStatus::InputRequired,
            Some("Ready for approval".to_string()),
        )
        .unwrap();

        assert_eq!(task.status, TaskStatus::InputRequired);
        assert_eq!(task.transitions.len(), 1);
        assert_eq!(task.transitions[0].from, TaskStatus::Working);
        assert_eq!(task.transitions[0].to, TaskStatus::InputRequired);
        assert_eq!(
            task.transitions[0].reason,
            Some("Ready for approval".to_string())
        );
    }

    /// Tests that terminal states are immutable.
    ///
    /// Verifies: REQ-GOV-001/F-001.4
    #[test]
    fn test_terminal_states_immutable() {
        let mut task = Task::new(
            test_request(),
            test_request(),
            test_principal(),
            Duration::from_secs(600),
            TimeoutAction::default(),
        )
        .unwrap();

        // Get to a terminal state
        task.transition(TaskStatus::InputRequired, None).unwrap();
        task.transition(TaskStatus::Cancelled, None).unwrap();

        // Try to transition from terminal
        let result = task.transition(TaskStatus::Completed, None);
        assert!(matches!(result, Err(TaskError::AlreadyTerminal { .. })));
    }

    /// Tests optimistic locking.
    ///
    /// Verifies: REQ-GOV-001/F-007
    #[test]
    fn test_optimistic_locking() {
        let mut task = Task::new(
            test_request(),
            test_request(),
            test_principal(),
            Duration::from_secs(600),
            TimeoutAction::default(),
        )
        .unwrap();

        // Correct expected status
        task.transition_if(TaskStatus::Working, TaskStatus::InputRequired, None)
            .unwrap();

        // Wrong expected status
        let result = task.transition_if(TaskStatus::Working, TaskStatus::Executing, None);
        assert!(matches!(
            result,
            Err(TaskError::ConcurrentModification { .. })
        ));
    }

    // ========================================================================
    // TaskStore Tests
    // ========================================================================

    /// Tests task store creation and retrieval.
    ///
    /// Verifies: EC-TASK-001, EC-TASK-002
    #[test]
    fn test_task_store_create_and_get() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        assert_eq!(task.status, TaskStatus::Working);
        assert_eq!(store.pending_count(), 1);

        let retrieved = store.get(&task.id).unwrap();
        assert_eq!(retrieved.id, task.id);
    }

    /// Tests task not found error.
    ///
    /// Verifies: EC-TASK-003
    #[test]
    fn test_task_store_not_found() {
        let store = TaskStore::with_defaults();
        let fake_id = TaskId::new();

        let result = store.get(&fake_id);
        assert!(matches!(result, Err(TaskError::NotFound { .. })));
    }

    /// Tests rate limiting per principal.
    ///
    /// Verifies: EC-TASK-014, REQ-GOV-001/F-009.2
    #[test]
    fn test_rate_limiting_per_principal() {
        let config = TaskStoreConfig {
            max_pending_per_principal: 2,
            ..Default::default()
        };
        let store = TaskStore::new(config);

        // Create up to limit
        store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();
        store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Third should fail
        let result = store.create(
            test_request(),
            test_request(),
            test_principal(),
            None,
            TimeoutAction::default(),
        );
        assert!(matches!(result, Err(TaskError::RateLimited { .. })));

        // Different principal should work
        let other_principal = Principal::new("other-app");
        store
            .create(
                test_request(),
                test_request(),
                other_principal,
                None,
                TimeoutAction::default(),
            )
            .unwrap();
    }

    /// Tests global capacity limit.
    ///
    /// Verifies: EC-TASK-015, REQ-GOV-001/F-009.4
    #[test]
    fn test_global_capacity_limit() {
        let config = TaskStoreConfig {
            max_pending_global: 2,
            max_pending_per_principal: 10,
            ..Default::default()
        };
        let store = TaskStore::new(config);

        store
            .create(
                test_request(),
                test_request(),
                Principal::new("app-1"),
                None,
                TimeoutAction::default(),
            )
            .unwrap();
        store
            .create(
                test_request(),
                test_request(),
                Principal::new("app-2"),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        let result = store.create(
            test_request(),
            test_request(),
            Principal::new("app-3"),
            None,
            TimeoutAction::default(),
        );
        assert!(matches!(result, Err(TaskError::CapacityExceeded)));
    }

    /// Tests task cancellation.
    ///
    /// Verifies: EC-TASK-007, REQ-GOV-001/F-006.1
    #[test]
    fn test_task_cancellation() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Transition to InputRequired first
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        // Cancel
        let cancelled = store.cancel(&task.id).unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(store.pending_count(), 0);
    }

    /// Tests cannot cancel completed task.
    ///
    /// Verifies: EC-TASK-008
    #[test]
    fn test_cannot_cancel_completed() {
        let store = TaskStore::with_defaults();

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
                ApprovalDecision::Approved,
                "tester".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();
        store
            .complete(
                &task.id,
                ToolCallResult {
                    content: serde_json::json!({}),
                    is_error: false,
                },
            )
            .unwrap();

        // Try to cancel
        let result = store.cancel(&task.id);
        assert!(matches!(result, Err(TaskError::AlreadyTerminal { .. })));
    }

    /// Tests cannot cancel executing task.
    ///
    /// Verifies: EC-TASK-009
    #[test]
    fn test_cannot_cancel_executing() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Move to Executing
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();
        store
            .record_approval(
                &task.id,
                ApprovalDecision::Approved,
                "tester".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();

        // Try to cancel - should fail with InvalidTransition since Executing is not terminal
        let result = store.cancel(&task.id);
        assert!(matches!(
            result,
            Err(TaskError::InvalidTransition {
                from: TaskStatus::Executing,
                to: TaskStatus::Cancelled,
                ..
            })
        ));
    }

    /// Tests task expiration.
    ///
    /// Verifies: EC-TASK-012, REQ-GOV-001/F-008
    #[tokio::test]
    async fn test_task_expiration() {
        let config = TaskStoreConfig {
            min_ttl: Duration::from_millis(10),
            default_ttl: Duration::from_millis(10),
            ..Default::default()
        };
        let store = TaskStore::new(config);

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Transition to InputRequired (expiration is allowed from this state)
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        // Wait for expiration (longer than TTL to ensure expiration)
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Run cleanup
        let expired = store.expire_overdue();
        assert_eq!(expired, 1);

        let task = store.get(&task.id).unwrap();
        assert_eq!(task.status, TaskStatus::Expired);
    }

    /// Tests concurrent modification detection.
    ///
    /// Verifies: EC-TASK-013
    #[test]
    fn test_concurrent_modification() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // First transition succeeds
        store
            .transition_if(
                &task.id,
                TaskStatus::Working,
                TaskStatus::InputRequired,
                None,
            )
            .unwrap();

        // Second transition with wrong expected status fails
        let result = store.transition_if(&task.id, TaskStatus::Working, TaskStatus::Failed, None);
        assert!(matches!(
            result,
            Err(TaskError::ConcurrentModification { .. })
        ));
    }

    /// Tests listing tasks for principal.
    ///
    /// Verifies: EC-TASK-010, EC-TASK-011
    #[test]
    fn test_list_for_principal() {
        let store = TaskStore::with_defaults();
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

        // List all
        let tasks = store.list_for_principal(&principal, 0, 10);
        assert_eq!(tasks.len(), 3);

        // List with pagination
        let page1 = store.list_for_principal(&principal, 0, 2);
        assert_eq!(page1.len(), 2);

        let page2 = store.list_for_principal(&principal, 2, 2);
        assert_eq!(page2.len(), 1);

        // Different principal sees nothing
        let other = Principal::new("other");
        let empty = store.list_for_principal(&other, 0, 10);
        assert!(empty.is_empty());
    }

    /// Tests shutdown marks pending tasks as failed.
    ///
    /// Verifies: EC-TASK-016
    #[test]
    fn test_shutdown_fails_pending() {
        let store = TaskStore::with_defaults();

        // Create 3 pending tasks
        for _ in 0..3 {
            store
                .create(
                    test_request(),
                    test_request(),
                    test_principal(),
                    None,
                    TimeoutAction::default(),
                )
                .unwrap();
        }

        assert_eq!(store.pending_count(), 3);

        // Shutdown
        let failed = store.fail_all_pending("Service shutting down");
        assert_eq!(failed, 3);
        assert_eq!(store.pending_count(), 0);
    }

    /// Tests poll interval computation.
    ///
    /// Verifies: REQ-GOV-001/F-002.7
    #[test]
    fn test_poll_interval_computation() {
        use helpers::compute_poll_interval;
        assert_eq!(
            compute_poll_interval(Duration::from_secs(30)),
            Duration::from_secs(2)
        );
        assert_eq!(
            compute_poll_interval(Duration::from_secs(60)),
            Duration::from_secs(2)
        );
        assert_eq!(
            compute_poll_interval(Duration::from_secs(120)),
            Duration::from_secs(5)
        );
        assert_eq!(
            compute_poll_interval(Duration::from_secs(600)),
            Duration::from_secs(10)
        );
        assert_eq!(
            compute_poll_interval(Duration::from_secs(1200)),
            Duration::from_secs(30)
        );
    }

    /// Tests request hash computation.
    ///
    /// Verifies: REQ-GOV-001/F-002.3
    #[test]
    fn test_request_hash() {
        let req1 = test_request();
        let req2 = test_request();
        let req3 = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "other_tool".to_string(),
            arguments: serde_json::json!({}),
            mcp_request_id: JsonRpcId::Number(1),
        };

        let hash1 = hash_request(&req1).unwrap();
        let hash2 = hash_request(&req2).unwrap();
        let hash3 = hash_request(&req3).unwrap();

        // Same request produces same hash
        assert_eq!(hash1, hash2);

        // Different request produces different hash
        assert_ne!(hash1, hash3);

        // Hash is a valid hex string
        assert!(hash1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Tests that canonical JSON hashing produces identical hashes regardless of
    /// key insertion order in `serde_json::Value` objects.
    ///
    /// This is critical because `serde_json/preserve_order` is transitively
    /// enabled by `cedar-policy-core`, making `Value::to_string()` dependent
    /// on insertion order.
    ///
    /// Verifies: REQ-GOV-001/F-002.3
    #[test]
    fn test_canonical_json_hash_key_order_independent() {
        // Build two Value::Objects with different key insertion orders
        let args_ab = serde_json::json!({"a": 1, "b": 2});
        let args_ba = serde_json::json!({"b": 2, "a": 1});

        let req_ab = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "test_tool".to_string(),
            arguments: args_ab,
            mcp_request_id: JsonRpcId::Number(1),
        };
        let req_ba = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "test_tool".to_string(),
            arguments: args_ba,
            mcp_request_id: JsonRpcId::Number(2),
        };

        // Must produce identical hashes despite different key orders
        assert_eq!(
            hash_request(&req_ab).unwrap(),
            hash_request(&req_ba).unwrap()
        );

        // Nested objects should also be order-independent
        let nested_1 = serde_json::json!({"outer": {"z": 3, "a": 1}, "list": [1, 2]});
        let nested_2 = serde_json::json!({"list": [1, 2], "outer": {"a": 1, "z": 3}});

        let req_nested_1 = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "nested_tool".to_string(),
            arguments: nested_1,
            mcp_request_id: JsonRpcId::Number(3),
        };
        let req_nested_2 = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "nested_tool".to_string(),
            arguments: nested_2,
            mcp_request_id: JsonRpcId::Number(4),
        };

        assert_eq!(
            hash_request(&req_nested_1).unwrap(),
            hash_request(&req_nested_2).unwrap()
        );
    }

    /// Tests approval recording and state transition.
    ///
    /// Verifies: REQ-GOV-001 (record_approval)
    #[test]
    fn test_approval_recording() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Move to InputRequired
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        // Approve
        let approved = store
            .record_approval(
                &task.id,
                ApprovalDecision::Approved,
                "approver@example.com".to_string(),
                Duration::from_secs(300),
            )
            .unwrap();

        assert_eq!(approved.status, TaskStatus::Executing);
        assert!(approved.approval.is_some());
        let approval = approved.approval.as_ref().unwrap();
        assert_eq!(approval.decision, ApprovalDecision::Approved);
        assert_eq!(approval.decided_by, "approver@example.com");
    }

    /// Tests rejection recording.
    #[test]
    fn test_rejection_recording() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Move to InputRequired
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        // Reject
        let rejected = store
            .record_approval(
                &task.id,
                ApprovalDecision::Rejected {
                    reason: Some("Too risky".to_string()),
                },
                "approver@example.com".to_string(),
                Duration::from_secs(0),
            )
            .unwrap();

        assert_eq!(rejected.status, TaskStatus::Rejected);
        assert!(rejected.approval.is_some());
    }

    /// Tests full task lifecycle: create → approve → complete.
    #[test]
    fn test_full_lifecycle() {
        let store = TaskStore::with_defaults();

        // Create
        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();
        assert_eq!(task.status, TaskStatus::Working);
        assert_eq!(store.pending_count(), 1);

        // Pre-approval complete
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        // Approve
        store
            .record_approval(
                &task.id,
                ApprovalDecision::Approved,
                "approver".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();

        // Executing (still pending)
        let task = store.get(&task.id).unwrap();
        assert_eq!(task.status, TaskStatus::Executing);
        assert_eq!(store.pending_count(), 1);

        // Complete
        let result = ToolCallResult {
            content: serde_json::json!({"success": true}),
            is_error: false,
        };
        let completed = store.complete(&task.id, result).unwrap();

        assert_eq!(completed.status, TaskStatus::Completed);
        assert!(completed.result.is_some());
        assert_eq!(store.pending_count(), 0);
    }

    /// Tests task failure recording.
    #[test]
    fn test_task_failure() {
        let store = TaskStore::with_defaults();

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        let failure = FailureInfo {
            stage: FailureStage::PreHitlInspection,
            reason: "Validation failed".to_string(),
            retriable: false,
        };

        let failed = store.fail(&task.id, failure).unwrap();
        assert_eq!(failed.status, TaskStatus::Failed);
        assert!(failed.failure.is_some());
        assert_eq!(store.pending_count(), 0);
    }

    /// Tests terminal grace period cleanup.
    #[tokio::test]
    async fn test_terminal_cleanup() {
        let config = TaskStoreConfig {
            terminal_grace_period: Duration::from_millis(1),
            ..Default::default()
        };
        let store = TaskStore::new(config);

        let task = store
            .create(
                test_request(),
                test_request(),
                test_principal(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Complete it
        store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();
        store
            .record_approval(
                &task.id,
                ApprovalDecision::Approved,
                "test".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();
        store
            .complete(
                &task.id,
                ToolCallResult {
                    content: serde_json::json!({}),
                    is_error: false,
                },
            )
            .unwrap();

        assert_eq!(store.total_count(), 1);

        // Wait for grace period
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Cleanup
        let removed = store.cleanup_terminal();
        assert_eq!(removed, 1);
        assert_eq!(store.total_count(), 0);
    }

    #[tokio::test]
    async fn test_cleanup_terminal_removes_empty_principal_entries() {
        let config = TaskStoreConfig {
            terminal_grace_period: Duration::from_millis(1),
            ..Default::default()
        };
        let store = TaskStore::new(config);

        let principal = test_principal();

        // Create two tasks for the same principal
        let task1 = store
            .create(
                test_request(),
                test_request(),
                principal.clone(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();
        let task2 = store
            .create(
                test_request(),
                test_request(),
                principal.clone(),
                None,
                TimeoutAction::default(),
            )
            .unwrap();

        // Principal should have entries
        let key = principal.rate_limit_key();
        assert!(
            store.has_principal_entry(&key),
            "by_principal should contain the principal"
        );

        // Complete both tasks
        for task_id in [&task1.id, &task2.id] {
            store
                .transition(task_id, TaskStatus::InputRequired, None)
                .unwrap();
            store
                .record_approval(
                    task_id,
                    ApprovalDecision::Approved,
                    "test".to_string(),
                    Duration::from_secs(60),
                )
                .unwrap();
            store
                .complete(
                    task_id,
                    ToolCallResult {
                        content: serde_json::json!({}),
                        is_error: false,
                    },
                )
                .unwrap();
        }

        // Wait for grace period
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Cleanup should remove both tasks AND the empty principal entry
        let removed = store.cleanup_terminal();
        assert_eq!(removed, 2);
        assert_eq!(store.total_count(), 0);
        assert!(
            !store.has_principal_entry(&key),
            "by_principal should not contain the principal after all tasks cleaned up"
        );
    }

    // ========================================================================
    // canonical_json Tests
    // ========================================================================

    #[test]
    fn test_canonical_json_depth_limit() {
        use helpers::canonical_json;
        // Build deeply nested JSON: {"a": {"a": {"a": ... }}}
        let mut deep = serde_json::json!("leaf");
        for _ in 0..100 {
            deep = serde_json::json!({ "a": deep });
        }

        // Should not stack overflow; returns error on excessive depth
        let result = canonical_json(&deep);
        assert!(result.is_err(), "Expected error for deeply nested JSON");
        assert!(
            result.unwrap_err().contains("nesting depth"),
            "Error should mention nesting depth"
        );
    }

    #[test]
    fn test_canonical_json_sorted_keys() {
        use helpers::canonical_json;
        let json = serde_json::json!({
            "z": "last",
            "a": "first",
            "m": {"nested": "value"}
        });

        let result = canonical_json(&json).unwrap();
        // Keys must be sorted: a before m before z
        let a_pos = result.find("\"a\"").expect("missing key a");
        let m_pos = result.find("\"m\"").expect("missing key m");
        let z_pos = result.find("\"z\"").expect("missing key z");
        assert!(a_pos < m_pos, "a should come before m");
        assert!(m_pos < z_pos, "m should come before z");
    }
}
