//! Operational lifecycle management for ThoughtGate.
//!
//! Implements: REQ-CORE-005 (Operational Lifecycle)
//!
//! This module provides lifecycle management including:
//! - Startup sequencing with phase tracking
//! - Health and readiness probes for Kubernetes
//! - Graceful shutdown with request draining
//! - Upstream health monitoring
//!
//! ## Lifecycle States
//!
//! ```text
//! Starting → Ready → ShuttingDown → Stopped
//! ```
//!
//! - **Starting**: Initialization in progress
//! - **Ready**: Accepting traffic
//! - **ShuttingDown**: Draining, rejecting new requests
//! - **Stopped**: Shutdown complete

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use arc_swap::{ArcSwap, ArcSwapOption};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub mod health;

pub use health::{HealthResponse, ReadinessChecks, ReadinessResponse, health_router};

// ============================================================================
// Lifecycle State
// ============================================================================

/// Lifecycle state machine.
///
/// Implements: REQ-CORE-005/§6.3
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    /// Initialization in progress
    Starting,
    /// Accepting traffic
    Ready,
    /// Draining, rejecting new requests
    ShuttingDown,
    /// Shutdown complete
    Stopped,
}

impl std::fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Ready => write!(f, "ready"),
            Self::ShuttingDown => write!(f, "shutting_down"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for lifecycle management.
///
/// Implements: REQ-CORE-005/§5.3
#[derive(Debug, Clone)]
pub struct LifecycleConfig {
    /// Overall shutdown timeout (default: 30s)
    pub shutdown_timeout: Duration,
    /// Connection drain timeout (default: 25s, must be < shutdown_timeout)
    pub drain_timeout: Duration,
    /// Startup timeout (default: 15s)
    pub startup_timeout: Duration,
    /// Require upstream connectivity at startup (default: false)
    pub require_upstream_at_startup: bool,
    /// Upstream health check interval (default: 30s)
    pub upstream_health_interval: Duration,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            shutdown_timeout: Duration::from_secs(30),
            drain_timeout: Duration::from_secs(25),
            startup_timeout: Duration::from_secs(15),
            require_upstream_at_startup: false,
            upstream_health_interval: Duration::from_secs(30),
        }
    }
}

