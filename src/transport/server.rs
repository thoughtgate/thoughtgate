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
    extract::{DefaultBodyLimit, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use bytes::Bytes;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use crate::error::ThoughtGateError;
use crate::governance::{Principal, TaskHandler, TaskStore};
use crate::policy::engine::CedarEngine;
use crate::policy::principal::infer_principal;
use crate::policy::{CedarContext, CedarDecision, CedarRequest, CedarResource, TimeContext};
use crate::protocol::{TasksCancelRequest, TasksGetRequest, TasksListRequest, TasksResultRequest};
use crate::transport::jsonrpc::{JsonRpcResponse, McpRequest, ParsedRequests, parse_jsonrpc};
use crate::transport::router::{McpRouter, RouteTarget, TaskMethod};
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
    /// SEP-1686 task handler
    pub task_handler: TaskHandler,
    /// Cedar policy engine (Gate 3)
    pub cedar_engine: Arc<CedarEngine>,
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

/// Create governance components (TaskHandler + CedarEngine).
///
/// This is extracted to avoid duplication between `McpServer::new()` and
/// `McpServer::with_upstream()`.
fn create_governance_components() -> Result<(TaskHandler, Arc<CedarEngine>), ThoughtGateError> {
    // Create task store and handler for SEP-1686 task methods
    let task_store = Arc::new(TaskStore::with_defaults());
    let task_handler = TaskHandler::new(task_store);

    // Create Cedar policy engine (Gate 3)
    let cedar_engine =
        Arc::new(
            CedarEngine::new().map_err(|e| ThoughtGateError::ServiceUnavailable {
                reason: format!("Failed to create Cedar engine: {}", e),
            })?,
        );

    Ok((task_handler, cedar_engine))
}

impl McpServer {
    /// Create a new MCP server.
    ///
    /// Implements: REQ-CORE-003/§5.2 (MCP Streamable HTTP Transport)
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
        let (task_handler, cedar_engine) = create_governance_components()?;

        let state = Arc::new(AppState {
            upstream,
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            semaphore,
            max_body_size: config.max_body_size,
        });

        Ok(Self { config, state })
    }

    /// Create a new MCP server with a custom upstream forwarder.
    ///
    /// This is useful for testing with mock upstreams.
    ///
    /// Implements: REQ-CORE-003/§5.2 (MCP Streamable HTTP Transport)
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    /// * `upstream` - Custom upstream forwarder implementation
    pub fn with_upstream(
        config: McpServerConfig,
        upstream: Arc<dyn UpstreamForwarder>,
    ) -> Result<Self, ThoughtGateError> {
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_requests));
        let (task_handler, cedar_engine) = create_governance_components()?;

        let state = Arc::new(AppState {
            upstream,
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            semaphore,
            max_body_size: config.max_body_size,
        });

        Ok(Self { config, state })
    }

    /// Create the axum Router.
    ///
    /// Implements: REQ-CORE-003/F-002 (Method Routing)
    ///
    /// The router includes:
    /// - `POST /mcp/v1` - Main MCP endpoint
    /// - Body size limit enforced at HTTP layer (before buffering)
    pub fn router(&self) -> Router {
        Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .layer(DefaultBodyLimit::max(self.state.max_body_size))
            .with_state(self.state.clone())
    }

    /// Run the server.
    ///
    /// Implements: REQ-CORE-003/§5.2 (MCP Streamable HTTP Transport)
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
            // Implements: REQ-POL-001/F-001 (Cedar Policy Evaluation - Gate 3)
            evaluate_with_cedar(state, request).await
        }
        RouteTarget::TaskHandler { method, request } => {
            // Implements: REQ-GOV-001/F-003 through F-006 (SEP-1686 task methods)
            debug!(
                correlation_id = %correlation_id,
                task_method = ?method,
                "Handling task method"
            );
            handle_task_method(&state.task_handler, method, &request)
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

/// Handle SEP-1686 task method requests.
///
/// Implements: REQ-GOV-001/F-003 through F-006
///
/// # Arguments
///
/// * `handler` - The task handler
/// * `method` - The specific task method being called
/// * `request` - The MCP request
///
/// # Returns
///
/// JSON-RPC response or error.
fn handle_task_method(
    handler: &TaskHandler,
    method: TaskMethod,
    request: &McpRequest,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    // Extract params, defaulting to empty object
    let params = request.params.clone().unwrap_or(serde_json::json!({}));

    match method {
        TaskMethod::Get => {
            // Implements: REQ-GOV-001/F-003 (tasks/get)
            let req: TasksGetRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/get params: {}", e),
                })?;

            let result = handler
                .handle_tasks_get(req)
                .map_err(task_error_to_thoughtgate)?;

            Ok(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::to_value(result).map_err(|e| ThoughtGateError::ServiceUnavailable {
                    reason: format!("Failed to serialize tasks/get response: {}", e),
                })?,
            ))
        }

        TaskMethod::Result => {
            // Implements: REQ-GOV-001/F-004 (tasks/result)
            let req: TasksResultRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/result params: {}", e),
                })?;

            let result = handler
                .handle_tasks_result(req)
                .map_err(task_error_to_thoughtgate)?;

            Ok(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::to_value(result).map_err(|e| ThoughtGateError::ServiceUnavailable {
                    reason: format!("Failed to serialize tasks/result response: {}", e),
                })?,
            ))
        }

        TaskMethod::List => {
            // Implements: REQ-GOV-001/F-005 (tasks/list)
            let req: TasksListRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/list params: {}", e),
                })?;

            // TODO(v0.3): Extract principal from request authentication context.
            // For v0.2, use a hardcoded default principal. When REQ-CFG-001 (YAML config)
            // is implemented, this should use the authenticated caller identity.
            let principal = Principal::new("default");

            let result = handler.handle_tasks_list(req, &principal);

            Ok(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::to_value(result).map_err(|e| ThoughtGateError::ServiceUnavailable {
                    reason: format!("Failed to serialize tasks/list response: {}", e),
                })?,
            ))
        }

        TaskMethod::Cancel => {
            // Implements: REQ-GOV-001/F-006 (tasks/cancel)
            let req: TasksCancelRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/cancel params: {}", e),
                })?;

            let result = handler
                .handle_tasks_cancel(req)
                .map_err(task_error_to_thoughtgate)?;

            Ok(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::to_value(result).map_err(|e| ThoughtGateError::ServiceUnavailable {
                    reason: format!("Failed to serialize tasks/cancel response: {}", e),
                })?,
            ))
        }
    }
}

