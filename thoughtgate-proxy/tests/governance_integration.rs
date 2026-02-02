//! Governance Integration Tests for ThoughtGate.
//!
//! # Traceability
//! - Implements: REQ-GOV-001 (Task Lifecycle)
//! - Implements: REQ-GOV-002 (Execution Pipeline)
//! - Implements: REQ-GOV-003 (Approval Integration)
//! - Implements: REQ-CORE-003 (4-Gate Decision Flow)
//!
//! These tests verify that the governance components work together correctly:
//! - 4-Gate decision flow (visibility, rules, Cedar policy, approval)
//! - Task lifecycle state machine
//! - Approval pipeline coordination
//! - Error propagation

mod helpers;

use helpers::{
    MockUpstream, TestClient, config_with_approval_yaml, config_with_deny_yaml, minimal_config_yaml,
};
use serde_json::json;
use serial_test::serial;
use std::sync::Arc;
use std::time::Duration;
use thoughtgate_core::config::Config;
use thoughtgate_core::governance::approval::MockAdapter;
use thoughtgate_core::governance::task::JsonRpcId;
use thoughtgate_core::governance::{
    ApprovalDecision, Principal, TaskHandler, TaskId, TaskStatus, TaskStore, TaskStoreConfig,
    TimeoutAction, ToolCallRequest,
};

// ============================================================================
// Task Store Integration Tests
// ============================================================================

/// Test: Task creation and retrieval works correctly.
///
/// Verifies: REQ-GOV-001/F-001 (Task Creation)
#[test]
fn test_task_creation_and_retrieval() {
    let store = Arc::new(TaskStore::with_defaults());
    let _handler = TaskHandler::new(store.clone());

    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "delete_user".to_string(),
        arguments: json!({"user_id": "123"}),
        mcp_request_id: JsonRpcId::Number(1),
    };

    let principal = Principal::new("test-app");

    // Create task
    let task = store
        .create(
            request.clone(),
            request,
            principal.clone(),
            Some(Duration::from_secs(300)),
            TimeoutAction::Deny,
        )
        .expect("Failed to create task");

    // Verify task ID format
    assert!(
        task.id.as_str().starts_with("tg_"),
        "Task ID should have tg_ prefix"
    );

    // Retrieve via handler
    let retrieved = store.get(&task.id).expect("Failed to retrieve task");
    assert_eq!(retrieved.id, task.id);
    assert_eq!(retrieved.status, TaskStatus::Working);
    assert_eq!(retrieved.principal, principal);
}

/// Test: Task state transitions follow the FSM correctly.
///
/// Verifies: REQ-GOV-001/F-002 (Task State Machine)
#[test]
fn test_task_state_transitions() {
    let store = Arc::new(TaskStore::with_defaults());

    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "delete_user".to_string(),
        arguments: json!({"user_id": "123"}),
        mcp_request_id: JsonRpcId::Number(1),
    };

    let task = store
        .create(
            request.clone(),
            request,
            Principal::new("test-app"),
            None,
            TimeoutAction::Deny,
        )
        .unwrap();

    // Working -> InputRequired
    store
        .transition(&task.id, TaskStatus::InputRequired, None)
        .expect("Should transition to InputRequired");

    let task = store.get(&task.id).unwrap();
    assert_eq!(task.status, TaskStatus::InputRequired);

    // Record approval (automatically transitions to Executing)
    let task = store
        .record_approval(
            &task.id,
            ApprovalDecision::Approved,
            "test-user".to_string(),
            Duration::from_secs(60),
        )
        .expect("Should record approval");

    // record_approval auto-transitions to Executing
    assert_eq!(task.status, TaskStatus::Executing);

    // Executing -> Completed
    store
        .complete(
            &task.id,
            thoughtgate_core::governance::ToolCallResult {
                content: json!({"success": true}),
                is_error: false,
            },
        )
        .expect("Should complete task");

    let task = store.get(&task.id).unwrap();
    assert_eq!(task.status, TaskStatus::Completed);
}

