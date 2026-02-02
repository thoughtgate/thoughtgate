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
//! - `POST /governance/evaluate` — 4-gate governance evaluation (stub: always Forward)
//! - `POST /governance/heartbeat` — shim liveness signal + shutdown notification
//! - `POST /governance/shutdown` — trigger graceful shutdown of all shims
//! - `GET /healthz` — health check for shim readiness polling (F-016a)

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use super::api::{GovernanceDecision, GovernanceEvaluateRequest, GovernanceEvaluateResponse};

// ─────────────────────────────────────────────────────────────────────────────
// Shared State
// ─────────────────────────────────────────────────────────────────────────────

/// Shared state for the governance HTTP service.
///
/// Wrapped in `Arc` and passed to all axum handlers via `State`. The shutdown
/// flag is piggy-backed onto every evaluate and heartbeat response so that
/// shims can detect when `wrap` wants them to drain (F-016 shutdown coordination).
///
/// Implements: REQ-CORE-008/F-008
pub struct GovernanceServiceState {
    /// Shutdown flag — set to `true` when `wrap` wants all shims to drain.
    /// Read on every evaluate/heartbeat response.
    shutdown: AtomicBool,

    /// Connected shim tracking: server_id → last heartbeat timestamp.
    /// Used to detect stale shims that have stopped sending heartbeats.
    shims: tokio::sync::Mutex<HashMap<String, Instant>>,
}

impl GovernanceServiceState {
    /// Create a new governance service state with shutdown flag unset.
    pub fn new() -> Self {
        Self {
            shutdown: AtomicBool::new(false),
            shims: tokio::sync::Mutex::new(HashMap::new()),
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
        .route("/healthz", get(healthz_handler))
        .with_state(state)
}

/// Handle `POST /governance/evaluate`.
///
/// Receives a classified JSON-RPC message from a shim and returns a governance
/// decision. Currently a stub that always returns `Forward` — real Cedar policy
/// evaluation will be wired in later.
///
/// The `shutdown` field is piggy-backed onto every response so the shim can
/// detect shutdown without waiting for the next heartbeat.
///
/// Implements: REQ-CORE-008/F-016
async fn evaluate_handler(
    State(state): State<Arc<GovernanceServiceState>>,
    Json(_req): Json<GovernanceEvaluateRequest>,
) -> Json<GovernanceEvaluateResponse> {
    // Stub: always forward. Real Cedar evaluation wired in later.
    Json(GovernanceEvaluateResponse {
        decision: GovernanceDecision::Forward,
        task_id: None,
        policy_id: None,
        reason: None,
        shutdown: state.shutdown.load(Ordering::SeqCst),
    })
}

/// Handle `POST /governance/heartbeat`.
///
/// Updates the last-seen timestamp for the shim and returns the current
/// shutdown flag. Shims must send a heartbeat every 5 seconds when idle.
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
}