impl LifecycleConfig {
    /// Load from environment variables.
    ///
    /// Implements: REQ-CORE-005/§5.3
    ///
    /// # Environment Variables
    ///
    /// - `THOUGHTGATE_SHUTDOWN_TIMEOUT_SECS` (default: 30)
    /// - `THOUGHTGATE_DRAIN_TIMEOUT_SECS` (default: 25)
    /// - `THOUGHTGATE_STARTUP_TIMEOUT_SECS` (default: 15)
    /// - `THOUGHTGATE_REQUIRE_UPSTREAM_AT_STARTUP` (default: false)
    /// - `THOUGHTGATE_UPSTREAM_HEALTH_INTERVAL_SECS` (default: 30)
    #[must_use]
    pub fn from_env() -> Self {
        let default = Self::default();

        let shutdown_timeout = parse_duration_env(
            "THOUGHTGATE_SHUTDOWN_TIMEOUT_SECS",
            default.shutdown_timeout,
        );

        let drain_timeout =
            parse_duration_env("THOUGHTGATE_DRAIN_TIMEOUT_SECS", default.drain_timeout);

        let startup_timeout =
            parse_duration_env("THOUGHTGATE_STARTUP_TIMEOUT_SECS", default.startup_timeout);

        let require_upstream_at_startup = std::env::var("THOUGHTGATE_REQUIRE_UPSTREAM_AT_STARTUP")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true") || s == "1")
            .unwrap_or(default.require_upstream_at_startup);

        let upstream_health_interval = parse_duration_env(
            "THOUGHTGATE_UPSTREAM_HEALTH_INTERVAL_SECS",
            default.upstream_health_interval,
        );

        // Validate drain_timeout < shutdown_timeout as documented
        // Reserve at least 1 second for post-drain cleanup
        // Also enforce a minimum drain of 1 second
        const MIN_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
        const MIN_POST_DRAIN_BUFFER: Duration = Duration::from_secs(1);

        // Calculate maximum allowed drain_timeout (must leave room for post-drain cleanup)
        let max_drain = Duration::from_secs(
            shutdown_timeout
                .as_secs()
                .saturating_sub(MIN_POST_DRAIN_BUFFER.as_secs()),
        );

        let drain_timeout = if drain_timeout >= shutdown_timeout {
            // drain_timeout >= shutdown_timeout: use 80% of shutdown or max_drain, whichever is smaller
            let adjusted = Duration::from_secs((shutdown_timeout.as_secs() * 4) / 5);
            let adjusted = adjusted.min(max_drain).max(MIN_DRAIN_TIMEOUT);
            warn!(
                drain_timeout_secs = drain_timeout.as_secs(),
                shutdown_timeout_secs = shutdown_timeout.as_secs(),
                adjusted_drain_secs = adjusted.as_secs(),
                "drain_timeout must be less than shutdown_timeout, adjusting"
            );
            adjusted
        } else if drain_timeout > max_drain {
            // drain_timeout valid but doesn't leave enough buffer
            warn!(
                drain_timeout_secs = drain_timeout.as_secs(),
                shutdown_timeout_secs = shutdown_timeout.as_secs(),
                adjusted_drain_secs = max_drain.as_secs(),
                "drain_timeout too close to shutdown_timeout, adjusting to leave cleanup buffer"
            );
            max_drain.max(MIN_DRAIN_TIMEOUT)
        } else if drain_timeout < MIN_DRAIN_TIMEOUT {
            warn!(
                drain_timeout_secs = drain_timeout.as_secs(),
                min_drain_secs = MIN_DRAIN_TIMEOUT.as_secs(),
                "drain_timeout below minimum, adjusting"
            );
            MIN_DRAIN_TIMEOUT
        } else {
            drain_timeout
        };

        Self {
            shutdown_timeout,
            drain_timeout,
            startup_timeout,
            require_upstream_at_startup,
            upstream_health_interval,
        }
    }
}

/// Parse a duration environment variable with warning on invalid values.
fn parse_duration_env(var_name: &str, default: Duration) -> Duration {
    match std::env::var(var_name) {
        Ok(value) => match value.parse::<u64>() {
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => {
                warn!(
                    var = var_name,
                    value = %value,
                    default_secs = default.as_secs(),
                    "Invalid value for environment variable, using default"
                );
                default
            }
        },
        Err(_) => default,
    }
}

// ============================================================================
// Upstream Health Status
// ============================================================================

/// Cached upstream health status.
///
/// Implements: REQ-CORE-005/F-007.2
#[derive(Debug, Clone)]
pub struct UpstreamHealthStatus {
    /// Whether upstream is currently healthy
    pub is_healthy: bool,
    /// Time of last health check
    pub last_check: Instant,
    /// Last error message (if unhealthy)
    pub last_error: Option<String>,
}

impl Default for UpstreamHealthStatus {
    fn default() -> Self {
        Self {
            is_healthy: false,
            last_check: Instant::now(),
            last_error: Some("Not checked yet".to_string()),
        }
    }
}

// ============================================================================
// Lifecycle Manager
// ============================================================================

/// The lifecycle manager coordinates startup, health, and shutdown.
///
/// Implements: REQ-CORE-005
///
/// This is the central coordination point for operational lifecycle. It:
/// - Tracks lifecycle state (Starting → Ready → ShuttingDown → Stopped)
/// - Manages request counting for graceful draining
/// - Caches upstream health status for readiness probes
/// - Provides shutdown coordination via CancellationToken
///
/// # Thread Safety
///
/// The manager is designed for concurrent access from multiple tasks.
/// All state is managed via atomic operations or lock-free structures.
pub struct LifecycleManager {
    /// Current lifecycle state
    state: ArcSwap<LifecycleState>,

    /// When the service started
    started_at: Instant,