/// Test: Task rejection flow updates state correctly.
///
/// Verifies: REQ-GOV-001/F-002 (Rejection Path)
#[test]
fn test_task_rejection_flow() {
    let store = Arc::new(TaskStore::with_defaults());

    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "delete_user".to_string(),
        arguments: json!({"user_id": "123"}),
        mcp_request_id: JsonRpcId::Number(1),
    };

    let task = store
        .create(
            request.clone(),
            request,
            Principal::new("test-app"),
            None,
            TimeoutAction::Deny,
        )
        .unwrap();

    // Move to InputRequired
    store
        .transition(&task.id, TaskStatus::InputRequired, None)
        .unwrap();

    // Record rejection
    store
        .record_approval(
            &task.id,
            ApprovalDecision::Rejected {
                reason: Some("Not authorized".to_string()),
            },
            "admin".to_string(),
            Duration::from_secs(30),
        )
        .expect("Should record rejection");

    let task = store.get(&task.id).unwrap();
    assert_eq!(task.status, TaskStatus::Rejected);
    assert!(task.approval.is_some());
    assert!(matches!(
        task.approval.as_ref().unwrap().decision,
        ApprovalDecision::Rejected { .. }
    ));
}

/// Test: Principal isolation - can't see other principal's tasks.
///
/// Verifies: REQ-GOV-001/F-005 (Principal Isolation)
#[test]
fn test_principal_isolation() {
    let store = Arc::new(TaskStore::with_defaults());

    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "test_tool".to_string(),
        arguments: json!({}),
        mcp_request_id: JsonRpcId::Number(1),
    };

    let principal_a = Principal::new("app-a");
    let principal_b = Principal::new("app-b");

    // Create tasks for both principals
    store
        .create(
            request.clone(),
            request.clone(),
            principal_a.clone(),
            None,
            TimeoutAction::Deny,
        )
        .unwrap();

    store
        .create(
            request.clone(),
            request,
            principal_b.clone(),
            None,
            TimeoutAction::Deny,
        )
        .unwrap();

    // List for principal A
    let tasks_a = store.list_for_principal(&principal_a, 0, 100);
    assert_eq!(tasks_a.len(), 1);

    // List for principal B
    let tasks_b = store.list_for_principal(&principal_b, 0, 100);
    assert_eq!(tasks_b.len(), 1);

    // Different task IDs
    assert_ne!(tasks_a[0].id, tasks_b[0].id);
}

/// Test: Max pending tasks per principal is enforced.
///
/// Verifies: REQ-GOV-001/F-006 (Rate Limiting)
#[test]
fn test_max_pending_per_principal() {
    let config = TaskStoreConfig {
        max_pending_per_principal: 3,
        ..Default::default()
    };
    let store = Arc::new(TaskStore::new(config));

    let principal = Principal::new("limited-app");

    // Create 3 tasks (should succeed)
    for i in 0..3 {
        let request = ToolCallRequest {
            method: "tools/call".to_string(),
            name: format!("tool_{}", i),
            arguments: json!({}),
            mcp_request_id: JsonRpcId::Number(i as i64),
        };

        store
            .create(
                request.clone(),
                request,
                principal.clone(),
                None,
                TimeoutAction::Deny,
            )
            .unwrap_or_else(|_| panic!("Should create task {}", i));
    }

    // 4th task should fail
    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "tool_overflow".to_string(),
        arguments: json!({}),
        mcp_request_id: JsonRpcId::Number(99),
    };

    let result = store.create(
        request.clone(),
        request,
        principal,
        None,
        TimeoutAction::Deny,
    );

    assert!(
        result.is_err(),
        "Should reject when max pending tasks exceeded"
    );
}

/// Test: Task cancellation works from InputRequired state.
///
/// Verifies: REQ-GOV-001/F-006 (Task Cancellation)
#[test]
fn test_task_cancellation() {
    let store = Arc::new(TaskStore::with_defaults());

    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "cancellable_tool".to_string(),
        arguments: json!({}),
        mcp_request_id: JsonRpcId::Number(1),
    };

    let task = store
        .create(
            request.clone(),
            request,
            Principal::new("test-app"),
            None,
            TimeoutAction::Deny,
        )
        .unwrap();

    // Move to InputRequired (cancellable)
    store
        .transition(&task.id, TaskStatus::InputRequired, None)
        .unwrap();

    // Cancel
    store.cancel(&task.id).expect("Should cancel task");

    let task = store.get(&task.id).unwrap();
    assert_eq!(task.status, TaskStatus::Cancelled);
}

