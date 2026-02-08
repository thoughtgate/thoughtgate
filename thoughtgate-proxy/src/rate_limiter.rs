//! Per-client IP rate limiting using the GCRA algorithm.
//!
//! Implements: REQ-CORE-005 (Operational Lifecycle - resource protection)
//!
//! Each unique peer IP address gets its own rate limiter instance, created
//! lazily on first connection. Stale entries are periodically cleaned up
//! to prevent unbounded memory growth.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use governor::clock::DefaultClock;
use governor::middleware::NoOpMiddleware;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter as GovernorLimiter};
use tracing::{debug, info, warn};

/// Type alias for the per-IP governor rate limiter.
type IpLimiter = GovernorLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>;

/// Entry in the per-IP rate limiter map.
struct RateLimitEntry {
    limiter: IpLimiter,
    last_seen: Instant,
}

/// Per-client IP rate limiter.
///
/// Wraps a `DashMap` of per-IP governor rate limiters with configurable
/// requests-per-second and burst limits. Stale entries (not seen for
/// `stale_after`) are periodically removed by a background task.
///
/// Implements: REQ-CORE-005 (Operational Lifecycle - resource protection)
pub struct PerIpRateLimiter {
    limiters: Arc<DashMap<IpAddr, RateLimitEntry>>,
    quota: Quota,
    stale_after: Duration,
}

/// Configuration for the per-IP rate limiter.
pub struct RateLimiterConfig {
    /// Maximum sustained requests per second per IP.
    pub rps: u32,
    /// Maximum burst size per IP.
    pub burst: u32,
    /// Duration after which an idle IP entry is considered stale.
    pub stale_after: Duration,
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            rps: 100,
            burst: 200,
            stale_after: Duration::from_secs(300),
        }
    }
}

impl RateLimiterConfig {
    /// Load configuration from environment variables.
    ///
    /// - `THOUGHTGATE_RATE_LIMIT_RPS` (default: 100)
    /// - `THOUGHTGATE_RATE_LIMIT_BURST` (default: 200)
    ///
    /// Implements: REQ-CORE-005 (Operational Lifecycle - configuration)
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(val) = std::env::var("THOUGHTGATE_RATE_LIMIT_RPS") {
            match val.parse::<u32>() {
                Ok(rps) if rps > 0 => config.rps = rps,
                _ => {
                    warn!(
                        env_var = "THOUGHTGATE_RATE_LIMIT_RPS",
                        value = %val,
                        default = 100u32,
                        "Invalid value for environment variable, using default"
                    );
                }
            }
        }

        if let Ok(val) = std::env::var("THOUGHTGATE_RATE_LIMIT_BURST") {
            match val.parse::<u32>() {
                Ok(burst) if burst > 0 => config.burst = burst,
                _ => {
                    warn!(
                        env_var = "THOUGHTGATE_RATE_LIMIT_BURST",
                        value = %val,
                        default = 200u32,
                        "Invalid value for environment variable, using default"
                    );
                }
            }
        }

        config
    }
}

impl PerIpRateLimiter {
    /// Create a new per-IP rate limiter with the given configuration.
    pub fn new(config: RateLimiterConfig) -> Self {
        // SAFETY: NonZeroU32::new on validated > 0 values from config.
        // Config defaults are non-zero and from_env() rejects zero.
        let burst = NonZeroU32::new(config.burst)
            .unwrap_or_else(|| NonZeroU32::new(200).expect("BUG: 200 is non-zero"));
        let quota = Quota::per_second(
            NonZeroU32::new(config.rps)
                .unwrap_or_else(|| NonZeroU32::new(100).expect("BUG: 100 is non-zero")),
        )
        .allow_burst(burst);

        info!(
            rps = config.rps,
            burst = config.burst,
            stale_secs = config.stale_after.as_secs(),
            "Per-IP rate limiter configured"
        );

        Self {
            limiters: Arc::new(DashMap::new()),
            quota,
            stale_after: config.stale_after,
        }
    }

    /// Check if a request from the given IP should be allowed.
    ///
    /// Returns `true` if the request is allowed, `false` if rate-limited.
    pub fn check(&self, ip: IpAddr) -> bool {
        let mut entry = self.limiters.entry(ip).or_insert_with(|| RateLimitEntry {
            limiter: GovernorLimiter::direct(self.quota),
            last_seen: Instant::now(),
        });
        entry.last_seen = Instant::now();
        entry.limiter.check().is_ok()
    }

    /// Remove stale entries that haven't been seen within `stale_after`.
    ///
    /// Returns the number of entries removed.
    pub fn cleanup_stale(&self) -> usize {
        let cutoff = Instant::now() - self.stale_after;
        let before = self.limiters.len();
        self.limiters.retain(|_, entry| entry.last_seen > cutoff);
        let removed = before - self.limiters.len();
        if removed > 0 {
            debug!(
                removed,
                remaining = self.limiters.len(),
                "Cleaned up stale rate limiter entries"
            );
        }
        removed
    }

