//! Health and readiness probe handlers.
//!
//! Implements: REQ-CORE-005/F-002, F-003
//!
//! This module provides HTTP handlers for Kubernetes health probes:
//!
//! - `/health` (liveness): Returns 200 if process is alive
//! - `/ready` (readiness): Returns 200 only when all checks pass
//!
//! ## Response Codes
//!
//! | Endpoint | Condition | Status |
//! |----------|-----------|--------|
//! | /health  | Process alive | 200 |
//! | /health  | Process stopped | 503 |
//! | /ready   | All checks pass | 200 |
//! | /ready   | Any check fails | 503 |
//! | /ready   | Shutting down | 503 |

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use std::sync::Arc;

use super::{LifecycleManager, LifecycleState};

// ============================================================================
// Response Types
// ============================================================================

/// Health probe response.
///
/// Implements: REQ-CORE-005/§6.1
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Health status ("healthy")
    pub status: &'static str,
    /// Service version
    pub version: &'static str,
    /// Uptime in seconds
    pub uptime_seconds: u64,
}

/// Unhealthy response.
///
/// Implements: REQ-CORE-005/§6.1
#[derive(Debug, Serialize)]
pub struct UnhealthyResponse {
    /// Health status ("unhealthy")
    pub status: &'static str,
    /// Reason for unhealthy status
    pub reason: String,
    /// Additional details
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Readiness checks result.
///
/// Implements: REQ-CORE-005/§6.2
#[derive(Debug, Clone, Serialize)]
pub struct ReadinessChecks {
    /// Whether configuration is loaded and validated
    pub config_loaded: bool,
    /// Whether upstream is reachable (cached)
    pub upstream_reachable: bool,
    /// Whether approval store is initialized
    pub approval_store_initialized: bool,
}

impl ReadinessChecks {
    /// Returns true if all checks pass.
    ///
    /// Implements: REQ-CORE-005/F-003.1
    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.config_loaded && self.upstream_reachable && self.approval_store_initialized
    }

    /// Returns the first failing check name.
    ///
    /// Implements: REQ-CORE-005/F-003.2
    #[must_use]
    pub fn first_failure(&self) -> Option<&'static str> {
        if !self.config_loaded {
            Some("config_loaded")
        } else if !self.upstream_reachable {
            Some("upstream_reachable")
        } else if !self.approval_store_initialized {
            Some("approval_store_initialized")
        } else {
            None
        }
    }
}

/// Readiness probe response.
///
/// Implements: REQ-CORE-005/§6.2
#[derive(Debug, Serialize)]
pub struct ReadinessResponse {
    /// Readiness status ("ready" or "not_ready")
    pub status: &'static str,
    /// Individual check results
    pub checks: ReadinessChecks,
    /// Reason for not ready (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ============================================================================
// Router
// ============================================================================

/// Create the health/readiness router.
///
/// Implements: REQ-CORE-005/F-002, F-003
///
/// Returns an Axum router with:
/// - `GET /health` - Liveness probe
/// - `GET /ready` - Readiness probe
pub fn health_router(lifecycle: Arc<LifecycleManager>) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(readiness_handler))
        .with_state(lifecycle)
}

// ============================================================================
// Handlers
// ============================================================================

/// Health probe handler.
///
/// Implements: REQ-CORE-005/F-002
///
/// Returns 200 if process is alive and responsive.
/// Returns 503 if service is in Stopped state.
///
/// # Requirements
///
/// - F-002.1: Return 200 if process is alive
/// - F-002.2: Return 503 if critical subsystem has failed
/// - F-002.3: Include version and uptime in response
/// - F-002.4: Health check must complete in < 100ms
/// - F-002.5: Health check must not have side effects
async fn health_handler(State(lifecycle): State<Arc<LifecycleManager>>) -> Response {
    // F-002.4: No blocking operations, completes quickly
    // F-002.5: Read-only, no side effects

    // Check if we're in Stopped state (critical failure)
    if matches!(lifecycle.state(), LifecycleState::Stopped) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(UnhealthyResponse {
                status: "unhealthy",
                reason: "service_stopped".to_string(),
                details: None,
            }),
        )
            .into_response();
    }

    // F-002.1: Return 200 if process is alive
    // F-002.3: Include version and uptime
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "healthy",
            version: lifecycle.version(),
            uptime_seconds: lifecycle.uptime_seconds(),
        }),
    )
        .into_response()
}