    /// Shutdown cancellation token (shared with background tasks)
    shutdown_token: CancellationToken,

    /// Active request counter (for draining)
    active_requests: AtomicUsize,

    /// Cached upstream health status
    upstream_health: ArcSwap<UpstreamHealthStatus>,

    /// Whether configuration is loaded and validated
    config_loaded: AtomicBool,

    /// Whether approval store is initialized
    approval_store_initialized: AtomicBool,

    /// Configuration
    config: LifecycleConfig,

    /// Version string (from Cargo.toml)
    version: &'static str,

    /// Optional Prometheus metrics (wired post-construction via set_metrics)
    tg_metrics: ArcSwapOption<crate::telemetry::ThoughtGateMetrics>,
}

impl LifecycleManager {
    /// Creates a new lifecycle manager.
    ///
    /// Implements: REQ-CORE-005/F-001
    ///
    /// The manager starts in the `Starting` state.
    #[must_use]
    pub fn new(config: LifecycleConfig) -> Self {
        Self {
            state: ArcSwap::new(Arc::new(LifecycleState::Starting)),
            started_at: Instant::now(),
            shutdown_token: CancellationToken::new(),
            active_requests: AtomicUsize::new(0),
            upstream_health: ArcSwap::new(Arc::new(UpstreamHealthStatus::default())),
            config_loaded: AtomicBool::new(false),
            approval_store_initialized: AtomicBool::new(false),
            config,
            version: env!("CARGO_PKG_VERSION"),
            tg_metrics: ArcSwapOption::empty(),
        }
    }

    /// Wire Prometheus metrics for upstream health tracking.
    ///
    /// Call after construction since LifecycleManager is typically wrapped in Arc.
    ///
    /// Implements: REQ-CORE-005/NFR-001
    pub fn set_metrics(&self, metrics: Arc<crate::telemetry::ThoughtGateMetrics>) {
        self.tg_metrics.store(Some(metrics));
    }

    /// Returns the current lifecycle state.
    #[must_use]
    pub fn state(&self) -> LifecycleState {
        **self.state.load()
    }

