//! Centralized configuration for ThoughtGate proxy.
//!
//! # v0.1 Configuration
//!
//! In v0.1, most HTTP proxy configuration is not used since Green Path (streaming)
//! and Amber Path (inspection) are deferred. The configuration is retained for
//! when these features are enabled.
//!
//! Active in v0.1:
//! - `metrics_port` - Prometheus metrics endpoint
//!
//! Deferred to v0.2+:
//! - Green Path config (streaming timeouts, TCP settings)
//! - Amber Path config (buffer limits, inspection timeouts)
//!
//! # Traceability
//! - Deferred: REQ-CORE-001 Section 3.2 (Network Optimization)
//! - Deferred: REQ-CORE-002 Section 3.2 (Memory Management)

use std::time::Duration;
use tracing::warn;

/// Runtime configuration for the ThoughtGate proxy.
///
/// All parameters can be overridden via environment variables.
///
/// # v0.1 Status
///
/// Most configuration is **deferred** since Green Path and Amber Path are not
/// active in v0.1. Configuration is retained for forward compatibility.
///
/// # Traceability
/// - Deferred: REQ-CORE-001 Section 3.2 (Configuration Loading)
/// - Deferred: REQ-CORE-002 Section 3.2 (Memory Management Config)
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    // ─────────────────────────────────────────────────────────────────────────
    // Green Path Configuration - DEFERRED TO v0.2+ (REQ-CORE-001)
    // ─────────────────────────────────────────────────────────────────────────
    /// Enable TCP_NODELAY (Nagle's algorithm disabled)
    pub tcp_nodelay: bool,

    /// TCP keepalive interval in seconds
    pub tcp_keepalive_secs: u64,

    /// Per-chunk read timeout
    pub stream_read_timeout: Duration,

    /// Per-chunk write timeout
    pub stream_write_timeout: Duration,

    /// Total stream timeout (prevents slow-drip attacks)
    pub stream_total_timeout: Duration,

    /// Maximum concurrent streams allowed (Green Path)
    pub max_concurrent_streams: usize,

    /// Socket buffer size (SO_RCVBUF / SO_SNDBUF)
    pub socket_buffer_size: usize,

    // NOTE: metrics_port removed - metrics are now served on the admin port (7469)
    // via the AdminServer module. See src/admin.rs and src/ports.rs.

    // ─────────────────────────────────────────────────────────────────────────
    // Amber Path Configuration - DEFERRED TO v0.2+ (REQ-CORE-002)
    // ─────────────────────────────────────────────────────────────────────────
    /// Maximum concurrent buffered connections (Amber Path).
    /// Prevents OOM attacks by limiting memory-intensive inspections.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 3.2 (Concurrency Limit)
    pub max_concurrent_buffers: usize,

    /// Maximum request body buffer size in bytes (Amber Path).
    /// Requests exceeding this limit receive 413 Payload Too Large.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 3.2 (Memory Management)
    pub req_buffer_max: usize,

    /// Maximum response body buffer size in bytes (Amber Path).
    /// Responses exceeding this limit are rejected with 502 Bad Gateway.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 3.2 (Memory Management)
    pub resp_buffer_max: usize,

    /// Total timeout for the entire Amber Path lifecycle (buffering + inspection).
    /// Operations exceeding this receive 408 Request Timeout.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 3.2 (THOUGHTGATE_BUFFER_TIMEOUT_SECS)
    pub buffer_timeout: Duration,

    /// Maximum number of idle connections per host in the connection pool.
    /// Higher values reduce latency under load by keeping connections warm.
    pub pool_max_idle_per_host: usize,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            // Green Path defaults
            tcp_nodelay: true,
            tcp_keepalive_secs: 60,
            stream_read_timeout: Duration::from_secs(300),
            stream_write_timeout: Duration::from_secs(300),
            stream_total_timeout: Duration::from_secs(3600),
            max_concurrent_streams: 10000,
            socket_buffer_size: 262144, // 256 KB

            // Amber Path defaults (REQ-CORE-002 Section 3.2)
            max_concurrent_buffers: 100,
            req_buffer_max: 2 * 1024 * 1024,   // 2 MB
            resp_buffer_max: 10 * 1024 * 1024, // 10 MB
            buffer_timeout: Duration::from_secs(30),
            pool_max_idle_per_host: 128,
        }
    }
}

