//! Centralized default values for ThoughtGate configuration.
//!
//! Implements: REQ-CFG-001 Section 5.6 (Centralized Default Values)
//!
//! # Traceability
//! - Implements: REQ-CFG-001/5.6

use std::time::Duration;
use tracing::warn;

/// Centralized default values for ThoughtGate configuration.
///
/// These defaults are used when explicit configuration is not provided.
/// All timing-related values should reference this struct to ensure consistency.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 5.6 (Centralized Default Values)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThoughtGateDefaults {
    /// Maximum time to wait for upstream tool execution (REQ-GOV-002).
    pub execution_timeout: Duration,

    /// Maximum time for graceful shutdown (REQ-CORE-005).
    pub shutdown_timeout: Duration,

    /// Time to wait for in-flight requests to complete during shutdown (REQ-CORE-005).
    /// Must be less than shutdown_timeout to allow cleanup.
    pub drain_timeout: Duration,

    /// Interval between Slack API polls for approval decisions (REQ-GOV-003).
    pub approval_poll_interval: Duration,

    /// Maximum interval between Slack polls after backoff (REQ-GOV-003).
    pub approval_poll_max_interval: Duration,

    /// Interval for health check probes (REQ-CORE-005).
    pub health_check_interval: Duration,

    /// Default approval workflow timeout if not specified in config.
    pub default_approval_timeout: Duration,

    /// Default task TTL for SEP-1686 tasks (REQ-GOV-001).
    pub default_task_ttl: Duration,

    /// Maximum task TTL allowed (REQ-GOV-001).
    pub max_task_ttl: Duration,

    /// Interval for expired task cleanup (REQ-GOV-001).
    pub task_cleanup_interval: Duration,
}

impl Default for ThoughtGateDefaults {
    fn default() -> Self {
        Self {
            execution_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(30),
            drain_timeout: Duration::from_secs(25), // Must be < shutdown_timeout
            approval_poll_interval: Duration::from_secs(5),
            approval_poll_max_interval: Duration::from_secs(30),
            health_check_interval: Duration::from_secs(10),
            default_approval_timeout: Duration::from_secs(600), // 10 minutes
            default_task_ttl: Duration::from_secs(600),         // 10 minutes
            max_task_ttl: Duration::from_secs(86400),           // 24 hours
            task_cleanup_interval: Duration::from_secs(60),
        }
    }
}

impl ThoughtGateDefaults {
    /// Create defaults from environment variables.
    ///
    /// # Environment Variables
    /// - `THOUGHTGATE_EXECUTION_TIMEOUT_SECS`
    /// - `THOUGHTGATE_SHUTDOWN_TIMEOUT_SECS`
    /// - `THOUGHTGATE_DRAIN_TIMEOUT_SECS`
    /// - `THOUGHTGATE_APPROVAL_POLL_INTERVAL_SECS`
    /// - `THOUGHTGATE_APPROVAL_POLL_MAX_INTERVAL_SECS`
    /// - `THOUGHTGATE_HEALTH_CHECK_INTERVAL_SECS`
    pub fn from_env() -> Self {
        let default = Self::default();

        Self {
            execution_timeout: Duration::from_secs(parse_env_warn(
                "THOUGHTGATE_EXECUTION_TIMEOUT_SECS",
                default.execution_timeout.as_secs(),
            )),

            shutdown_timeout: Duration::from_secs(parse_env_warn(
                "THOUGHTGATE_SHUTDOWN_TIMEOUT_SECS",
                default.shutdown_timeout.as_secs(),
            )),

            drain_timeout: Duration::from_secs(parse_env_warn(
                "THOUGHTGATE_DRAIN_TIMEOUT_SECS",
                default.drain_timeout.as_secs(),
            )),

            approval_poll_interval: Duration::from_secs(parse_env_warn(
                "THOUGHTGATE_APPROVAL_POLL_INTERVAL_SECS",
                default.approval_poll_interval.as_secs(),
            )),

            approval_poll_max_interval: Duration::from_secs(parse_env_warn(
                "THOUGHTGATE_APPROVAL_POLL_MAX_INTERVAL_SECS",
                default.approval_poll_max_interval.as_secs(),
            )),

            health_check_interval: Duration::from_secs(parse_env_warn(
                "THOUGHTGATE_HEALTH_CHECK_INTERVAL_SECS",
                default.health_check_interval.as_secs(),
            )),

            // These are not typically overridden via env var
            default_approval_timeout: default.default_approval_timeout,
            default_task_ttl: default.default_task_ttl,
            max_task_ttl: default.max_task_ttl,
            task_cleanup_interval: default.task_cleanup_interval,
        }
    }

