//! Upstream MCP client with connection pooling.
//!
//! Implements: REQ-CORE-003/F-004 (Upstream Forwarding)
//!
//! This client maintains persistent connections to the upstream MCP server
//! and handles request forwarding with timeout and error classification.
//!
//! # Connection Pooling
//!
//! The client uses reqwest's built-in connection pooling to maintain
//! persistent connections to the upstream server. This reduces latency
//! for subsequent requests by avoiding TCP handshake and TLS negotiation.
//!
//! # Error Classification
//!
//! Errors are classified into ThoughtGateError variants:
//! - Timeout errors → UpstreamTimeout (-32001)
//! - Connection errors → UpstreamConnectionFailed (-32000)
//! - Other errors → UpstreamError (-32002)
//!
//! # Security
//!
//! - TLS certificate verification is enabled by default
//! - TLS 1.2+ enforced by rustls (no TLS 1.0/1.1 support, preventing downgrade attacks)
//! - No automatic retry (prevents duplicate side effects)

use std::time::Duration;

use reqwest::Client;
use tracing::{debug, error, warn};

use crate::error::ThoughtGateError;
use crate::transport::jsonrpc::{JsonRpcResponse, McpRequest};

/// Configuration for the upstream client.
///
/// Implements: REQ-CORE-003/§5.3 (Configuration)
#[derive(Debug, Clone)]
pub struct UpstreamConfig {
    /// Base URL of the upstream MCP server (e.g., "http://localhost:3000")
    pub base_url: String,
    /// Request timeout (includes connection + response)
    pub timeout: Duration,
    /// Connection timeout (TCP + TLS handshake)
    pub connect_timeout: Duration,
    /// Maximum idle connections per host
    pub pool_max_idle_per_host: usize,
    /// Idle connection timeout
    pub pool_idle_timeout: Duration,
    /// Maximum response body size in bytes (default: 10MB).
    /// Prevents unbounded memory allocation from oversized upstream responses.
    pub max_response_size: usize,
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(5),
            pool_max_idle_per_host: 32,
            pool_idle_timeout: Duration::from_secs(90),
            max_response_size: 10 * 1024 * 1024, // 10 MB
        }
    }
}

impl UpstreamConfig {
    /// Load configuration from environment variables.
    ///
    /// Implements: REQ-CORE-003/§5.3 (Configuration)
    ///
    /// # Environment Variables
    ///
    /// - `THOUGHTGATE_UPSTREAM` (required): Base URL of upstream MCP server
    /// - `THOUGHTGATE_REQUEST_TIMEOUT_SECS` (default: 30): Request timeout
    /// - `THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS` (default: 5): Connection timeout
    ///
    /// # Errors
    ///
    /// Returns `ThoughtGateError::InvalidParams` if:
    /// - `THOUGHTGATE_UPSTREAM` is not set
    /// - `THOUGHTGATE_REQUEST_TIMEOUT_SECS` is set but not a valid u64
    /// - `THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS` is set but not a valid u64
    pub fn from_env() -> Result<Self, ThoughtGateError> {
        let base_url =
            std::env::var("THOUGHTGATE_UPSTREAM").map_err(|_| ThoughtGateError::InvalidParams {
                details: "THOUGHTGATE_UPSTREAM environment variable is required".to_string(),
            })?;

        let timeout_secs: u64 = match std::env::var("THOUGHTGATE_REQUEST_TIMEOUT_SECS") {
            Ok(val) => val.parse().map_err(|_| ThoughtGateError::InvalidParams {
                details: format!(
                    "THOUGHTGATE_REQUEST_TIMEOUT_SECS must be a valid integer, got: '{}'",
                    val
                ),
            })?,
            Err(_) => 30, // Default when not set
        };

        let connect_timeout_secs: u64 = match std::env::var(
            "THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS",
        ) {
            Ok(val) => val.parse().map_err(|_| ThoughtGateError::InvalidParams {
                details: format!(
                    "THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS must be a valid integer, got: '{}'",
                    val
                ),
            })?,
            Err(_) => 5, // Default when not set
        };

        Ok(Self {
            base_url,
            timeout: Duration::from_secs(timeout_secs),
            connect_timeout: Duration::from_secs(connect_timeout_secs),
            ..Default::default()
        })
    }

