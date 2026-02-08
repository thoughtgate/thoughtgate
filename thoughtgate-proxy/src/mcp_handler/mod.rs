//! MCP request handler and optional HTTP server.
//!
//! Implements: REQ-CORE-003/§5.2 (MCP Streamable HTTP Transport)
//!
//! # Overview
//!
//! This module provides `McpHandler`, a direct handler that processes buffered
//! request bodies. Used by ProxyService for in-process MCP handling.
//!
//! # Request Flow
//!
//! 1. Receive buffered request body (Bytes)
//! 2. Check body size against limit
//! 3. Acquire semaphore permit (or return 503)
//! 4. Parse JSON-RPC request(s)
//! 5. Route each request via McpRouter
//! 6. Execute policy evaluation (Cedar) or task handling
//! 7. Return JSON-RPC response(s)

mod capabilities;
pub(crate) mod cedar_eval;
mod factory;
pub(crate) mod gate_routing;
mod handler_config;
mod helpers;
mod request;
pub(crate) mod task_methods;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use bytes::Bytes;
use tokio::sync::Semaphore;

use thoughtgate_core::config::Config;
use thoughtgate_core::governance::evaluator::GovernanceEvaluator;
use thoughtgate_core::governance::{ApprovalEngine, Principal, TaskHandler, TaskStore};
use thoughtgate_core::policy::engine::CedarEngine;
use thoughtgate_core::profile::Profile;
use thoughtgate_core::protocol::CapabilityCache;
use thoughtgate_core::telemetry::ThoughtGateMetrics;
use thoughtgate_core::transport::jsonrpc::McpRequest;
use thoughtgate_core::transport::router::{McpRouter, RouteTarget};
use thoughtgate_core::transport::upstream::UpstreamForwarder;

// Re-exported for tests (used via `use super::*` in mod tests).
#[cfg(test)]
pub(crate) use axum::http::StatusCode;
#[cfg(test)]
pub(crate) use thoughtgate_core::error::ThoughtGateError;
#[cfg(test)]
pub(crate) use thoughtgate_core::transport::jsonrpc::JsonRpcResponse;

// Re-export public API
pub use factory::create_governance_components_with_metrics;
pub use handler_config::McpHandlerConfig;
pub use request::McpResponse;

// Re-export for gate_routing (used via `super::`)
use capabilities::validate_task_metadata;

/// Shared MCP handler state.
///
/// This state is shared across all request handlers via `McpHandler`.
pub struct McpState {
    /// Upstream client for forwarding requests
    pub upstream: Arc<dyn UpstreamForwarder>,
    /// Method router
    pub router: McpRouter,
    /// SEP-1686 task handler
    pub task_handler: TaskHandler,
    /// Cedar policy engine (Gate 3)
    pub cedar_engine: Arc<CedarEngine>,
    /// YAML configuration (Gate 1 & 2)
    pub config: Option<Arc<Config>>,
    /// Approval engine (Gate 4)
    pub approval_engine: Option<Arc<ApprovalEngine>>,
    /// Concurrency semaphore
    pub semaphore: Arc<Semaphore>,
    /// Maximum body size in bytes
    pub max_body_size: usize,
    /// Maximum number of requests in a JSON-RPC batch
    pub max_batch_size: usize,
    /// Capability cache for upstream detection (REQ-CORE-007)
    pub capability_cache: Arc<CapabilityCache>,
    /// Current aggregate buffered bytes across all in-flight requests.
    pub buffered_bytes: AtomicUsize,
    /// Maximum aggregate buffered bytes before rejecting new requests.
    pub max_aggregate_buffer: usize,
    /// Prometheus metrics for request counting and latency (REQ-OBS-002 §6).
    pub tg_metrics: Option<Arc<ThoughtGateMetrics>>,
    /// Unified governance evaluator (Gates 1-4).
    /// When present, `route_through_gates()` delegates to this instead of
    /// reimplementing gate logic. None in legacy mode (no YAML config).
    pub evaluator: Option<Arc<GovernanceEvaluator>>,
    /// Global fallback timeout (seconds) for blocking approval mode.
    pub blocking_approval_timeout_secs: u64,
    /// Governance profile (production or development).
    pub profile: Profile,
}