    /// Returns true if the service is ready to accept traffic.
    ///
    /// Implements: REQ-CORE-005/F-003.1
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self.state(), LifecycleState::Ready)
    }

    /// Returns true if the service is shutting down or stopped.
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        matches!(
            self.state(),
            LifecycleState::ShuttingDown | LifecycleState::Stopped
        )
    }

    /// Transition to Ready state.
    ///
    /// Implements: REQ-CORE-005/F-001
    pub fn mark_ready(&self) {
        self.state.store(Arc::new(LifecycleState::Ready));
        info!(
            version = %self.version,
            startup_duration_ms = self.started_at.elapsed().as_millis(),
            "ThoughtGate ready"
        );
    }

    /// Mark configuration as loaded and validated.
    ///
    /// Implements: REQ-CORE-005/F-003.3
    pub fn mark_config_loaded(&self) {
        self.config_loaded.store(true, Ordering::SeqCst);
    }

    /// Mark approval store as initialized.
    ///
    /// Implements: REQ-CORE-005/F-003.5
    pub fn mark_approval_store_initialized(&self) {
        self.approval_store_initialized
            .store(true, Ordering::SeqCst);
    }

    /// Update cached upstream health status.
    ///
    /// Implements: REQ-CORE-005/F-007.2
    pub fn update_upstream_health(&self, is_healthy: bool, error: Option<String>) {
        self.upstream_health.store(Arc::new(UpstreamHealthStatus {
            is_healthy,
            last_check: Instant::now(),
            last_error: error,
        }));

        // Update Prometheus gauge if metrics are wired
        if let Some(metrics) = self.tg_metrics.load().as_ref() {
            metrics.set_upstream_health(is_healthy);
        }
    }

    /// Returns a clone of the shutdown token.
    ///
    /// Use this to coordinate shutdown with background tasks.
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }

    /// Begin graceful shutdown.
    ///
    /// Implements: REQ-CORE-005/F-004.1
    ///
    /// This:
    /// 1. Sets state to ShuttingDown
    /// 2. Cancels the shutdown token (signals background tasks)
    /// 3. Logs the shutdown with active request count
    pub fn begin_shutdown(&self) {
        self.state.store(Arc::new(LifecycleState::ShuttingDown));
        self.shutdown_token.cancel();
        info!(
            active_requests = self.active_requests.load(Ordering::SeqCst),
            "Shutdown initiated"
        );
    }

    /// Track an active request (returns RAII guard).
    ///
    /// Implements: REQ-CORE-005/F-005
    ///
    /// Returns `None` if the service is shutting down (new requests rejected).
    /// The returned guard automatically decrements the counter when dropped.
    ///
    /// # Edge Cases
    ///
    /// - EC-OPS-006: Requests during shutdown return None
    #[must_use]
    pub fn track_request(self: &Arc<Self>) -> Option<RequestGuard> {
        if self.is_shutting_down() {
            return None; // Reject new requests during shutdown
        }
        self.active_requests.fetch_add(1, Ordering::SeqCst);
        Some(RequestGuard {
            manager: Arc::clone(self),
        })
    }

    /// Returns the current active request count.
    #[must_use]
    pub fn active_request_count(&self) -> usize {
        self.active_requests.load(Ordering::SeqCst)
    }

    /// Returns uptime in seconds.
    ///
    /// Implements: REQ-CORE-005/F-002.3
    #[must_use]
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Returns the version string.
    #[must_use]
    pub fn version(&self) -> &'static str {
        self.version
    }

    /// Returns the configuration.
    #[must_use]
    pub fn config(&self) -> &LifecycleConfig {
        &self.config
    }

    /// Get readiness checks status.
    ///
    /// Implements: REQ-CORE-005/F-003
    #[must_use]
    pub fn readiness_checks(&self) -> ReadinessChecks {
        let upstream_health = self.upstream_health.load();
        ReadinessChecks {
            config_loaded: self.config_loaded.load(Ordering::SeqCst),
            upstream_reachable: upstream_health.is_healthy,
            approval_store_initialized: self.approval_store_initialized.load(Ordering::SeqCst),
        }
    }

    /// Drain active requests with timeout.
    ///
    /// Implements: REQ-CORE-005/F-005
    ///
    /// Waits for all active requests to complete, polling every 100ms.
    /// Returns `DrainResult::Complete` if all requests finish, or
    /// `DrainResult::Timeout` if the drain timeout is exceeded.
    ///
    /// # Edge Cases
    ///
    /// - EC-OPS-007: Drain completes → returns `DrainResult::Complete`
    /// - EC-OPS-008: Drain timeout → returns `DrainResult::Timeout`
    pub async fn drain_requests(&self) -> DrainResult {
        let deadline = Instant::now() + self.config.drain_timeout;
        let mut last_log = Instant::now();

        loop {
            let active = self.active_requests.load(Ordering::SeqCst);

            if active == 0 {
                return DrainResult::Complete;
            }

            if Instant::now() > deadline {
                warn!(
                    active_requests = active,
                    "Drain timeout exceeded, forcing shutdown"
                );
                return DrainResult::Timeout { remaining: active };
            }

            // Log every 5 seconds
            if last_log.elapsed() >= Duration::from_secs(5) {
                info!(active_requests = active, "Draining requests...");
                last_log = Instant::now();
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Mark as stopped.
    ///
    /// Implements: REQ-CORE-005/F-004
    pub fn mark_stopped(&self) {
        self.state.store(Arc::new(LifecycleState::Stopped));
    }

    /// Spawns a background task that periodically checks upstream health.
    ///
    /// Implements: REQ-CORE-005/F-008
    ///
    /// The task runs at `upstream_health_interval` (default 30s) and updates
    /// the cached upstream health status. It stops when the shutdown token
    /// is cancelled.
    ///
    /// # Arguments
    ///
    /// * `upstream_url` - The upstream URL to check (e.g., "http://backend:8080")
    /// * `client` - A pre-configured reqwest client with appropriate timeouts
    ///
    /// # Returns
    ///
    /// A JoinHandle for the spawned task. The task will run until shutdown.
    pub fn spawn_upstream_health_checker(
        self: &Arc<Self>,
        upstream_url: String,
        client: reqwest::Client,
    ) -> tokio::task::JoinHandle<()> {
        let lifecycle = Arc::clone(self);
        let shutdown_token = self.shutdown_token.clone();
        let interval_duration = self.config.upstream_health_interval;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(interval_duration);
            // Don't check immediately on first tick (give service time to start)
            interval.tick().await;

            loop {
                tokio::select! {
                    _ = shutdown_token.cancelled() => {
                        tracing::debug!("Upstream health checker stopping due to shutdown");
                        break;
                    }
                    _ = interval.tick() => {
                        let result = client
                            .head(&upstream_url)
                            .send()
                            .await;

                        match result {
                            Ok(resp) if resp.status().is_server_error() => {
                                // Only 5xx indicates an unhealthy upstream.
                                let error_msg = format!("HTTP {}", resp.status());
                                lifecycle.update_upstream_health(false, Some(error_msg.clone()));
                                tracing::warn!(
                                    upstream = %upstream_url,
                                    status = %resp.status(),
                                    "Upstream health check failed: server error"
                                );
                            }
                            Ok(resp) => {
                                // Any non-5xx HTTP response means the upstream is reachable.
                                // 4xx (404, 405) is expected — MCP servers may not handle HEAD.
                                lifecycle.update_upstream_health(true, None);
                                tracing::debug!(
                                    upstream = %upstream_url,
                                    status = %resp.status(),
                                    "Upstream health check passed (server reachable)"
                                );
                            }
                            Err(e) => {
                                let error_msg = e.to_string();
                                lifecycle.update_upstream_health(false, Some(error_msg.clone()));
                                tracing::warn!(
                                    upstream = %upstream_url,
                                    error = %e,
                                    "Upstream health check failed: connection error"
                                );
                            }
                        }
                    }
                }
            }
        })
    }
}

