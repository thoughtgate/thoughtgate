//! End-to-end integration test for the approval pipeline.
//!
//! Verifies: REQ-GOV-002 (Approval Execution Pipeline)
//!
//! This test wires up all components with mocks:
//! 1. ApprovalEngine with MockAdapter (auto-approve) and MockUpstream
//! 2. Full pipeline: start_approval → poll → execute_on_result
//! 3. Asserts upstream receives the forwarded request and result is returned

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use thoughtgate::error::ThoughtGateError;
use thoughtgate::governance::approval::{
    AdapterError, ApprovalAdapter, ApprovalReference, DecisionMethod, PollDecision, PollResult,
};
use thoughtgate::governance::engine::{ApprovalEngineConfig, TimeoutAction};
use thoughtgate::governance::task::{JsonRpcId, ToolCallRequest};
use thoughtgate::governance::{ApprovalEngine, Principal, TaskStatus, TaskStore};
use thoughtgate::policy::engine::CedarEngine;
use thoughtgate::transport::{JsonRpcResponse, McpRequest, UpstreamForwarder};

// ============================================================================
// Mock Approval Adapter
// ============================================================================

/// Mock adapter that auto-approves instantly for E2E testing.
struct E2eApprovalAdapter {
    post_count: AtomicU32,
    poll_count: AtomicU32,
}

impl E2eApprovalAdapter {
    fn new() -> Self {
        Self {
            post_count: AtomicU32::new(0),
            poll_count: AtomicU32::new(0),
        }
    }

    fn post_count(&self) -> u32 {
        self.post_count.load(Ordering::SeqCst)
    }

    fn poll_count(&self) -> u32 {
        self.poll_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ApprovalAdapter for E2eApprovalAdapter {
    async fn post_approval_request(
        &self,
        request: &thoughtgate::governance::approval::ApprovalRequest,
    ) -> Result<ApprovalReference, AdapterError> {
        self.post_count.fetch_add(1, Ordering::SeqCst);

        Ok(ApprovalReference {
            task_id: request.task_id.clone(),
            external_id: format!("e2e-mock-{}", request.task_id),
            channel: "e2e-test-channel".to_string(),
            posted_at: chrono::Utc::now(),
            // Poll immediately (instant approval)
            next_poll_at: std::time::Instant::now(),
            poll_count: 0,
        })
    }

    async fn poll_for_decision(
        &self,
        _reference: &ApprovalReference,
    ) -> Result<Option<PollResult>, AdapterError> {
        self.poll_count.fetch_add(1, Ordering::SeqCst);

        // Always return approval immediately
        Ok(Some(PollResult {
            decision: PollDecision::Approved,
            decided_by: "e2e-test-approver".to_string(),
            decided_at: chrono::Utc::now(),
            method: DecisionMethod::Reaction {
                emoji: "+1".to_string(),
            },
        }))
    }

    async fn cancel_approval(&self, _reference: &ApprovalReference) -> Result<(), AdapterError> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "e2e-mock"
    }
}

// ============================================================================
// Mock Upstream
// ============================================================================

/// Mock upstream that records forwarded requests and returns configurable responses.
struct E2eUpstream {
    response: Mutex<serde_json::Value>,
    forward_count: AtomicU32,
    last_method: Mutex<Option<String>>,
    last_params: Mutex<Option<serde_json::Value>>,
}

impl E2eUpstream {
    fn new(response: serde_json::Value) -> Self {
        Self {
            response: Mutex::new(response),
            forward_count: AtomicU32::new(0),
            last_method: Mutex::new(None),
            last_params: Mutex::new(None),
        }
    }

    fn forward_count(&self) -> u32 {
        self.forward_count.load(Ordering::SeqCst)
    }

    async fn last_method(&self) -> Option<String> {
        self.last_method.lock().await.clone()
    }

    async fn last_params(&self) -> Option<serde_json::Value> {
        self.last_params.lock().await.clone()
    }
}

#[async_trait]
impl UpstreamForwarder for E2eUpstream {
    async fn forward(&self, request: &McpRequest) -> Result<JsonRpcResponse, ThoughtGateError> {
        self.forward_count.fetch_add(1, Ordering::SeqCst);
        *self.last_method.lock().await = Some(request.method.clone());
        *self.last_params.lock().await = request.params.as_deref().cloned();

        let result = self.response.lock().await.clone();
        Ok(JsonRpcResponse {
            jsonrpc: std::borrow::Cow::Borrowed("2.0"),
            id: Some(thoughtgate::transport::JsonRpcId::Number(1)),
            result: Some(result),
            error: None,
        })
    }

    async fn forward_batch(
        &self,
        requests: &[McpRequest],
    ) -> Result<Vec<JsonRpcResponse>, ThoughtGateError> {
        let mut responses = Vec::with_capacity(requests.len());
        for req in requests {
            responses.push(self.forward(req).await?);
        }
        Ok(responses)
    }
}

// ============================================================================
// E2E Tests
// ============================================================================

/// Full E2E test: start approval → background poll → execute on result.
///
/// Verifies the complete approval pipeline:
/// 1. start_approval creates a task and posts to mock adapter
/// 2. Background polling picks up the mock approval
/// 3. execute_on_result forwards to mock upstream and returns the result
///
/// Implements: REQ-GOV-002 (Approval Execution Pipeline)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_e2e_approval_flow() {
    let task_store = Arc::new(TaskStore::with_defaults());
    let adapter = Arc::new(E2eApprovalAdapter::new());
    let upstream = Arc::new(E2eUpstream::new(serde_json::json!({
        "content": [{"type": "text", "text": "User deleted successfully"}],
        "isError": false
    })));
    let config = ApprovalEngineConfig {
        approval_timeout: Duration::from_secs(60),
        on_timeout: TimeoutAction::Deny,
        execution_timeout: Duration::from_secs(10),
    };
    let shutdown = CancellationToken::new();

