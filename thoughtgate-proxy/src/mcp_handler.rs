//! MCP request handler and optional HTTP server.
//!
//! Implements: REQ-CORE-003/§5.2 (MCP Streamable HTTP Transport)
//!
//! # Overview
//!
//! This module provides two ways to handle MCP requests:
//!
//! 1. **`McpHandler`** - Direct handler that processes buffered request bodies.
//!    Used by ProxyService for in-process MCP handling.
//!
//! 2. **`McpServer`** - Optional standalone Axum server for testing or
//!    running MCP handling separately (deprecated in v0.2).
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

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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

use thoughtgate_core::config::{Action, Config, MatchResult};
use thoughtgate_core::error::ThoughtGateError;
use thoughtgate_core::governance::{
    ApprovalAdapter, ApprovalEngine, ApprovalEngineConfig, Principal, SlackAdapter, TaskHandler,
    TaskStore, ToolCallRequest,
};
use thoughtgate_core::policy::engine::CedarEngine;
use thoughtgate_core::policy::principal::infer_principal;
use thoughtgate_core::policy::{
    CedarContext, CedarDecision, CedarRequest, CedarResource, TimeContext,
};
use thoughtgate_core::protocol::{
    CapabilityCache, TasksCancelRequest, TasksGetRequest, TasksListRequest, TasksResultRequest,
    extract_upstream_sse_support, extract_upstream_task_support, inject_task_capability,
    strip_sse_capability,
};
use thoughtgate_core::telemetry::{
    BoxedSpan, McpMessageType, McpSpanData, ThoughtGateMetrics, finish_mcp_span, start_mcp_span,
};
use thoughtgate_core::transport::jsonrpc::{
    JsonRpcId, JsonRpcResponse, McpRequest, ParsedRequests, parse_jsonrpc,
};
use thoughtgate_core::transport::router::{McpRouter, RouteTarget, TaskMethod};
use thoughtgate_core::transport::upstream::{UpstreamClient, UpstreamConfig, UpstreamForwarder};
use tokio_util::sync::CancellationToken;

/// RAII guard that decrements the aggregate buffer counter on drop.
///
/// Ensures buffered_bytes is always decremented even if the handler panics.
struct BufferGuard<'a> {
    counter: &'a AtomicUsize,
    size: usize,
}

impl<'a> Drop for BufferGuard<'a> {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.size, Ordering::Release);
    }
}

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
    /// Maximum number of requests in a JSON-RPC batch
    pub max_batch_size: usize,
    /// Upstream client configuration
    pub upstream: UpstreamConfig,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:8080".to_string(),
            max_body_size: 1024 * 1024, // 1MB
            max_concurrent_requests: 10000,
            max_batch_size: 100,
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
    /// - `THOUGHTGATE_MAX_BATCH_SIZE` (default: 100): Max JSON-RPC batch array length
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

        let max_batch_size: usize = std::env::var("THOUGHTGATE_MAX_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        Ok(Self {
            listen_addr,
            max_body_size,
            max_concurrent_requests,
            max_batch_size,
            upstream: UpstreamConfig::from_env()?,
        })
    }
}

/// Shared MCP handler state.
///
/// This state is shared across all request handlers. Used by both
/// `McpHandler` (direct invocation) and `McpServer` (Axum server).
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
}

/// Configuration for the MCP handler.
///
/// Implements: REQ-CORE-003/§5.3 (Configuration)
#[derive(Debug, Clone)]
pub struct McpHandlerConfig {
    /// Maximum request body size in bytes
    pub max_body_size: usize,
    /// Maximum concurrent requests
    pub max_concurrent_requests: usize,
    /// Maximum number of requests in a JSON-RPC batch
    pub max_batch_size: usize,
    /// Maximum aggregate buffered bytes across all in-flight requests.
    /// Prevents OOM when many concurrent requests buffer large bodies.
    pub max_aggregate_buffer: usize,
}

impl Default for McpHandlerConfig {
    fn default() -> Self {
        Self {
            max_body_size: 1024 * 1024, // 1MB
            max_concurrent_requests: 10000,
            max_batch_size: 100,
            max_aggregate_buffer: 512 * 1024 * 1024, // 512MB
        }
    }
}

impl McpHandlerConfig {
    /// Load configuration from environment variables.
    ///
    /// # Environment Variables
    ///
    /// - `THOUGHTGATE_MAX_REQUEST_BODY_BYTES` (default: 1048576): Max body size
    /// - `THOUGHTGATE_MAX_CONCURRENT_REQUESTS` (default: 10000): Max concurrent requests
    /// - `THOUGHTGATE_MAX_AGGREGATE_BUFFER` (default: 536870912 = 512MB): Max aggregate buffered bytes
    pub fn from_env() -> Self {
        let max_body_size: usize = std::env::var("THOUGHTGATE_MAX_REQUEST_BODY_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1024 * 1024); // 1MB default

        let max_concurrent_requests: usize = std::env::var("THOUGHTGATE_MAX_CONCURRENT_REQUESTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10000);

        let max_batch_size: usize = std::env::var("THOUGHTGATE_MAX_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        let max_aggregate_buffer: usize = std::env::var("THOUGHTGATE_MAX_AGGREGATE_BUFFER")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(512 * 1024 * 1024); // 512MB default

        Self {
            max_body_size,
            max_concurrent_requests,
            max_batch_size,
            max_aggregate_buffer,
        }
    }
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
        });

        Self { state }
    }