/// Test: Cannot cancel a completed task.
///
/// Verifies: REQ-GOV-001/EC-TASK-003
#[test]
fn test_cannot_cancel_completed_task() {
    let store = Arc::new(TaskStore::with_defaults());

    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "test_tool".to_string(),
        arguments: json!({}),
        mcp_request_id: JsonRpcId::Number(1),
    };

    let task = store
        .create(
            request.clone(),
            request,
            Principal::new("test-app"),
            None,
            TimeoutAction::Deny,
        )
        .unwrap();

    // Complete the task via full lifecycle
    store
        .transition(&task.id, TaskStatus::InputRequired, None)
        .unwrap();
    // record_approval automatically transitions to Executing
    store
        .record_approval(
            &task.id,
            ApprovalDecision::Approved,
            "tester".to_string(),
            Duration::from_secs(60),
        )
        .unwrap();
    // Now complete (already in Executing state)
    store
        .complete(
            &task.id,
            thoughtgate_core::governance::ToolCallResult {
                content: json!({"done": true}),
                is_error: false,
            },
        )
        .unwrap();

    // Try to cancel
    let result = store.cancel(&task.id);
    assert!(result.is_err(), "Should not cancel completed task");
}

// ============================================================================
// Mock Adapter Integration Tests
// ============================================================================

/// Test: MockAdapter instant approval works.
///
/// Verifies: REQ-GOV-003/T-001 (Testability)
#[tokio::test]
async fn test_mock_adapter_instant_approval() {
    use chrono::Utc;
    use thoughtgate_core::governance::ApprovalAdapter;
    use thoughtgate_core::governance::ApprovalRequest;

    let adapter = MockAdapter::instant_approve();
    let request = ApprovalRequest {
        task_id: TaskId::new(),
        tool_name: "test_tool".to_string(),
        tool_arguments: json!({"key": "value"}),
        principal: Principal::new("test-app"),
        expires_at: Utc::now() + chrono::Duration::minutes(5),
        created_at: Utc::now(),
        correlation_id: "test-123".to_string(),
    };

    // Post request
    let reference = adapter
        .post_approval_request(&request)
        .await
        .expect("Should post request");

    // Poll immediately - should get approval
    let result = adapter
        .poll_for_decision(&reference)
        .await
        .expect("Should poll");

    assert!(result.is_some());
    assert_eq!(
        result.unwrap().decision,
        thoughtgate_core::governance::PollDecision::Approved
    );
}

/// Test: MockAdapter delayed approval respects timing.
///
/// Verifies: REQ-GOV-003/T-001 (Testability)
#[tokio::test]
async fn test_mock_adapter_delayed_approval() {
    use chrono::Utc;
    use thoughtgate_core::governance::ApprovalAdapter;
    use thoughtgate_core::governance::ApprovalRequest;

    let adapter = MockAdapter::new(Duration::from_millis(100), true);
    let request = ApprovalRequest {
        task_id: TaskId::new(),
        tool_name: "test_tool".to_string(),
        tool_arguments: json!({}),
        principal: Principal::new("test-app"),
        expires_at: Utc::now() + chrono::Duration::minutes(5),
        created_at: Utc::now(),
        correlation_id: "test-456".to_string(),
    };

    let reference = adapter.post_approval_request(&request).await.unwrap();

    // Poll immediately - should be pending
    let result = adapter.poll_for_decision(&reference).await.unwrap();
    assert!(result.is_none(), "Should be pending before delay");

    // Wait for delay
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Poll again - should be approved
    let result = adapter.poll_for_decision(&reference).await.unwrap();
    assert!(result.is_some(), "Should be approved after delay");
}

// ============================================================================
// TaskStore Sharing Tests
// ============================================================================

/// Test: TaskStore is correctly shared between components.
///
/// Verifies: REQ-GOV-002 (Pipeline Coordination)
#[test]
fn test_taskstore_shared_between_components() {
    let store = Arc::new(TaskStore::with_defaults());

    // Create handler with the store
    let handler = TaskHandler::new(store.clone());

    // Verify they share the same store (via task creation/retrieval)
    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "shared_test".to_string(),
        arguments: json!({}),
        mcp_request_id: JsonRpcId::Number(1),
    };

    let task = store
        .create(
            request.clone(),
            request,
            Principal::new("test-app"),
            None,
            TimeoutAction::Deny,
        )
        .unwrap();

    // Handler should see the same task
    let retrieved = handler.store().get(&task.id).unwrap();
    assert_eq!(retrieved.id, task.id);

    // Modification via store should be visible via handler
    store
        .transition(&task.id, TaskStatus::InputRequired, None)
        .unwrap();

    let updated = handler.store().get(&task.id).unwrap();
    assert_eq!(updated.status, TaskStatus::InputRequired);
}

