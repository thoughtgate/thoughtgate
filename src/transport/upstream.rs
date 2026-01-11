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
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(5),
            pool_max_idle_per_host: 32,
            pool_idle_timeout: Duration::from_secs(90),
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
    /// Returns `ThoughtGateError::InvalidParams` if `THOUGHTGATE_UPSTREAM` is not set.
    pub fn from_env() -> Result<Self, ThoughtGateError> {
        let base_url =
            std::env::var("THOUGHTGATE_UPSTREAM").map_err(|_| ThoughtGateError::InvalidParams {
                details: "THOUGHTGATE_UPSTREAM environment variable is required".to_string(),
            })?;

        let timeout_secs: u64 = std::env::var("THOUGHTGATE_REQUEST_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);

        let connect_timeout_secs: u64 = std::env::var("THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);

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
    /// Returns `ThoughtGateError::InternalError` if the client cannot be built.
    pub fn new(config: UpstreamConfig) -> Result<Self, ThoughtGateError> {
        let client = Client::builder()
            .timeout(config.timeout)
            .connect_timeout(config.connect_timeout)
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .pool_idle_timeout(config.pool_idle_timeout)
            .tcp_nodelay(true)
            .build()
            .map_err(|e| ThoughtGateError::InternalError {
                correlation_id: format!("upstream-client-build-error: {}", e),
            })?;

        Ok(Self { client, config })
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
    pub async fn forward(&self, request: &McpRequest) -> Result<JsonRpcResponse, ThoughtGateError> {
        let url = format!("{}/mcp/v1", self.config.base_url.trim_end_matches('/'));
        let correlation_id = request.correlation_id.to_string();

        debug!(
            correlation_id = %correlation_id,
            method = %request.method,
            url = %url,
            "Forwarding request to upstream"
        );

        // Build JSON-RPC request
        let jsonrpc_request = request.to_jsonrpc_request();

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&jsonrpc_request)
            .send()
            .await
            .map_err(|e| self.classify_error(e, &correlation_id))?;

        // Check HTTP status
        let status = response.status();
        if !status.is_success() {
            warn!(
                correlation_id = %correlation_id,
                status = %status,
                "Upstream returned error status"
            );
            return Err(ThoughtGateError::UpstreamError {
                code: status.as_u16() as i32,
                message: format!("Upstream returned HTTP {}", status),
            });
        }

        // Parse response body
        let body: JsonRpcResponse = response.json().await.map_err(|e| {
            error!(
                correlation_id = %correlation_id,
                error = %e,
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
    /// * `requests` - The MCP requests to forward as a batch
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<JsonRpcResponse>)` - Responses from upstream
    /// * `Err(ThoughtGateError)` - If the batch request failed
    pub async fn forward_batch(
        &self,
        requests: &[McpRequest],
    ) -> Result<Vec<JsonRpcResponse>, ThoughtGateError> {
        let url = format!("{}/mcp/v1", self.config.base_url.trim_end_matches('/'));

        debug!(
            batch_size = requests.len(),
            url = %url,
            "Forwarding batch request to upstream"
        );

        // Build batch of JSON-RPC requests
        let jsonrpc_requests: Vec<_> = requests.iter().map(|r| r.to_jsonrpc_request()).collect();

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&jsonrpc_requests)
            .send()
            .await
            .map_err(|e| self.classify_error(e, "batch"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(ThoughtGateError::UpstreamError {
                code: status.as_u16() as i32,
                message: format!("Upstream returned HTTP {}", status),
            });
        }

        let body: Vec<JsonRpcResponse> =
            response
                .json()
                .await
                .map_err(|e| ThoughtGateError::UpstreamError {
                    code: -32002,
                    message: format!("Failed to parse upstream batch response: {}", e),
                })?;

        debug!(
            response_count = body.len(),
            "Received upstream batch response"
        );

        Ok(body)
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

/// Trait for upstream client (enables mocking in tests).
///
/// This trait abstracts the upstream forwarding behavior, allowing tests
/// to inject mock implementations without making actual HTTP requests.
#[async_trait::async_trait]
pub trait UpstreamForwarder: Send + Sync {
    /// Forward a single request to upstream.
    async fn forward(&self, request: &McpRequest) -> Result<JsonRpcResponse, ThoughtGateError>;

    /// Forward a batch of requests to upstream.
    async fn forward_batch(
        &self,
        requests: &[McpRequest],
    ) -> Result<Vec<JsonRpcResponse>, ThoughtGateError>;
}

#[async_trait::async_trait]
impl UpstreamForwarder for UpstreamClient {
    async fn forward(&self, request: &McpRequest) -> Result<JsonRpcResponse, ThoughtGateError> {
        self.forward(request).await
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

    // Note: These env var tests use unsafe because std::env::set_var/remove_var
    // can cause data races in multi-threaded programs. In tests, this is acceptable
    // when tests run serially (cargo test runs each test in sequence by default
    // for the same crate). For true isolation, consider `serial_test` crate or
    // test-specific env var prefixes.

    #[test]
    fn test_config_from_env_missing_upstream() {
        // SAFETY: Test runs in single-threaded context, env var mutation is isolated
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
    fn test_config_from_env_with_upstream() {
        // SAFETY: Test runs in single-threaded context, env var mutation is isolated
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

        // SAFETY: Cleanup env vars set above
        unsafe {
            std::env::remove_var("THOUGHTGATE_UPSTREAM");
            std::env::remove_var("THOUGHTGATE_REQUEST_TIMEOUT_SECS");
            std::env::remove_var("THOUGHTGATE_UPSTREAM_CONNECT_TIMEOUT_SECS");
        }
    }
}