    // Wire up the engine
    let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create CedarEngine"));
    let engine = ApprovalEngine::new(
        task_store.clone(),
        adapter.clone(),
        upstream.clone(),
        cedar_engine,
        config,
        shutdown.clone(),
    );

    // Spawn background polling
    engine.spawn_background_tasks().await;

    // Step 1: Start approval
    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "delete_user".to_string(),
        arguments: serde_json::json!({"user_id": "u-42"}),
        mcp_request_id: JsonRpcId::Number(1),
    };
    let principal = Principal::new("e2e-test-app");

    let start_result = engine
        .start_approval(request, principal, None)
        .await
        .expect("start_approval should succeed");

    assert_eq!(start_result.status, TaskStatus::InputRequired);
    assert!(start_result.task_id.is_thoughtgate_owned());

    // Adapter should have been called to post approval
    assert_eq!(adapter.post_count(), 1);

    // Step 2: Wait for background poller to detect the approval
    // The mock adapter returns approval instantly, but the polling loop
    // needs time for the rate limiter (1 token/sec) and scheduling loop.
    let mut approved = false;
    for _ in 0..100 {
        let task = task_store
            .get(&start_result.task_id)
            .expect("Task should exist");
        if task.status == TaskStatus::Executing {
            approved = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        approved,
        "Task should have been approved by background poller"
    );
    assert!(
        adapter.poll_count() >= 1,
        "Adapter should have been polled at least once"
    );

    // Step 3: Execute the approved request
    let result = engine
        .execute_on_result(&start_result.task_id)
        .await
        .expect("execute_on_result should succeed");

    // Verify result
    assert!(!result.is_error);
    assert_eq!(
        result.content["content"][0]["text"],
        "User deleted successfully"
    );

    // Verify upstream received the forwarded request
    assert_eq!(upstream.forward_count(), 1);
    let method = upstream.last_method().await;
    assert_eq!(method.as_deref(), Some("tools/call"));

    let params = upstream.last_params().await.expect("params should exist");
    assert_eq!(params["name"], "delete_user");
    assert_eq!(params["arguments"]["user_id"], "u-42");

    // Verify task is completed
    let final_task = task_store
        .get(&start_result.task_id)
        .expect("Task should still exist");
    assert_eq!(
        final_task.status,
        TaskStatus::Completed,
        "Task should be in Completed state"
    );

    // Clean shutdown
    shutdown.cancel();
}

/// E2E test: rejection flow.
///
/// Verifies that a rejected approval returns the correct error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_e2e_rejection_flow() {
    // Adapter that always rejects
    struct RejectingAdapter;

    #[async_trait]
    impl ApprovalAdapter for RejectingAdapter {
        async fn post_approval_request(
            &self,
            request: &thoughtgate::governance::approval::ApprovalRequest,
        ) -> Result<ApprovalReference, AdapterError> {
            Ok(ApprovalReference {
                task_id: request.task_id.clone(),
                external_id: "reject-mock".to_string(),
                channel: "reject-channel".to_string(),
                posted_at: chrono::Utc::now(),
                next_poll_at: std::time::Instant::now(),
                poll_count: 0,
            })
        }

        async fn poll_for_decision(
            &self,
            _reference: &ApprovalReference,
        ) -> Result<Option<PollResult>, AdapterError> {
            Ok(Some(PollResult {
                decision: PollDecision::Rejected,
                decided_by: "security-reviewer".to_string(),
                decided_at: chrono::Utc::now(),
                method: DecisionMethod::Reaction {
                    emoji: "-1".to_string(),
                },
            }))
        }

        async fn cancel_approval(
            &self,
            _reference: &ApprovalReference,
        ) -> Result<(), AdapterError> {
            Ok(())
        }

        fn name(&self) -> &'static str {
            "rejecting-mock"
        }
    }

    let task_store = Arc::new(TaskStore::with_defaults());
    let adapter = Arc::new(RejectingAdapter);
    let upstream = Arc::new(E2eUpstream::new(serde_json::json!({})));
    let config = ApprovalEngineConfig::default();
    let shutdown = CancellationToken::new();

    let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create CedarEngine"));
    let engine = ApprovalEngine::new(
        task_store.clone(),
        adapter,
        upstream.clone(),
        cedar_engine,
        config,
        shutdown.clone(),
    );

    engine.spawn_background_tasks().await;

    // Start approval
    let request = ToolCallRequest {
        method: "tools/call".to_string(),
        name: "drop_database".to_string(),
        arguments: serde_json::json!({}),
        mcp_request_id: JsonRpcId::Number(2),
    };

    let start_result = engine
        .start_approval(request, Principal::new("test-app"), None)
        .await
        .expect("start_approval should succeed");

    // Wait for rejection
    // Background poller needs time for rate limiter + scheduling loop
    let mut rejected = false;
    for _ in 0..100 {
        let task = task_store
            .get(&start_result.task_id)
            .expect("Task should exist");
        if task.status == TaskStatus::Rejected {
            rejected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        rejected,
        "Task should have been rejected by background poller"
    );

    // Execute should return ApprovalRejected error
    let result = engine.execute_on_result(&start_result.task_id).await;
    assert!(result.is_err());

    match result.unwrap_err() {
        ThoughtGateError::ApprovalRejected { tool, .. } => {
            assert_eq!(tool, "drop_database");
        }
        other => panic!("Expected ApprovalRejected, got: {other:?}"),
    }

    // Upstream should NOT have been called
    assert_eq!(upstream.forward_count(), 0);

    shutdown.cancel();
}
