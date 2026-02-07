//! MCP handler configuration.
//!
//! Implements: REQ-CORE-003/§5.3 (Configuration)

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
    /// Global fallback timeout (seconds) for blocking approval mode.
    /// Used when no workflow-specific blocking_timeout is configured.
    pub blocking_approval_timeout_secs: u64,
}

impl Default for McpHandlerConfig {
    fn default() -> Self {
        Self {
            max_body_size: 1024 * 1024, // 1MB
            max_concurrent_requests: 10000,
            max_batch_size: 100,
            max_aggregate_buffer: 512 * 1024 * 1024, // 512MB
            blocking_approval_timeout_secs: 300,
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

        let blocking_approval_timeout_secs: u64 =
            std::env::var("THOUGHTGATE_BLOCKING_APPROVAL_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300);

        Self {
            max_body_size,
            max_concurrent_requests,
            max_batch_size,
            max_aggregate_buffer,
            blocking_approval_timeout_secs,
        }
    }
}