/// Convert TaskError to ThoughtGateError.
///
/// Implements: REQ-CORE-004 (Error Handling)
fn task_error_to_thoughtgate(error: crate::governance::TaskError) -> ThoughtGateError {
    use crate::governance::TaskError;

    match error {
        TaskError::NotFound { task_id } => ThoughtGateError::TaskNotFound {
            task_id: task_id.to_string(),
        },
        TaskError::Expired { task_id } => ThoughtGateError::TaskExpired {
            task_id: task_id.to_string(),
        },
        TaskError::AlreadyTerminal { task_id, status } => ThoughtGateError::InvalidRequest {
            details: format!("Task {} is already in terminal state: {}", task_id, status),
        },
        TaskError::InvalidTransition { task_id, from, to } => ThoughtGateError::InvalidRequest {
            details: format!(
                "Invalid task transition for {}: {} -> {}",
                task_id, from, to
            ),
        },
        TaskError::ConcurrentModification {
            task_id,
            expected,
            actual,
        } => ThoughtGateError::InvalidRequest {
            details: format!(
                "Concurrent modification of task {}: expected {}, found {}",
                task_id, expected, actual
            ),
        },
        TaskError::RateLimited { retry_after, .. } => ThoughtGateError::RateLimited {
            retry_after_secs: Some(retry_after.as_secs()),
        },
        TaskError::ResultNotReady { task_id } => ThoughtGateError::TaskResultNotReady {
            task_id: task_id.to_string(),
        },
        TaskError::CapacityExceeded => ThoughtGateError::ServiceUnavailable {
            reason: "Task capacity exceeded".to_string(),
        },
        TaskError::Internal { details } => ThoughtGateError::ServiceUnavailable {
            reason: format!("Internal task error: {}", details),
        },
    }
}