// ============================================================================
// Concurrent Access Tests
// ============================================================================

/// Test: Concurrent task creation succeeds.
///
/// Verifies: REQ-GOV-001/NF-001 (Thread Safety)
#[tokio::test]
async fn test_concurrent_task_creation() {
    let store = Arc::new(TaskStore::with_defaults());
    let principal = Principal::new("concurrent-app");

    let mut handles = Vec::new();

    for i in 0..10 {
        let store = store.clone();
        let principal = principal.clone();

        handles.push(tokio::spawn(async move {
            let request = ToolCallRequest {
                method: "tools/call".to_string(),
                name: format!("tool_{}", i),
                arguments: json!({"index": i}),
                mcp_request_id: JsonRpcId::Number(i as i64),
            };

            store.create(
                request.clone(),
                request,
                principal,
                None,
                TimeoutAction::Deny,
            )
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // All should succeed
    for result in results {
        assert!(
            result.unwrap().is_ok(),
            "Concurrent task creation should succeed"
        );
    }

    // Verify all tasks exist
    let tasks = store.list_for_principal(&principal, 0, 100);
    assert_eq!(tasks.len(), 10);
}

// ============================================================================
// Mock Upstream Integration Tests
// ============================================================================

/// Test: Mock upstream handles tools/list correctly.
#[tokio::test]
async fn test_mock_upstream_tools_list() {
    let mock = MockUpstream::new()
        .with_tool("read_file", "Read a file", json!({"content": "test"}))
        .with_tool("write_file", "Write a file", json!({"success": true}));

    let (addr, handle) = mock.start().await;

    let client = TestClient::new(&format!("http://{}", addr));
    let response = client.tools_list().await;

    assert!(response.is_success());
    let result = response.result.unwrap();
    let tools = result["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(handle.request_count().await, 1);
}

/// Test: Mock upstream handles tools/call correctly.
#[tokio::test]
async fn test_mock_upstream_tools_call() {
    let mock =
        MockUpstream::new().with_tool("echo", "Echo back input", json!({"echoed": "hello world"}));

    let (addr, _handle) = mock.start().await;

    let client = TestClient::new(&format!("http://{}", addr));
    let response = client
        .tools_call("echo", json!({"input": "hello world"}))
        .await;

    assert!(response.is_success());
    let result = response.result.unwrap();
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("echoed")
    );
}

/// Test: Mock upstream returns configured error.
#[tokio::test]
async fn test_mock_upstream_error_response() {
    let mock = MockUpstream::new()
        .with_tool("failing_tool", "Always fails", json!({}))
        .with_error("failing_tool", -32000, "Simulated upstream failure");

    let (addr, _handle) = mock.start().await;

    let client = TestClient::new(&format!("http://{}", addr));
    let response = client.tools_call("failing_tool", json!({})).await;

    assert!(response.is_error());
    assert_eq!(response.error_code(), Some(-32000));
    assert!(response.error.unwrap().message.contains("Simulated"));
}

// ============================================================================
// Configuration Parsing Tests
// ============================================================================

/// Test: Minimal config parses correctly.
#[test]
#[serial]
fn test_minimal_config_parsing() {
    // Set required env var (unsafe in Rust 2024 edition)
    // SAFETY: This is a test environment with controlled access
    unsafe {
        std::env::set_var("THOUGHTGATE_UPSTREAM", "http://localhost:9999");
    }

    let yaml = minimal_config_yaml();
    let config: Config = serde_saphyr::from_str(&yaml).expect("Should parse minimal config");

    assert_eq!(config.schema, 1);
    assert_eq!(config.sources.len(), 1);
    assert_eq!(
        config.governance.defaults.action,
        thoughtgate_core::config::Action::Forward
    );

    unsafe {
        std::env::remove_var("THOUGHTGATE_UPSTREAM");
    }
}

/// Test: Config with approval workflow parses correctly.
#[test]
#[serial]
fn test_config_with_approval_parsing() {
    unsafe {
        std::env::set_var("THOUGHTGATE_UPSTREAM", "http://localhost:9999");
    }

    let yaml = config_with_approval_yaml("default", "delete_*");
    let config: Config = serde_saphyr::from_str(&yaml).expect("Should parse approval config");

    assert_eq!(config.governance.rules.len(), 1);
    assert_eq!(
        config.governance.rules[0].action,
        thoughtgate_core::config::Action::Approve
    );
    assert!(
        config
            .approval
            .as_ref()
            .is_some_and(|a| a.contains_key("default"))
    );

    unsafe {
        std::env::remove_var("THOUGHTGATE_UPSTREAM");
    }
}

/// Test: Config with deny rule parses correctly.
#[test]
#[serial]
fn test_config_with_deny_parsing() {
    unsafe {
        std::env::set_var("THOUGHTGATE_UPSTREAM", "http://localhost:9999");
    }

    let yaml = config_with_deny_yaml("dangerous_*");
    let config: Config = serde_saphyr::from_str(&yaml).expect("Should parse deny config");

    assert_eq!(config.governance.rules.len(), 1);
    assert_eq!(
        config.governance.rules[0].action,
        thoughtgate_core::config::Action::Deny
    );

    unsafe {
        std::env::remove_var("THOUGHTGATE_UPSTREAM");
    }
}

// ============================================================================
// 4-Gate Decision Flow Tests (REQ-CORE-003)
// ============================================================================

/// Test: Gate 2 - Forward action allows request through.
///
/// Verifies: REQ-CORE-003/Gate-2, REQ-POL-001/F-001
///
/// When governance rules match with action=forward, the request should
/// be sent directly to upstream without policy evaluation or approval.
#[tokio::test]
#[serial]
async fn test_gate2_forward_action_passthrough() {
    use bytes::Bytes;
    use std::sync::Arc;
    use thoughtgate_core::policy::engine::CedarEngine;
    use thoughtgate_proxy::mcp_handler::{McpHandler, McpHandlerConfig};

    // SAFETY: Test environment with controlled access
    unsafe {
        std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
    }

    // Set up mock upstream
    let mock = MockUpstream::new().with_tool("safe_tool", "A safe tool", json!({"result": "ok"}));
    let (addr, _handle) = mock.start().await;

    // Create handler with config that forwards safe_tool
    let upstream = Arc::new(
        thoughtgate_core::transport::UpstreamClient::new(
            thoughtgate_core::transport::UpstreamConfig {
                base_url: format!("http://{}", addr),
                ..Default::default()
            },
        )
        .expect("Failed to create upstream"),
    );

    let cedar_engine = Arc::new(CedarEngine::new().unwrap());
    let task_store = Arc::new(TaskStore::with_defaults());

    // Create a minimal config with forward default
    let handler = McpHandler::with_governance(
        upstream,
        cedar_engine,
        task_store,
        McpHandlerConfig::default(),
        None, // No YAML config = Cedar-only mode (default forward)
        None, // No approval engine
    );

    // Send a tools/call request
    let request = json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "safe_tool",
            "arguments": {"input": "test"}
        },
        "id": 1
    });

    let (status, body) = handler.handle(Bytes::from(request.to_string())).await;

    assert_eq!(status, axum::http::StatusCode::OK);
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        response.get("result").is_some(),
        "Should have result: {}",
        response
    );
    assert!(response.get("error").is_none(), "Should not have error");

    unsafe {
        std::env::remove_var("THOUGHTGATE_DEV_MODE");
    }
}

