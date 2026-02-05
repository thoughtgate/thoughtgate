//! Admin server for health checks and metrics.
//!
//! # Traceability
//! - Implements: REQ-CORE-005/§5.1 (Admin Server)
//! - Implements: REQ-OBS-002 §6 (Prometheus Metrics Endpoint)
//!
//! # Overview
//!
//! The admin server runs on a dedicated port (default: 7469) and provides:
//!
//! - **Health Endpoints**: `/health` (liveness) and `/ready` (readiness)
//! - **Metrics Endpoint**: `/metrics` (OpenMetrics format via prometheus-client)
//!
//! This is separate from the main proxy port to allow:
//! - Independent health monitoring
//! - Security isolation (admin endpoints not exposed to proxy clients)
//! - Dedicated resource allocation
//!
//! # Metrics Migration Note
//!
//! The `/metrics` endpoint now uses `prometheus-client` crate with OpenMetrics format.
//! Previous OTel-based metrics (`mcp_*`, etc.) are replaced by `thoughtgate_*` prefixed
//! metrics. This is an intentional migration per REQ-OBS-002.

use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use prometheus_client::registry::Registry;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::ports::admin_port;
use thoughtgate_core::lifecycle::{LifecycleManager, health_router};

/// Admin server configuration.
#[derive(Debug, Clone)]
pub struct AdminServerConfig {
    /// Port to listen on (default: 7469)
    pub port: u16,
    /// Bind address (default: 127.0.0.1)
    pub bind_addr: String,
}

impl Default for AdminServerConfig {
    fn default() -> Self {
        Self {
            port: admin_port(),
            bind_addr: "127.0.0.1".to_string(),
        }
    }
}

impl AdminServerConfig {
    /// Create a new admin server config with the default port.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new admin server config with a custom port.
    pub fn with_port(port: u16) -> Self {
        Self {
            port,
            ..Self::default()
        }
    }

    /// Get the full bind address string.
    pub fn bind_string(&self) -> String {
        format!("{}:{}", self.bind_addr, self.port)
    }
}

/// Shared state for the admin server.
#[derive(Clone)]
pub struct AdminState {
    /// Lifecycle manager for health checks.
    pub lifecycle: Arc<LifecycleManager>,
    /// Prometheus registry for metrics endpoint (REQ-OBS-002 §6).
    pub prom_registry: Arc<Registry>,
}

/// Admin server for health checks and metrics.
///
/// # Traceability
/// - Implements: REQ-CORE-005/§5.1 (Admin Server)
pub struct AdminServer {
    config: AdminServerConfig,
    state: AdminState,
}

impl AdminServer {
    /// Create a new admin server.
    ///
    /// # Arguments
    ///
    /// * `lifecycle` - Lifecycle manager for health checks
    /// * `prom_registry` - Prometheus registry for metrics
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use thoughtgate_proxy::admin::AdminServer;
    /// use thoughtgate_core::lifecycle::LifecycleManager;
    /// use prometheus_client::registry::Registry;
    ///
    /// let lifecycle = Arc::new(LifecycleManager::new(Default::default()));
    /// let prom_registry = Arc::new(Registry::default());
    /// let admin = AdminServer::new(lifecycle, prom_registry);
    /// ```
    pub fn new(lifecycle: Arc<LifecycleManager>, prom_registry: Arc<Registry>) -> Self {
        Self {
            config: AdminServerConfig::default(),
            state: AdminState {
                lifecycle,
                prom_registry,
            },
        }
    }

    /// Create a new admin server with custom configuration.
    pub fn with_config(
        lifecycle: Arc<LifecycleManager>,
        prom_registry: Arc<Registry>,
        config: AdminServerConfig,
    ) -> Self {
        Self {
            config,
            state: AdminState {
                lifecycle,
                prom_registry,
            },
        }
    }

    /// Create the Axum router for the admin server.
    ///
    /// # Endpoints
    ///
    /// - `GET /health` - Liveness probe (JSON, lifecycle-aware)
    /// - `GET /ready` - Readiness probe (JSON, checks all subsystems)
    /// - `GET /metrics` - Prometheus metrics
    ///
    /// Health and readiness handlers delegate to the core library's
    /// lifecycle-aware implementations that return JSON with version,
    /// uptime, and individual check results.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-005/F-002, F-003
    pub fn router(&self) -> Router {
        // Merge core library health/readiness router (JSON, lifecycle-aware)
        // with our metrics endpoint
        let metrics_router = Router::new()
            .route("/metrics", get(metrics_handler))
            .with_state(self.state.clone());

        health_router(self.state.lifecycle.clone()).merge(metrics_router)
    }

