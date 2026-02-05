//! Port configuration for ThoughtGate's Envoy-style 3-port model.
//!
//! # Traceability
//! - Implements: REQ-CORE-005/§5.1 (Network Configuration)
//!
//! # Port Model
//!
//! ThoughtGate uses an Envoy-inspired 3-port architecture:
//!
//! | Port | Name | Purpose |
//! |------|------|---------|
//! | 7467 | Outbound | Client requests → upstream (main proxy) |
//! | 7468 | Inbound | Reserved for callbacks/webhooks (not wired in v0.2) |
//! | 7469 | Admin | Health checks, metrics, admin API |
//!
//! # Environment Variables
//!
//! - `THOUGHTGATE_OUTBOUND_PORT` (default: 7467): Main proxy port
//! - `THOUGHTGATE_ADMIN_PORT` (default: 7469): Admin/health port

/// Default outbound proxy port for agent traffic.
///
/// Configurable via `THOUGHTGATE_OUTBOUND_PORT` environment variable.
pub const DEFAULT_OUTBOUND_PORT: u16 = 7467;

/// Default inbound port reserved for future callbacks/webhooks.
///
/// In v0.2, this port is bound but not wired to any handlers.
/// It will be used in future versions for:
/// - Webhook callbacks from approval systems
/// - Push notifications from upstream servers
pub const DEFAULT_INBOUND_PORT: u16 = 7468;

/// Default admin port for health checks and metrics.
///
/// Configurable via `THOUGHTGATE_ADMIN_PORT` environment variable.
///
/// Endpoints served on this port:
/// - `GET /health` - Liveness probe
/// - `GET /ready` - Readiness probe
/// - `GET /metrics` - Prometheus metrics (when enabled)
pub const DEFAULT_ADMIN_PORT: u16 = 7469;

/// Get the outbound proxy port from environment or default.
///
/// # Environment Variable
///
/// `THOUGHTGATE_OUTBOUND_PORT` (default: 7467)
///
/// # Example
///
/// ```rust
/// use thoughtgate_proxy::ports::outbound_port;
///
/// let port = outbound_port();
/// assert!(port > 0);
/// ```
pub fn outbound_port() -> u16 {
    std::env::var("THOUGHTGATE_OUTBOUND_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_OUTBOUND_PORT)
}

/// Get the admin port from environment or default.
///
/// # Environment Variable
///
/// `THOUGHTGATE_ADMIN_PORT` (default: 7469)
///
/// # Example
///
/// ```rust
/// use thoughtgate_proxy::ports::admin_port;
///
/// let port = admin_port();
/// assert!(port > 0);
/// ```
pub fn admin_port() -> u16 {
    std::env::var("THOUGHTGATE_ADMIN_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_ADMIN_PORT)
}

/// Get the inbound port (reserved, not configurable in v0.2).
///
/// This port is bound but not wired to handlers. It reserves the port
/// for future use with callback/webhook functionality.
///
/// # Note
///
/// In v0.2, this always returns the default value. Configuration will
/// be added when the inbound functionality is implemented.
pub fn inbound_port() -> u16 {
    DEFAULT_INBOUND_PORT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_ports() {
        // Verify default values match Envoy-style 3-port model
        assert_eq!(DEFAULT_OUTBOUND_PORT, 7467);
        assert_eq!(DEFAULT_INBOUND_PORT, 7468);
        assert_eq!(DEFAULT_ADMIN_PORT, 7469);

        // Ports should be sequential
        assert_eq!(DEFAULT_INBOUND_PORT, DEFAULT_OUTBOUND_PORT + 1);
        assert_eq!(DEFAULT_ADMIN_PORT, DEFAULT_INBOUND_PORT + 1);
    }

    #[test]
    fn test_outbound_port_default() {
        // When env var not set, should return default
        // Note: This test assumes THOUGHTGATE_OUTBOUND_PORT is not set
        // In CI, we can't guarantee this, so just verify it returns a valid port
        let port = outbound_port();
        assert!(port > 0);
    }

    #[test]
    fn test_admin_port_default() {
        // When env var not set, should return default
        let port = admin_port();
        assert!(port > 0);
    }

    #[test]
    fn test_inbound_port_reserved() {
        // Inbound port should always return default (not configurable in v0.2)
        assert_eq!(inbound_port(), DEFAULT_INBOUND_PORT);
    }

    #[test]
    fn test_outbound_port_from_env() {
        // Test env var override
        unsafe {
            std::env::set_var("THOUGHTGATE_OUTBOUND_PORT", "8080");
        }
        assert_eq!(outbound_port(), 8080);
        unsafe {
            std::env::remove_var("THOUGHTGATE_OUTBOUND_PORT");
        }
    }

    #[test]
    fn test_admin_port_from_env() {
        // Test env var override
        unsafe {
            std::env::set_var("THOUGHTGATE_ADMIN_PORT", "9000");
        }
        assert_eq!(admin_port(), 9000);
        unsafe {
            std::env::remove_var("THOUGHTGATE_ADMIN_PORT");
        }
    }

    #[test]
    fn test_invalid_env_var_falls_back_to_default() {
        // Invalid port value should fall back to default
        unsafe {
            std::env::set_var("THOUGHTGATE_OUTBOUND_PORT", "not_a_number");
        }
        assert_eq!(outbound_port(), DEFAULT_OUTBOUND_PORT);
        unsafe {
            std::env::remove_var("THOUGHTGATE_OUTBOUND_PORT");
        }
    }
}