    /// Create a new config with the specified base URL.
    ///
    /// Uses default values for all other settings.
    ///
    /// Implements: REQ-CORE-003/§5.3 (Configuration)
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            ..Default::default()
        }
    }
}

/// Upstream MCP client.
///
/// Implements: REQ-CORE-003/F-004 (Upstream Forwarding)
/// Implements: REQ-CORE-003/§5.5 (Upstream Client)
///
/// # Thread Safety
///
/// The client is `Clone` and can be shared across tasks. The underlying
/// reqwest client handles connection pooling internally.
#[derive(Clone)]
pub struct UpstreamClient {
    client: Client,
    config: UpstreamConfig,
    /// Pre-computed MCP endpoint URL (avoids `format!()` per request).
    mcp_url: String,
    /// Optional prometheus-client metrics for upstream request tracking (REQ-OBS-002 §6.1/MC-006)
    tg_metrics: Option<std::sync::Arc<crate::telemetry::ThoughtGateMetrics>>,
}

impl UpstreamClient {
    /// Create a new upstream client.
    ///
    /// Implements: REQ-CORE-003/F-004.1 (Connection Pool)
    ///
    /// # Arguments
    ///
    /// * `config` - Client configuration
    ///
    /// # Errors
    ///
    /// Returns `ThoughtGateError::InternalError` if:
    /// - The base_url is empty or not a valid absolute URL
    /// - The HTTP client cannot be built
    pub fn new(config: UpstreamConfig) -> Result<Self, ThoughtGateError> {
        // Validate base_url is non-empty and parseable
        if config.base_url.is_empty() {
            return Err(ThoughtGateError::InternalError {
                correlation_id: "upstream-client-config-error: base_url is empty".to_string(),
            });
        }

        // Validate URL is parseable (reqwest uses url::Url internally)
        if let Err(e) = reqwest::Url::parse(&config.base_url) {
            return Err(ThoughtGateError::InternalError {
                correlation_id: format!(
                    "upstream-client-config-error: invalid base_url '{}': {}",
                    config.base_url, e
                ),
            });
        }

        let client = Client::builder()
            .timeout(config.timeout)
            .connect_timeout(config.connect_timeout)
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .pool_idle_timeout(config.pool_idle_timeout)
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .map_err(|e| ThoughtGateError::InternalError {
                correlation_id: format!("upstream-client-build-error: {}", e),
            })?;

        let mcp_url = format!("{}/mcp/v1", config.base_url.trim_end_matches('/'));

        Ok(Self {
            client,
            config,
            mcp_url,
            tg_metrics: None,
        })
    }

    /// Add prometheus-client metrics to this upstream client.
    ///
    /// Enables recording of upstream request counts and latencies.
    ///
    /// Implements: REQ-OBS-002 §6.1/MC-006, §6.2/MH-003
    pub fn with_metrics(
        mut self,
        metrics: std::sync::Arc<crate::telemetry::ThoughtGateMetrics>,
    ) -> Self {
        self.tg_metrics = Some(metrics);
        self
    }