    /// Run the admin server.
    ///
    /// This will bind to the configured address and serve requests until
    /// the shutdown token is cancelled.
    ///
    /// # Arguments
    ///
    /// * `shutdown` - Cancellation token for graceful shutdown
    ///
    /// # Errors
    ///
    /// Returns an error if the server fails to bind or serve.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-005/§5.1 (Admin Server Lifecycle)
    pub async fn run(
        self,
        shutdown: CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bind_addr = self.config.bind_string();
        let listener = TcpListener::bind(&bind_addr).await?;

        info!(addr = %bind_addr, "Admin server listening");

        axum::serve(listener, self.router())
            .with_graceful_shutdown(async move {
                shutdown.cancelled().await;
                info!("Admin server shutting down");
            })
            .await?;

        Ok(())
    }
}

/// Metrics handler using prometheus-client (OpenMetrics format).
///
/// Returns metrics in OpenMetrics text format. This replaces the previous
/// OTel-prometheus based metrics handler.
///
/// # Traceability
/// - Implements: REQ-OBS-002 §6 (Prometheus Metrics Endpoint)
async fn metrics_handler(State(state): State<AdminState>) -> impl IntoResponse {
    let mut buffer = String::new();

    if let Err(e) = prometheus_client::encoding::text::encode(&mut buffer, &state.prom_registry) {
        error!(error = %e, "Failed to encode metrics");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to encode metrics: {}", e),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )],
        buffer,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use thoughtgate_core::lifecycle::LifecycleConfig;
    use tower::ServiceExt;

    fn create_test_state() -> AdminState {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        let prom_registry = Arc::new(Registry::default());
        AdminState {
            lifecycle,
            prom_registry,
        }
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let state = create_test_state();
        let admin = AdminServer::with_config(
            state.lifecycle.clone(),
            state.prom_registry.clone(),
            AdminServerConfig::default(),
        );
        let router = admin.router();

        let request = Request::builder()
            .method("GET")
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Now returns JSON from core library handler
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "healthy");
        assert!(json["version"].is_string());
        assert!(json["uptime_seconds"].is_number());
    }

    #[tokio::test]
    async fn test_readiness_endpoint_not_ready() {
        let state = create_test_state();
        let admin = AdminServer::with_config(
            state.lifecycle.clone(),
            state.prom_registry.clone(),
            AdminServerConfig::default(),
        );
        let router = admin.router();

        // Lifecycle starts not ready
        let request = Request::builder()
            .method("GET")
            .uri("/ready")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // Now returns JSON with check details
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "not_ready");
        assert!(json["checks"].is_object());
    }

    #[tokio::test]
    async fn test_readiness_endpoint_ready() {
        let state = create_test_state();
        state.lifecycle.mark_config_loaded();
        state.lifecycle.mark_approval_store_initialized();
        state.lifecycle.update_upstream_health(true, None);
        state.lifecycle.mark_ready();

        let admin = AdminServer::with_config(
            state.lifecycle.clone(),
            state.prom_registry.clone(),
            AdminServerConfig::default(),
        );
        let router = admin.router();

        let request = Request::builder()
            .method("GET")
            .uri("/ready")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Now returns JSON with check details
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ready");
        assert_eq!(json["checks"]["config_loaded"], true);
        assert_eq!(json["checks"]["upstream_reachable"], true);
        assert_eq!(json["checks"]["approval_store_initialized"], true);
    }

    #[tokio::test]
    async fn test_metrics_endpoint() {
        let state = create_test_state();
        let admin = AdminServer::with_config(
            state.lifecycle.clone(),
            state.prom_registry.clone(),
            AdminServerConfig::default(),
        );
        let router = admin.router();

        let request = Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        // Metrics endpoint should return 200 OK
        assert_eq!(response.status(), StatusCode::OK);

        // Verify Content-Type is OpenMetrics
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("Content-Type header should be present");
        assert!(
            content_type
                .to_str()
                .expect("Content-Type should be valid string")
                .contains("openmetrics")
        );
    }

    #[tokio::test]
    async fn test_metrics_endpoint_with_registered_metrics() {
        use thoughtgate_core::telemetry::ThoughtGateMetrics;

        // Create registry with actual metrics registered
        let mut registry = Registry::default();
        let metrics = ThoughtGateMetrics::new(&mut registry);

        // Record some metrics
        metrics.record_request("tools/call", Some("test_tool"), "success");
        metrics.record_request_duration("tools/call", Some("test_tool"), 42.5);

        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        let prom_registry = Arc::new(registry);
        let admin =
            AdminServer::with_config(lifecycle, prom_registry, AdminServerConfig::default());
        let router = admin.router();

        let request = Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body);

        // Verify our metrics are present
        assert!(body_str.contains("thoughtgate_requests_total"));
        assert!(body_str.contains("thoughtgate_request_duration_ms"));
    }

    #[test]
    fn test_admin_config_default() {
        let config = AdminServerConfig::default();
        assert_eq!(config.port, 7469);
        assert_eq!(config.bind_addr, "127.0.0.1");
        assert_eq!(config.bind_string(), "127.0.0.1:7469");
    }

    #[test]
    fn test_admin_config_with_port() {
        let config = AdminServerConfig::with_port(9000);
        assert_eq!(config.port, 9000);
        assert_eq!(config.bind_string(), "127.0.0.1:9000");
    }
}