/// MCP request handler for direct invocation.
///
/// This handler processes buffered MCP request bodies and returns HTTP responses.
/// It's designed to be called directly from ProxyService without going through
/// a separate Axum server.
///
/// # Example
///
/// ```rust,ignore
/// use thoughtgate_proxy::mcp_handler::{McpHandler, McpHandlerConfig};
///
/// let handler = McpHandler::new(
///     upstream,
///     cedar_engine,
///     task_store,
///     McpHandlerConfig::default(),
/// )?;
///
/// let response = handler.handle(body_bytes).await;
/// ```
///
/// # Traceability
/// - Implements: REQ-CORE-003/F-002 (Method Routing)
/// - Implements: REQ-POL-001/F-001 (Cedar Policy Evaluation)
pub struct McpHandler {
    state: Arc<McpState>,
}

impl McpHandler {
    /// Create a new MCP handler.
    ///
    /// # Arguments
    ///
    /// * `upstream` - Upstream forwarder for proxying requests
    /// * `cedar_engine` - Cedar policy engine for authorization
    /// * `task_store` - Task store for SEP-1686 task methods
    /// * `config` - Handler configuration
    ///
    /// # Returns
    ///
    /// A new McpHandler ready to process requests.
    pub fn new(
        upstream: Arc<dyn UpstreamForwarder>,
        cedar_engine: Arc<CedarEngine>,
        task_store: Arc<TaskStore>,
        config: McpHandlerConfig,
    ) -> Self {
        let task_handler = TaskHandler::new(task_store);
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_requests));

        let state = Arc::new(McpState {
            upstream,
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            config: None,
            approval_engine: None,
            semaphore,
            max_body_size: config.max_body_size,
            max_batch_size: config.max_batch_size,
            capability_cache: Arc::new(CapabilityCache::new()),
            buffered_bytes: AtomicUsize::new(0),
            max_aggregate_buffer: config.max_aggregate_buffer,
            tg_metrics: None,
            evaluator: None,
            blocking_approval_timeout_secs: config.blocking_approval_timeout_secs,
            profile: Profile::Production,
        });

        Self { state }
    }

    /// Create a new MCP handler with full 4-gate governance stack.
    ///
    /// This constructor is used when wiring governance into `ProxyService` in `main.rs`.
    /// It accepts pre-built governance components from `create_governance_components()`.
    ///
    /// # Arguments
    ///
    /// * `upstream` - Upstream forwarder for proxying requests
    /// * `cedar_engine` - Cedar policy engine (Gate 3)
    /// * `task_store` - Task store for SEP-1686 task methods
    /// * `handler_config` - Handler configuration (body size, concurrency)
    /// * `yaml_config` - Optional YAML configuration (Gates 1 & 2)
    /// * `approval_engine` - Optional approval engine (Gate 4)
    /// * `tg_metrics` - Optional Prometheus metrics (REQ-OBS-002 §6)
    ///
    /// # Returns
    ///
    /// A new McpHandler with full governance wiring.
    ///
    /// # Traceability
    /// - Implements: REQ-GOV-002 (Governance Pipeline)
    /// - Implements: REQ-OBS-002 §6 (Prometheus Metrics)
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_governance(
        upstream: Arc<dyn UpstreamForwarder>,
        cedar_engine: Arc<CedarEngine>,
        task_store: Arc<TaskStore>,
        handler_config: McpHandlerConfig,
        yaml_config: Option<Arc<Config>>,
        approval_engine: Option<Arc<ApprovalEngine>>,
        tg_metrics: Option<Arc<ThoughtGateMetrics>>,
        profile: Profile,
    ) -> Self {
        let task_handler = TaskHandler::new(task_store.clone());
        let semaphore = Arc::new(Semaphore::new(handler_config.max_concurrent_requests));

        // Construct the unified evaluator when YAML config is present.
        // In legacy mode (no config), route_request() falls back to
        // evaluate_with_cedar() directly, so no evaluator is needed.
        let evaluator = yaml_config.as_ref().map(|config| {
            let principal = Principal::new("proxy");
            let mut eval = GovernanceEvaluator::new(
                config.clone(),
                Some(cedar_engine.clone()),
                task_store,
                principal,
                profile,
            );
            if let Some(ref metrics) = tg_metrics {
                eval = eval.with_metrics(metrics.clone());
            }
            if let Some(ref engine) = approval_engine {
                eval = eval.with_approval_engine(engine.clone());
            }
            Arc::new(eval)
        });

        let state = Arc::new(McpState {
            upstream,
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            config: yaml_config,
            approval_engine,
            semaphore,
            max_body_size: handler_config.max_body_size,
            max_batch_size: handler_config.max_batch_size,
            capability_cache: Arc::new(CapabilityCache::new()),
            buffered_bytes: AtomicUsize::new(0),
            max_aggregate_buffer: handler_config.max_aggregate_buffer,
            tg_metrics,
            evaluator,
            blocking_approval_timeout_secs: handler_config.blocking_approval_timeout_secs,
            profile,
        });

        Self { state }
    }

    /// Get the maximum body size.
    pub fn max_body_size(&self) -> usize {
        self.state.max_body_size
    }

    /// Handle a buffered MCP request body.
    ///
    /// This is the main entry point for processing MCP requests. It:
    /// 1. Checks body size
    /// 2. Acquires concurrency permit
    /// 3. Parses JSON-RPC
    /// 4. Routes and executes the request(s)
    ///
    /// # Arguments
    ///
    /// * `body` - The buffered request body
    ///
    /// # Returns
    ///
    /// An `McpResponse` — either buffered bytes or a streaming body for blocking
    /// approval mode.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/§10 (Request Handler Pattern)
    pub async fn handle(&self, body: Bytes) -> request::McpResponse {
        request::handle_mcp_body_bytes(&self.state, body, None).await
    }

    /// Handle a buffered MCP request body with W3C trace context.
    ///
    /// This entry point accepts an OpenTelemetry `Context` extracted from
    /// inbound HTTP headers, enabling MCP spans to become children of the
    /// caller's trace for end-to-end distributed tracing.
    ///
    /// # Arguments
    ///
    /// * `body` - The buffered request body
    /// * `parent_context` - OpenTelemetry context extracted from HTTP headers
    ///
    /// # Returns
    ///
    /// An `McpResponse` — either buffered bytes or a streaming body for blocking
    /// approval mode.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/§10 (Request Handler Pattern)
    /// - Implements: REQ-OBS-002 §7.1 (W3C Trace Context Propagation)
    pub async fn handle_with_context(
        &self,
        body: Bytes,
        parent_context: opentelemetry::Context,
    ) -> request::McpResponse {
        request::handle_mcp_body_bytes(&self.state, body, Some(parent_context)).await
    }

    /// Get a reference to the internal state (for testing).
    #[cfg(test)]
    pub fn state(&self) -> &Arc<McpState> {
        &self.state
    }
}