    /// Perform a health check to verify upstream connectivity.
    ///
    /// Implements: REQ-CORE-005/F-007.3
    ///
    /// This performs a simple TCP connectivity check to the upstream server
    /// without sending a full MCP request. Used for readiness probes.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the upstream is reachable (any HTTP response, including 4xx)
    /// - `Err(ThoughtGateError::UpstreamConnectionFailed)` if connection fails
    /// - `Err(ThoughtGateError::UpstreamTimeout)` if connection times out
    #[tracing::instrument(skip(self))]
    pub async fn health_check(&self) -> Result<(), ThoughtGateError> {
        // Use the HTTP client (with its connection pool and TLS stack) rather
        // than raw TCP, so the health check exercises the same path as real
        // requests and properly tests TLS negotiation.
        let result = self
            .client
            .head(&self.config.base_url)
            .timeout(self.config.connect_timeout)
            .send()
            .await;

        match result {
            Ok(resp) => {
                // Any HTTP response means the server is reachable.
                // 4xx/405 is expected (HEAD on an MCP endpoint may not be supported).
                // Only 5xx indicates an unhealthy server, but for a connectivity check
                // even that means "reachable".
                debug!(
                    url = %self.config.base_url,
                    status = %resp.status(),
                    "Upstream health check: server reachable"
                );
                Ok(())
            }
            Err(e) => Err(self.classify_error(e, "health-check")),
        }
    }

    /// Forward a single request to upstream.
    ///
    /// Implements: REQ-CORE-003/F-004 (Upstream Forwarding)
    ///
    /// # Arguments
    ///
    /// * `request` - The MCP request to forward
    ///
    /// # Returns
    ///
    /// * `Ok(JsonRpcResponse)` - Response from upstream
    /// * `Err(ThoughtGateError::UpstreamTimeout)` - Request timed out
    /// * `Err(ThoughtGateError::UpstreamConnectionFailed)` - Failed to connect
    /// * `Err(ThoughtGateError::UpstreamError)` - Other upstream errors
    ///
    /// # Handles
    ///
    /// - EC-MCP-009: Upstream timeout
    /// - EC-MCP-010: Upstream connection refused
    ///
    /// # Note on Header Forwarding
    ///
    /// Currently only sets `Content-Type: application/json`. Header forwarding
    /// (F-004.2) is not implemented as it requires security review - forwarding
    /// Authorization headers to upstream could leak credentials. For MCP traffic,
    /// the JSON-RPC body contains all necessary context.
    /// Maximum number of retry attempts for transport-level failures.
    const MAX_TRANSPORT_RETRIES: u32 = 2;