    /// Create a new MCP handler with custom task handler.
    ///
    /// This is useful for testing with mock components.
    pub fn with_task_handler(
        upstream: Arc<dyn UpstreamForwarder>,
        cedar_engine: Arc<CedarEngine>,
        task_handler: TaskHandler,
        config: McpHandlerConfig,
    ) -> Self {
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
    pub fn with_governance(
        upstream: Arc<dyn UpstreamForwarder>,
        cedar_engine: Arc<CedarEngine>,
        task_store: Arc<TaskStore>,
        handler_config: McpHandlerConfig,
        yaml_config: Option<Arc<Config>>,
        approval_engine: Option<Arc<ApprovalEngine>>,
        tg_metrics: Option<Arc<ThoughtGateMetrics>>,
    ) -> Self {
        let task_handler = TaskHandler::new(task_store);
        let semaphore = Arc::new(Semaphore::new(handler_config.max_concurrent_requests));

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
    /// A tuple of (StatusCode, response bytes) for direct use by ProxyService.
    /// This avoids double-buffering when converting to UnifiedBody.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/§10 (Request Handler Pattern)
    pub async fn handle(&self, body: Bytes) -> (StatusCode, Bytes) {
        handle_mcp_body_bytes(&self.state, body).await
    }

    /// Handle a buffered MCP request body and return a full Response.
    ///
    /// This is used by McpServer for backwards compatibility with Axum.
    pub async fn handle_response(&self, body: Bytes) -> Response {
        let (status, bytes) = self.handle(body).await;
        (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response()
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

/// The MCP server (standalone Axum server).
///
/// **Note:** In v0.2, prefer using `McpHandler` via `ProxyService` for unified
/// traffic handling. This standalone server is retained for testing and
/// backwards compatibility.
///
/// Implements: REQ-CORE-003/§5.2 (MCP Streamable HTTP Transport)
pub struct McpServer {
    config: McpServerConfig,
    state: Arc<McpState>,
    /// Cancellation token for graceful shutdown of background tasks.
    /// Cancelling this token will stop the ApprovalEngine's polling scheduler.
    shutdown: CancellationToken,
}

/// Create governance components (TaskHandler + CedarEngine + optional ApprovalEngine).
///
/// This is extracted to avoid duplication between `McpServer::new()` and
/// `McpServer::with_upstream()`.
///
/// # Arguments
///
/// * `upstream` - Upstream forwarder for approved requests
/// * `config` - Optional YAML config (enables Gate 1, 2, 4)
/// * `shutdown` - Cancellation token for graceful shutdown
///
/// # Returns
///
/// Tuple of (TaskHandler, CedarEngine, optional ApprovalEngine)
#[allow(clippy::type_complexity)]
pub async fn create_governance_components(
    upstream: Arc<dyn UpstreamForwarder>,
    config: Option<&Config>,
    shutdown: CancellationToken,
) -> Result<(TaskHandler, Arc<CedarEngine>, Option<Arc<ApprovalEngine>>), ThoughtGateError> {
    // Create task store and handler for SEP-1686 task methods
    let task_store = Arc::new(TaskStore::with_defaults());
    let task_handler = TaskHandler::new(task_store.clone());

    // Create Cedar policy engine (Gate 3)
    let cedar_engine =
        Arc::new(
            CedarEngine::new().map_err(|e| ThoughtGateError::ServiceUnavailable {
                reason: format!("Failed to create Cedar engine: {}", e),
            })?,
        );

    // Create ApprovalEngine only if config uses approval rules (Gate 4)
    // This avoids requiring Slack credentials when approvals are not used
    let needs_approval = config
        .map(|c| c.requires_approval_engine())
        .unwrap_or(false);

    let approval_engine = if needs_approval {
        info!("Approval rules detected, initializing ApprovalEngine");

        // Create approval adapter based on environment
        let adapter: Arc<dyn ApprovalAdapter> =
            match std::env::var("THOUGHTGATE_APPROVAL_ADAPTER").as_deref() {
                Ok("mock") => {
                    // Use mock adapter for testing
                    Arc::new(thoughtgate_core::governance::approval::mock::MockAdapter::from_env())
                }
                _ => {
                    // Default to Slack adapter
                    let slack_config = thoughtgate_core::governance::SlackConfig::from_env()
                        .map_err(|e| ThoughtGateError::ServiceUnavailable {
                            reason: format!("Failed to create Slack config: {}", e),
                        })?;
                    let slack_adapter = SlackAdapter::new(slack_config).map_err(|e| {
                        ThoughtGateError::ServiceUnavailable {
                            reason: format!("Failed to create Slack adapter: {}", e),
                        }
                    })?;
                    Arc::new(slack_adapter)
                }
            };

        let engine_config = ApprovalEngineConfig::from_env();

        let engine = ApprovalEngine::new(
            task_store,
            adapter,
            upstream,
            cedar_engine.clone(),
            engine_config,
            shutdown,
        );

        // Spawn background polling loop for approval decisions
        // Implements: REQ-GOV-003/F-002, REQ-GOV-001/F-008
        engine.spawn_background_tasks().await;
        info!("ApprovalEngine background tasks started");

        Some(Arc::new(engine))
    } else {
        if config.is_some() {
            debug!("No approval rules detected, skipping ApprovalEngine initialization");
        }
        None
    };

    Ok((task_handler, cedar_engine, approval_engine))
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
    pub async fn new(config: McpServerConfig) -> Result<Self, ThoughtGateError> {
        let upstream = Arc::new(UpstreamClient::new(config.upstream.clone())?);
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_requests));
        let shutdown = CancellationToken::new();
        let (task_handler, cedar_engine, approval_engine) =
            create_governance_components(upstream.clone(), None, shutdown.clone()).await?;

        let state = Arc::new(McpState {
            upstream,
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            config: None,
            approval_engine,
            semaphore,
            max_body_size: config.max_body_size,
            max_batch_size: config.max_batch_size,
            capability_cache: Arc::new(CapabilityCache::new()),
            buffered_bytes: AtomicUsize::new(0),
            max_aggregate_buffer: 512 * 1024 * 1024, // 512MB default for McpServer
            tg_metrics: None, // McpServer doesn't use prometheus-client metrics
        });

        Ok(Self {
            config,
            state,
            shutdown,
        })
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
    pub async fn with_upstream(
        config: McpServerConfig,
        upstream: Arc<dyn UpstreamForwarder>,
    ) -> Result<Self, ThoughtGateError> {
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_requests));
        let shutdown = CancellationToken::new();
        let (task_handler, cedar_engine, approval_engine) =
            create_governance_components(upstream.clone(), None, shutdown.clone()).await?;

        let state = Arc::new(McpState {
            upstream,
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            config: None,
            approval_engine,
            semaphore,
            max_body_size: config.max_body_size,
            max_batch_size: config.max_batch_size,
            capability_cache: Arc::new(CapabilityCache::new()),
            buffered_bytes: AtomicUsize::new(0),
            max_aggregate_buffer: 512 * 1024 * 1024, // 512MB default for McpServer
            tg_metrics: None, // McpServer doesn't use prometheus-client metrics
        });

        Ok(Self {
            config,
            state,
            shutdown,
        })
    }

    /// Create a new MCP server with full v0.2 4-gate configuration.
    ///
    /// This enables the complete 4-gate model:
    /// - Gate 1: Visibility (ExposeConfig filtering)
    /// - Gate 2: Governance Rules (YAML rule matching)
    /// - Gate 3: Cedar Policy (when action: policy)
    /// - Gate 4: Approval Workflow (when action: approve)
    ///
    /// Implements: REQ-CFG-001 (YAML Configuration)
    /// Implements: REQ-GOV-002 (Approval Execution Pipeline)
    ///
    /// # Arguments
    ///
    /// * `server_config` - Server configuration
    /// * `yaml_config` - YAML configuration for gates 1, 2, 4
    /// * `shutdown` - Cancellation token for graceful shutdown
    ///
    /// # Errors
    ///
    /// Returns error if upstream client or approval engine cannot be created.
    pub async fn with_config(
        server_config: McpServerConfig,
        yaml_config: Config,
        shutdown: CancellationToken,
    ) -> Result<Self, ThoughtGateError> {
        let upstream = Arc::new(UpstreamClient::new(server_config.upstream.clone())?);
        let semaphore = Arc::new(Semaphore::new(server_config.max_concurrent_requests));
        let (task_handler, cedar_engine, approval_engine) =
            create_governance_components(upstream.clone(), Some(&yaml_config), shutdown.clone())
                .await?;

        let state = Arc::new(McpState {
            upstream,
            router: McpRouter::new(),
            task_handler,
            cedar_engine,
            config: Some(Arc::new(yaml_config)),
            approval_engine,
            semaphore,
            max_body_size: server_config.max_body_size,
            max_batch_size: server_config.max_batch_size,
            capability_cache: Arc::new(CapabilityCache::new()),
            buffered_bytes: AtomicUsize::new(0),
            max_aggregate_buffer: 512 * 1024 * 1024, // 512MB default for McpServer
            tg_metrics: None, // McpServer doesn't use prometheus-client metrics
        });

        Ok(Self {
            config: server_config,
            state,
            shutdown,
        })
    }

    /// Create the axum Router.
    ///
    /// Implements: REQ-CORE-003/F-002 (Method Routing)
    ///
    /// The router includes:
    /// - `POST /mcp/v1` - Main MCP endpoint
    /// - Body size limit enforced at HTTP layer (before buffering)
    pub fn router(&self) -> Router {
        // Note: We don't use DefaultBodyLimit here because it returns HTTP 413
        // instead of a JSON-RPC error. We manually check size in handle_mcp_body_bytes
        // and return a proper JSON-RPC -32600 error per EC-MCP-004.
        Router::new()
            .route("/mcp/v1", post(handle_mcp_request))
            .layer(DefaultBodyLimit::disable())
            .with_state(self.state.clone())
    }

    /// Get the shutdown token.
    ///
    /// This can be used to coordinate shutdown with external systems.
    /// Clone the token to share it with other components that need to be
    /// notified when the server is shutting down.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Trigger graceful shutdown of background tasks.
    ///
    /// This cancels the shutdown token, which will stop the ApprovalEngine's
    /// background polling scheduler and any other tasks listening to the token.
    ///
    /// Note: This does not stop the HTTP server itself. Use this in conjunction
    /// with server shutdown to ensure all background tasks are cleaned up.
    pub fn shutdown(&self) {
        info!("Triggering graceful shutdown of background tasks");
        self.shutdown.cancel();
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

/// Handle POST /mcp/v1 requests (Axum handler).
///
/// This is the Axum handler that extracts state and body, then delegates
/// to `handle_mcp_body_bytes` for the actual processing.
///
/// Implements: REQ-CORE-003/§10 (Request Handler Pattern)
async fn handle_mcp_request(State(state): State<Arc<McpState>>, body: Bytes) -> Response {
    let (status, bytes) = handle_mcp_body_bytes(&state, body).await;
    (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}

/// Handle a buffered MCP request body, returning (StatusCode, Bytes).
///
/// This is the core MCP processing logic, used by both:
/// - `McpHandler::handle()` (direct invocation from ProxyService)
/// - `handle_mcp_request()` (Axum handler for standalone server)
///
/// Returns `(StatusCode, Bytes)` to avoid double-buffering when ProxyService
/// converts to UnifiedBody.
///
/// # Request Flow
///
/// 1. Check body size limit
/// 2. Acquire semaphore permit (EC-MCP-011)
/// 3. Parse JSON-RPC request(s)
/// 4. Route and handle each request
/// 5. Return response(s)
///
/// # Traceability
/// - Implements: REQ-CORE-003/§10 (Request Handler Pattern)
async fn handle_mcp_body_bytes(state: &McpState, body: Bytes) -> (StatusCode, Bytes) {
    // Check body size limit (generate unique correlation ID per REQ-CORE-004)
    if body.len() > state.max_body_size {
        let correlation_id =
            thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
        let error = ThoughtGateError::InvalidRequest {
            details: format!(
                "Request body exceeds maximum size of {} bytes",
                state.max_body_size
            ),
        };
        return error_bytes(None, &error, &correlation_id);
    }

    // Track aggregate buffered bytes to prevent OOM.
    // Increment before processing; decrement when this request completes.
    let body_size = body.len();
    let prev = state.buffered_bytes.fetch_add(body_size, Ordering::AcqRel);
    if prev + body_size > state.max_aggregate_buffer {
        state.buffered_bytes.fetch_sub(body_size, Ordering::Release);
        let correlation_id =
            thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
        warn!(
            correlation_id = %correlation_id,
            buffered = prev + body_size,
            limit = state.max_aggregate_buffer,
            "Aggregate buffer limit exceeded"
        );
        let error = ThoughtGateError::RateLimited {
            retry_after_secs: Some(1),
        };
        return error_bytes(None, &error, &correlation_id);
    }

    // Ensure we decrement buffered_bytes when this request completes.
    let _buffer_guard = BufferGuard {
        counter: &state.buffered_bytes,
        size: body_size,
    };

    // Parse JSON-RPC request(s) first to determine permit count
    // (generate unique correlation ID per REQ-CORE-004)
    let parsed = match parse_jsonrpc(&body) {
        Ok(p) => p,
        Err(e) => {
            let correlation_id =
                thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
            return error_bytes(None, &e, &correlation_id);
        }
    };

    // Determine how many permits to acquire: 1 for single, batch_size for batch.
    // This prevents a batch of N requests from consuming only 1 concurrency slot.
    let permit_count = match &parsed {
        ParsedRequests::Single(_) => 1,
        ParsedRequests::Batch(requests) => {
            if requests.len() > state.max_batch_size {
                let correlation_id =
                    thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
                let error = ThoughtGateError::InvalidRequest {
                    details: format!(
                        "Batch size {} exceeds maximum of {}",
                        requests.len(),
                        state.max_batch_size
                    ),
                };
                return error_bytes(None, &error, &correlation_id);
            }
            requests.len().max(1) as u32
        }
    };

    // Try to acquire semaphore permits weighted by request count (EC-MCP-011)
    let _permit = match state.semaphore.clone().try_acquire_many_owned(permit_count) {
        Ok(permit) => permit,
        Err(_) => {
            let correlation_id =
                thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
            warn!(
                correlation_id = %correlation_id,
                permits_requested = permit_count,
                "Max concurrent requests reached, returning 503"
            );
            // Return HTTP 200 with JSON-RPC error per MCP spec.
            // MCP clients expect JSON-RPC error frames over HTTP 200, not HTTP 503.
            let error = ThoughtGateError::RateLimited {
                retry_after_secs: Some(1),
            };
            let jsonrpc_error = error.to_jsonrpc_error(&correlation_id);
            let response = JsonRpcResponse::error(None, jsonrpc_error);
            let bytes = serde_json::to_vec(&response)
                .map(Bytes::from)
                .unwrap_or_else(|_| {
                    Bytes::from_static(
                        br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32009,"message":"Rate limited"}}"#,
                    )
                });
            return (StatusCode::OK, bytes);
        }
    };

    match parsed {
        ParsedRequests::Single(request) => handle_single_request_bytes(state, request).await,
        ParsedRequests::Batch(requests) => handle_batch_request_bytes(state, requests).await,
    }
}

// ============================================================================
// Helper Functions for 4-Gate Model
// ============================================================================

/// Extract the governable resource name from an MCP request.
///
/// Implements: REQ-CORE-003/F-002 (Method Routing)
///
/// Returns the name/identifier that governance rules should match against:
/// - `tools/call` → params.name (tool name)
/// - `resources/read` → params.uri (resource URI)
/// - `resources/subscribe` → params.uri (resource URI)
/// - `prompts/get` → params.name (prompt name)
///
/// Returns `None` for list methods or unsupported methods.
fn extract_governable_name(request: &McpRequest) -> Option<String> {
    let params = request.params.as_ref()?;

    match request.method.as_str() {
        "tools/call" => params.get("name")?.as_str().map(String::from),
        "resources/read" | "resources/subscribe" => params.get("uri")?.as_str().map(String::from),
        "prompts/get" => params.get("name")?.as_str().map(String::from),
        // List methods don't have a specific resource to check
        // They require response filtering instead (not implemented in v0.2)
        _ => None,
    }
}

/// Check if a method requires gate routing (vs pass-through).
///
/// Returns true if the method should go through the 4-gate model.
fn method_requires_gates(method: &str) -> bool {
    matches!(
        method,
        "tools/call" | "resources/read" | "resources/subscribe" | "prompts/get"
    )
}

/// Extract tool arguments from a tools/call request.
///
/// Implements: REQ-GOV-002/F-001 (Task creation with tool arguments)
///
/// Extracts the `arguments` field from the request params. This is used
/// when creating an approval task to store the full request context.
///
/// # Returns
///
/// The arguments object if present, or an empty JSON object `{}` if:
/// - `params` is None
/// - `params.arguments` is missing
///
/// # Note
///
/// An empty object is returned rather than an error because MCP tools
/// may have no required arguments, so missing arguments is valid.
fn extract_tool_arguments(request: &McpRequest) -> serde_json::Value {
    request
        .params
        .as_ref()
        .and_then(|p| p.get("arguments").cloned())
        .unwrap_or(serde_json::json!({}))
}

/// Get source ID for the request.
///
/// v0.2: Single upstream, hardcoded to "upstream".
fn get_source_id(_state: &McpState) -> &'static str {
    "upstream"
}

// ============================================================================
// Initialize Method Handler (REQ-CORE-007: Capability Injection)
// ============================================================================

/// Handle the `initialize` method with capability injection.
///
/// Implements: REQ-CORE-007/F-001 (Capability Injection)
///
/// This function:
/// 1. Forwards the `initialize` request to upstream
/// 2. Extracts and caches upstream capability detection
/// 3. Injects ThoughtGate's task capability (always)
/// 4. Conditionally advertises SSE (only if upstream supports it)
///
/// # Arguments
///
/// * `state` - MCP handler state with capability cache and upstream client
/// * `request` - The initialize request
///
/// # Returns
///
/// Modified initialize response with injected capabilities.
async fn handle_initialize_method(
    state: &McpState,
    request: McpRequest,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    // Forward to upstream to get the raw initialize response
    let response = state.upstream.forward(&request).await?;

    // If error response, return as-is (don't inject capabilities on error)
    if response.error.is_some() {
        return Ok(response);
    }

    // Extract result, return as-is if missing
    let result = match &response.result {
        Some(r) => r.clone(),
        None => return Ok(response),
    };

    // Extract and cache upstream capabilities
    let upstream_tasks = extract_upstream_task_support(&result);
    let upstream_sse = extract_upstream_sse_support(&result);

    state
        .capability_cache
        .set_upstream_supports_tasks(upstream_tasks);
    state
        .capability_cache
        .set_upstream_supports_task_sse(upstream_sse);

    info!(
        upstream_tasks = upstream_tasks,
        upstream_sse = upstream_sse,
        "Detected upstream capabilities"
    );

    // Inject ThoughtGate's capabilities
    let mut new_result = result;

    // Always inject task capability (ThoughtGate supports tasks)
    inject_task_capability(&mut new_result);

    // v0.2: Strip SSE capability entirely
    // ThoughtGate does not yet implement SSE endpoints, so we must not
    // advertise the capability - even if upstream supports it. Clients would
    // attempt to subscribe and fail. Detection is preserved in CapabilityCache
    // for future use in v0.3+.
    // See: REQ-GOV-004 (Upstream Task Orchestration) - DEFERRED to v0.3+
    strip_sse_capability(&mut new_result);

    debug!(
        injected_tasks = true,
        upstream_sse_detected = upstream_sse,
        sse_stripped = true,
        "Injected capabilities into initialize response (SSE stripped for v0.2)"
    );

    Ok(JsonRpcResponse::success(response.id, new_result))
}

// ============================================================================
// List Method Handler (SEP-1686: tools/list interception)
// ============================================================================

/// Handle list methods (tools/list, resources/list, prompts/list) with response filtering.
///
/// Implements: SEP-1686 Section 3.2 (Tool Advertisement)
///
/// This function:
/// 1. Forwards the request to upstream
/// 2. Parses the tool list from the response
/// 3. Applies Gate 1: Filters tools by visibility (ExposeConfig)
/// 4. Applies Gate 2: Annotates taskSupport based on governance rules
///
/// # Arguments
///
/// * `state` - MCP handler state with config and upstream client
/// * `request` - The list request (e.g., tools/list)
///
/// # Returns
///
/// Modified response with filtered tools and taskSupport annotations.
async fn handle_list_method(
    state: &McpState,
    request: McpRequest,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    // Forward to upstream to get the raw tool list
    let response = state.upstream.forward(&request).await?;

    // If error response, return as-is
    if response.error.is_some() {
        return Ok(response);
    }

    // If no config, return response as-is (transparent proxy mode)
    let config = match &state.config {
        Some(c) => c,
        None => return Ok(response),
    };

    // Extract result object from response
    let result = match &response.result {
        Some(r) => r,
        None => return Ok(response),
    };

    let source_id = get_source_id(state);

    // Gate 1: Filter by visibility (ExposeConfig) for all list methods
    // Per REQ-CORE-003/F-003: Gate 1 applies to tools, resources, and prompts
    match request.method.as_str() {
        "tools/list" => {
            // Operate directly on the Value tree to avoid deserialize/serialize cycle.
            let mut new_result = result.clone();
            let tools_arr = match new_result
                .as_object_mut()
                .and_then(|obj| obj.get_mut("tools"))
                .and_then(|v| v.as_array_mut())
            {
                Some(arr) => arr,
                None => return Ok(response),
            };

            // Gate 1: Filter by visibility (ExposeConfig)
            if let Some(source) = config.get_source(source_id) {
                let expose = source.expose();
                let original_count = tools_arr.len();
                tools_arr.retain(|tool| {
                    tool.get("name")
                        .and_then(|n| n.as_str())
                        .map(|name| expose.is_visible(name))
                        .unwrap_or(false) // Fail closed: hide unparseable tools
                });
                let filtered_count = original_count - tools_arr.len();
                if filtered_count > 0 {
                    debug!(
                        source = source_id,
                        filtered = filtered_count,
                        remaining = tools_arr.len(),
                        "Gate 1: Filtered tools by visibility"
                    );
                }
            } else {
                // Fail closed: source not in config, hide all tools
                warn!(
                    source = source_id,
                    "Gate 1: source not in config, hiding all tools"
                );
                tools_arr.clear();
            }

            // Gate 2: Annotate execution.taskSupport based on governance rules
            // Per MCP Tasks Specification (Protocol Revision 2025-11-25)
            for tool in tools_arr.iter_mut() {
                let tool_name = match tool.get("name").and_then(|n| n.as_str()) {
                    Some(name) => name,
                    None => continue,
                };
                let match_result = config.governance.evaluate(tool_name, source_id);
                match match_result.action {
                    thoughtgate_core::config::Action::Approve
                    | thoughtgate_core::config::Action::Policy => {
                        // ThoughtGate requires async task mode for approval/policy actions
                        // Set execution.taskSupport = "required" per MCP spec
                        if let Some(obj) = tool.as_object_mut() {
                            obj.insert(
                                "execution".to_string(),
                                serde_json::json!({"taskSupport": "required"}),
                            );
                        }
                    }
                    thoughtgate_core::config::Action::Forward
                    | thoughtgate_core::config::Action::Deny => {
                        // Preserve upstream's execution.taskSupport (don't modify)
                        // Note: Deny tools are visible but denied at call-time
                    }
                }
            }

            Ok(JsonRpcResponse::success(response.id, new_result))
        }
        "resources/list" => {
            // Operate directly on the Value tree to avoid deserialize/serialize cycle.
            let mut new_result = result.clone();
            let resources_arr = match new_result
                .as_object_mut()
                .and_then(|obj| obj.get_mut("resources"))
                .and_then(|v| v.as_array_mut())
            {
                Some(arr) => arr,
                None => return Ok(response),
            };

            // Gate 1: Filter by visibility (ExposeConfig)
            // Resources are filtered by URI pattern
            if let Some(source) = config.get_source(source_id) {
                let expose = source.expose();
                let original_count = resources_arr.len();
                resources_arr.retain(|resource| {
                    resource
                        .get("uri")
                        .and_then(|u| u.as_str())
                        .map(|uri| expose.is_visible(uri))
                        .unwrap_or(false) // Fail closed: hide unparseable resources
                });
                let filtered_count = original_count - resources_arr.len();
                if filtered_count > 0 {
                    debug!(
                        source = source_id,
                        filtered = filtered_count,
                        remaining = resources_arr.len(),
                        "Gate 1: Filtered resources by visibility"
                    );
                }
            } else {
                // Fail closed: source not in config, hide all resources
                warn!(
                    source = source_id,
                    "Gate 1: source not in config, hiding all resources"
                );
                resources_arr.clear();
            }

            Ok(JsonRpcResponse::success(response.id, new_result))
        }
        "prompts/list" => {
            // Operate directly on the Value tree to avoid deserialize/serialize cycle.
            let mut new_result = result.clone();
            let prompts_arr = match new_result
                .as_object_mut()
                .and_then(|obj| obj.get_mut("prompts"))
                .and_then(|v| v.as_array_mut())
            {
                Some(arr) => arr,
                None => return Ok(response),
            };

            // Gate 1: Filter by visibility (ExposeConfig)
            // Prompts are filtered by name pattern
            if let Some(source) = config.get_source(source_id) {
                let expose = source.expose();
                let original_count = prompts_arr.len();
                prompts_arr.retain(|prompt| {
                    prompt
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(|name| expose.is_visible(name))
                        .unwrap_or(false) // Fail closed: hide unparseable prompts
                });
                let filtered_count = original_count - prompts_arr.len();
                if filtered_count > 0 {
                    debug!(
                        source = source_id,
                        filtered = filtered_count,
                        remaining = prompts_arr.len(),
                        "Gate 1: Filtered prompts by visibility"
                    );
                }
            } else {
                // Fail closed: source not in config, hide all prompts
                warn!(
                    source = source_id,
                    "Gate 1: source not in config, hiding all prompts"
                );
                prompts_arr.clear();
            }

            Ok(JsonRpcResponse::success(response.id, new_result))
        }
        _ => {
            // Not a list method we handle, return as-is
            Ok(response)
        }
    }
}

/// Check if a method is a list method that requires response filtering.
fn is_list_method(method: &str) -> bool {
    matches!(method, "tools/list" | "resources/list" | "prompts/list")
}

/// Validate task metadata presence for SEP-1686 compliance.
///
/// Implements: SEP-1686 Section 3.3 (TaskRequired/TaskForbidden)
///
/// - For `action: approve` or `action: policy`: client MUST send `params.task`
/// - For `action: forward` or `action: deny`: if client sent task metadata but
///   upstream doesn't support tasks, return TaskForbidden error
fn validate_task_metadata(
    request: &mut McpRequest,
    action: &thoughtgate_core::config::Action,
    tool_name: &str,
    upstream_supports_tasks: bool,
) -> Result<(), ThoughtGateError> {
    let has_task_metadata = request.is_task_augmented();

    match action {
        thoughtgate_core::config::Action::Approve | thoughtgate_core::config::Action::Policy => {
            // Task-required: client MUST send params.task
            if !has_task_metadata {
                return Err(ThoughtGateError::TaskRequired {
                    tool: tool_name.to_string(),
                    hint: "Include params.task per tools/list taskSupport annotation".to_string(),
                });
            }
        }
        thoughtgate_core::config::Action::Forward | thoughtgate_core::config::Action::Deny => {
            // If client sent task metadata but upstream doesn't support tasks,
            // strip the metadata and forward anyway. This avoids breaking forward
            // compatibility as upstreams gradually add task support.
            if has_task_metadata && !upstream_supports_tasks {
                warn!(
                    tool = %tool_name,
                    "Stripping task metadata: upstream does not support tasks"
                );
                request.task_metadata = None;
            }
        }
    }

    Ok(())
}

// ============================================================================
// Request Handler with 4-Gate Model
// ============================================================================

/// Handle a single JSON-RPC request, returning (StatusCode, Bytes).
///
/// Implements the v0.2 4-gate model:
/// - Gate 1: Visibility (ExposeConfig filtering)
/// - Gate 2: Governance Rules (YAML rule matching)
/// - Gate 3: Cedar Policy (when action: policy)
/// - Gate 4: Approval Workflow (when action: approve)
///
/// # Arguments
///
/// * `state` - MCP handler state
/// * `request` - The parsed MCP request
///
/// # Returns
///
/// (StatusCode, Bytes) tuple with JSON-RPC result or error.
/// Routes a single MCP request through the appropriate handler.
///
/// Implements: REQ-CORE-003/F-002 (Method Routing)
///
/// Shared routing logic used by both single and batch request handlers.
/// Dispatches to the 4-gate model, task handler, initialize handler,
/// or pass-through based on the router's classification.
async fn route_request(
    state: &McpState,
    request: McpRequest,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    match state.router.route(request) {
        RouteTarget::PolicyEvaluation { request } => {
            // Apply 4-gate model for governable methods when config is present
            if state.config.is_some() {
                if method_requires_gates(&request.method) {
                    // Governable methods: tools/call, resources/read, etc.
                    route_through_gates(state, request).await
                } else if is_list_method(&request.method) {
                    // List methods: tools/list, resources/list, prompts/list
                    // Intercept response, apply Gate 1 filter, annotate taskSupport
                    handle_list_method(state, request).await
                } else {
                    // Other methods: forward directly
                    state.upstream.forward(&request).await
                }
            } else {
                // Legacy mode (no config): direct Cedar evaluation (Gate 3 only)
                evaluate_with_cedar(state, request, None).await
            }
        }
        RouteTarget::TaskHandler { method, request } => {
            handle_task_method(state, method, &request).await
        }
        RouteTarget::InitializeHandler { request } => {
            // Implements: REQ-CORE-007/F-001 (Capability Injection)
            handle_initialize_method(state, request).await
        }
        RouteTarget::PassThrough { request } => state.upstream.forward(&request).await,
    }
}

async fn handle_single_request_bytes(state: &McpState, request: McpRequest) -> (StatusCode, Bytes) {
    let correlation_id = request.correlation_id.to_string();
    let id = request.id.clone();
    let is_notification = request.is_notification();
    let method = request.method.clone();
    let request_start = std::time::Instant::now();

    // Extract tool name for tools/call requests (REQ-OBS-002 §5.1.1)
    let tool_name = extract_tool_name(&request);

    // Start MCP span (REQ-OBS-002 §5.1)
    let span_data = McpSpanData {
        method: &method,
        message_type: if is_notification {
            McpMessageType::Notification
        } else {
            McpMessageType::Request
        },
        message_id: id.as_ref().map(jsonrpc_id_to_string),
        correlation_id: &correlation_id,
        tool_name: tool_name.as_deref(),
    };
    let mut span: BoxedSpan = start_mcp_span(&span_data);

    debug!(
        correlation_id = %correlation_id,
        method = %request.method,
        is_notification = is_notification,
        is_task_augmented = request.is_task_augmented(),
        "Processing single request"
    );

    // Route the request through shared routing logic
    let result = route_request(state, request).await;

    // Determine error info for span (REQ-OBS-002 §5.1.1)
    let (is_error, error_code, error_type): (bool, Option<i32>, Option<String>) = match &result {
        Ok(_) => (false, None, None),
        Err(e) => (
            true,
            Some(e.to_jsonrpc_code()),
            Some(e.error_type_name().to_string()),
        ),
    };

    // Finish span with result attributes
    finish_mcp_span(&mut span, is_error, error_code, error_type.as_deref());
    drop(span);

    // Record MCP request metrics (OTel-based, deprecated)
    let outcome = if result.is_ok() { "success" } else { "error" };
    if let Some(mcp_metrics) = thoughtgate_core::metrics::get_mcp_metrics() {
        mcp_metrics.record_request(&method, outcome, request_start.elapsed().as_secs_f64());
    }

    // Record prometheus-client metrics (REQ-OBS-002 §6.1, §6.2)
    if let Some(ref metrics) = state.tg_metrics {
        let duration_ms = request_start.elapsed().as_secs_f64() * 1000.0;
        metrics.record_request(&method, tool_name.as_deref(), outcome);
        metrics.record_request_duration(&method, tool_name.as_deref(), duration_ms);

        if let Err(ref e) = result {
            metrics.record_error(e.error_type_name(), &method);
        }
    }

    // Handle notification - no response (empty body with 204)
    if is_notification {
        if let Err(e) = result {
            error!(
                correlation_id = %correlation_id,
                error = %e,
                "Notification processing failed"
            );
        }
        return (StatusCode::NO_CONTENT, Bytes::new());
    }

    // Return response
    match result {
        Ok(response) => json_bytes(&response),
        Err(e) => error_bytes(id, &e, &correlation_id),
    }
}

/// Extract tool name from tools/call request params.
///
/// Implements: REQ-OBS-002 §5.1.1 (gen_ai.tool.name attribute)
fn extract_tool_name(request: &McpRequest) -> Option<String> {
    if request.method != "tools/call" {
        return None;
    }
    request
        .params
        .as_ref()?
        .get("name")?
        .as_str()
        .map(|s| s.to_string())
}

/// Convert JsonRpcId to string for span attributes.
///
/// Implements: REQ-OBS-002 §5.1.1 (mcp.message.id attribute)
fn jsonrpc_id_to_string(id: &JsonRpcId) -> String {
    match id {
        JsonRpcId::Number(n) => n.to_string(),
        JsonRpcId::String(s) => s.clone(),
        JsonRpcId::Null => "null".to_string(),
    }
}

/// Handle SEP-1686 task method requests.
///
/// Implements: REQ-GOV-001/F-003 through F-006
///
/// For tasks/result, integrates with the ApprovalEngine to execute approved
/// requests on the upstream server.
///
/// # Arguments
///
/// * `state` - Application state (includes TaskHandler and ApprovalEngine)
/// * `method` - The specific task method being called
/// * `request` - The MCP request
///
/// # Returns
///
/// JSON-RPC response or error.
async fn handle_task_method(
    state: &McpState,
    method: TaskMethod,
    request: &McpRequest,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    // Extract params, defaulting to empty object
    let params = request
        .params
        .as_deref()
        .cloned()
        .unwrap_or(serde_json::json!({}));

    match method {
        TaskMethod::Get => {
            // Implements: REQ-GOV-001/F-003 (tasks/get)
            let req: TasksGetRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/get params: {}", e),
                })?;

            // Verify caller owns this task (returns TaskNotFound on mismatch)
            verify_task_principal(state, &req.task_id).await?;

            let result = state
                .task_handler
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
            // Implements: REQ-GOV-002/F-005 (Result retrieval with execution)
            let req: TasksResultRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/result params: {}", e),
                })?;