impl Clone for McpHandler {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

// ============================================================================
// Request Routing
// ============================================================================

/// Routes a single MCP request through the appropriate handler.
///
/// Implements: REQ-CORE-003/F-002 (Method Routing)
///
/// Shared routing logic used by both single and batch request handlers.
/// Dispatches to the 4-gate model, task handler, initialize handler,
/// or pass-through based on the router's classification.
async fn route_request(state: &McpState, request: McpRequest) -> gate_routing::GateResult {
    use gate_routing::GateResult;
    match state.router.route(request) {
        RouteTarget::PolicyEvaluation { request } => {
            // Apply 4-gate model for governable methods when config is present
            if state.config.is_some() {
                if thoughtgate_core::governance::evaluator::method_requires_gates(&request.method) {
                    // Governable methods: tools/call, resources/read, etc.
                    gate_routing::route_through_gates(state, request).await
                } else if helpers::is_list_method(&request.method) {
                    // List methods: tools/list, resources/list, prompts/list
                    // Intercept response, apply Gate 1 filter, annotate taskSupport
                    GateResult::Immediate(capabilities::handle_list_method(state, request).await)
                } else {
                    // Other methods: forward directly
                    GateResult::Immediate(state.upstream.forward(&request).await)
                }
            } else {
                // Legacy mode (no config): direct Cedar evaluation (Gate 3 only)
                GateResult::Immediate(
                    cedar_eval::evaluate_with_cedar(state, request, None, None).await,
                )
            }
        }
        RouteTarget::TaskHandler { method, request } => {
            GateResult::Immediate(task_methods::handle_task_method(state, method, &request).await)
        }
        RouteTarget::InitializeHandler { request } => {
            // Implements: REQ-CORE-007/F-001 (Capability Injection)
            GateResult::Immediate(capabilities::handle_initialize_method(state, request).await)
        }
        RouteTarget::PassThrough { request } => {
            GateResult::Immediate(state.upstream.forward(&request).await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::extract::DefaultBodyLimit;
    use axum::http::Request;
    use axum::routing::post;
    use http_body_util::BodyExt;
    use serial_test::serial;
    use tower::ServiceExt;

    use thoughtgate_core::governance::TaskStore;
    use thoughtgate_core::protocol::CapabilityCache;

    // Axum test handler is in request.rs
    use request::handle_mcp_request;

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

    fn create_test_state() -> Arc<McpState> {
        let task_store = Arc::new(TaskStore::with_defaults());
        let task_handler = TaskHandler::new(task_store);
        let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));

        Arc::new(McpState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            config: None,
            approval_engine: None,
            semaphore: Arc::new(Semaphore::new(100)),
            max_body_size: 1024 * 1024,
            max_batch_size: 100,
            capability_cache: Arc::new(CapabilityCache::new()),
            buffered_bytes: AtomicUsize::new(0),
            max_aggregate_buffer: 512 * 1024 * 1024,
            tg_metrics: None,
            evaluator: None,
            blocking_approval_timeout_secs: 300,
            profile: Profile::Production,
        })
    }

    async fn response_body(response: axum::response::Response) -> String {
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

        let state = Arc::new(McpState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            config: None,
            approval_engine: None,
            semaphore: Arc::new(Semaphore::new(0)), // No permits available
            max_body_size: 1024 * 1024,
            max_batch_size: 100,
            capability_cache: Arc::new(CapabilityCache::new()),
            buffered_bytes: AtomicUsize::new(0),
            max_aggregate_buffer: 512 * 1024 * 1024,
            tg_metrics: None,
            evaluator: None,
            blocking_approval_timeout_secs: 300,
            profile: Profile::Production,
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
        // MCP clients expect JSON-RPC errors over HTTP 200 (not HTTP 503)
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        assert!(
            body.contains("-32009"),
            "Expected RateLimited error code (-32009), got: {}",
            body
        );
    }

    /// Verifies: EC-MCP-004 (Body size limit returns JSON-RPC error)
    #[tokio::test]
    async fn test_body_size_limit() {
        // Create state with small body limit
        let task_store = Arc::new(TaskStore::with_defaults());
        let task_handler = TaskHandler::new(task_store);
        let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));

        let state = Arc::new(McpState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            config: None,
            approval_engine: None,
            semaphore: Arc::new(Semaphore::new(100)),
            max_body_size: 10, // Very small limit
            max_batch_size: 100,
            capability_cache: Arc::new(CapabilityCache::new()),
            buffered_bytes: AtomicUsize::new(0),
            max_aggregate_buffer: 512 * 1024 * 1024,
            tg_metrics: None,
            evaluator: None,
            blocking_approval_timeout_secs: 300,
            profile: Profile::Production,
        });

