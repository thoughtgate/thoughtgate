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

pub(crate) mod cedar_eval;
pub(crate) mod gate_routing;
pub(crate) mod task_methods;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::StatusCode;
use bytes::Bytes;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use thoughtgate_core::config::Config;
use thoughtgate_core::error::ThoughtGateError;
use thoughtgate_core::governance::evaluator::GovernanceEvaluator;
use thoughtgate_core::governance::{
    ApprovalAdapter, ApprovalEngine, ApprovalEngineConfig, Principal, SlackAdapter, TaskHandler,
    TaskStore,
};
use thoughtgate_core::policy::engine::CedarEngine;
use thoughtgate_core::profile::Profile;
use thoughtgate_core::protocol::{
    CapabilityCache, extract_upstream_sse_support, extract_upstream_task_support,
    inject_task_capability, strip_sse_capability,
};
use thoughtgate_core::telemetry::{
    BoxedSpan, McpMessageType, McpSpanData, ThoughtGateMetrics, finish_mcp_span, start_mcp_span,
};
use thoughtgate_core::transport::jsonrpc::{
    JsonRpcId, JsonRpcResponse, McpRequest, ParsedRequests, parse_jsonrpc,
};
use thoughtgate_core::transport::router::{McpRouter, RouteTarget};
use thoughtgate_core::transport::upstream::UpstreamForwarder;
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
            evaluator: None,
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
                Profile::Production,
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
        handle_mcp_body_bytes(&self.state, body, None).await
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
    /// A tuple of (StatusCode, response bytes) for direct use by ProxyService.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/§10 (Request Handler Pattern)
    /// - Implements: REQ-OBS-002 §7.1 (W3C Trace Context Propagation)
    pub async fn handle_with_context(
        &self,
        body: Bytes,
        parent_context: opentelemetry::Context,
    ) -> (StatusCode, Bytes) {
        handle_mcp_body_bytes(&self.state, body, Some(parent_context)).await
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

/// Create governance components with optional Prometheus metrics for gauge updates.
///
/// This variant accepts `ThoughtGateMetrics` to wire the `tasks_pending` gauge
/// (REQ-OBS-002 §6.4/MG-002).
///
/// # Arguments
///
/// * `upstream` - Upstream forwarder for approved requests
/// * `config` - Optional YAML config (enables Gate 1, 2, 4)
/// * `shutdown` - Cancellation token for graceful shutdown
/// * `tg_metrics` - Optional Prometheus metrics for gauge updates
///
/// # Returns
///
/// Tuple of (TaskHandler, CedarEngine, optional ApprovalEngine)
#[allow(clippy::type_complexity)]
pub async fn create_governance_components_with_metrics(
    upstream: Arc<dyn UpstreamForwarder>,
    config: Option<&Config>,
    shutdown: CancellationToken,
    tg_metrics: Option<Arc<ThoughtGateMetrics>>,
) -> Result<(TaskHandler, Arc<CedarEngine>, Option<Arc<ApprovalEngine>>), ThoughtGateError> {
    // Create task store with optional metrics wiring (REQ-OBS-002 §6.4/MG-002)
    let task_store = if let Some(ref metrics) = tg_metrics {
        Arc::new(TaskStore::with_metrics(
            thoughtgate_core::governance::task::TaskStoreConfig::default(),
            metrics.clone(),
        ))
    } else {
        Arc::new(TaskStore::with_defaults())
    };
    let task_handler = TaskHandler::new(task_store.clone());

    // Create Cedar policy engine (Gate 3) with optional metrics wiring (REQ-OBS-002 §6.4/MG-003)
    let cedar_engine = {
        let engine = CedarEngine::new().map_err(|e| ThoughtGateError::ServiceUnavailable {
            reason: format!("Failed to create Cedar engine: {}", e),
        })?;
        if let Some(ref metrics) = tg_metrics {
            Arc::new(engine.with_metrics(metrics.clone()))
        } else {
            Arc::new(engine)
        }
    };

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

        // Wire metrics for task counters (REQ-OBS-002 §6.4/MC-007, MC-008)
        let engine = if let Some(ref metrics) = tg_metrics {
            engine.with_metrics(metrics.clone())
        } else {
            engine
        };

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

#[cfg(test)]
use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::header,
    response::{IntoResponse, Response},
    routing::post,
};

/// Handle POST /mcp/v1 requests (Axum handler for tests).
///
/// Implements: REQ-CORE-003/§10 (Request Handler Pattern)
#[cfg(test)]
async fn handle_mcp_request(State(state): State<Arc<McpState>>, body: Bytes) -> Response {
    let (status, bytes) = handle_mcp_body_bytes(&state, body, None).await;
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
/// - Implements: REQ-OBS-002 §7.1 (W3C Trace Context Propagation)
async fn handle_mcp_body_bytes(
    state: &McpState,
    body: Bytes,
    parent_context: Option<opentelemetry::Context>,
) -> (StatusCode, Bytes) {
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
    if prev.saturating_add(body_size) > state.max_aggregate_buffer {
        state.buffered_bytes.fetch_sub(body_size, Ordering::Release);
        let correlation_id =
            thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
        warn!(
            correlation_id = %correlation_id,
            buffered = prev.saturating_add(body_size),
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
    // Also extract method name for payload size metrics (REQ-OBS-002 §6.2/MH-005).
    let (permit_count, inbound_method) = match &parsed {
        ParsedRequests::Single(req) => (1, req.method.clone()),
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
            // For batch, use "batch" as method label
            (requests.len().max(1) as u32, "batch".to_string())
        }
    };

    // Record inbound payload size (REQ-OBS-002 §6.2/MH-005)
    if let Some(ref metrics) = state.tg_metrics {
        metrics.record_payload_size("inbound", &inbound_method, body_size as f64);
    }

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

    let (status, response_bytes) = match parsed {
        ParsedRequests::Single(request) => {
            handle_single_request_bytes(state, request, parent_context.as_ref()).await
        }
        ParsedRequests::Batch(requests) => {
            handle_batch_request_bytes(state, requests, parent_context.as_ref()).await
        }
    };

    // Record outbound payload size (REQ-OBS-002 §6.2/MH-005)
    if let Some(ref metrics) = state.tg_metrics {
        metrics.record_payload_size("outbound", &inbound_method, response_bytes.len() as f64);
    }

    (status, response_bytes)
}

// ============================================================================
// Helper Functions for 4-Gate Model
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

    let source_id = cedar_eval::get_source_id(state);

    use thoughtgate_core::governance::list_filtering;

    // Gate 1+2: Filter by visibility and annotate taskSupport.
    // Delegates to core so both proxy and CLI share identical filtering.
    // Per REQ-CORE-003/F-003: Gate 1 applies to tools, resources, and prompts.
    match request.method.as_str() {
        "tools/list" => {
            let mut new_result = result.clone();
            let tools_arr = match new_result
                .as_object_mut()
                .and_then(|obj| obj.get_mut("tools"))
                .and_then(|v| v.as_array_mut())
            {
                Some(arr) => arr,
                None => return Ok(response),
            };
            list_filtering::filter_and_annotate_tools(tools_arr, config, source_id);
            Ok(JsonRpcResponse::success(response.id, new_result))
        }
        "resources/list" => {
            let mut new_result = result.clone();
            let resources_arr = match new_result
                .as_object_mut()
                .and_then(|obj| obj.get_mut("resources"))
                .and_then(|v| v.as_array_mut())
            {
                Some(arr) => arr,
                None => return Ok(response),
            };
            list_filtering::filter_resources_by_visibility(resources_arr, config, source_id);
            Ok(JsonRpcResponse::success(response.id, new_result))
        }
        "prompts/list" => {
            let mut new_result = result.clone();
            let prompts_arr = match new_result
                .as_object_mut()
                .and_then(|obj| obj.get_mut("prompts"))
                .and_then(|v| v.as_array_mut())
            {
                Some(arr) => arr,
                None => return Ok(response),
            };
            list_filtering::filter_prompts_by_visibility(prompts_arr, config, source_id);
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
                if cedar_eval::method_requires_gates(&request.method) {
                    // Governable methods: tools/call, resources/read, etc.
                    gate_routing::route_through_gates(state, request).await
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
                cedar_eval::evaluate_with_cedar(state, request, None, None).await
            }
        }
        RouteTarget::TaskHandler { method, request } => {
            task_methods::handle_task_method(state, method, &request).await
        }
        RouteTarget::InitializeHandler { request } => {
            // Implements: REQ-CORE-007/F-001 (Capability Injection)
            handle_initialize_method(state, request).await
        }
        RouteTarget::PassThrough { request } => state.upstream.forward(&request).await,
    }
}

async fn handle_single_request_bytes(
    state: &McpState,
    request: McpRequest,
    parent_context: Option<&opentelemetry::Context>,
) -> (StatusCode, Bytes) {
    let correlation_id = request.correlation_id.to_string();
    let id = request.id.clone();
    let is_notification = request.is_notification();
    let method = request.method.clone();
    let request_start = std::time::Instant::now();

    // Extract tool name for tools/call requests (REQ-OBS-002 §5.1.1)
    // Borrows from request.params (Arc<Value>) — must convert to owned before request is moved
    let tool_name: Option<String> = extract_tool_name(&request).map(String::from);

    // Start MCP span with optional parent context (REQ-OBS-002 §5.1, §7.1)
    // When parent_context is provided, the span becomes a child of the caller's trace.
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
        session_id: None, // TODO: extract from MCP handshake when available
        parent_context,
    };
    let mut mcp_span: BoxedSpan = start_mcp_span(&span_data);

    debug!(
        correlation_id = %correlation_id,
        method = %request.method,
        is_notification = is_notification,
        is_task_augmented = request.is_task_augmented(),
        "Processing single request"
    );

    // Route the request through shared routing logic.
    // Note: Trace context propagation to upstream uses Context::current() in forward_once().
    // The MCP span is part of the correct trace because we used parent_context when
    // creating it via start_mcp_span(). For outbound propagation to upstream, the
    // global propagator was installed in init_telemetry() and will inject the trace
    // context based on the current span.
    //
    // IMPORTANT: ContextGuard is !Send and cannot be held across .await points.
    // The trace context for upstream injection works because:
    // 1. The MCP span was created with the correct parent context
    // 2. The global propagator extracts context from the current span
    // 3. forward_once() calls Context::current() which finds the span
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
    finish_mcp_span(&mut mcp_span, is_error, error_code, error_type.as_deref());
    drop(mcp_span);

    // Record prometheus-client metrics (REQ-OBS-002 §6.1, §6.2)
    let outcome = if result.is_ok() { "success" } else { "error" };
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
fn extract_tool_name(request: &McpRequest) -> Option<&str> {
    if request.method != "tools/call" {
        return None;
    }
    request.params.as_ref()?.get("name")?.as_str()
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
/// Batch items are processed concurrently via `buffer_unordered` with a cap
/// of 16 in-flight items. This is safe because:
///
/// - **Independent routing**: Each item routes through `route_request()`
///   independently. Approval decisions are per-request, not per-batch.
/// - **Bounded concurrency**: The `.min(16)` cap prevents a single batch
///   from monopolizing the upstream connection pool, even when
///   `max_batch_size` allows up to 100 items.
/// - **Response ordering**: JSON-RPC 2.0 §6 specifies batch responses may
///   be returned in any order — clients match by `id`.
async fn handle_batch_request_bytes(
    state: &McpState,
    items: Vec<thoughtgate_core::transport::jsonrpc::BatchItem>,
    parent_context: Option<&opentelemetry::Context>,
) -> (StatusCode, Bytes) {
    use futures_util::stream::{self, StreamExt};
    use opentelemetry::trace::{Span, SpanKind, Tracer};
    use thoughtgate_core::transport::jsonrpc::BatchItem;

    // Create batch-level span for observability (REQ-OBS-002 §5.1).
    // ContextGuard is !Send so we cannot attach the span across async
    // boundaries, but the span itself tracks batch-level metadata.
    let tracer = opentelemetry::global::tracer("thoughtgate");
    let batch_size = items.len();
    let mut batch_span = {
        let builder = tracer
            .span_builder(format!("jsonrpc.batch[{batch_size}]"))
            .with_kind(SpanKind::Server)
            .with_attributes(vec![opentelemetry::KeyValue::new(
                "mcp.batch.size",
                batch_size as i64,
            )]);
        match parent_context {
            Some(ctx) => builder.start_with_context(&tracer, ctx),
            None => builder.start(&tracer),
        }
    };

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

    // Record error count on the batch span.
    let error_count = responses.iter().filter(|r| r.error.is_some()).count();
    if error_count > 0 {
        batch_span.set_attribute(opentelemetry::KeyValue::new(
            "mcp.batch.error_count",
            error_count as i64,
        ));
    }
    batch_span.end();

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
            evaluator: None,
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
            evaluator: None,
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