/// Test: Gate 2 - Deny action rejects request immediately.
///
/// Verifies: REQ-CORE-003/Gate-2, REQ-POL-001/F-002
///
/// When governance rules match with action=deny, the request should
/// be rejected with error code -32014 (Governance Rule Denied).
#[tokio::test]
#[serial]
async fn test_gate2_deny_action_rejects() {
    use bytes::Bytes;
    use std::sync::Arc;
    use thoughtgate_core::config::Config;
    use thoughtgate_core::policy::engine::CedarEngine;
    use thoughtgate_proxy::mcp_handler::{McpHandler, McpHandlerConfig};

    // SAFETY: Test environment with controlled access
    unsafe {
        std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
    }

    // Set up mock upstream (won't be called due to deny)
    let mock = MockUpstream::new().with_tool(
        "dangerous_tool",
        "A dangerous tool",
        json!({"result": "should not see"}),
    );
    let (addr, _handle) = mock.start().await;

    unsafe {
        std::env::set_var("THOUGHTGATE_UPSTREAM", format!("http://{}", addr));
    }

    // Parse config with deny rule
    let yaml = config_with_deny_yaml("dangerous_*");
    let config: Config = serde_saphyr::from_str(&yaml).expect("Should parse config");

    let upstream = Arc::new(
        thoughtgate_core::transport::UpstreamClient::new(
            thoughtgate_core::transport::UpstreamConfig {
                base_url: format!("http://{}", addr),
                ..Default::default()
            },
        )
        .expect("Failed to create upstream"),
    );

    let cedar_engine = Arc::new(CedarEngine::new().unwrap());
    let task_store = Arc::new(TaskStore::with_defaults());

    let handler = McpHandler::with_governance(
        upstream,
        cedar_engine,
        task_store,
        McpHandlerConfig::default(),
        Some(Arc::new(config)),
        None, // No approval engine
    );

    // Send a tools/call request for the denied tool
    let request = json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "dangerous_tool",
            "arguments": {}
        },
        "id": 1
    });

    let (status, body) = handler.handle(Bytes::from(request.to_string())).await;

    assert_eq!(status, axum::http::StatusCode::OK); // JSON-RPC errors return 200
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(response.get("error").is_some(), "Should have error");
    let error = response.get("error").unwrap();
    // Should be -32014 (GOVERNANCE_RULE_DENIED) for governance deny
    assert_eq!(error["code"].as_i64(), Some(-32014));

    unsafe {
        std::env::remove_var("THOUGHTGATE_UPSTREAM");
        std::env::remove_var("THOUGHTGATE_DEV_MODE");
    }
}

