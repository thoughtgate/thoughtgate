//! MCP HTTP server with axum.
//!
//! Implements: REQ-CORE-003/§5.2 (MCP Streamable HTTP Transport)
//!
//! Provides the `POST /mcp/v1` endpoint for receiving MCP JSON-RPC requests.
//!
//! # Architecture
//!
//! The server uses axum for HTTP handling with:
//! - Semaphore-based concurrency limiting
//! - Body size limits
//! - Proper JSON-RPC error responses
//!
//! # Request Flow
//!
//! 1. Receive POST request at `/mcp/v1`
//! 2. Check body size against limit
//! 3. Acquire semaphore permit (or return 503)
//! 4. Parse JSON-RPC request(s)
//! 5. Route each request to appropriate handler
//! 6. Return response(s)

use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use bytes::Bytes;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use crate::error::ThoughtGateError;
use crate::transport::jsonrpc::{JsonRpcResponse, McpRequest, ParsedRequests, parse_jsonrpc};
use crate::transport::router::{McpRouter, RouteTarget};
use crate::transport::upstream::{UpstreamClient, UpstreamConfig, UpstreamForwarder};

/// Configuration for the MCP server.
///
/// Implements: REQ-CORE-003/§5.3 (Configuration)
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    /// Listen address (e.g., "0.0.0.0:8080")
    pub listen_addr: String,
    /// Maximum request body size in bytes
    pub max_body_size: usize,
    /// Maximum concurrent requests
    pub max_concurrent_requests: usize,
    /// Upstream client configuration
    pub upstream: UpstreamConfig,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:8080".to_string(),
            max_body_size: 1024 * 1024, // 1MB
            max_concurrent_requests: 10000,
            upstream: UpstreamConfig::default(),
        }
    }
}

impl McpServerConfig {
    /// Load configuration from environment variables.
    ///
    /// Implements: REQ-CORE-003/§5.3 (Configuration)
    ///
    /// # Environment Variables
    ///
    /// - `THOUGHTGATE_LISTEN` (default: "0.0.0.0:8080"): Listen address
    /// - `THOUGHTGATE_MAX_REQUEST_BODY_BYTES` (default: 1048576): Max body size
    /// - `THOUGHTGATE_MAX_CONCURRENT_REQUESTS` (default: 10000): Max concurrent requests
    ///
    /// Plus all upstream configuration variables (see `UpstreamConfig::from_env`).
    ///
    /// # Errors
    ///
    /// Returns error if upstream configuration is invalid.
    pub fn from_env() -> Result<Self, ThoughtGateError> {
        let listen_addr =
            std::env::var("THOUGHTGATE_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

        let max_body_size: usize = std::env::var("THOUGHTGATE_MAX_REQUEST_BODY_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1024 * 1024); // 1MB default

        let max_concurrent_requests: usize = std::env::var("THOUGHTGATE_MAX_CONCURRENT_REQUESTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10000);

        Ok(Self {
            listen_addr,
            max_body_size,
            max_concurrent_requests,
            upstream: UpstreamConfig::from_env()?,
        })
    }
}

/// Shared application state.
///
/// This state is shared across all request handlers via axum's State extractor.
pub struct AppState {
    /// Upstream client for forwarding requests
    pub upstream: Arc<dyn UpstreamForwarder>,
    /// Method router
    pub router: McpRouter,
    /// Concurrency semaphore
    pub semaphore: Arc<Semaphore>,
    /// Maximum body size in bytes
    pub max_body_size: usize,
}

/// The MCP server.
///
/// Implements: REQ-CORE-003/§5.2 (MCP Streamable HTTP Transport)
pub struct McpServer {
    config: McpServerConfig,
    state: Arc<AppState>,
}

impl McpServer {
    /// Create a new MCP server.
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    ///
    /// # Errors
    ///
    /// Returns error if the upstream client cannot be created.
    pub fn new(config: McpServerConfig) -> Result<Self, ThoughtGateError> {
        let upstream = Arc::new(UpstreamClient::new(config.upstream.clone())?);
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_requests));

        let state = Arc::new(AppState {
            upstream,
            router: McpRouter::new(),
            semaphore,
            max_body_size: config.max_body_size,
        });

        Ok(Self { config, state })
    }

    /// Create a new MCP server with a custom upstream forwarder.
    ///
    /// This is useful for testing with mock upstreams.
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    /// * `upstream` - Custom upstream forwarder implementation
    pub fn with_upstream(config: McpServerConfig, upstream: Arc<dyn UpstreamForwarder>) -> Self {
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_requests));