// ============================================================================
// Environment Validation
// ============================================================================

/// Validate environment configuration for production safety.
///
/// Implements: REQ-CORE-005/F-001 (Startup Validation)
///
/// Checks that dangerous development-only configurations are not active in
/// production environments. Defaults to production when `THOUGHTGATE_ENVIRONMENT`
/// is unset (fail-safe behavior).
///
/// # Environment Variables
///
/// - `THOUGHTGATE_ENVIRONMENT` (default: "production"): Must be set to
///   "development" or "test" to allow dev mode or mock adapter.
/// - `THOUGHTGATE_DEV_MODE`: Rejected if set to "true" in production.
/// - `THOUGHTGATE_APPROVAL_ADAPTER`: Rejected if set to "mock" in production.
///
/// # Errors
///
/// Returns a descriptive error string if unsafe configuration is detected.
pub fn validate_environment() -> Result<(), String> {
    let environment =
        std::env::var("THOUGHTGATE_ENVIRONMENT").unwrap_or_else(|_| "production".to_string());

    let is_production = matches!(environment.as_str(), "production" | "prod" | "");

    let dev_mode = std::env::var("THOUGHTGATE_DEV_MODE").as_deref() == Ok("true");
    let mock_adapter = std::env::var("THOUGHTGATE_APPROVAL_ADAPTER").as_deref() == Ok("mock");

    if dev_mode {
        if is_production {
            return Err("THOUGHTGATE_DEV_MODE=true is not allowed in production. \
                 Set THOUGHTGATE_ENVIRONMENT=development to use dev mode."
                .to_string());
        }
        warn!(
            environment = %environment,
            "Dev mode is active — identity inference uses synthetic principal"
        );
    }

    if mock_adapter {
        if is_production {
            return Err(
                "THOUGHTGATE_APPROVAL_ADAPTER=mock is not allowed in production. \
                 Set THOUGHTGATE_ENVIRONMENT=development to use the mock adapter."
                    .to_string(),
            );
        }
        warn!(
            environment = %environment,
            "Mock approval adapter is active — approvals will be auto-decided"
        );
    }

    if is_production {
        info!("Production environment validated — no dev bypasses active");
    } else {
        info!(
            environment = %environment,
            "Non-production environment — dev features may be enabled"
        );
    }

    Ok(())
}