    /// Forward a single request with retry for transport-level failures.
    ///
    /// Retries on `UpstreamConnectionFailed` only (connection refused, DNS
    /// failure). Does NOT retry on timeouts, HTTP errors, or JSON-RPC errors
    /// to prevent duplicate side effects.
    ///
    /// Implements: REQ-CORE-003/F-004 (Upstream Forwarding with resilience)
    pub async fn forward_with_retry(
        &self,
        request: &McpRequest,
    ) -> Result<JsonRpcResponse, ThoughtGateError> {
        let mut last_error = None;

        for attempt in 0..=Self::MAX_TRANSPORT_RETRIES {
            match self.forward_once(request).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    let is_retriable =
                        matches!(&e, ThoughtGateError::UpstreamConnectionFailed { .. });

                    if !is_retriable || attempt == Self::MAX_TRANSPORT_RETRIES {
                        return Err(e);
                    }

                    // Exponential backoff: 100ms, 400ms
                    let backoff = Duration::from_millis(100 * 4u64.pow(attempt));
                    warn!(
                        attempt = attempt + 1,
                        max_retries = Self::MAX_TRANSPORT_RETRIES,
                        backoff_ms = backoff.as_millis() as u64,
                        error = %e,
                        correlation_id = %request.correlation_id,
                        "Transport connection failure, retrying"
                    );
                    tokio::time::sleep(backoff).await;
                    last_error = Some(e);
                }
            }
        }

        Err(
            last_error.unwrap_or_else(|| ThoughtGateError::InternalError {
                correlation_id: "retry-exhausted".to_string(),
            }),
        )
    }

    /// Forward a single request to upstream (single attempt, no retry).
    ///
    /// Implements: REQ-CORE-003/F-004 (Upstream Forwarding)
    /// Implements: REQ-OBS-002 §7.2 (Outbound Trace Context Injection)
    /// Implements: REQ-OBS-002 §6.1/MC-006, §6.2/MH-003 (Upstream Metrics)
    #[tracing::instrument(skip(self, request), fields(method = %request.method, correlation_id = %request.correlation_id))]
    async fn forward_once(
        &self,
        request: &McpRequest,
    ) -> Result<JsonRpcResponse, ThoughtGateError> {
        let url = &self.mcp_url;
        let correlation_id = request.correlation_id.to_string();

        debug!(
            correlation_id = %correlation_id,
            method = %request.method,
            url = %url,
            "Forwarding request to upstream"
        );

        // Build JSON-RPC request
        let jsonrpc_request = request.to_jsonrpc_request();

        // Inject W3C trace context into outbound request headers.
        // This enables end-to-end distributed tracing - upstream will see
        // traceparent header linking back to ThoughtGate's MCP span.
        // The correlation_id is added to tracestate for additional correlation.
        // Implements: REQ-OBS-002 §7.2
        let current_ctx = opentelemetry::Context::current();
        let mut trace_headers = reqwest::header::HeaderMap::new();
        crate::telemetry::inject_context_into_headers(
            &current_ctx,
            &mut trace_headers,
            Some(&correlation_id),
        );

        // Start timing for upstream latency metric (REQ-OBS-002 §6.2/MH-003)
        let upstream_start = std::time::Instant::now();

        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .headers(trace_headers)
            .json(&jsonrpc_request)
            .send()
            .await
            .map_err(|e| self.classify_error(e, &correlation_id))?;

        // Check HTTP status
        let status = response.status();
        let status_str = status.as_u16().to_string();

        // Record upstream request metric (REQ-OBS-002 §6.1/MC-006, §6.2/MH-003)
        let elapsed_ms = upstream_start.elapsed().as_secs_f64() * 1000.0;
        if let Some(ref metrics) = self.tg_metrics {
            metrics.record_upstream_request("upstream", &status_str, elapsed_ms);
        }

        // Handle 204 No Content (notification response from upstream)
        if status == reqwest::StatusCode::NO_CONTENT {
            debug!(
                correlation_id = %correlation_id,
                "Upstream returned 204 No Content (notification acknowledged)"
            );
            // Return a synthetic success response for internal processing.
            // Note: For notifications, request.id is None per JSON-RPC 2.0 spec.
            // This synthetic response is only used internally by the routing layer
            // and is NOT sent to the client (server.rs checks is_notification() and
            // returns 204 No Content to the client without a response body).
            return Ok(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::Value::Null,
            ));
        }

        if !status.is_success() {
            warn!(
                correlation_id = %correlation_id,
                status = %status,
                "Upstream returned error status"
            );
            return Err(classify_upstream_http_error(status));
        }

        // Read response body with size limit, then parse
        let body_bytes = self.read_body_limited(response, &correlation_id).await?;
        let body: JsonRpcResponse = serde_json::from_slice(&body_bytes).map_err(|e| {
            error!(
                correlation_id = %correlation_id,
                error = %e,
                body_size = body_bytes.len(),
                "Failed to parse upstream response"
            );
            ThoughtGateError::UpstreamError {
                code: -32002,
                message: format!("Failed to parse upstream response: {}", e),
            }
        })?;

        debug!(
            correlation_id = %correlation_id,
            has_error = body.error.is_some(),
            "Received upstream response"
        );

        Ok(body)
    }

    /// Forward a batch of requests to upstream.
    ///
    /// Implements: REQ-CORE-003/F-007 (Batch Request Handling)
    ///
    /// # Arguments
    ///
    /// * `requests` - The MCP requests to forward as a batch (must be non-empty)
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<JsonRpcResponse>)` - Responses from upstream
    /// * `Err(ThoughtGateError::InvalidRequest)` - If the batch is empty
    /// * `Err(ThoughtGateError)` - If the batch request failed
    /// Implements: REQ-OBS-002 §7.2 (Outbound Trace Context Injection)
    /// Implements: REQ-OBS-002 §6.1/MC-006, §6.2/MH-003 (Upstream Metrics)
    #[tracing::instrument(skip(self, requests), fields(batch_size = requests.len()))]
    pub async fn forward_batch(
        &self,
        requests: &[McpRequest],
    ) -> Result<Vec<JsonRpcResponse>, ThoughtGateError> {
        // Reject empty batches - don't send [] to upstream
        if requests.is_empty() {
            return Err(ThoughtGateError::InvalidRequest {
                details: "Empty batch requests are invalid".to_string(),
            });
        }

        let url = &self.mcp_url;

        debug!(
            batch_size = requests.len(),
            url = %url,
            "Forwarding batch request to upstream"
        );

        // Build batch of JSON-RPC requests
        let jsonrpc_requests: Vec<_> = requests.iter().map(|r| r.to_jsonrpc_request()).collect();

        // Inject W3C trace context into outbound batch request headers.
        // Uses "batch" as the correlation identifier in tracestate.
        // Implements: REQ-OBS-002 §7.2
        let current_ctx = opentelemetry::Context::current();
        let mut trace_headers = reqwest::header::HeaderMap::new();
        crate::telemetry::inject_context_into_headers(
            &current_ctx,
            &mut trace_headers,
            Some("batch"),
        );

        // Start timing for upstream latency metric (REQ-OBS-002 §6.2/MH-003)
        let upstream_start = std::time::Instant::now();

        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .headers(trace_headers)
            .json(&jsonrpc_requests)
            .send()
            .await
            .map_err(|e| self.classify_error(e, "batch"))?;

        let status = response.status();
        let status_str = status.as_u16().to_string();

        // Record upstream request metric (REQ-OBS-002 §6.1/MC-006, §6.2/MH-003)
        let elapsed_ms = upstream_start.elapsed().as_secs_f64() * 1000.0;
        if let Some(ref metrics) = self.tg_metrics {
            metrics.record_upstream_request("upstream", &status_str, elapsed_ms);
        }

        // Handle 204 No Content (all requests were notifications)
        if status == reqwest::StatusCode::NO_CONTENT {
            debug!(
                batch_size = requests.len(),
                "Upstream returned 204 No Content (all notifications acknowledged)"
            );
            // Return empty response array - no responses expected for notification-only batches
            return Ok(Vec::new());
        }

        if !status.is_success() {
            return Err(classify_upstream_http_error(status));
        }

        let body_bytes = self.read_body_limited(response, "batch").await?;
        let body: Vec<JsonRpcResponse> =
            serde_json::from_slice(&body_bytes).map_err(|e| ThoughtGateError::UpstreamError {
                code: -32002,
                message: format!("Failed to parse upstream batch response: {}", e),
            })?;

        debug!(
            response_count = body.len(),
            "Received upstream batch response"
        );

        Ok(body)
    }

    /// Read the response body with a size limit.
    ///
    /// Checks the `Content-Length` header first (if present) for early rejection,
    /// then reads the body up to `max_response_size`. Returns an error if the
    /// response exceeds the limit.
    async fn read_body_limited(
        &self,
        response: reqwest::Response,
        correlation_id: &str,
    ) -> Result<bytes::Bytes, ThoughtGateError> {
        let max_size = self.config.max_response_size;

        // Early reject if Content-Length exceeds limit
        if let Some(content_length) = response.content_length() {
            if content_length as usize > max_size {
                warn!(
                    correlation_id = %correlation_id,
                    content_length = content_length,
                    max_response_size = max_size,
                    "Upstream response exceeds size limit (Content-Length)"
                );
                return Err(ThoughtGateError::UpstreamError {
                    code: -32002,
                    message: format!(
                        "Upstream response too large: {} bytes exceeds {} byte limit",
                        content_length, max_size
                    ),
                });
            }
        }

        // Stream body chunk-by-chunk with size enforcement.
        // This prevents unbounded memory growth for chunked responses
        // that lack a Content-Length header.
        let mut buf = Vec::with_capacity(
            response
                .content_length()
                .map(|cl| cl as usize)
                .unwrap_or(8192)
                .min(max_size),
        );

        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            error!(
                correlation_id = %correlation_id,
                error = %e,
                "Failed to read upstream response body chunk"
            );
            ThoughtGateError::UpstreamError {
                code: -32002,
                message: format!("Failed to read upstream response: {}", e),
            }
        })? {
            if buf.len() + chunk.len() > max_size {
                warn!(
                    correlation_id = %correlation_id,
                    accumulated = buf.len(),
                    chunk_size = chunk.len(),
                    max_response_size = max_size,
                    "Upstream response exceeds size limit during streaming"
                );
                return Err(ThoughtGateError::UpstreamError {
                    code: -32002,
                    message: format!(
                        "Upstream response too large: >={} bytes exceeds {} byte limit",
                        buf.len() + chunk.len(),
                        max_size
                    ),
                });
            }
            buf.extend_from_slice(&chunk);
        }

        Ok(buf.into())
    }

    /// Classify a reqwest error into ThoughtGateError.
    ///
    /// Implements: REQ-CORE-003/§6.5 (Error handling)
    ///
    /// # Arguments
    ///
    /// * `error` - The reqwest error to classify
    /// * `correlation_id` - Correlation ID for logging
    ///
    /// # Returns
    ///
    /// The appropriate ThoughtGateError variant.
    fn classify_error(&self, error: reqwest::Error, correlation_id: &str) -> ThoughtGateError {
        if error.is_timeout() {
            warn!(
                correlation_id = %correlation_id,
                timeout_secs = self.config.timeout.as_secs(),
                "Upstream request timed out"
            );
            ThoughtGateError::UpstreamTimeout {
                url: self.config.base_url.clone(),
                timeout_secs: self.config.timeout.as_secs(),
            }
        } else if error.is_connect() {
            warn!(
                correlation_id = %correlation_id,
                url = %self.config.base_url,
                "Failed to connect to upstream"
            );
            ThoughtGateError::UpstreamConnectionFailed {
                url: self.config.base_url.clone(),
                reason: error.to_string(),
            }
        } else {
            error!(
                correlation_id = %correlation_id,
                error = %error,
                "Upstream request failed"
            );
            ThoughtGateError::UpstreamError {
                code: -32002,
                message: error.to_string(),
            }
        }
    }
}

/// Classify an upstream HTTP error status into the appropriate JSON-RPC error code.
///
/// Maps specific HTTP status codes to semantically correct error types:
/// - 401/403 → UpstreamError with -32002 (auth error from upstream, not client policy)
/// - 404 → UpstreamError with -32002 (resource/endpoint not found, NOT JSON-RPC method)
/// - 429 → RateLimited (-32009)
/// - Other 4xx → UpstreamError with -32602 (client error)
/// - 503 → ServiceUnavailable (-32013)
/// - Other 5xx → UpstreamError with -32002 (server error)
fn classify_upstream_http_error(status: reqwest::StatusCode) -> ThoughtGateError {
    match status.as_u16() {
        401 | 403 => ThoughtGateError::UpstreamError {
            code: -32002,
            message: format!("Upstream authentication error: HTTP {status}"),
        },
        404 => ThoughtGateError::UpstreamError {
            code: -32002,
            message: format!("Upstream resource not found: HTTP {status}"),
        },
        429 => ThoughtGateError::RateLimited {
            retry_after_secs: None,
        },
        400..=499 => ThoughtGateError::UpstreamError {
            code: -32602,
            message: format!("Upstream client error: HTTP {status}"),
        },
        503 => ThoughtGateError::ServiceUnavailable {
            reason: format!("Upstream service unavailable: HTTP {status}"),
        },
        _ => ThoughtGateError::UpstreamError {
            code: -32002,
            message: format!("Upstream server error: HTTP {status}"),
        },
    }
}

/// Trait for upstream client (enables mocking in tests).
///
/// This trait abstracts the upstream forwarding behavior, allowing tests
/// to inject mock implementations without making actual HTTP requests.
///
/// Implements: REQ-CORE-003/F-004 (Upstream Forwarding)
#[async_trait::async_trait]
pub trait UpstreamForwarder: Send + Sync {
    /// Forward a single request to upstream.
    ///
    /// Implements: REQ-CORE-003/F-004.4 (Return upstream response)
    async fn forward(&self, request: &McpRequest) -> Result<JsonRpcResponse, ThoughtGateError>;

    /// Forward a batch of requests to upstream.
    ///
    /// Implements: REQ-CORE-003/F-007 (Batch Request Handling)
    async fn forward_batch(
        &self,
        requests: &[McpRequest],
    ) -> Result<Vec<JsonRpcResponse>, ThoughtGateError>;
}

#[async_trait::async_trait]
impl UpstreamForwarder for UpstreamClient {
    async fn forward(&self, request: &McpRequest) -> Result<JsonRpcResponse, ThoughtGateError> {
        self.forward_with_retry(request).await
    }

    async fn forward_batch(
        &self,
        requests: &[McpRequest],
    ) -> Result<Vec<JsonRpcResponse>, ThoughtGateError> {
        self.forward_batch(requests).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// RAII guard for env var tests that saves and restores env var state.
    struct EnvVarGuard {
        vars: Vec<(&'static str, Option<String>)>,
    }

    impl EnvVarGuard {
        fn new(var_names: &[&'static str]) -> Self {
            let vars = var_names
                .iter()
                .map(|&name| (name, std::env::var(name).ok()))
                .collect();
            Self { vars }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            for (name, original) in &self.vars {
                // SAFETY: We're in a single-threaded test context (enforced by #[serial])
                unsafe {
                    match original {
                        Some(val) => std::env::set_var(name, val),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    #[test]
    fn test_default_config() {
        let config = UpstreamConfig::default();
        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(config.connect_timeout, Duration::from_secs(5));
        assert_eq!(config.pool_max_idle_per_host, 32);
        assert_eq!(config.pool_idle_timeout, Duration::from_secs(90));
        assert!(config.base_url.is_empty());
    }

    #[test]
    fn test_config_with_base_url() {
        let config = UpstreamConfig::with_base_url("http://localhost:3000");
        assert_eq!(config.base_url, "http://localhost:3000");
        assert_eq!(config.timeout, Duration::from_secs(30)); // Default
    }

    #[test]
    fn test_upstream_client_creation() {
        let config = UpstreamConfig::with_base_url("http://localhost:3000");
        let client = UpstreamClient::new(config);
        assert!(client.is_ok());
    }

    #[test]
    fn test_upstream_client_empty_url() {
        let config = UpstreamConfig::default(); // Empty base_url
        let result = UpstreamClient::new(config);
        assert!(matches!(
            result,
            Err(ThoughtGateError::InternalError { .. })
        ));
    }

    #[test]
    fn test_upstream_client_invalid_url() {
        let config = UpstreamConfig::with_base_url("not-a-valid-url");
        let result = UpstreamClient::new(config);
        assert!(matches!(
            result,
            Err(ThoughtGateError::InternalError { .. })
        ));
    }

    #[test]
    #[serial]
    fn test_config_from_env_missing_upstream() {
        let _guard = EnvVarGuard::new(&[
            "THOUGHTGATE_UPSTREAM",
            "THOUGHTGATE_REQUEST_TIMEOUT_SECS",
            "THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS",
        ]);

        // SAFETY: Test runs serially via #[serial], env var mutation is isolated
        unsafe {
            std::env::remove_var("THOUGHTGATE_UPSTREAM");
        }

        let result = UpstreamConfig::from_env();
        assert!(matches!(
            result,
            Err(ThoughtGateError::InvalidParams { .. })
        ));
    }

    #[test]
    #[serial]
    fn test_config_from_env_with_upstream() {
        let _guard = EnvVarGuard::new(&[
            "THOUGHTGATE_UPSTREAM",
            "THOUGHTGATE_REQUEST_TIMEOUT_SECS",
            "THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS",
        ]);

        // SAFETY: Test runs serially via #[serial], env var mutation is isolated
        unsafe {
            std::env::set_var("THOUGHTGATE_UPSTREAM", "http://test:3000");
            std::env::set_var("THOUGHTGATE_REQUEST_TIMEOUT_SECS", "60");
            std::env::set_var("THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS", "10");
        }

        let result = UpstreamConfig::from_env();
        assert!(result.is_ok());

        let config = result.expect("should parse config");
        assert_eq!(config.base_url, "http://test:3000");
        assert_eq!(config.timeout, Duration::from_secs(60));
        assert_eq!(config.connect_timeout, Duration::from_secs(10));
        // EnvVarGuard restores original values on drop
    }

    #[test]
    #[serial]
    fn test_config_from_env_invalid_timeout() {
        let _guard = EnvVarGuard::new(&[
            "THOUGHTGATE_UPSTREAM",
            "THOUGHTGATE_REQUEST_TIMEOUT_SECS",
            "THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS",
        ]);

        // SAFETY: Test runs serially via #[serial], env var mutation is isolated
        unsafe {
            std::env::set_var("THOUGHTGATE_UPSTREAM", "http://test:3000");
            std::env::set_var("THOUGHTGATE_REQUEST_TIMEOUT_SECS", "not-a-number");
        }

        let result = UpstreamConfig::from_env();
        assert!(matches!(
            result,
            Err(ThoughtGateError::InvalidParams { .. })
        ));
        if let Err(ThoughtGateError::InvalidParams { details }) = result {
            assert!(details.contains("THOUGHTGATE_REQUEST_TIMEOUT_SECS"));
            assert!(details.contains("not-a-number"));
        }
    }

    #[test]
    #[serial]
    fn test_config_from_env_invalid_connect_timeout() {
        let _guard = EnvVarGuard::new(&[
            "THOUGHTGATE_UPSTREAM",
            "THOUGHTGATE_REQUEST_TIMEOUT_SECS",
            "THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS",
        ]);

        // SAFETY: Test runs serially via #[serial], env var mutation is isolated
        unsafe {
            std::env::set_var("THOUGHTGATE_UPSTREAM", "http://test:3000");
            std::env::set_var("THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS", "invalid");
        }

        let result = UpstreamConfig::from_env();
        assert!(matches!(
            result,
            Err(ThoughtGateError::InvalidParams { .. })
        ));
        if let Err(ThoughtGateError::InvalidParams { details }) = result {
            assert!(details.contains("THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS"));
            assert!(details.contains("invalid"));
        }
    }

    // ========================================================================
    // read_body_limited Tests
    // ========================================================================

    /// Tests that responses within the size limit are read successfully.
    #[tokio::test]
    async fn test_read_body_limited_within_limit() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello"))
            .mount(&mock_server)
            .await;

        let config = UpstreamConfig {
            base_url: mock_server.uri(),
            max_response_size: 1024,
            ..UpstreamConfig::default()
        };
        let client = UpstreamClient::new(config).unwrap();

        let response = client
            .client
            .post(format!("{}/", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let body = client.read_body_limited(response, "test-123").await;
        assert!(body.is_ok());
        assert_eq!(&body.unwrap()[..], b"hello");
    }

    /// Tests that responses exceeding the limit via Content-Length are rejected early.
    #[tokio::test]
    async fn test_read_body_limited_content_length_exceeds() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let large_body = vec![b'x'; 2048];
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(large_body))
            .mount(&mock_server)
            .await;

        let config = UpstreamConfig {
            base_url: mock_server.uri(),
            max_response_size: 1024, // Smaller than body
            ..UpstreamConfig::default()
        };
        let client = UpstreamClient::new(config).unwrap();

        let response = client
            .client
            .post(format!("{}/", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let result = client.read_body_limited(response, "test-456").await;
        assert!(result.is_err());
        match result {
            Err(ThoughtGateError::UpstreamError { message, .. }) => {
                assert!(
                    message.contains("too large"),
                    "Expected 'too large' in: {message}"
                );
            }
            other => panic!("Expected UpstreamError, got: {other:?}"),
        }
    }
}