        // Router with DefaultBodyLimit disabled - we check size manually and return JSON-RPC error
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .layer(DefaultBodyLimit::disable())
            .with_state(state);

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"test"}"#; // Exceeds 10 bytes
        let request = Request::builder()
            .method("POST")
            .uri("/mcp/v1")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("should build request");

        let response = router.oneshot(request).await.expect("should get response");
        // EC-MCP-004: Returns JSON-RPC error, not HTTP 413
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body(response).await;
        assert!(body.contains("-32600")); // InvalidRequest error code
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
        // All notifications in batch = 204 No Content per JSON-RPC 2.0 §6:
        // "The client MUST NOT expect the server to return any Response for
        // a Batch that only contains Notification objects."
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let body_str = response_body(response).await;
        assert!(body_str.is_empty());
    }

    // ========================================================================
    // SEP-1686 Task Handler Integration Tests
    // ========================================================================

    /// Verifies: REQ-GOV-001/F-005 (tasks/list routing)
    #[tokio::test]
    #[serial]
    async fn test_tasks_list_routing() {
        // Set dev mode to allow principal inference without K8s identity
        unsafe {
            std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
        }

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

        unsafe {
            std::env::remove_var("THOUGHTGATE_DEV_MODE");
        }
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
            -32602,
            "Should be TaskNotFound (Invalid params) error code"
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
            -32602,
            "Should be TaskNotFound (Invalid params) error code"
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
            -32602,
            "Should be TaskNotFound (Invalid params) error code"
        );
    }

    /// Verifies: REQ-GOV-001/F-003 (tasks/get - invalid params)
    #[tokio::test]
    async fn test_tasks_get_missing_taskid_forwarded() {
        let state = create_test_state();
        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        // Request tasks/get with missing taskId — forwarded to upstream
        // (no tg_ prefix means it's not a local task)
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

        // Without a tg_ taskId, the router forwards to upstream (PassThrough)
        // MockUpstream returns a success response
        assert!(
            parsed.get("result").is_some(),
            "Should be forwarded to upstream (success response)"
        );
    }

    // ========================================================================
    // Cedar Policy Integration Tests (Gate 3)
    // ========================================================================

    /// Helper to create test state with custom Cedar policy.
    fn create_test_state_with_policy(policy: &str) -> Arc<McpState> {
        // Set policy env var (caller must use #[serial] and clean up)
        unsafe {
            std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
            std::env::set_var("THOUGHTGATE_POLICIES", policy);
        }

        let task_store = Arc::new(TaskStore::with_defaults());
        let task_handler = TaskHandler::new(task_store);
        let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));

        Arc::new(McpState {
            upstream: Arc::new(MockUpstream),
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            config: None,
            approval_engine: None,
            semaphore: Arc::new(Semaphore::new(100)),
            max_body_size: 1024 * 1024,
            max_batch_size: 100,
            capability_cache: Arc::new(CapabilityCache::new()),
            buffered_bytes: AtomicUsize::new(0),
            max_aggregate_buffer: 512 * 1024 * 1024,
            tg_metrics: None,
            evaluator: None,
            blocking_approval_timeout_secs: 300,
            profile: Profile::Production,
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

    // ========================================================================
    // Initialize Capability Injection Tests (REQ-CORE-007)
    // ========================================================================

    /// Mock upstream that returns a specific initialize response.
    struct MockInitializeUpstream {
        /// The response to return for initialize requests.
        response: serde_json::Value,
    }

    impl MockInitializeUpstream {
        fn with_capabilities(tasks: bool, sse: bool) -> Self {
            let mut capabilities = serde_json::Map::new();

            if tasks {
                capabilities.insert(
                    "tasks".to_string(),
                    serde_json::json!({
                        "requests": {
                            "tools/call": true
                        }
                    }),
                );
            }

            if sse {
                capabilities.insert(
                    "notifications".to_string(),
                    serde_json::json!({
                        "tasks": {
                            "status": true
                        }
                    }),
                );
            }

            let response = serde_json::json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "mock-upstream",
                    "version": "1.0.0"
                },
                "capabilities": capabilities
            });

            Self { response }
        }

        fn without_capabilities() -> Self {
            Self {
                response: serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": "mock-upstream",
                        "version": "1.0.0"
                    },
                    "capabilities": {}
                }),
            }
        }
    }

    #[async_trait::async_trait]
    impl UpstreamForwarder for MockInitializeUpstream {
        async fn forward(&self, request: &McpRequest) -> Result<JsonRpcResponse, ThoughtGateError> {
            if request.method == "initialize" {
                Ok(JsonRpcResponse::success(
                    request.id.clone(),
                    self.response.clone(),
                ))
            } else {
                Ok(JsonRpcResponse::success(
                    request.id.clone(),
                    serde_json::json!({"mock": "response"}),
                ))
            }
        }

        async fn forward_batch(
            &self,
            requests: &[McpRequest],
        ) -> Result<Vec<JsonRpcResponse>, ThoughtGateError> {
            let mut responses = Vec::new();
            for request in requests {
                if !request.is_notification() {
                    responses.push(self.forward(request).await?);
                }
            }
            Ok(responses)
        }
    }

    fn create_test_state_with_upstream(upstream: Arc<dyn UpstreamForwarder>) -> Arc<McpState> {
        let task_store = Arc::new(TaskStore::with_defaults());
        let task_handler = TaskHandler::new(task_store);
        let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));

        Arc::new(McpState {
            upstream,
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            config: None,
            approval_engine: None,
            semaphore: Arc::new(Semaphore::new(100)),
            max_body_size: 1024 * 1024,
            max_batch_size: 100,
            capability_cache: Arc::new(CapabilityCache::new()),
            buffered_bytes: AtomicUsize::new(0),
            max_aggregate_buffer: 512 * 1024 * 1024,
            tg_metrics: None,
            evaluator: None,
            blocking_approval_timeout_secs: 300,
            profile: Profile::Production,
        })
    }

    /// Verifies: REQ-CORE-007/F-001.1 (ThoughtGate injects task capability)
    #[tokio::test]
    async fn test_initialize_injects_task_capability() {
        // Upstream without task support
        let upstream = Arc::new(MockInitializeUpstream::without_capabilities());
        let state = create_test_state_with_upstream(upstream);

        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state.clone());

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test"}}}"#;
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

        // Should have injected task capability
        let result = &parsed["result"];
        assert_eq!(
            result["capabilities"]["tasks"]["requests"]["tools/call"], true,
            "ThoughtGate should inject task capability"
        );

        // SSE should NOT be present (upstream doesn't support it)
        assert!(
            result["capabilities"]["notifications"]["tasks"]["status"].is_null(),
            "SSE should not be advertised when upstream doesn't support it"
        );

        // Cache should reflect upstream capabilities (no tasks)
        assert!(
            !state.capability_cache.upstream_supports_tasks(),
            "Cache should show upstream does NOT support tasks"
        );
    }

    /// Verifies: REQ-CORE-007/F-001.2, F-001.3 (Upstream capability detection and caching)
    #[tokio::test]
    async fn test_initialize_detects_upstream_capabilities() {
        // Upstream WITH task and SSE support
        let upstream = Arc::new(MockInitializeUpstream::with_capabilities(true, true));
        let state = create_test_state_with_upstream(upstream);

        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state.clone());

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test"}}}"#;
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

        // Should have task capability (both from upstream and ThoughtGate)
        let result = &parsed["result"];
        assert_eq!(
            result["capabilities"]["tasks"]["requests"]["tools/call"], true,
            "Task capability should be present"
        );

        // SSE should NOT be advertised (disabled for v0.2, even if upstream supports it)
        assert!(
            result["capabilities"]["notifications"]["tasks"]["status"].is_null(),
            "SSE should NOT be advertised in v0.2 (disabled until SSE endpoint implemented)"
        );

        // Cache should still reflect upstream capabilities (detection works, just not advertised)
        assert!(
            state.capability_cache.upstream_supports_tasks(),
            "Cache should show upstream supports tasks"
        );
        assert!(
            state.capability_cache.upstream_supports_task_sse(),
            "Cache should show upstream supports SSE (detected but not advertised)"
        );
    }

    /// Verifies: SSE detection works but is not advertised (v0.2)
    #[tokio::test]
    async fn test_initialize_sse_detection_without_advertisement() {
        // Upstream with tasks but NO SSE - verify detection still works
        let upstream = Arc::new(MockInitializeUpstream::with_capabilities(true, false));
        let state = create_test_state_with_upstream(upstream);

        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state.clone());

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test"}}}"#;
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

        let result = &parsed["result"];

        // Task capability should be present
        assert_eq!(
            result["capabilities"]["tasks"]["requests"]["tools/call"], true,
            "Task capability should be present"
        );

        // SSE should NOT be present (disabled for v0.2)
        assert!(
            result["capabilities"]["notifications"]["tasks"]["status"].is_null(),
            "SSE should NOT be advertised in v0.2"
        );

        // Cache should reflect upstream capabilities (detection works)
        assert!(
            state.capability_cache.upstream_supports_tasks(),
            "Cache should show upstream supports tasks"
        );
        assert!(
            !state.capability_cache.upstream_supports_task_sse(),
            "Cache should show upstream does NOT support SSE"
        );
    }

    /// Verifies: REQ-CORE-007 (Original fields preserved)
    #[tokio::test]
    async fn test_initialize_preserves_original_fields() {
        // Upstream without capabilities
        let upstream = Arc::new(MockInitializeUpstream::without_capabilities());
        let state = create_test_state_with_upstream(upstream);

        let router = Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .with_state(state);

        let body = r#"{"jsonrpc":"2.0","id":"init-123","method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test"}}}"#;
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

        // Original fields should be preserved
        let result = &parsed["result"];
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "mock-upstream");
        assert_eq!(result["serverInfo"]["version"], "1.0.0");

        // ID should be preserved
        assert_eq!(parsed["id"], "init-123");
    }

    // ========================================================================
    // Profile Configuration Tests
    // ========================================================================

    /// Verifies: Default profile is Production when no flag is passed.
    #[test]
    fn test_default_profile_is_production() {
        let state = create_test_state();
        assert_eq!(state.profile, Profile::Production);
    }

    /// Verifies: Profile is threaded through with_governance() to the evaluator.
    #[tokio::test]
    async fn test_profile_passed_to_governance() {
        let task_store = Arc::new(TaskStore::with_defaults());
        let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));

        let handler = McpHandler::with_governance(
            Arc::new(MockUpstream),
            cedar_engine,
            task_store,
            McpHandlerConfig::default(),
            None,
            None,
            None,
            Profile::Development,
        );

        assert_eq!(handler.state().profile, Profile::Development);
    }
}