        let state = Arc::new(AppState {
            upstream,
            router: McpRouter::new(),
            semaphore,
            max_body_size: config.max_body_size,
        });

        Self { config, state }
    }

    /// Create the axum Router.
    ///
    /// The router includes:
    /// - `POST /mcp/v1` - Main MCP endpoint
    pub fn router(&self) -> Router {
        Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(self.state.clone())
    }

    /// Run the server.
    ///
    /// This blocks until the server is shut down.
    ///
    /// # Errors
    ///
    /// Returns error if the server fails to bind or serve.
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = tokio::net::TcpListener::bind(&self.config.listen_addr).await?;
        info!(addr = %self.config.listen_addr, "MCP server listening");
        axum::serve(listener, self.router()).await?;
        Ok(())
    }
}

/// Handle POST /mcp/v1 requests.
///
/// Implements: REQ-CORE-003/§10 (Request Handler Pattern)
///
/// # Request Flow
///
/// 1. Check body size limit
/// 2. Acquire semaphore permit (EC-MCP-011)
/// 3. Parse JSON-RPC request(s)
/// 4. Route and handle each request
/// 5. Return response(s)
async fn handle_mcp_request(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    // Check body size limit
    if body.len() > state.max_body_size {
        let error = ThoughtGateError::InvalidRequest {
            details: format!(
                "Request body exceeds maximum size of {} bytes",
                state.max_body_size
            ),
        };
        return error_response(None, &error, "size-limit");
    }

    // Try to acquire semaphore permit (EC-MCP-011)
    let _permit = match state.semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            warn!("Max concurrent requests reached, returning 503");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32013,"message":"Service temporarily unavailable"}}"#,
            )
                .into_response();
        }
    };

    // Parse JSON-RPC request(s)
    let parsed = match parse_jsonrpc(&body) {
        Ok(p) => p,
        Err(e) => {
            return error_response(None, &e, "parse-error");
        }
    };

    match parsed {
        ParsedRequests::Single(request) => handle_single_request(&state, request).await,
        ParsedRequests::Batch(requests) => handle_batch_request(&state, requests).await,
    }
}

/// Handle a single JSON-RPC request.
///
/// # Arguments
///
/// * `state` - Application state
/// * `request` - The parsed MCP request
///
/// # Returns
///
/// HTTP response with JSON-RPC result or error.
async fn handle_single_request(state: &AppState, request: McpRequest) -> Response {
    let correlation_id = request.correlation_id.to_string();
    let id = request.id.clone();
    let is_notification = request.is_notification();

    debug!(
        correlation_id = %correlation_id,
        method = %request.method,
        is_notification = is_notification,
        is_task_augmented = request.is_task_augmented(),
        "Processing single request"
    );

    // Route the request
    let result = match state.router.route(request) {
        RouteTarget::PolicyEvaluation { request } => {
            // TODO: Integrate with policy engine (REQ-POL-001)
            // For now, pass through to upstream
            state.upstream.forward(&request).await
        }
        RouteTarget::TaskHandler { method, request } => {
            // TODO: Integrate with task handler (REQ-GOV-001)
            // For now, return method not found
            debug!(
                correlation_id = %correlation_id,
                task_method = ?method,
                "Task handler not yet implemented"
            );
            Err(ThoughtGateError::MethodNotFound {
                method: request.method,
            })
        }
        RouteTarget::PassThrough { request } => state.upstream.forward(&request).await,
    };

    // Handle notification - no response
    if is_notification {
        if let Err(e) = result {
            error!(
                correlation_id = %correlation_id,
                error = %e,
                "Notification processing failed"
            );
        }
        return StatusCode::NO_CONTENT.into_response();
    }

    // Return response
    match result {
        Ok(response) => json_response(&response),
        Err(e) => error_response(id, &e, &correlation_id),
    }
}