impl ProxyConfig {
    /// Load configuration from environment variables with defaults.
    ///
    /// # Environment Variables (Green Path - REQ-CORE-001)
    ///
    /// - `THOUGHTGATE_TCP_NODELAY` (default: true)
    /// - `THOUGHTGATE_TCP_KEEPALIVE_SECS` (default: 60)
    /// - `THOUGHTGATE_STREAM_READ_TIMEOUT_SECS` (default: 300)
    /// - `THOUGHTGATE_STREAM_WRITE_TIMEOUT_SECS` (default: 300)
    /// - `THOUGHTGATE_STREAM_TOTAL_TIMEOUT_SECS` (default: 3600)
    /// - `THOUGHTGATE_MAX_CONCURRENT_STREAMS` (default: 10000)
    /// - `THOUGHTGATE_SOCKET_BUFFER_SIZE` (default: 262144)
    ///
    /// Note: THOUGHTGATE_METRICS_PORT is no longer used. Metrics are served on
    /// the admin port (default: 7469). See THOUGHTGATE_ADMIN_PORT.
    ///
    /// # Environment Variables (Amber Path - REQ-CORE-002)
    ///
    /// - `THOUGHTGATE_MAX_CONCURRENT_BUFFERS` (default: 100)
    /// - `THOUGHTGATE_REQ_BUFFER_MAX` (default: 2097152 = 2MB)
    /// - `THOUGHTGATE_RESP_BUFFER_MAX` (default: 10485760 = 10MB)
    /// - `THOUGHTGATE_BUFFER_TIMEOUT_SECS` (default: 30)
    ///
    /// # Environment Variables (Connection Pool)
    ///
    /// - `THOUGHTGATE_POOL_MAX_IDLE` (default: 128)
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-001 Section 3.2 (Config Loading)
    /// - Implements: REQ-CORE-002 Section 3.2 (Config Loading)
    pub fn from_env() -> Self {
        let default = Self::default();

        Self {
            // Green Path configuration
            tcp_nodelay: parse_env_warn("THOUGHTGATE_TCP_NODELAY", default.tcp_nodelay),

            tcp_keepalive_secs: parse_env_warn(
                "THOUGHTGATE_TCP_KEEPALIVE_SECS",
                default.tcp_keepalive_secs,
            ),

            stream_read_timeout: Duration::from_secs(parse_env_warn(
                "THOUGHTGATE_STREAM_READ_TIMEOUT_SECS",
                default.stream_read_timeout.as_secs(),
            )),

            stream_write_timeout: Duration::from_secs(parse_env_warn(
                "THOUGHTGATE_STREAM_WRITE_TIMEOUT_SECS",
                default.stream_write_timeout.as_secs(),
            )),

            stream_total_timeout: Duration::from_secs(parse_env_warn(
                "THOUGHTGATE_STREAM_TOTAL_TIMEOUT_SECS",
                default.stream_total_timeout.as_secs(),
            )),

            max_concurrent_streams: parse_env_warn(
                "THOUGHTGATE_MAX_CONCURRENT_STREAMS",
                default.max_concurrent_streams,
            ),

            socket_buffer_size: parse_env_warn(
                "THOUGHTGATE_SOCKET_BUFFER_SIZE",
                default.socket_buffer_size,
            ),

            // Amber Path configuration
            max_concurrent_buffers: parse_env_warn(
                "THOUGHTGATE_MAX_CONCURRENT_BUFFERS",
                default.max_concurrent_buffers,
            ),

            req_buffer_max: parse_env_warn("THOUGHTGATE_REQ_BUFFER_MAX", default.req_buffer_max),

            resp_buffer_max: parse_env_warn("THOUGHTGATE_RESP_BUFFER_MAX", default.resp_buffer_max),

            buffer_timeout: Duration::from_secs(parse_env_warn(
                "THOUGHTGATE_BUFFER_TIMEOUT_SECS",
                default.buffer_timeout.as_secs(),
            )),

            // Connection pool configuration
            pool_max_idle_per_host: parse_env_warn(
                "THOUGHTGATE_POOL_MAX_IDLE",
                default.pool_max_idle_per_host,
            ),
        }
    }
}