// ============================================================================
// Request Guard
// ============================================================================

/// RAII guard for request tracking.
///
/// Implements: REQ-CORE-005/F-005
///
/// When this guard is dropped, the active request counter is decremented.
/// This ensures proper counting even if the request handler panics.
///
/// The guard holds an Arc reference to the LifecycleManager, allowing it
/// to be moved into spawned tasks.
pub struct RequestGuard {
    manager: Arc<LifecycleManager>,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.manager.active_requests.fetch_sub(1, Ordering::SeqCst);
    }
}

// ============================================================================
// Drain Result
// ============================================================================

/// Result of draining requests.
///
/// Implements: REQ-CORE-005/F-005
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainResult {
    /// All requests completed before timeout
    Complete,
    /// Timeout reached with remaining requests
    Timeout {
        /// Number of requests still active
        remaining: usize,
    },
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Test lifecycle state transitions.
    ///
    /// Verifies: REQ-CORE-005/§6.3
    #[test]
    fn test_lifecycle_state_transitions() {
        let lifecycle = LifecycleManager::new(LifecycleConfig::default());
        assert_eq!(lifecycle.state(), LifecycleState::Starting);
        assert!(!lifecycle.is_ready());
        assert!(!lifecycle.is_shutting_down());

        lifecycle.mark_ready();
        assert_eq!(lifecycle.state(), LifecycleState::Ready);
        assert!(lifecycle.is_ready());
        assert!(!lifecycle.is_shutting_down());

        lifecycle.begin_shutdown();
        assert_eq!(lifecycle.state(), LifecycleState::ShuttingDown);
        assert!(!lifecycle.is_ready());
        assert!(lifecycle.is_shutting_down());

        lifecycle.mark_stopped();
        assert_eq!(lifecycle.state(), LifecycleState::Stopped);
        assert!(!lifecycle.is_ready());
        assert!(lifecycle.is_shutting_down());
    }

    /// Test request tracking during normal operation.
    #[test]
    fn test_request_tracking() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_ready();

        assert_eq!(lifecycle.active_request_count(), 0);

        {
            let guard = lifecycle.track_request();
            assert!(guard.is_some());
            assert_eq!(lifecycle.active_request_count(), 1);
        }

        // Guard dropped, count should be back to 0
        assert_eq!(lifecycle.active_request_count(), 0);
    }

    /// Test request tracking rejects during shutdown.
    ///
    /// Verifies: EC-OPS-006
    #[test]
    fn test_request_tracking_rejects_during_shutdown() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_ready();

        // Can track before shutdown
        let guard = lifecycle.track_request();
        assert!(guard.is_some());
        assert_eq!(lifecycle.active_request_count(), 1);
        drop(guard);
        assert_eq!(lifecycle.active_request_count(), 0);

        // Cannot track after shutdown
        lifecycle.begin_shutdown();
        let guard = lifecycle.track_request();
        assert!(guard.is_none());
        assert_eq!(lifecycle.active_request_count(), 0);
    }

    /// Test multiple concurrent requests.
    #[test]
    fn test_multiple_requests() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_ready();

        let _guard1 = lifecycle.track_request();
        assert_eq!(lifecycle.active_request_count(), 1);

        let _guard2 = lifecycle.track_request();
        assert_eq!(lifecycle.active_request_count(), 2);

        let _guard3 = lifecycle.track_request();
        assert_eq!(lifecycle.active_request_count(), 3);

        drop(_guard1);
        assert_eq!(lifecycle.active_request_count(), 2);

        drop(_guard2);
        assert_eq!(lifecycle.active_request_count(), 1);

        drop(_guard3);
        assert_eq!(lifecycle.active_request_count(), 0);
    }

    /// Test readiness checks.
    ///
    /// Verifies: REQ-CORE-005/F-003
    #[test]
    fn test_readiness_checks() {
        let lifecycle = LifecycleManager::new(LifecycleConfig::default());

        let checks = lifecycle.readiness_checks();
        assert!(!checks.all_pass());
        assert!(!checks.config_loaded);
        assert!(!checks.upstream_reachable);
        assert!(!checks.approval_store_initialized);

        lifecycle.mark_config_loaded();
        let checks = lifecycle.readiness_checks();
        assert!(checks.config_loaded);
        assert!(!checks.all_pass());

        lifecycle.mark_approval_store_initialized();
        let checks = lifecycle.readiness_checks();
        assert!(checks.approval_store_initialized);
        assert!(!checks.all_pass()); // upstream still unhealthy

        lifecycle.update_upstream_health(true, None);
        let checks = lifecycle.readiness_checks();
        assert!(checks.upstream_reachable);
        assert!(checks.all_pass());
    }

    /// Test configuration defaults.
    ///
    /// Verifies: REQ-CORE-005/§5.3
    #[test]
    fn test_config_defaults() {
        let config = LifecycleConfig::default();
        assert_eq!(config.shutdown_timeout, Duration::from_secs(30));
        assert_eq!(config.drain_timeout, Duration::from_secs(25));
        assert_eq!(config.startup_timeout, Duration::from_secs(15));
        assert!(!config.require_upstream_at_startup);
        assert_eq!(config.upstream_health_interval, Duration::from_secs(30));
    }

    /// Test uptime tracking.
    #[test]
    fn test_uptime() {
        let lifecycle = LifecycleManager::new(LifecycleConfig::default());

        // Uptime should be close to 0 initially
        assert!(lifecycle.uptime_seconds() < 2);
    }

    /// Test version is set.
    #[test]
    fn test_version() {
        let lifecycle = LifecycleManager::new(LifecycleConfig::default());
        assert!(!lifecycle.version().is_empty());
    }

    /// Test shutdown token cancellation.
    #[test]
    fn test_shutdown_token() {
        let lifecycle = LifecycleManager::new(LifecycleConfig::default());
        let token = lifecycle.shutdown_token();

        assert!(!token.is_cancelled());

        lifecycle.begin_shutdown();

        assert!(token.is_cancelled());
    }

    /// Test drain completes immediately when no requests.
    ///
    /// Verifies: EC-OPS-007
    #[tokio::test]
    async fn test_drain_completes_no_requests() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_ready();
        lifecycle.begin_shutdown();

        let result = lifecycle.drain_requests().await;
        assert_eq!(result, DrainResult::Complete);
    }

    /// Test drain completes when requests finish.
    ///
    /// Verifies: EC-OPS-007
    #[tokio::test]
    async fn test_drain_completes_with_requests() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig {
            drain_timeout: Duration::from_millis(500),
            ..Default::default()
        }));
        lifecycle.mark_ready();

        // Start a request
        let guard = lifecycle.track_request();
        assert!(guard.is_some());

        // Begin shutdown
        lifecycle.begin_shutdown();

        // Spawn drain task
        let lifecycle_clone = lifecycle.clone();
        let drain_handle = tokio::spawn(async move { lifecycle_clone.drain_requests().await });

        // Wait a bit then drop the guard (request completes)
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(guard);

        // Should complete
        let result = drain_handle.await.unwrap();
        assert_eq!(result, DrainResult::Complete);
    }

    /// Test drain timeout.
    ///
    /// Verifies: EC-OPS-008
    #[tokio::test]
    async fn test_drain_timeout() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig {
            drain_timeout: Duration::from_millis(100),
            ..Default::default()
        }));
        lifecycle.mark_ready();

        // Keep a request active (don't drop the guard)
        let _guard = lifecycle.track_request();

        lifecycle.begin_shutdown();

        let result = lifecycle.drain_requests().await;
        assert!(matches!(result, DrainResult::Timeout { remaining: 1 }));
    }

    /// Test panic safety of request guard (via async task).
    ///
    /// The guard holds an Arc, so we test panic safety via tokio::spawn
    /// which is more realistic for actual usage.
    #[tokio::test]
    async fn test_request_guard_panic_safety() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_ready();

        assert_eq!(lifecycle.active_request_count(), 0);

        // Simulate a panic in a spawned task
        let lifecycle_clone = lifecycle.clone();
        let handle = tokio::spawn(async move {
            let _guard = lifecycle_clone.track_request();
            // Simulate some async work
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            panic!("Simulated panic in request handler");
        });

        // Wait for the task to complete (it will panic)
        let result = handle.await;
        assert!(result.is_err());

        // Even after panic, counter should be decremented
        assert_eq!(lifecycle.active_request_count(), 0);
    }

    /// Test lifecycle state display.
    #[test]
    fn test_state_display() {
        assert_eq!(format!("{}", LifecycleState::Starting), "starting");
        assert_eq!(format!("{}", LifecycleState::Ready), "ready");
        assert_eq!(format!("{}", LifecycleState::ShuttingDown), "shutting_down");
        assert_eq!(format!("{}", LifecycleState::Stopped), "stopped");
    }

    // ═══════════════════════════════════════════════════════════
    // Environment Validation Tests (B-003)
    // ═══════════════════════════════════════════════════════════

    use serial_test::serial;

    /// Helper to clear all environment-validation env vars.
    fn clear_env_validation_vars() {
        unsafe {
            std::env::remove_var("THOUGHTGATE_ENVIRONMENT");
            std::env::remove_var("THOUGHTGATE_DEV_MODE");
            std::env::remove_var("THOUGHTGATE_APPROVAL_ADAPTER");
        }
    }

    /// Production (default) rejects dev mode.
    #[test]
    #[serial]
    fn test_validate_env_production_rejects_dev_mode() {
        clear_env_validation_vars();
        unsafe { std::env::set_var("THOUGHTGATE_DEV_MODE", "true") };
        let result = validate_environment();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("DEV_MODE"));
        clear_env_validation_vars();
    }

    /// Production (default) rejects mock adapter.
    #[test]
    #[serial]
    fn test_validate_env_production_rejects_mock_adapter() {
        clear_env_validation_vars();
        unsafe { std::env::set_var("THOUGHTGATE_APPROVAL_ADAPTER", "mock") };
        let result = validate_environment();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("mock"));
        clear_env_validation_vars();
    }

    /// Explicit production also rejects dev mode.
    #[test]
    #[serial]
    fn test_validate_env_explicit_production_rejects_dev_mode() {
        clear_env_validation_vars();
        unsafe {
            std::env::set_var("THOUGHTGATE_ENVIRONMENT", "production");
            std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
        }
        let result = validate_environment();
        assert!(result.is_err());
        clear_env_validation_vars();
    }

    /// Development environment allows dev mode.
    #[test]
    #[serial]
    fn test_validate_env_development_allows_dev_mode() {
        clear_env_validation_vars();
        unsafe {
            std::env::set_var("THOUGHTGATE_ENVIRONMENT", "development");
            std::env::set_var("THOUGHTGATE_DEV_MODE", "true");
        }
        let result = validate_environment();
        assert!(result.is_ok());
        clear_env_validation_vars();
    }

    /// Development environment allows mock adapter.
    #[test]
    #[serial]
    fn test_validate_env_development_allows_mock_adapter() {
        clear_env_validation_vars();
        unsafe {
            std::env::set_var("THOUGHTGATE_ENVIRONMENT", "development");
            std::env::set_var("THOUGHTGATE_APPROVAL_ADAPTER", "mock");
        }
        let result = validate_environment();
        assert!(result.is_ok());
        clear_env_validation_vars();
    }

    /// Clean production environment passes validation.
    #[test]
    #[serial]
    fn test_validate_env_clean_production_passes() {
        clear_env_validation_vars();
        let result = validate_environment();
        assert!(result.is_ok());
        clear_env_validation_vars();
    }
}