/// Test: Gate 3 - Cedar permit continues to execution.
///
/// Verifies: REQ-POL-001/F-001 (Cedar Policy Evaluation)
///
/// When Cedar policy permits the request, it should continue to execution.
#[tokio::test]
#[serial]
async fn test_gate3_cedar_permit_continues() {
    use bytes::Bytes;
    use std::sync::Arc;
    use thoughtgate_core::policy::engine::CedarEngine;
    use thoughtgate_proxy::mcp_handler::{McpHandler, McpHandlerConfig};

    // SAFETY: Test environment with controlled access
    unsafe {
        std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
    }

    // Set up mock upstream
    let mock =
        MockUpstream::new().with_tool("test_tool", "A test tool", json!({"permitted": true}));
    let (addr, _handle) = mock.start().await;

    let upstream = Arc::new(
        thoughtgate_core::transport::UpstreamClient::new(
            thoughtgate_core::transport::UpstreamConfig {
                base_url: format!("http://{}", addr),
                ..Default::default()
            },
        )
        .expect("Failed to create upstream"),
    );

    // Create Cedar engine with permit-all policy (default)
    let cedar_engine = Arc::new(CedarEngine::new().unwrap());
    let task_store = Arc::new(TaskStore::with_defaults());

    let handler = McpHandler::with_governance(
        upstream,
        cedar_engine,
        task_store,
        McpHandlerConfig::default(),
        None, // Cedar-only mode
        None,
    );

    let request = json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "test_tool",
            "arguments": {}
        },
        "id": 1
    });

    let (status, body) = handler.handle(Bytes::from(request.to_string())).await;

    assert_eq!(status, axum::http::StatusCode::OK);
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        response.get("result").is_some(),
        "Should have result (Cedar permitted)"
    );

    unsafe {
        std::env::remove_var("THOUGHTGATE_DEV_MODE");
    }
}