/// Handle a batch JSON-RPC request.
///
/// Implements: REQ-CORE-003/F-007 (Batch Request Handling)
///
/// # Arguments
///
/// * `state` - Application state
/// * `requests` - The parsed MCP requests
///
/// # Returns
///
/// HTTP response with JSON-RPC batch response or 204 No Content if all notifications.
///
/// # Design Note
///
/// Requests are processed sequentially rather than in parallel because:
/// 1. F-007.5 requires that if ANY request needs approval, the entire batch
///    becomes task-augmented - requests are not independent
/// 2. Sequential processing simplifies response ordering guarantees
/// 3. The upstream connection pool handles actual HTTP request parallelism
async fn handle_batch_request(state: &AppState, requests: Vec<McpRequest>) -> Response {
    let mut responses: Vec<JsonRpcResponse> = Vec::new();

    // Process each request sequentially - see Design Note above
    // F-007.5 (batch approval) will be added with REQ-GOV implementation
    for request in requests {
        let is_notification = request.is_notification();
        let id = request.id.clone();
        let correlation_id = request.correlation_id.to_string();

        let result = match state.router.route(request) {
            RouteTarget::PolicyEvaluation { request } => state.upstream.forward(&request).await,
            RouteTarget::TaskHandler { method: _, request } => {
                Err(ThoughtGateError::MethodNotFound {
                    method: request.method,
                })
            }
            RouteTarget::PassThrough { request } => state.upstream.forward(&request).await,
        };

        // F-007.4: Notifications don't produce response entries
        if is_notification {
            if let Err(e) = result {
                error!(
                    correlation_id = %correlation_id,
                    error = %e,
                    "Notification in batch failed"
                );
            }
            continue;
        }

        let response = match result {
            Ok(r) => r,
            Err(e) => JsonRpcResponse::error(id, e.to_jsonrpc_error(&correlation_id)),
        };
        responses.push(response);
    }

    // Return batch response
    if responses.is_empty() {
        // All were notifications
        return StatusCode::NO_CONTENT.into_response();
    }

    // Serialize batch response
    match serde_json::to_string(&responses) {
        Ok(json) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json,
        )
            .into_response(),
        Err(e) => {
            error!(error = %e, "Failed to serialize batch response");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error: failed to serialize response"}}"#,
            )
                .into_response()
        }
    }
}

/// Build a JSON response from a JsonRpcResponse.
fn json_response(response: &JsonRpcResponse) -> Response {
    match serde_json::to_string(response) {
        Ok(json) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json,
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}"#,
        )
            .into_response(),
    }
}

