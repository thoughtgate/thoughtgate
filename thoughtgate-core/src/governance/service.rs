//! Governance HTTP service for stdio shim communication.
//!
//! Implements: REQ-CORE-008/F-008 (Governance Service Startup),
//!             REQ-CORE-008/F-016 (Governance Integration)
//!
//! This module provides a lightweight axum HTTP service that stdio shim
//! instances connect to on `127.0.0.1` for governance decisions, heartbeat
//! liveness signals, and shutdown coordination.
//!
//! The service binds to a configurable port (default: 0 for OS-assigned
//! ephemeral) and exposes four endpoints:
//!
//! - `POST /governance/evaluate` — 4-gate governance evaluation
//! - `POST /governance/heartbeat` — shim liveness signal + shutdown notification
//! - `POST /governance/shutdown` — trigger graceful shutdown of all shims
//! - `GET /governance/task/{task_id}` — task approval status polling
//! - `GET /healthz` — health check for shim readiness polling (F-016a)

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::telemetry::{
    GateOutcomes, GatewayDecisionSpanData, extract_context_from_headers,
    finish_gateway_decision_span, start_gateway_decision_span,
};

use super::api::{
    ApprovalOutcome, GovernanceDecision, GovernanceEvaluateRequest, GovernanceEvaluateResponse,
    TaskStatusResponse,
};
use super::evaluator::GovernanceEvaluator;
use super::task::{ApprovalDecision, TaskStatus, TaskStore};

// ─────────────────────────────────────────────────────────────────────────────
// Shared State
// ─────────────────────────────────────────────────────────────────────────────

/// Shared state for the governance HTTP service.
///
/// Wrapped in `Arc` and passed to all axum handlers via `State`. The shutdown
/// flag is piggy-backed onto every evaluate and heartbeat response so that
/// shims can detect when `wrap` wants them to drain (F-016 shutdown coordination).
///
/// When `evaluator` is `Some`, the evaluate handler delegates to the 4-gate
/// governance logic. When `None` (stub mode), it always returns Forward.
///
/// Implements: REQ-CORE-008/F-008
pub struct GovernanceServiceState {
    /// Shutdown flag — set to `true` when `wrap` wants all shims to drain.
    /// Read on every evaluate/heartbeat response.
    shutdown: AtomicBool,

    /// Connected shim tracking: server_id → last heartbeat timestamp.
    /// Used to detect stale shims that have stopped sending heartbeats.
    shims: tokio::sync::Mutex<HashMap<String, Instant>>,

    /// Governance evaluator for 4-gate evaluation. When `None`, the
    /// evaluate endpoint operates in stub mode (always Forward).
    evaluator: Option<Arc<GovernanceEvaluator>>,

    /// Task store for approval workflow tasks. Required when `evaluator`
    /// is `Some` — used by the task status endpoint.
    task_store: Option<Arc<TaskStore>>,
}

impl GovernanceServiceState {
    /// Create a new governance service state in stub mode (always Forward).
    pub fn new() -> Self {
        Self {
            shutdown: AtomicBool::new(false),
            shims: tokio::sync::Mutex::new(HashMap::new()),
            evaluator: None,
            task_store: None,
        }
    }

    /// Create a governance service state with a real evaluator and task store.
    ///
    /// When constructed this way, the evaluate endpoint delegates to the
    /// evaluator's 4-gate logic, and the task status endpoint queries
    /// the task store.
    ///
    /// Implements: REQ-CORE-008/F-016
    pub fn with_evaluator(evaluator: Arc<GovernanceEvaluator>, task_store: Arc<TaskStore>) -> Self {
        Self {
            shutdown: AtomicBool::new(false),
            shims: tokio::sync::Mutex::new(HashMap::new()),
            evaluator: Some(evaluator),
            task_store: Some(task_store),
        }
    }