/// Test: Full pipeline - non-tools methods bypass governance.
///
/// Verifies: REQ-CORE-003/F-002 (Method Routing)
///
/// Methods like tools/list, prompts/list, resources/list should bypass
/// governance gates and be forwarded directly to upstream.
#[tokio::test]
#[serial]
async fn test_non_tool_methods_bypass_governance() {
    use bytes::Bytes;
    use std::sync::Arc;
    use thoughtgate_core::config::Config;
    use thoughtgate_core::policy::engine::CedarEngine;
    use thoughtgate_proxy::mcp_handler::{McpHandler, McpHandlerConfig};

    // SAFETY: Test environment with controlled access
    unsafe {
        std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
    }

    // Set up mock upstream with tools
    let mock = MockUpstream::new()
        .with_tool("tool1", "First tool", json!({}))
        .with_tool("tool2", "Second tool", json!({}));
    let (addr, _handle) = mock.start().await;

    unsafe {
        std::env::set_var("THOUGHTGATE_UPSTREAM", format!("http://{}", addr));
    }

    // Parse config that denies all tools
    let yaml = config_with_deny_yaml("*");
    let config: Config = serde_saphyr::from_str(&yaml).expect("Should parse config");

    let upstream = Arc::new(
        thoughtgate_core::transport::UpstreamClient::new(
            thoughtgate_core::transport::UpstreamConfig {
                base_url: format!("http://{}", addr),
                ..Default::default()
            },
        )
        .expect("Failed to create upstream"),
    );

    let cedar_engine = Arc::new(CedarEngine::new().unwrap());
    let task_store = Arc::new(TaskStore::with_defaults());

    let handler = McpHandler::with_governance(
        upstream,
        cedar_engine,
        task_store,
        McpHandlerConfig::default(),
        Some(Arc::new(config)),
        None,
    );

    // tools/list should bypass governance even with deny-all config
    let request = json!({
        "jsonrpc": "2.0",
        "method": "tools/list",
        "params": {},
        "id": 1
    });

    let (status, body) = handler.handle(Bytes::from(request.to_string())).await;

    assert_eq!(status, axum::http::StatusCode::OK);
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        response.get("result").is_some(),
        "tools/list should succeed"
    );
    let tools = response["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2, "Should list both tools");

    unsafe {
        std::env::remove_var("THOUGHTGATE_UPSTREAM");
        std::env::remove_var("THOUGHTGATE_DEV_MODE");
    }
}

/// Test: Task methods are handled locally without forwarding.
///
/// Verifies: REQ-GOV-001/F-003 through F-006 (Task Methods)
///
/// SEP-1686 task methods (tasks/get, tasks/list, etc.) should be handled
/// by the local TaskHandler, not forwarded to upstream.
#[tokio::test]
#[serial]
async fn test_task_methods_handled_locally() {
    use bytes::Bytes;
    use std::sync::Arc;
    use thoughtgate_core::policy::engine::CedarEngine;
    use thoughtgate_proxy::mcp_handler::{McpHandler, McpHandlerConfig};

    // SAFETY: Test environment with controlled access
    unsafe {
        std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
    }

    // Set up mock upstream (shouldn't receive task requests)
    let mock = MockUpstream::new();
    let (addr, handle) = mock.start().await;

    let upstream = Arc::new(
        thoughtgate_core::transport::UpstreamClient::new(
            thoughtgate_core::transport::UpstreamConfig {
                base_url: format!("http://{}", addr),
                ..Default::default()
            },
        )
        .expect("Failed to create upstream"),
    );

    let cedar_engine = Arc::new(CedarEngine::new().unwrap());
    let task_store = Arc::new(TaskStore::with_defaults());

    // Pre-create a task so we have something to query.
    // Use "dev-app" principal to match the dev mode principal from infer_principal()
    // (THOUGHTGATE_DEV_MODE=true defaults to app_name="dev-app").
    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "test_tool".to_string(),
        arguments: json!({}),
        mcp_request_id: JsonRpcId::Number(99),
    };
    let task = task_store
        .create(
            request.clone(),
            request,
            Principal::new("dev-app"),
            None,
            TimeoutAction::Deny,
        )
        .unwrap();

    let handler = McpHandler::with_governance(
        upstream,
        cedar_engine,
        task_store,
        McpHandlerConfig::default(),
        None,
        None,
    );

    // tasks/get should be handled locally
    // Note: SEP-1686 uses camelCase for JSON field names
    let request = json!({
        "jsonrpc": "2.0",
        "method": "tasks/get",
        "params": {
            "taskId": task.id.as_str()
        },
        "id": 1
    });

    let (_status, body) = handler.handle(Bytes::from(request.to_string())).await;
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(
        response.get("result").is_some(),
        "tasks/get should return result: {}",
        response
    );
    assert_eq!(response["result"]["taskId"], task.id.as_str());

    // Verify upstream wasn't called
    assert_eq!(
        handle.request_count().await,
        0,
        "Upstream should not receive task requests"
    );

    unsafe {
        std::env::remove_var("THOUGHTGATE_DEV_MODE");
    }
}