/// Evaluate a request against Cedar policy engine (Gate 3).
///
/// Implements: REQ-POL-001/F-001 (Policy Evaluation)
///
/// For `tools/call` requests, evaluates Cedar policy:
/// - Permit → forward to upstream
/// - Forbid → return PolicyDenied error
///
/// For other methods (tools/list, resources/*, prompts/*), passes through to upstream.
async fn evaluate_with_cedar(
    state: &AppState,
    request: McpRequest,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    // Only evaluate tools/call requests with Cedar
    if request.method != "tools/call" {
        // For tools/list, resources/*, prompts/* - pass through to upstream
        return state.upstream.forward(&request).await;
    }

    // Extract tool name and arguments from params
    let params = request.params.clone().unwrap_or(serde_json::json!({}));
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    // Infer principal from environment
    let policy_principal = infer_principal().map_err(|e| ThoughtGateError::ServiceUnavailable {
        reason: format!("Failed to infer principal: {}", e),
    })?;

    // Build Cedar request
    let cedar_request = CedarRequest {
        principal: policy_principal,
        resource: CedarResource::ToolCall {
            name: tool_name.clone(),
            server: "upstream".to_string(), // v0.2: single upstream
            arguments,
        },
        context: CedarContext {
            policy_id: "default".to_string(), // v0.2: no YAML governance rules
            source_id: "upstream".to_string(),
            time: TimeContext::now(),
        },
    };

    // Evaluate Cedar policy
    match state.cedar_engine.evaluate_v2(&cedar_request) {
        CedarDecision::Permit { .. } => {
            // v0.2: Permit means forward immediately (no approval workflow)
            debug!(
                tool = %tool_name,
                "Cedar permit - forwarding to upstream"
            );
            state.upstream.forward(&request).await
        }
        CedarDecision::Forbid { reason, .. } => {
            // Cedar forbid → return PolicyDenied error
            warn!(
                tool = %tool_name,
                reason = %reason,
                "Cedar forbid - denying request"
            );
            Err(ThoughtGateError::PolicyDenied {
                tool: tool_name,
                policy_id: None, // v0.2: policy_id not tracked
                reason: Some(reason),
            })
        }
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
            RouteTarget::PolicyEvaluation { request } => evaluate_with_cedar(state, request).await,
            RouteTarget::TaskHandler { method, request } => {
                handle_task_method(&state.task_handler, method, &request)
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
    use serial_test::serial;
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
        let task_store = Arc::new(TaskStore::with_defaults());
        let task_handler = TaskHandler::new(task_store);
        let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));

        Arc::new(AppState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
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
        let task_store = Arc::new(TaskStore::with_defaults());
        let task_handler = TaskHandler::new(task_store);
        let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));

        let state = Arc::new(AppState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
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
        let task_store = Arc::new(TaskStore::with_defaults());
        let task_handler = TaskHandler::new(task_store);
        let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));

        let state = Arc::new(AppState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            semaphore: Arc::new(Semaphore::new(100)),
            max_body_size: 10, // Very small limit
        });

        // Router with DefaultBodyLimit layer - rejects oversized bodies at HTTP layer
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .layer(DefaultBodyLimit::max(state.max_body_size))
            .with_state(state);

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"test"}"#; // Exceeds 10 bytes
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        // DefaultBodyLimit returns 413 Payload Too Large
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
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

    // ========================================================================
    // SEP-1686 Task Handler Integration Tests
    // ========================================================================

    /// Verifies: REQ-GOV-001/F-005 (tasks/list routing)
    #[tokio::test]
    async fn test_tasks_list_routing() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Request tasks/list - should route to TaskHandler and return empty list
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tasks/list"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("should parse response");

        // Should be a success response with empty tasks array
        assert!(parsed.get("result").is_some(), "Should have result");
        let result = &parsed["result"];
        assert!(result.get("tasks").is_some(), "Should have tasks array");
        assert!(
            result["tasks"].as_array().unwrap().is_empty(),
            "Tasks should be empty"
        );
    }

    /// Verifies: REQ-GOV-001/F-003 (tasks/get routing - task not found)
    #[tokio::test]
    async fn test_tasks_get_not_found() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Request tasks/get with non-existent task ID
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tasks/get","params":{"taskId":"tg_nonexistent12345678"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("should parse response");

        // Should be an error response with task not found code
        assert!(parsed.get("error").is_some(), "Should have error");
        let error = &parsed["error"];
        assert_eq!(
            error["code"].as_i64().unwrap(),
            -32004,
            "Should be TaskNotFound error code"
        );
    }

    /// Verifies: REQ-GOV-001/F-006 (tasks/cancel routing - task not found)
    #[tokio::test]
    async fn test_tasks_cancel_not_found() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Request tasks/cancel with non-existent task ID
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tasks/cancel","params":{"taskId":"tg_nonexistent12345678"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("should parse response");

        // Should be an error response with task not found code
        assert!(parsed.get("error").is_some(), "Should have error");
        let error = &parsed["error"];
        assert_eq!(
            error["code"].as_i64().unwrap(),
            -32004,
            "Should be TaskNotFound error code"
        );
    }

    /// Verifies: REQ-GOV-001/F-004 (tasks/result routing - task not found)
    #[tokio::test]
    async fn test_tasks_result_not_found() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Request tasks/result with non-existent task ID
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tasks/result","params":{"taskId":"tg_nonexistent12345678"}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("should parse response");

        // Should be an error response with task not found code
        assert!(parsed.get("error").is_some(), "Should have error");
        let error = &parsed["error"];
        assert_eq!(
            error["code"].as_i64().unwrap(),
            -32004,
            "Should be TaskNotFound error code"
        );
    }

    /// Verifies: REQ-GOV-001/F-003 (tasks/get - invalid params)
    #[tokio::test]
    async fn test_tasks_get_invalid_params() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Request tasks/get with missing taskId
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tasks/get","params":{}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("should parse response");

        // Should be an error response with invalid params code
        assert!(parsed.get("error").is_some(), "Should have error");
        let error = &parsed["error"];
        assert_eq!(
            error["code"].as_i64().unwrap(),
            -32602,
            "Should be InvalidParams error code"
        );
    }

    // ========================================================================
    // Cedar Policy Integration Tests (Gate 3)
    // ========================================================================

    /// Helper to create test state with custom Cedar policy.
    fn create_test_state_with_policy(policy: &str) -> Arc<AppState> {
        // Set policy env var (caller must use #[serial] and clean up)
        unsafe {
            std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
            std::env::set_var("THOUGHTGATE_POLICIES", policy);
        }

        let task_store = Arc::new(TaskStore::with_defaults());
        let task_handler = TaskHandler::new(task_store);
        let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));

        Arc::new(AppState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            semaphore: Arc::new(Semaphore::new(100)),
            max_body_size: 1024 * 1024,
        })
    }

    /// Verifies: REQ-POL-001/F-001 (Cedar permit → forward to upstream)
    #[tokio::test]
    #[serial]
    async fn test_tools_call_cedar_permit() {
        // Policy that permits all tools/call requests
        let policy = r#"
            permit(
                principal,
                action == ThoughtGate::Action::"tools/call",
                resource
            );
        "#;

        let state = create_test_state_with_policy(policy);
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Request tools/call - should be permitted and forwarded
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"test_tool","arguments":{}}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("should parse response");

        // Should be a success response (mock upstream returns {"mock":"response"})
        assert!(
            parsed.get("result").is_some(),
            "Should have result (permit → forward)"
        );
        assert_eq!(parsed["result"]["mock"], "response");

        unsafe {
            std::env::remove_var("THOUGHTGATE_DEV_MODE");
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    /// Verifies: REQ-POL-001/F-001 (Cedar forbid → PolicyDenied error)
    #[tokio::test]
    #[serial]
    async fn test_tools_call_cedar_forbid() {
        // Policy that only permits a different principal
        let policy = r#"
            permit(
                principal == ThoughtGate::App::"other-app",
                action == ThoughtGate::Action::"tools/call",
                resource
            );
        "#;

        let state = create_test_state_with_policy(policy);
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Request tools/call - should be forbidden (dev-app != other-app)
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"delete_user","arguments":{}}}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("should parse response");

        // Should be a PolicyDenied error (-32003)
        assert!(parsed.get("error").is_some(), "Should have error (forbid)");
        let error = &parsed["error"];
        assert_eq!(
            error["code"].as_i64().unwrap(),
            -32003,
            "Should be PolicyDenied error code"
        );
        assert!(
            error["message"].as_str().unwrap().contains("denied"),
            "Error message should mention denial"
        );

        unsafe {
            std::env::remove_var("THOUGHTGATE_DEV_MODE");
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    /// Verifies: REQ-POL-001 (tools/list passes through without Cedar evaluation)
    #[tokio::test]
    #[serial]
    async fn test_tools_list_bypasses_cedar() {
        // Restrictive policy that denies everything
        let policy = r#"
            permit(
                principal == ThoughtGate::App::"nonexistent",
                action,
                resource
            );
        "#;

        let state = create_test_state_with_policy(policy);
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Request tools/list - should bypass Cedar and forward to upstream
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("should parse response");

        // Should be a success response (tools/list bypasses Cedar)
        assert!(
            parsed.get("result").is_some(),
            "Should have result (tools/list bypasses Cedar)"
        );

        unsafe {
            std::env::remove_var("THOUGHTGATE_DEV_MODE");
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }
}