    /// Validate the defaults satisfy invariants.
    ///
    /// # Invariants
    /// 1. `drain_timeout` < `shutdown_timeout`
    /// 2. `approval_poll_interval` < `approval_poll_max_interval`
    /// 3. `default_task_ttl` <= `max_task_ttl`
    pub fn validate(&self) -> Result<(), String> {
        // Invariant 1: drain_timeout < shutdown_timeout
        if self.drain_timeout >= self.shutdown_timeout {
            return Err(format!(
                "drain_timeout ({:?}) must be less than shutdown_timeout ({:?})",
                self.drain_timeout, self.shutdown_timeout
            ));
        }

        // Invariant 2: approval_poll_interval < approval_poll_max_interval
        if self.approval_poll_interval >= self.approval_poll_max_interval {
            return Err(format!(
                "approval_poll_interval ({:?}) must be less than approval_poll_max_interval ({:?})",
                self.approval_poll_interval, self.approval_poll_max_interval
            ));
        }

        // Invariant 3: default_task_ttl <= max_task_ttl
        if self.default_task_ttl > self.max_task_ttl {
            return Err(format!(
                "default_task_ttl ({:?}) must be <= max_task_ttl ({:?})",
                self.default_task_ttl, self.max_task_ttl
            ));
        }

        Ok(())
    }
}

/// Parse an environment variable with a warning on invalid values.
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
    fn test_defaults() {
        let defaults = ThoughtGateDefaults::default();

        assert_eq!(defaults.execution_timeout, Duration::from_secs(30));
        assert_eq!(defaults.shutdown_timeout, Duration::from_secs(30));
        assert_eq!(defaults.drain_timeout, Duration::from_secs(25));
        assert_eq!(defaults.approval_poll_interval, Duration::from_secs(5));
        assert_eq!(defaults.approval_poll_max_interval, Duration::from_secs(30));
        assert_eq!(defaults.health_check_interval, Duration::from_secs(10));
        assert_eq!(defaults.default_approval_timeout, Duration::from_secs(600));
        assert_eq!(defaults.default_task_ttl, Duration::from_secs(600));
        assert_eq!(defaults.max_task_ttl, Duration::from_secs(86400));
        assert_eq!(defaults.task_cleanup_interval, Duration::from_secs(60));
    }

    #[test]
    fn test_defaults_validate_ok() {
        let defaults = ThoughtGateDefaults::default();
        assert!(defaults.validate().is_ok());
    }

    #[test]
    fn test_defaults_validate_drain_timeout() {
        let defaults = ThoughtGateDefaults {
            drain_timeout: Duration::from_secs(35), // > shutdown_timeout
            ..Default::default()
        };
        assert!(defaults.validate().is_err());
    }

    #[test]
    fn test_defaults_validate_poll_interval() {
        let defaults = ThoughtGateDefaults {
            approval_poll_interval: Duration::from_secs(60), // > max
            ..Default::default()
        };
        assert!(defaults.validate().is_err());
    }

    #[test]
    fn test_defaults_validate_task_ttl() {
        let defaults = ThoughtGateDefaults {
            default_task_ttl: Duration::from_secs(100000), // > max
            ..Default::default()
        };
        assert!(defaults.validate().is_err());
    }

    #[test]
    fn test_from_env() {
        // Note: This test may be affected by environment variables
        // In a real test suite, we'd use temp_env or similar
        let defaults = ThoughtGateDefaults::from_env();
        // Just verify it doesn't panic and returns something reasonable
        assert!(defaults.execution_timeout.as_secs() > 0);
    }
}