/// Test: Batch requests are processed correctly.
///
/// Verifies: REQ-CORE-003/F-001 (JSON-RPC Batching)
///
/// Multiple requests in a batch should be processed independently,
/// with each going through the appropriate gates.
#[tokio::test]
#[serial]
async fn test_batch_requests_processed_correctly() {
    use bytes::Bytes;
    use std::sync::Arc;
    use thoughtgate_core::policy::engine::CedarEngine;
    use thoughtgate_proxy::mcp_handler::{McpHandler, McpHandlerConfig};

    // SAFETY: Test environment with controlled access
    unsafe {
        std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
    }

    // Set up mock upstream
    let mock = MockUpstream::new().with_tool("allowed_tool", "Allowed", json!({"status": "ok"}));
    let (addr, _handle) = mock.start().await;

    let upstream = Arc::new(
        thoughtgate_core::transport::UpstreamClient::new(
            thoughtgate_core::transport::UpstreamConfig {
                base_url: format!("http://{}", addr),
                ..Default::default()
            },
        )
        .expect("Failed to create upstream"),
    );

    let cedar_engine = Arc::new(CedarEngine::new().unwrap());
    let task_store = Arc::new(TaskStore::with_defaults());

    let handler = McpHandler::with_governance(
        upstream,
        cedar_engine,
        task_store,
        McpHandlerConfig::default(),
        None,
        None,
    );

    // Batch with multiple request types
    let batch = json!([
        {
            "jsonrpc": "2.0",
            "method": "tools/list",
            "params": {},
            "id": 1
        },
        {
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "allowed_tool",
                "arguments": {}
            },
            "id": 2
        }
    ]);

    let (status, body) = handler.handle(Bytes::from(batch.to_string())).await;

    assert_eq!(status, axum::http::StatusCode::OK);
    let responses: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(responses.len(), 2, "Should have 2 responses");

    // Check each response
    let resp1 = responses.iter().find(|r| r["id"] == 1).unwrap();
    assert!(
        resp1.get("result").is_some(),
        "tools/list should succeed: {}",
        resp1
    );

    let resp2 = responses.iter().find(|r| r["id"] == 2).unwrap();
    assert!(
        resp2.get("result").is_some(),
        "tools/call should succeed: {}",
        resp2
    );

    unsafe {
        std::env::remove_var("THOUGHTGATE_DEV_MODE");
    }
}

/// Test: Invalid JSON-RPC returns parse error.
///
/// Verifies: REQ-CORE-004/EC-MCP-001 (Parse Error Handling)
///
/// Malformed JSON should return -32700 (Parse Error).
#[tokio::test]
async fn test_invalid_json_returns_parse_error() {
    use bytes::Bytes;
    use std::sync::Arc;
    use thoughtgate_core::policy::engine::CedarEngine;
    use thoughtgate_proxy::mcp_handler::{McpHandler, McpHandlerConfig};

    let mock = MockUpstream::new();
    let (addr, _handle) = mock.start().await;

    let upstream = Arc::new(
        thoughtgate_core::transport::UpstreamClient::new(
            thoughtgate_core::transport::UpstreamConfig {
                base_url: format!("http://{}", addr),
                ..Default::default()
            },
        )
        .expect("Failed to create upstream"),
    );

    let cedar_engine = Arc::new(CedarEngine::new().unwrap());
    let task_store = Arc::new(TaskStore::with_defaults());

    let handler = McpHandler::with_governance(
        upstream,
        cedar_engine,
        task_store,
        McpHandlerConfig::default(),
        None,
        None,
    );

    // Send invalid JSON
    let invalid = "{ not valid json }";
    let (status, body) = handler.handle(Bytes::from(invalid)).await;

    assert_eq!(status, axum::http::StatusCode::OK); // JSON-RPC errors return 200
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(response.get("error").is_some());
    assert_eq!(response["error"]["code"].as_i64(), Some(-32700)); // Parse error
}