    /// Get the number of tracked IPs.
    pub fn tracked_ips(&self) -> usize {
        self.limiters.len()
    }

    /// Spawn a background task that periodically cleans up stale entries.
    ///
    /// The task runs every `stale_after / 2` and stops when the
    /// cancellation token is triggered.
    pub fn spawn_cleanup_task(self: &Arc<Self>, shutdown: tokio_util::sync::CancellationToken) {
        let limiter = Arc::clone(self);
        let interval = limiter.stale_after / 2;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.tick().await; // Skip immediate first tick
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        limiter.cleanup_stale();
                    }
                    _ = shutdown.cancelled() => {
                        debug!("Rate limiter cleanup task shutting down");
                        break;
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(rps: u32, burst: u32) -> RateLimiterConfig {
        RateLimiterConfig {
            rps,
            burst,
            stale_after: Duration::from_secs(60),
        }
    }

    #[test]
    fn test_allows_requests_under_limit() {
        let limiter = PerIpRateLimiter::new(test_config(10, 10));
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        // First request should always be allowed
        assert!(limiter.check(ip));
    }

    #[test]
    fn test_rejects_after_burst_exceeded() {
        let limiter = PerIpRateLimiter::new(test_config(1, 3));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        // Burst of 3 should be allowed
        assert!(limiter.check(ip));
        assert!(limiter.check(ip));
        assert!(limiter.check(ip));
        // Fourth should be rejected (burst exceeded, only 1 rps replenish)
        assert!(!limiter.check(ip));
    }

    #[test]
    fn test_different_ips_have_independent_limits() {
        let limiter = PerIpRateLimiter::new(test_config(1, 2));
        let ip1: IpAddr = "10.0.0.1".parse().unwrap();
        let ip2: IpAddr = "10.0.0.2".parse().unwrap();

        // Exhaust ip1's burst
        assert!(limiter.check(ip1));
        assert!(limiter.check(ip1));
        assert!(!limiter.check(ip1));

        // ip2 should still have its full burst
        assert!(limiter.check(ip2));
        assert!(limiter.check(ip2));
        assert!(!limiter.check(ip2));
    }

    #[test]
    fn test_tracked_ips_count() {
        let limiter = PerIpRateLimiter::new(test_config(10, 10));
        assert_eq!(limiter.tracked_ips(), 0);

        let ip1: IpAddr = "10.0.0.1".parse().unwrap();
        let ip2: IpAddr = "10.0.0.2".parse().unwrap();
        limiter.check(ip1);
        assert_eq!(limiter.tracked_ips(), 1);
        limiter.check(ip2);
        assert_eq!(limiter.tracked_ips(), 2);
        // Same IP doesn't create new entry
        limiter.check(ip1);
        assert_eq!(limiter.tracked_ips(), 2);
    }

    #[test]
    fn test_cleanup_removes_stale_entries() {
        let limiter = PerIpRateLimiter::new(RateLimiterConfig {
            rps: 10,
            burst: 10,
            stale_after: Duration::from_secs(0), // Everything is immediately stale
        });
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        limiter.check(ip);
        assert_eq!(limiter.tracked_ips(), 1);

        // With stale_after=0, cleanup should remove everything
        std::thread::sleep(Duration::from_millis(1));
        let removed = limiter.cleanup_stale();
        assert_eq!(removed, 1);
        assert_eq!(limiter.tracked_ips(), 0);
    }

    #[test]
    fn test_cleanup_retains_active_entries() {
        let limiter = PerIpRateLimiter::new(RateLimiterConfig {
            rps: 10,
            burst: 10,
            stale_after: Duration::from_secs(3600), // 1 hour — nothing is stale
        });
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        limiter.check(ip);
        let removed = limiter.cleanup_stale();
        assert_eq!(removed, 0);
        assert_eq!(limiter.tracked_ips(), 1);
    }

    #[test]
    fn test_default_config() {
        let config = RateLimiterConfig::default();
        assert_eq!(config.rps, 100);
        assert_eq!(config.burst, 200);
        assert_eq!(config.stale_after, Duration::from_secs(300));
    }

    #[test]
    fn test_ipv6_support() {
        let limiter = PerIpRateLimiter::new(test_config(10, 10));
        let ipv4: IpAddr = "10.0.0.1".parse().unwrap();
        let ipv6: IpAddr = "::1".parse().unwrap();

        limiter.check(ipv4);
        limiter.check(ipv6);
        assert_eq!(limiter.tracked_ips(), 2);
    }
}