    /// Check whether the shutdown flag is set.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Set the shutdown flag to true.
    ///
    /// Once set, all subsequent evaluate and heartbeat responses will include
    /// `shutdown: true`, signalling connected shims to initiate graceful
    /// shutdown (F-018).
    pub fn trigger_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

impl Default for GovernanceServiceState {
    fn default() -> Self {
        Self::new()
    }
}

/// Shims that haven't sent a heartbeat within this duration are considered stale.
///
/// The heartbeat interval is 5 seconds, so 15 seconds (3× interval) provides
/// enough margin for network jitter while still cleaning up disconnected shims
/// promptly. This prevents unbounded HashMap growth from shims that disconnect
/// without proper cleanup.
const STALE_SHIM_THRESHOLD: Duration = Duration::from_secs(15);

impl GovernanceServiceState {
    /// Remove shims that haven't sent a heartbeat recently.
    ///
    /// Shims are expected to heartbeat every 5 seconds. Entries older than
    /// [`STALE_SHIM_THRESHOLD`] (15 seconds = 3× interval) are considered stale
    /// and removed to prevent unbounded HashMap growth from disconnected shims.
    ///
    /// This method is called opportunistically during heartbeat handling to
    /// piggyback on the existing lock acquisition.
    pub async fn cleanup_stale_shims(&self) {
        let now = Instant::now();
        let mut shims = self.shims.lock().await;
        let before_count = shims.len();
        shims.retain(|_, last_seen| now.duration_since(*last_seen) < STALE_SHIM_THRESHOLD);
        let removed = before_count - shims.len();
        if removed > 0 {
            tracing::debug!(removed, "Cleaned up stale shim entries");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Heartbeat Types
// ─────────────────────────────────────────────────────────────────────────────

/// Heartbeat request from a stdio shim.
///
/// Shims send a heartbeat every 5 seconds when idle (no messages flowing).
/// The governance service uses this to track connected shims.
///
/// Implements: REQ-CORE-008/F-016
#[derive(Debug, Serialize, Deserialize)]
pub struct HeartbeatRequest {
    /// MCP server identifier (from config rewrite).
    pub server_id: String,
}

/// Heartbeat response from governance service to shim.
///
/// When `shutdown` is `true`, the shim must initiate graceful shutdown (F-018).
///
/// Implements: REQ-CORE-008/F-016
#[derive(Debug, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    /// Whether the shim should initiate graceful shutdown.
    pub shutdown: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Router & Handlers
// ─────────────────────────────────────────────────────────────────────────────

/// Build the axum router for the governance HTTP service.
///
/// All routes share `Arc<GovernanceServiceState>` via axum's `State` extractor.
///
/// Implements: REQ-CORE-008/F-008
pub fn governance_router(state: Arc<GovernanceServiceState>) -> Router {
    Router::new()
        .route("/governance/evaluate", post(evaluate_handler))
        .route("/governance/heartbeat", post(heartbeat_handler))
        .route("/governance/shutdown", post(shutdown_handler))
        .route("/governance/task/{task_id}", get(task_status_handler))
        .route("/healthz", get(healthz_handler))
        .with_state(state)
}

/// Handle `POST /governance/evaluate`.
///
/// Receives a classified JSON-RPC message from a shim and returns a governance
/// decision. Delegates to the [`GovernanceEvaluator`] when configured, otherwise
/// operates in stub mode (always Forward).
///
/// The `shutdown` field is piggy-backed onto every response so the shim can
/// detect shutdown without waiting for the next heartbeat.
///
/// Extracts W3C trace context from `traceparent`/`tracestate` HTTP headers
/// sent by the shim, enabling distributed tracing through stdio transport.
///
/// Implements: REQ-CORE-008/F-016
/// Implements: REQ-OBS-002 §7.1 (Trace Context Extraction)
async fn evaluate_handler(
    State(state): State<Arc<GovernanceServiceState>>,
    headers: HeaderMap,
    Json(req): Json<GovernanceEvaluateRequest>,
) -> Json<GovernanceEvaluateResponse> {
    let shutdown = state.shutdown.load(Ordering::SeqCst);

    // REQ-OBS-002 §7.1: Extract trace context from HTTP headers
    let parent_ctx = extract_context_from_headers(&headers);

    // Generate correlation ID for this request
    let correlation_id = uuid::Uuid::new_v4().to_string();

    // Create gateway decision span with extracted parent context
    let span_data = GatewayDecisionSpanData {
        request_id: &correlation_id,
        upstream_target: &req.server_id,
    };
    let mut decision_span = start_gateway_decision_span(&span_data, &parent_ctx);

    let resp = if let Some(ref evaluator) = state.evaluator {
        let mut resp = evaluator.evaluate(&req).await;
        resp.shutdown = shutdown;
        resp
    } else {
        // Stub mode (backward compat): always forward.
        GovernanceEvaluateResponse {
            decision: GovernanceDecision::Forward,
            task_id: None,
            policy_id: None,
            reason: None,
            poll_interval_ms: None,
            shutdown,
        }
    };

    // Populate gate outcomes from the response for span attributes
    let outcomes = GateOutcomes {
        visibility: Some("pass".to_string()), // Gate 1 always passes in v0.3
        governance: Some(match resp.decision {
            GovernanceDecision::Forward => "forward".to_string(),
            GovernanceDecision::Deny => "deny".to_string(),
            GovernanceDecision::PendingApproval => "approve".to_string(),
        }),
        cedar: resp.policy_id.as_ref().map(|_| {
            match resp.decision {
                GovernanceDecision::Forward => "allow".to_string(),
                GovernanceDecision::Deny => "deny".to_string(),
                GovernanceDecision::PendingApproval => "allow".to_string(), // Cedar allowed, approval required
            }
        }),
        approval: if resp.task_id.is_some() {
            Some("started".to_string())
        } else {
            None
        },
        governance_rule_id: resp.policy_id.clone(),
        policy_evaluated: resp.policy_id.is_some(),
    };

    finish_gateway_decision_span(&mut decision_span, &outcomes, None);

    Json(resp)
}

/// Handle `POST /governance/heartbeat`.
///
/// Updates the last-seen timestamp for the shim and returns the current
/// shutdown flag. Shims must send a heartbeat every 5 seconds when idle.
///
/// Also performs opportunistic cleanup of stale shim entries to prevent
/// unbounded HashMap growth from disconnected shims.
///
/// Implements: REQ-CORE-008/F-016
async fn heartbeat_handler(
    State(state): State<Arc<GovernanceServiceState>>,
    Json(req): Json<HeartbeatRequest>,
) -> Json<HeartbeatResponse> {
    {
        let mut shims = state.shims.lock().await;
        shims.insert(req.server_id, Instant::now());
    }

    // Opportunistic cleanup of stale shims (piggybacks on heartbeat traffic).
    // This is called after releasing the lock above to avoid holding it during cleanup.
    state.cleanup_stale_shims().await;

    Json(HeartbeatResponse {
        shutdown: state.shutdown.load(Ordering::SeqCst),
    })
}

/// Handle `POST /governance/shutdown`.
///
/// Sets the shutdown flag to true. All subsequent evaluate and heartbeat
/// responses will include `shutdown: true`, signalling connected shims to
/// initiate graceful shutdown.
///
/// Called by `wrap` when the agent process exits (F-010 step 2).
///
/// Implements: REQ-CORE-008/F-010
async fn shutdown_handler(State(state): State<Arc<GovernanceServiceState>>) -> StatusCode {
    state.shutdown.store(true, Ordering::SeqCst);
    StatusCode::OK
}

/// Handle `GET /governance/task/{task_id}`.
///
/// Returns the current task status and approval outcome so the shim can
/// determine whether to forward, deny, or continue polling. Maps internal
/// task state to the simplified [`ApprovalOutcome`] the shim needs.
///
/// Returns 404 if the task_id is unknown or the task store is not configured.
///
/// Implements: REQ-CORE-008/F-016
async fn task_status_handler(
    State(state): State<Arc<GovernanceServiceState>>,
    Path(task_id): Path<String>,
) -> Result<Json<TaskStatusResponse>, StatusCode> {
    let task_store = state.task_store.as_ref().ok_or(StatusCode::NOT_FOUND)?;

    let task_id_parsed = task_id.parse().map_err(|_| StatusCode::NOT_FOUND)?;
    let task = task_store
        .get(&task_id_parsed)
        .map_err(|_| StatusCode::NOT_FOUND)?;

    // Map internal task state to shim-visible approval outcome.
    // The shim cares about the *approval decision*, not the execution lifecycle.
    let (decision, reason) = if let Some(ref approval) = task.approval {
        // Primary signal: the approval record (set by PollingScheduler).
        match &approval.decision {
            ApprovalDecision::Approved => (Some(ApprovalOutcome::Approved), None),
            ApprovalDecision::Rejected { reason } => {
                (Some(ApprovalOutcome::Rejected), reason.clone())
            }
        }
    } else {
        // No approval record — check terminal states.
        match task.status {
            TaskStatus::Expired => (Some(ApprovalOutcome::Expired), None),
            TaskStatus::Cancelled => (Some(ApprovalOutcome::Cancelled), None),
            _ => (None, None), // Still pending (InputRequired, Working, etc.)
        }
    };

    Ok(Json(TaskStatusResponse {
        task_id,
        status: task.status,
        decision,
        reason,
    }))
}

/// Handle `GET /healthz`.
///
/// Returns 200 OK with a simple status payload. Used by shims during startup
/// readiness polling (F-016a).
///
/// Implements: REQ-CORE-008/F-008
async fn healthz_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

// ─────────────────────────────────────────────────────────────────────────────
// Service Startup
// ─────────────────────────────────────────────────────────────────────────────

/// Start the governance HTTP service, returning the actual bound port.
///
/// Binds to `127.0.0.1` on the given port (0 = OS-assigned ephemeral).
/// Returns the actual port and a `JoinHandle` for the serving task.
///
/// The caller typically spawns this before rewriting the agent config so that
/// the actual port can be injected into shim args via `--governance-endpoint`.
///
/// # Arguments
///
/// * `port` - Port to bind to. Use 0 for OS-assigned ephemeral port.
/// * `state` - Shared governance service state.
///
/// # Errors
///
/// Returns `std::io::Error` if binding to the port fails.
///
/// Implements: REQ-CORE-008/F-008
pub async fn start_governance_service(
    port: u16,
    state: Arc<GovernanceServiceState>,
) -> Result<(u16, tokio::task::JoinHandle<Result<(), std::io::Error>>), std::io::Error> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let actual_port = listener.local_addr()?.port();

    let router = governance_router(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .map_err(std::io::Error::other)
    });

    Ok((actual_port, handle))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StreamDirection;
    use crate::governance::api::MessageType;
    use crate::profile::Profile;

    async fn start_test_service() -> (u16, Arc<GovernanceServiceState>) {
        let state = Arc::new(GovernanceServiceState::new());
        let (port, _handle) = start_governance_service(0, state.clone()).await.unwrap();
        (port, state)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_healthz_returns_ok() {
        let (port, _state) = start_test_service().await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/healthz"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_evaluate_returns_forward() {
        let (port, _state) = start_test_service().await;

        let client = reqwest::Client::new();
        let req = GovernanceEvaluateRequest {
            server_id: "filesystem".to_string(),
            direction: StreamDirection::AgentToServer,
            method: "tools/call".to_string(),
            id: None,
            params: None,
            message_type: MessageType::Request,
            profile: Profile::Production,
        };

        let resp = client
            .post(format!("http://127.0.0.1:{port}/governance/evaluate"))
            .json(&req)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: GovernanceEvaluateResponse = resp.json().await.unwrap();
        assert_eq!(body.decision, GovernanceDecision::Forward);
        assert!(!body.shutdown);
        assert!(body.task_id.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_shutdown_then_heartbeat() {
        let (port, _state) = start_test_service().await;

        let client = reqwest::Client::new();

        // Trigger shutdown.
        let resp = client
            .post(format!("http://127.0.0.1:{port}/governance/shutdown"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Heartbeat should now return shutdown: true.
        let hb_req = HeartbeatRequest {
            server_id: "filesystem".to_string(),
        };
        let resp = client
            .post(format!("http://127.0.0.1:{port}/governance/heartbeat"))
            .json(&hb_req)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: HeartbeatResponse = resp.json().await.unwrap();
        assert!(body.shutdown);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_concurrent_evaluate_no_deadlock() {
        let (port, _state) = start_test_service().await;

        let client = reqwest::Client::new();
        let mut handles = Vec::new();

        for i in 0..10 {
            let client = client.clone();
            let url = format!("http://127.0.0.1:{port}/governance/evaluate");
            handles.push(tokio::spawn(async move {
                let req = GovernanceEvaluateRequest {
                    server_id: format!("server-{i}"),
                    direction: StreamDirection::AgentToServer,
                    method: "tools/call".to_string(),
                    id: None,
                    params: None,
                    message_type: MessageType::Request,
                    profile: Profile::Production,
                };
                let resp = client.post(&url).json(&req).send().await.unwrap();
                assert_eq!(resp.status(), 200);
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }
    }

    // ── Evaluator-backed service tests ──────────────────────────────────

    fn test_config() -> Arc<crate::config::Config> {
        Arc::new(
            serde_saphyr::from_str(
                r#"
schema: 1
sources:
  - id: test-server
    kind: mcp
    url: http://localhost:3000
    expose:
      mode: blocklist
      tools:
        - "blocked_*"
governance:
  defaults:
    action: forward
  rules:
    - match: "denied_*"
      action: deny
"#,
            )
            .unwrap(),
        )
    }

    async fn start_evaluator_service() -> (u16, Arc<GovernanceServiceState>, Arc<TaskStore>) {
        use crate::governance::evaluator::GovernanceEvaluator;
        use crate::governance::task::Principal;

        let config = test_config();
        let task_store = Arc::new(TaskStore::with_defaults());
        let principal = Principal::new("test-app");
        let evaluator = Arc::new(GovernanceEvaluator::new(
            config,
            None,
            task_store.clone(),
            principal,
            Profile::Production,
        ));

        let state = Arc::new(GovernanceServiceState::with_evaluator(
            evaluator,
            task_store.clone(),
        ));
        let (port, _handle) = start_governance_service(0, state.clone()).await.unwrap();
        (port, state, task_store)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_evaluate_with_evaluator_forward() {
        let (port, _state, _) = start_evaluator_service().await;

        let client = reqwest::Client::new();
        let req = GovernanceEvaluateRequest {
            server_id: "test-server".to_string(),
            direction: StreamDirection::AgentToServer,
            method: "tools/call".to_string(),
            id: Some(crate::jsonrpc::JsonRpcId::Number(1)),
            params: Some(serde_json::json!({"name": "safe_tool"})),
            message_type: MessageType::Request,
            profile: Profile::Production,
        };

        let resp = client
            .post(format!("http://127.0.0.1:{port}/governance/evaluate"))
            .json(&req)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: GovernanceEvaluateResponse = resp.json().await.unwrap();
        assert_eq!(body.decision, GovernanceDecision::Forward);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_evaluate_with_evaluator_deny() {
        let (port, _state, _) = start_evaluator_service().await;

        let client = reqwest::Client::new();
        let req = GovernanceEvaluateRequest {
            server_id: "test-server".to_string(),
            direction: StreamDirection::AgentToServer,
            method: "tools/call".to_string(),
            id: Some(crate::jsonrpc::JsonRpcId::Number(1)),
            params: Some(serde_json::json!({"name": "denied_delete"})),
            message_type: MessageType::Request,
            profile: Profile::Production,
        };

        let resp = client
            .post(format!("http://127.0.0.1:{port}/governance/evaluate"))
            .json(&req)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: GovernanceEvaluateResponse = resp.json().await.unwrap();
        assert_eq!(body.decision, GovernanceDecision::Deny);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_evaluate_with_evaluator_deny_blocked_tool() {
        let (port, _state, _) = start_evaluator_service().await;

        let client = reqwest::Client::new();
        let req = GovernanceEvaluateRequest {
            server_id: "test-server".to_string(),
            direction: StreamDirection::AgentToServer,
            method: "tools/call".to_string(),
            id: Some(crate::jsonrpc::JsonRpcId::Number(1)),
            params: Some(serde_json::json!({"name": "blocked_admin"})),
            message_type: MessageType::Request,
            profile: Profile::Production,
        };

        let resp = client
            .post(format!("http://127.0.0.1:{port}/governance/evaluate"))
            .json(&req)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: GovernanceEvaluateResponse = resp.json().await.unwrap();
        assert_eq!(body.decision, GovernanceDecision::Deny);
        assert!(body.reason.as_deref().unwrap_or("").contains("not exposed"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_task_status_not_found() {
        let (port, _state, _) = start_evaluator_service().await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "http://127.0.0.1:{port}/governance/task/tg_nonexistent"
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_task_status_pending() {
        use crate::governance::engine::TimeoutAction;
        use crate::governance::task::{Principal, ToolCallRequest};
        use crate::jsonrpc::JsonRpcId;

        let (port, _state, task_store) = start_evaluator_service().await;

        // Manually create a task in InputRequired state.
        let tool_req = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "deploy".to_string(),
            arguments: serde_json::json!({}),
            mcp_request_id: JsonRpcId::Number(1),
        };
        let task = task_store
            .create(
                tool_req.clone(),
                tool_req,
                Principal::new("test"),
                None,
                TimeoutAction::Deny,
            )
            .unwrap();
        let task_id = task.id.to_string();
        task_store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/governance/task/{task_id}"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: TaskStatusResponse = resp.json().await.unwrap();
        assert_eq!(body.task_id, task_id);
        assert_eq!(body.status, TaskStatus::InputRequired);
        assert!(body.decision.is_none()); // No approval yet.
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_task_status_approved() {
        use crate::governance::engine::TimeoutAction;
        use crate::governance::task::{ApprovalDecision, Principal, ToolCallRequest};
        use crate::jsonrpc::JsonRpcId;
        use std::time::Duration;

        let (port, _state, task_store) = start_evaluator_service().await;

        // Create task, transition to InputRequired, then record approval.
        let tool_req = ToolCallRequest {
            method: "tools/call".to_string(),
            name: "deploy".to_string(),
            arguments: serde_json::json!({}),
            mcp_request_id: JsonRpcId::Number(1),
        };
        let task = task_store
            .create(
                tool_req.clone(),
                tool_req,
                Principal::new("test"),
                None,
                TimeoutAction::Deny,
            )
            .unwrap();
        let task_id = task.id.to_string();
        task_store
            .transition(&task.id, TaskStatus::InputRequired, None)
            .unwrap();
        task_store
            .record_approval(
                &task.id,
                ApprovalDecision::Approved,
                "reviewer".to_string(),
                Duration::from_secs(60),
            )
            .unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/governance/task/{task_id}"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: TaskStatusResponse = resp.json().await.unwrap();
        assert_eq!(body.decision, Some(ApprovalOutcome::Approved));
    }

    // ── Stale shim cleanup tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_cleanup_stale_shims_removes_old_entries() {
        let state = GovernanceServiceState::new();

        // Insert a shim with an artificially old timestamp
        {
            let mut shims = state.shims.lock().await;
            // Insert an entry that appears to be from 20 seconds ago (stale)
            let stale_time = Instant::now() - Duration::from_secs(20);
            shims.insert("stale-server".to_string(), stale_time);
            // Insert a fresh entry
            shims.insert("fresh-server".to_string(), Instant::now());
        }

        // Verify both entries exist before cleanup
        {
            let shims = state.shims.lock().await;
            assert_eq!(shims.len(), 2);
        }

        // Run cleanup
        state.cleanup_stale_shims().await;

        // Only the fresh entry should remain
        {
            let shims = state.shims.lock().await;
            assert_eq!(shims.len(), 1);
            assert!(shims.contains_key("fresh-server"));
            assert!(!shims.contains_key("stale-server"));
        }
    }

    #[tokio::test]
    async fn test_cleanup_stale_shims_keeps_recent_entries() {
        let state = GovernanceServiceState::new();

        // Insert entries that are within the threshold (fresh)
        {
            let mut shims = state.shims.lock().await;
            shims.insert("server-1".to_string(), Instant::now());
            shims.insert("server-2".to_string(), Instant::now() - Duration::from_secs(10));
            shims.insert("server-3".to_string(), Instant::now() - Duration::from_secs(14));
        }

        // Run cleanup
        state.cleanup_stale_shims().await;

        // All entries should remain (all within 15s threshold)
        {
            let shims = state.shims.lock().await;
            assert_eq!(shims.len(), 3);
        }
    }
}