/// Readiness probe handler.
///
/// Implements: REQ-CORE-005/F-003
///
/// Returns 200 only when ALL checks pass and service is ready.
/// Returns 503 during shutdown or if any check fails.
///
/// # Requirements
///
/// - F-003.1: Return 200 only when ALL checks pass
/// - F-003.2: Return 503 with failed checks in response
/// - F-003.3: Check policy loading status
/// - F-003.4: Check upstream connectivity (cached)
/// - F-003.5: Check task store initialization
/// - F-003.6: During shutdown, return 503
///
/// # Edge Cases
///
/// - EC-OPS-010: Health during startup returns 503
async fn readiness_handler(State(lifecycle): State<Arc<LifecycleManager>>) -> Response {
    // F-003.6: During shutdown, return 503
    if lifecycle.is_shutting_down() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadinessResponse {
                status: "not_ready",
                checks: lifecycle.readiness_checks(),
                reason: Some("shutting_down".to_string()),
            }),
        )
            .into_response();
    }

    let checks = lifecycle.readiness_checks();

    // F-003.1: Return 200 only when ALL checks pass
    if checks.all_pass() && lifecycle.is_ready() {
        (
            StatusCode::OK,
            Json(ReadinessResponse {
                status: "ready",
                checks,
                reason: None,
            }),
        )
            .into_response()
    } else {
        // F-003.2: Return 503 with failed checks
        // Determine reason: check individual checks first, then lifecycle state
        let reason = if let Some(failed_check) = checks.first_failure() {
            Some(failed_check.to_string())
        } else if !lifecycle.is_ready() {
            // All checks pass but lifecycle not ready (e.g., still starting up)
            Some(format!("lifecycle_state: {}", lifecycle.state()))
        } else {
            None
        };
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadinessResponse {
                status: "not_ready",
                checks,
                reason,
            }),
        )
            .into_response()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::LifecycleConfig;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde::Deserialize;
    use tower::ServiceExt;

    // Test-only deserializable versions of response types
    // (Production types use &'static str which can't be deserialized)

    #[derive(Debug, Deserialize)]
    struct TestHealthResponse {
        status: String,
        #[allow(dead_code)]
        version: String,
        #[allow(dead_code)]
        uptime_seconds: u64,
    }

    #[derive(Debug, Deserialize)]
    struct TestReadinessChecks {
        config_loaded: bool,
        upstream_reachable: bool,
        approval_store_initialized: bool,
    }

    impl TestReadinessChecks {
        fn all_pass(&self) -> bool {
            self.config_loaded && self.upstream_reachable && self.approval_store_initialized
        }
    }

    #[derive(Debug, Deserialize)]
    struct TestReadinessResponse {
        status: String,
        checks: TestReadinessChecks,
        reason: Option<String>,
    }

    /// Test health endpoint returns 200 during startup.
    ///
    /// Verifies: REQ-CORE-005/F-002.1
    #[tokio::test]
    async fn test_health_during_startup() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        let router = health_router(lifecycle);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: TestHealthResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.status, "healthy");
    }

    /// Test health endpoint returns 200 when ready.
    ///
    /// Verifies: REQ-CORE-005/F-002.1
    #[tokio::test]
    async fn test_health_when_ready() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_ready();

        let router = health_router(lifecycle);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Test health endpoint returns 200 during shutdown.
    ///
    /// Liveness should pass even during shutdown (process still alive).
    #[tokio::test]
    async fn test_health_during_shutdown() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_ready();
        lifecycle.begin_shutdown();

        let router = health_router(lifecycle);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        // Still healthy during shutdown (process is alive)
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Test health endpoint returns 503 when stopped.
    ///
    /// Verifies: REQ-CORE-005/F-002.2
    #[tokio::test]
    async fn test_health_when_stopped() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_stopped();

        let router = health_router(lifecycle);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Test readiness returns 503 during startup.
    ///
    /// Verifies: EC-OPS-010
    #[tokio::test]
    async fn test_ready_during_startup() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        let router = health_router(lifecycle);

        let req = Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: TestReadinessResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.status, "not_ready");
    }

    /// Test readiness returns 200 when all checks pass.
    ///
    /// Verifies: REQ-CORE-005/F-003.1
    #[tokio::test]
    async fn test_ready_all_checks_pass() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_config_loaded();
        lifecycle.mark_approval_store_initialized();
        lifecycle.update_upstream_health(true, None);
        lifecycle.mark_ready();

        let router = health_router(lifecycle);

        let req = Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: TestReadinessResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.status, "ready");
        assert!(json.checks.all_pass());
    }

    /// Test readiness returns 503 when upstream unhealthy.
    ///
    /// Verifies: EC-OPS-011
    #[tokio::test]
    async fn test_ready_upstream_unhealthy() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_config_loaded();
        lifecycle.mark_approval_store_initialized();
        // upstream_health stays false (default)
        lifecycle.mark_ready();

        let router = health_router(lifecycle);

        let req = Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: TestReadinessResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.reason, Some("upstream_reachable".to_string()));
    }

    /// Test readiness returns 503 during shutdown.
    ///
    /// Verifies: REQ-CORE-005/F-003.6
    #[tokio::test]
    async fn test_ready_during_shutdown() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_config_loaded();
        lifecycle.mark_approval_store_initialized();
        lifecycle.update_upstream_health(true, None);
        lifecycle.mark_ready();
        lifecycle.begin_shutdown();

        let router = health_router(lifecycle);

        let req = Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: TestReadinessResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.reason, Some("shutting_down".to_string()));
    }

    /// Test readiness checks helper methods.
    #[test]
    fn test_readiness_checks_helpers() {
        let checks = ReadinessChecks {
            config_loaded: false,
            upstream_reachable: true,
            approval_store_initialized: true,
        };
        assert!(!checks.all_pass());
        assert_eq!(checks.first_failure(), Some("config_loaded"));

        let checks = ReadinessChecks {
            config_loaded: true,
            upstream_reachable: false,
            approval_store_initialized: true,
        };
        assert!(!checks.all_pass());
        assert_eq!(checks.first_failure(), Some("upstream_reachable"));

        let checks = ReadinessChecks {
            config_loaded: true,
            upstream_reachable: true,
            approval_store_initialized: false,
        };
        assert!(!checks.all_pass());
        assert_eq!(checks.first_failure(), Some("approval_store_initialized"));

        let checks = ReadinessChecks {
            config_loaded: true,
            upstream_reachable: true,
            approval_store_initialized: true,
        };
        assert!(checks.all_pass());
        assert_eq!(checks.first_failure(), None);
    }
}