            // Verify caller owns this task (returns TaskNotFound on mismatch)
            verify_task_principal(state, &req.task_id).await?;

            // If we have an approval engine, use it to execute approved tasks
            if let Some(approval_engine) = &state.approval_engine {
                // req.task_id is already a Sep1686TaskId (aliased as TaskId)
                let task_id = req.task_id.clone();

                let tool_result = approval_engine.execute_on_result(&task_id).await?;

                Ok(JsonRpcResponse::success(
                    request.id.clone(),
                    serde_json::to_value(tool_result).map_err(|e| {
                        ThoughtGateError::ServiceUnavailable {
                            reason: format!("Failed to serialize tasks/result response: {}", e),
                        }
                    })?,
                ))
            } else {
                // Fallback to basic task handler (no execution)
                let result = state
                    .task_handler
                    .handle_tasks_result(req)
                    .map_err(task_error_to_thoughtgate)?;

                Ok(JsonRpcResponse::success(
                    request.id.clone(),
                    serde_json::to_value(result).map_err(|e| {
                        ThoughtGateError::ServiceUnavailable {
                            reason: format!("Failed to serialize tasks/result response: {}", e),
                        }
                    })?,
                ))
            }
        }

        TaskMethod::List => {
            // Implements: REQ-GOV-001/F-005 (tasks/list)
            let req: TasksListRequest =
                serde_json::from_value(params).map_err(|e| ThoughtGateError::InvalidParams {
                    details: format!("Invalid tasks/list params: {}", e),
                })?;

            // Infer principal from environment (sidecar mode).
            // TODO(gateway): In gateway/multi-tenant mode, extract principal from
            // request headers (e.g. X-Forwarded-User, mTLS client cert CN) instead
            // of the sidecar's own K8s identity. Without this, all tasks/list calls
            // in gateway mode would return the same principal's tasks.
            let policy_principal = tokio::task::spawn_blocking(infer_principal)
                .await
                .map_err(|e| ThoughtGateError::ServiceUnavailable {
                    reason: format!("Principal inference task failed: {}", e),
                })?
                .map_err(|e| ThoughtGateError::PolicyDenied {
                    tool: String::new(),
                    policy_id: None,
                    reason: Some(format!("Identity unavailable: {}", e)),
                })?;
            let principal = Principal::from_policy(
                &policy_principal.app_name,
                &policy_principal.namespace,
                &policy_principal.service_account,
                policy_principal.roles.clone(),
            );

            let result = state.task_handler.handle_tasks_list(req, &principal);

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

            // Verify caller owns this task (returns TaskNotFound on mismatch)
            verify_task_principal(state, &req.task_id).await?;

            let result = state
                .task_handler
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

/// Verify that the caller's principal matches the task's principal.
///
/// Returns TaskNotFound (not "access denied") to prevent information leakage
/// about tasks owned by other principals.
///
/// Fails closed: if principal inference fails for any reason (I/O error,
/// misconfiguration, spawn_blocking panic), access is denied rather than
/// silently allowed.
///
/// Implements: REQ-GOV-001/F-011 (Principal isolation)
async fn verify_task_principal(
    state: &McpState,
    task_id: &thoughtgate_core::governance::task::TaskId,
) -> Result<(), ThoughtGateError> {
    // Fail closed: if identity cannot be established, deny access.
    // This prevents unauthorized access to tasks when identity inference
    // fails due to I/O errors, misconfiguration, or spawn_blocking panics.
    let caller_principal = match tokio::task::spawn_blocking(infer_principal).await {
        Ok(Ok(p)) => Principal::new(&p.app_name),
        Ok(Err(e)) => {
            warn!(error = %e, "Principal inference failed, denying task access");
            return Err(ThoughtGateError::TaskNotFound {
                task_id: task_id.to_string(),
            });
        }
        Err(e) => {
            warn!(error = %e, "Principal inference task panicked, denying task access");
            return Err(ThoughtGateError::TaskNotFound {
                task_id: task_id.to_string(),
            });
        }
    };

    let task = state
        .task_handler
        .store()
        .get(task_id)
        .map_err(task_error_to_thoughtgate)?;

    if task.principal != caller_principal {
        return Err(ThoughtGateError::TaskNotFound {
            task_id: task_id.to_string(),
        });
    }

    Ok(())
}

/// Convert TaskError to ThoughtGateError.
///
/// Implements: REQ-CORE-004 (Error Handling)
fn task_error_to_thoughtgate(error: thoughtgate_core::governance::TaskError) -> ThoughtGateError {
    use thoughtgate_core::governance::TaskError;

    match error {
        TaskError::NotFound { task_id } => ThoughtGateError::TaskNotFound {
            task_id: task_id.to_string(),
        },
        TaskError::Expired { task_id } => ThoughtGateError::TaskExpired {
            task_id: task_id.to_string(),
        },
        TaskError::AlreadyTerminal { task_id, status } => ThoughtGateError::InvalidParams {
            // MCP spec: -32602 with message "Cannot cancel task: already in terminal status '{status}'"
            details: format!(
                "Cannot cancel task {}: already in terminal status '{}'",
                task_id, status
            ),
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

/// Route a tools/call request through the 4-gate model.
///
/// Implements: REQ-CFG-001 Section 9 (Request Processing)
///
/// # Gate Flow
///
/// ```text
/// tools/call, resources/read, prompts/get
///     │
/// ┌─ Gate 1: Visibility ─────────────────────────┐
/// │  expose.is_visible(resource_name)?           │
/// │  Not visible → Error -32015                  │
/// └──────────────────────────────────────────────┘
///     │
/// ┌─ Gate 2: Governance Rules ───────────────────┐
/// │  governance.evaluate(resource_name, source)  │
/// │  Returns: MatchResult { action, policy_id }  │
/// └──────────────────────────────────────────────┘
///     │ (route by action)
/// ┌────────────────┬────────────────┬────────────────┬────────────────┐
/// │ Forward        │ Deny           │ Approve        │ Policy         │
/// │ → Upstream     │ → Error -32014 │ → Gate 4       │ → Gate 3       │
/// └────────────────┴────────────────┴────────────────┴────────────────┘
/// ```
async fn route_through_gates(
    state: &McpState,
    mut request: McpRequest,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    let config = state
        .config
        .as_ref()
        .ok_or_else(|| ThoughtGateError::ServiceUnavailable {
            reason: "Configuration not loaded".to_string(),
        })?;

    // Extract the governable resource name (tool name, resource URI, or prompt name)
    // Implements: REQ-CORE-003/F-002 (Method Routing)
    let resource_name = match extract_governable_name(&request) {
        Some(name) => name,
        None => {
            // Request without required identifier is invalid params
            let field = match request.method.as_str() {
                "tools/call" | "prompts/get" => "name",
                "resources/read" | "resources/subscribe" => "uri",
                _ => "identifier",
            };
            return Err(ThoughtGateError::InvalidParams {
                details: format!(
                    "Missing required field '{}' in {} params",
                    field, request.method
                ),
            });
        }
    };
    let source_id = get_source_id(state);

    debug!(
        resource = %resource_name,
        method = %request.method,
        source = %source_id,
        "Routing through 4-gate model"
    );

    // ========================================================================
    // Gate 1: Visibility Check
    // ========================================================================
    // Check if the tool is visible based on ExposeConfig
    // Implements: REQ-CFG-001 Section 9.3 (Exposure Filtering)
    //
    // Note: If the source is not found in config, we fail closed (block the
    // request). A missing source indicates a misconfiguration — the hardcoded
    // "upstream" source_id doesn't match any configured source. Config
    // validation should catch this at load time, but we enforce here as defense
    // in depth.

    if let Some(source) = config.get_source(source_id) {
        let expose: thoughtgate_core::config::ExposeConfig = source.expose();
        if !expose.is_visible(&resource_name) {
            warn!(
                resource = %resource_name,
                method = %request.method,
                source = %source_id,
                "Gate 1: Resource not exposed"
            );
            return Err(ThoughtGateError::ToolNotExposed {
                tool: resource_name,
                source_id: source_id.to_string(),
            });
        }
        debug!(resource = %resource_name, method = %request.method, "Gate 1 passed: resource is visible");
    } else {
        // Source not found in config — fail closed
        // This indicates a misconfiguration (get_source_id returns a name
        // that doesn't match any configured source). Allowing traffic through
        // would bypass visibility checks entirely.
        warn!(
            resource = %resource_name,
            source = %source_id,
            "Gate 1 blocked: source not found in config"
        );
        return Err(ThoughtGateError::ServiceUnavailable {
            reason: format!("Source '{}' not found in configuration", source_id),
        });
    }

    // ========================================================================
    // Gate 2: Governance Rules Evaluation
    // ========================================================================
    // Match against YAML governance rules to determine action
    // Implements: REQ-CFG-001 Section 9.2 (Rule Matching)

    let match_result = config.governance.evaluate(&resource_name, source_id);

    info!(
        resource = %resource_name,
        method = %request.method,
        action = %match_result.action,
        matched_rule = ?match_result.matched_rule,
        policy_id = ?match_result.policy_id,
        "Gate 2: Governance rule matched"
    );

    // ========================================================================
    // SEP-1686: Task Metadata Validation
    // ========================================================================
    // Validate that client sent params.task for actions that require it
    // This is checked AFTER Gate 2 because we need to know the action first
    validate_task_metadata(
        &mut request,
        &match_result.action,
        &resource_name,
        state.capability_cache.upstream_supports_tasks(),
    )?;

    // ========================================================================
    // Route by Gate 2 Action
    // ========================================================================

    match match_result.action {
        Action::Forward => {
            // Skip all policy checks, forward directly
            debug!(resource = %resource_name, "Gate 2: Forwarding directly to upstream");
            state.upstream.forward(&request).await
        }

        Action::Deny => {
            // Immediate rejection
            warn!(resource = %resource_name, "Gate 2: Request denied by governance rule");
            Err(ThoughtGateError::GovernanceRuleDenied {
                tool: resource_name,
                rule: match_result.matched_rule,
            })
        }

        Action::Approve => {
            // Gate 4: Create approval task
            debug!(resource = %resource_name, "Gate 2 → Gate 4: Starting approval workflow");
            start_approval_flow(state, request, &resource_name, &match_result).await
        }

        Action::Policy => {
            // Gate 3: Cedar evaluation with proper context
            debug!(resource = %resource_name, "Gate 2 → Gate 3: Evaluating Cedar policy");
            evaluate_with_cedar(state, request, Some(&match_result)).await
        }
    }
}

/// Start an approval workflow (Gate 4).
///
/// Implements: REQ-GOV-002/F-001, F-002 (Task creation and approval posting)
///
/// Creates a task, posts the approval request, and returns a task ID response.
/// The agent will poll for the result using tasks/get and tasks/result.
async fn start_approval_flow(
    state: &McpState,
    request: McpRequest,
    tool_name: &str,
    match_result: &MatchResult,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    let approval_engine =
        state
            .approval_engine
            .as_ref()
            .ok_or_else(|| ThoughtGateError::ServiceUnavailable {
                reason: "Approval engine not configured".to_string(),
            })?;

    // Infer principal from environment (uses spawn_blocking for file I/O)
    let policy_principal = tokio::task::spawn_blocking(infer_principal)
        .await
        .map_err(|e| ThoughtGateError::ServiceUnavailable {
            reason: format!("Principal inference task failed: {}", e),
        })?
        .map_err(|e| ThoughtGateError::PolicyDenied {
            tool: String::new(),
            policy_id: None,
            reason: Some(format!("Identity unavailable: {}", e)),
        })?;

    // Create ToolCallRequest for the approval engine
    // Transport and governance now share the same JsonRpcId type
    let mcp_request_id = request.id.clone().unwrap_or(JsonRpcId::Null);

    let tool_request = ToolCallRequest {
        method: request.method.clone(),
        name: tool_name.to_string(),
        arguments: extract_tool_arguments(&request),
        mcp_request_id,
    };

    // Create Principal for governance (preserve full K8s identity for re-evaluation)
    let principal = Principal::from_policy(
        &policy_principal.app_name,
        &policy_principal.namespace,
        &policy_principal.service_account,
        policy_principal.roles.clone(),
    );

    // Look up workflow-specific timeout from config
    let workflow_timeout = match_result
        .approval_workflow
        .as_ref()
        .and_then(|workflow_name| {
            state
                .config
                .as_ref()
                .and_then(|c| c.get_workflow(workflow_name))
                .map(|w| w.timeout_or_default())
        });

    // Start the approval workflow with workflow-specific timeout
    let result = approval_engine
        .start_approval(tool_request, principal, workflow_timeout)
        .await
        .map_err(|e| ThoughtGateError::ServiceUnavailable {
            reason: format!("Failed to start approval: {}", e),
        })?;

    info!(
        task_id = %result.task_id,
        tool = %tool_name,
        workflow = ?match_result.approval_workflow,
        timeout_secs = ?workflow_timeout.map(|d| d.as_secs()),
        "Gate 4: Approval workflow started"
    );

    // Return SEP-1686 task response
    Ok(JsonRpcResponse::task_created(
        request.id.clone(),
        result.task_id.to_string(),
        result.status.to_string(),
        result.poll_interval,
    ))
}

/// Evaluate a request against Cedar policy engine (Gate 3).
///
/// Implements: REQ-POL-001/F-001 (Policy Evaluation)
///
/// Evaluates Cedar policy for governable MCP methods:
/// - `tools/call` → uses CedarResource::ToolCall with name and arguments
/// - `resources/read`, `resources/subscribe`, `prompts/get` → uses CedarResource::McpMethod
///
/// List methods (`tools/list`, `resources/list`, `prompts/list`) are forwarded to
/// upstream without Cedar evaluation since they don't reference a specific resource.
/// Response filtering for list methods is a v0.3+ enhancement.
///
/// Cedar decisions:
/// - Permit → forward to upstream (or to Gate 4 if approval workflow specified)
/// - Forbid → return PolicyDenied error
///
/// # Arguments
///
/// * `state` - Application state
/// * `request` - The MCP request
/// * `match_result` - Optional Gate 2 result (provides policy_id and approval_workflow)
async fn evaluate_with_cedar(
    state: &McpState,
    request: McpRequest,
    match_result: Option<&MatchResult>,
) -> Result<JsonRpcResponse, ThoughtGateError> {
    // List methods bypass Cedar - they don't reference a specific resource
    // Response filtering for list methods is a v0.3+ enhancement
    if !method_requires_gates(&request.method) {
        debug!(
            method = %request.method,
            "Gate 3: Bypassing Cedar for list method - forwarding to upstream"
        );
        return state.upstream.forward(&request).await;
    }

    // Extract resource name for all governable methods
    let resource_name = match extract_governable_name(&request) {
        Some(name) => name,
        None => {
            // Governable method without required identifier is invalid params
            let field = match request.method.as_str() {
                "tools/call" | "prompts/get" => "name",
                "resources/read" | "resources/subscribe" => "uri",
                _ => "identifier",
            };
            return Err(ThoughtGateError::InvalidParams {
                details: format!(
                    "Missing required field '{}' in {} params",
                    field, request.method
                ),
            });
        }
    };

    // Infer principal from environment (uses spawn_blocking for file I/O)
    let policy_principal = tokio::task::spawn_blocking(infer_principal)
        .await
        .map_err(|e| ThoughtGateError::ServiceUnavailable {
            reason: format!("Principal inference task failed: {}", e),
        })?
        .map_err(|e| ThoughtGateError::PolicyDenied {
            tool: String::new(),
            policy_id: None,
            reason: Some(format!("Identity unavailable: {}", e)),
        })?;

    // Get policy_id and source_id from Gate 2 result, or use defaults
    let policy_id = match_result
        .and_then(|m| m.policy_id.clone())
        .unwrap_or_else(|| "default".to_string());
    let source_id = get_source_id(state).to_string();

    // Build Cedar resource based on method type
    let cedar_resource = if request.method == "tools/call" {
        // tools/call includes arguments for fine-grained policy checks
        CedarResource::ToolCall {
            name: resource_name.clone(),
            server: source_id.clone(),
            arguments: extract_tool_arguments(&request),
        }
    } else {
        // resources/read, resources/subscribe, prompts/get use McpMethod
        CedarResource::McpMethod {
            method: request.method.clone(),
            server: source_id.clone(),
        }
    };

    // Build Cedar request
    let cedar_request = CedarRequest {
        principal: policy_principal,
        resource: cedar_resource,
        context: CedarContext {
            policy_id: policy_id.clone(),
            source_id,
            time: TimeContext::now(),
        },
    };

    // Evaluate Cedar policy
    match state.cedar_engine.evaluate_v2(&cedar_request) {
        CedarDecision::Permit { .. } => {
            // For action: policy, Cedar permit → Gate 4 (approval workflow)
            // Per REQ-CORE-003 Section 8: policy action always goes to Gate 4 after permit
            if let Some(match_result) = match_result {
                // Use specified workflow or default to "default"
                let workflow = match_result
                    .approval_workflow
                    .clone()
                    .unwrap_or_else(|| "default".to_string());
                debug!(
                    resource = %resource_name,
                    method = %request.method,
                    workflow = %workflow,
                    policy_id = %policy_id,
                    "Gate 3: Cedar permit → Gate 4 approval"
                );
                return start_approval_flow(state, request, &resource_name, match_result).await;
            }

            // Legacy mode (no Gate 2 result): Cedar permit → forward to upstream
            debug!(
                resource = %resource_name,
                method = %request.method,
                policy_id = %policy_id,
                "Gate 3: Cedar permit (legacy mode) - forwarding to upstream"
            );
            state.upstream.forward(&request).await
        }
        CedarDecision::Forbid { reason, .. } => {
            // Cedar forbid → return PolicyDenied error
            warn!(
                resource = %resource_name,
                method = %request.method,
                policy_id = %policy_id,
                reason = %reason,
                "Gate 3: Cedar forbid - denying request"
            );
            Err(ThoughtGateError::PolicyDenied {
                tool: resource_name,
                policy_id: Some(policy_id),
                reason: Some(reason),
            })
        }
    }
}

/// Handle a batch JSON-RPC request, returning (StatusCode, Bytes).
///
/// Implements: REQ-CORE-003/F-007 (Batch Request Handling)
///
/// # Arguments
///
/// * `state` - MCP handler state
/// * `requests` - The parsed MCP requests
///
/// # Returns
///
/// (StatusCode, Bytes) with JSON-RPC batch response or empty array `[]` if all notifications.
///
/// # Design Note: Batch Concurrency
///
/// Requests are processed sequentially (single-permit-per-batch) rather than
/// in parallel. This design choice is intentional:
///
/// 1. **Approval coupling** (F-007.5): If ANY request needs approval, the
///    entire batch becomes task-augmented — requests are not independent.
/// 2. **Bounded resource usage**: With `max_batch_size` limiting array length,
///    sequential processing bounds total work per request to O(max_batch_size).
///    Parallel processing would require additional concurrency limits to
///    prevent a single batch from monopolizing the connection pool.
/// 3. **Deadlock prevention**: Parallel batch items competing for the same
///    upstream connection pool under load could deadlock if the pool is
///    exhausted by one batch while another waits.
/// 4. **Response ordering**: Sequential processing trivially preserves
///    response ordering without additional synchronization.
async fn handle_batch_request_bytes(
    state: &McpState,
    items: Vec<thoughtgate_core::transport::jsonrpc::BatchItem>,
) -> (StatusCode, Bytes) {
    use futures_util::stream::{self, StreamExt};
    use thoughtgate_core::transport::jsonrpc::BatchItem;

    // Process batch items concurrently using buffer_unordered.
    // JSON-RPC batch spec allows responses in any order (matched by id).
    // EC-MCP-006: Handle mixed valid/invalid items
    let batch_concurrency = items.len().min(16); // Cap concurrent items

    let responses: Vec<Option<JsonRpcResponse>> = stream::iter(items)
        .map(|item| async move {
            match item {
                BatchItem::Invalid { id, error } => {
                    // EC-MCP-006: Include error response for invalid items
                    let correlation_id =
                        thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
                    Some(JsonRpcResponse::error(
                        id,
                        error.to_jsonrpc_error(&correlation_id),
                    ))
                }
                BatchItem::Valid(request) => {
                    let is_notification = request.is_notification();
                    let id = request.id.clone();
                    let correlation_id = request.correlation_id.to_string();

                    // Route through shared routing logic
                    let result = route_request(state, request).await;

                    // F-007.4: Notifications don't produce response entries
                    if is_notification {
                        if let Err(e) = result {
                            error!(
                                correlation_id = %correlation_id,
                                error = %e,
                                "Notification in batch failed"
                            );
                        }
                        return None;
                    }

                    let response = match result {
                        Ok(r) => r,
                        Err(e) => JsonRpcResponse::error(id, e.to_jsonrpc_error(&correlation_id)),
                    };
                    Some(response)
                }
            }
        })
        .buffer_unordered(batch_concurrency)
        .collect()
        .await;

    // Filter out None entries (notifications)
    let responses: Vec<JsonRpcResponse> = responses.into_iter().flatten().collect();

    // Return batch response
    if responses.is_empty() {
        // All were notifications — per JSON-RPC 2.0 §6: "The client MUST NOT
        // expect the server to return any Response for a Batch that only
        // contains Notification objects." Return 204 No Content.
        return (StatusCode::NO_CONTENT, Bytes::new());
    }

    // Serialize batch response
    match serde_json::to_vec(&responses) {
        Ok(bytes) => (StatusCode::OK, Bytes::from(bytes)),
        Err(e) => {
            error!(error = %e, "Failed to serialize batch response");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Bytes::from_static(
                    br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error: failed to serialize response"}}"#,
                ),
            )
        }
    }
}

/// Build JSON bytes from a JsonRpcResponse.
fn json_bytes(response: &JsonRpcResponse) -> (StatusCode, Bytes) {
    match serde_json::to_vec(response) {
        Ok(bytes) => (StatusCode::OK, Bytes::from(bytes)),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(
                br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}"#,
            ),
        ),
    }
}

/// Build error bytes from a ThoughtGateError.
///
/// # Arguments
///
/// * `id` - The request ID (may be None for parse errors)
/// * `error` - The ThoughtGateError
/// * `correlation_id` - Correlation ID for the error response
fn error_bytes(
    id: Option<thoughtgate_core::transport::jsonrpc::JsonRpcId>,
    error: &ThoughtGateError,
    correlation_id: &str,
) -> (StatusCode, Bytes) {
    let jsonrpc_error = error.to_jsonrpc_error(correlation_id);
    let response = JsonRpcResponse::error(id, jsonrpc_error);

    // JSON-RPC errors still return HTTP 200
    match serde_json::to_vec(&response) {
        Ok(bytes) => (StatusCode::OK, Bytes::from(bytes)),
        Err(_) => (
            StatusCode::OK,
            Bytes::from_static(
                br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}"#,
            ),
        ),
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
}