/// Build an error response.
///
/// # Arguments
///
/// * `id` - The request ID (may be None for parse errors)
/// * `error` - The ThoughtGateError
/// * `correlation_id` - Correlation ID for the error response
fn error_response(
    id: Option<crate::transport::jsonrpc::JsonRpcId>,
    error: &ThoughtGateError,
    correlation_id: &str,
) -> Response {
    let jsonrpc_error = error.to_jsonrpc_error(correlation_id);
    let response = JsonRpcResponse::error(id, jsonrpc_error);

    // JSON-RPC errors still return HTTP 200
    match serde_json::to_string(&response) {
        Ok(json) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json,
        )
            .into_response(),
        Err(_) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}"#,
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Mock upstream for testing.
    struct MockUpstream;

    #[async_trait::async_trait]
    impl UpstreamForwarder for MockUpstream {
        async fn forward(&self, request: &McpRequest) -> Result<JsonRpcResponse, ThoughtGateError> {
            Ok(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::json!({"mock": "response"}),
            ))
        }

        async fn forward_batch(
            &self,
            requests: &[McpRequest],
        ) -> Result<Vec<JsonRpcResponse>, ThoughtGateError> {
            Ok(requests
                .iter()
                .filter(|r| !r.is_notification())
                .map(|r| {
                    JsonRpcResponse::success(r.id.clone(), serde_json::json!({"mock": "response"}))
                })
                .collect())
        }
    }

    fn create_test_state() -> Arc<AppState> {
        Arc::new(AppState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            semaphore: Arc::new(Semaphore::new(100)),
            max_body_size: 1024 * 1024,
        })
    }

    async fn response_body(response: Response) -> String {
        let body = response.into_body();
        let bytes = body
            .collect()
            .await
            .expect("should collect body")
            .to_bytes();
        String::from_utf8(bytes.to_vec()).expect("should be utf8")
    }

    /// Verifies: EC-MCP-001 (Valid JSON-RPC request)
    #[tokio::test]
    async fn test_valid_request() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"test"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        assert!(body.contains("\"jsonrpc\":\"2.0\""));
        assert!(body.contains("\"result\""));
    }

    /// Verifies: EC-MCP-004 (Notification - no response)
    #[tokio::test]
    async fn test_notification_no_content() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body = r#"{"jsonrpc":"2.0","method":"test"}"#; // No id = notification
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    /// Verifies: EC-MCP-002 (Malformed JSON)
    #[tokio::test]
    async fn test_malformed_json() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body = r#"{"invalid json"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        // JSON-RPC errors return HTTP 200
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        assert!(body.contains("-32700")); // Parse error
    }

    /// Verifies: EC-MCP-003 (Missing jsonrpc field)
    #[tokio::test]
    async fn test_invalid_jsonrpc() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body = r#"{"id":1,"method":"test"}"#; // Missing jsonrpc
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        assert!(body.contains("-32600")); // Invalid Request
    }

    /// Verifies: EC-MCP-005 (Batch request)
    #[tokio::test]
    async fn test_batch_request() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body =
            r#"[{"jsonrpc":"2.0","id":1,"method":"a"},{"jsonrpc":"2.0","id":2,"method":"b"}]"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        // Should be an array response
        assert!(body.starts_with('['));
        assert!(body.ends_with(']'));
    }

    /// Verifies: EC-MCP-006 (Empty batch)
    #[tokio::test]
    async fn test_empty_batch() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body = r#"[]"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        assert!(body.contains("-32600")); // Invalid Request
    }

    /// Verifies: EC-MCP-011 (Max concurrency reached)
    #[tokio::test]
    async fn test_max_concurrency() {
        // Create state with 0 permits
        let state = Arc::new(AppState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            semaphore: Arc::new(Semaphore::new(0)), // No permits available
            max_body_size: 1024 * 1024,
        });

        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"test"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = response_body(response).await;
        assert!(body.contains("-32013")); // Service unavailable
    }

    #[tokio::test]
    async fn test_body_size_limit() {
        // Create state with small body limit
        let state = Arc::new(AppState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            semaphore: Arc::new(Semaphore::new(100)),
            max_body_size: 10, // Very small limit
        });

        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"test"}"#; // Exceeds 10 bytes
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        assert!(body.contains("-32600")); // Invalid Request
        assert!(body.contains("exceeds maximum size"));
    }

    /// Verifies: EC-MCP-013 (Integer ID preserved)
    #[tokio::test]
    async fn test_integer_id_preserved() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body = r#"{"jsonrpc":"2.0","id":42,"method":"test"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        let body = response_body(response).await;
        assert!(body.contains("\"id\":42"));
    }

    /// Verifies: EC-MCP-014 (String ID preserved)
    #[tokio::test]
    async fn test_string_id_preserved() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body = r#"{"jsonrpc":"2.0","id":"abc-123","method":"test"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        let body = response_body(response).await;
        assert!(body.contains("\"id\":\"abc-123\""));
    }

    #[tokio::test]
    async fn test_batch_with_notifications() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Batch with 2 requests and 1 notification
        let body = r#"[
            {"jsonrpc":"2.0","id":1,"method":"a"},
            {"jsonrpc":"2.0","method":"notify"},
            {"jsonrpc":"2.0","id":2,"method":"b"}
        ]"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("should parse response");
        // Should have 2 responses (notifications excluded)
        assert_eq!(parsed.len(), 2);
    }

    #[tokio::test]
    async fn test_batch_all_notifications() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Batch with only notifications
        let body = r#"[
            {"jsonrpc":"2.0","method":"a"},
            {"jsonrpc":"2.0","method":"b"}
        ]"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        // All notifications = no content
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[test]
    fn test_default_config() {
        let config = McpServerConfig::default();
        assert_eq!(config.listen_addr, "0.0.0.0:8080");
        assert_eq!(config.max_body_size, 1024 * 1024);
        assert_eq!(config.max_concurrent_requests, 10000);
    }
}