/// Parse an environment variable with a warning on invalid values.
///
/// If the env var is set but cannot be parsed, logs a warning and returns the default.
/// If the env var is not set, returns the default silently.
fn parse_env_warn<T: std::str::FromStr + std::fmt::Display>(name: &str, default: T) -> T {
    match std::env::var(name) {
        Ok(val) => match val.parse::<T>() {
            Ok(parsed) => parsed,
            Err(_) => {
                warn!(
                    env_var = name,
                    value = %val,
                    default = %default,
                    "Invalid value for environment variable, using default"
                );
                default
            }
        },
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ProxyConfig::default();

        // Green Path defaults
        assert!(config.tcp_nodelay);
        assert_eq!(config.tcp_keepalive_secs, 60);
        assert_eq!(config.max_concurrent_streams, 10000);
        assert_eq!(config.socket_buffer_size, 262144);

        // Amber Path defaults (REQ-CORE-002)
        assert_eq!(config.max_concurrent_buffers, 100);
        assert_eq!(config.req_buffer_max, 2 * 1024 * 1024); // 2 MB
        assert_eq!(config.resp_buffer_max, 10 * 1024 * 1024); // 10 MB
        assert_eq!(config.buffer_timeout, Duration::from_secs(30));

        // Connection pool defaults
        assert_eq!(config.pool_max_idle_per_host, 128);
    }

    #[test]
    #[serial_test::serial]
    fn test_config_env_loading() {
        // Test 1: Default values (explicit construction to avoid env var pollution)
        let default_config = ProxyConfig::default();
        assert_eq!(default_config.max_concurrent_streams, 10000);
        assert!(default_config.tcp_nodelay);
        assert_eq!(default_config.socket_buffer_size, 262144);

        // Test 2: Environment variable override (test in isolation)
        // Note: from_env() may be affected by global env state in parallel tests
        // So we only test the override behavior here
        unsafe {
            std::env::set_var("THOUGHTGATE_MAX_CONCURRENT_STREAMS", "5000");
        }
        let config_with_override = ProxyConfig::from_env();
        assert_eq!(config_with_override.max_concurrent_streams, 5000);
        unsafe {
            std::env::remove_var("THOUGHTGATE_MAX_CONCURRENT_STREAMS");
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_amber_path_env_loading() {
        // Test Amber Path configuration from environment variables
        unsafe {
            std::env::set_var("THOUGHTGATE_MAX_CONCURRENT_BUFFERS", "50");
            std::env::set_var("THOUGHTGATE_REQ_BUFFER_MAX", "1048576"); // 1 MB
            std::env::set_var("THOUGHTGATE_RESP_BUFFER_MAX", "5242880"); // 5 MB
            std::env::set_var("THOUGHTGATE_BUFFER_TIMEOUT_SECS", "60");
        }

        let config = ProxyConfig::from_env();

        assert_eq!(config.max_concurrent_buffers, 50);
        assert_eq!(config.req_buffer_max, 1048576);
        assert_eq!(config.resp_buffer_max, 5242880);
        assert_eq!(config.buffer_timeout, Duration::from_secs(60));

        // Clean up
        unsafe {
            std::env::remove_var("THOUGHTGATE_MAX_CONCURRENT_BUFFERS");
            std::env::remove_var("THOUGHTGATE_REQ_BUFFER_MAX");
            std::env::remove_var("THOUGHTGATE_RESP_BUFFER_MAX");
            std::env::remove_var("THOUGHTGATE_BUFFER_TIMEOUT_SECS");
        }
    }
}
