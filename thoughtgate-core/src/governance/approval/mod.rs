//! Approval adapter module for external approval systems.
//!
//! Implements: REQ-GOV-003 (Approval Integration)
//!
//! This module provides the polling-based approval architecture for ThoughtGate.
//! Sidecars post approval requests to external systems (Slack) and poll for
//! decisions rather than receiving callbacks—necessary because sidecars are
//! not individually addressable from external systems.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                     POLLING MODEL (Sidecar-compatible)                      │
//! │                                                                             │
//! │   Pod 1 ──post message──▶ Slack ◀──poll for reactions── Pod 1              │
//! │   Pod 2 ──post message──▶ Slack ◀──poll for reactions── Pod 2              │
//! │   Pod 3 ──post message──▶ Slack ◀──poll for reactions── Pod 3              │
//! │                                                                             │
//! │   Each sidecar polls for its own task's approval decision                  │
//! └─────────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Module Organization
//!
//! - `mod.rs` - Trait definitions, types, and configuration
//! - `slack.rs` - Slack adapter implementation
//! - `scheduler.rs` - Polling scheduler with rate limiting (uses `governor` crate)

pub mod mock;
pub mod scheduler;
pub mod slack;

// Re-exports
pub use mock::MockAdapter;
pub use scheduler::PollingScheduler;
pub use slack::{SlackAdapter, SlackConfig};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::governance::{Principal, TaskId};

// ============================================================================
// Approval Request
// ============================================================================

/// Request to post an approval message to an external system.
///
/// Implements: REQ-GOV-003/§6.4
///
/// Contains all information needed to construct an approval request
/// message for human review.
#[derive(Clone)]
pub struct ApprovalRequest {
    /// The task ID this approval is for
    pub task_id: TaskId,
    /// Name of the tool being called
    pub tool_name: String,
    /// Arguments to the tool call (JSON)
    /// SECURITY: Redacted in Debug output to prevent leaking sensitive data
    pub tool_arguments: serde_json::Value,
    /// Principal (app/user) requesting the action
    pub principal: Principal,
    /// When the task expires
    pub expires_at: DateTime<Utc>,
    /// When the task was created
    pub created_at: DateTime<Utc>,
    /// Correlation ID for tracing
    pub correlation_id: String,
}

impl std::fmt::Debug for ApprovalRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalRequest")
            .field("task_id", &self.task_id)
            .field("tool_name", &self.tool_name)
            .field("tool_arguments", &"[REDACTED]")
            .field("principal", &self.principal)
            .field("expires_at", &self.expires_at)
            .field("created_at", &self.created_at)
            .field("correlation_id", &self.correlation_id)
            .finish()
    }
}

// ============================================================================
// Approval Reference
// ============================================================================

/// Reference to a posted approval message for polling.
///
/// Implements: REQ-GOV-003/§6.4
///
/// Stored after successfully posting an approval request, this contains
/// the information needed to poll for and cancel the approval.
#[derive(Debug, Clone)]
pub struct ApprovalReference {
    /// The task ID this reference is for
    pub task_id: TaskId,
    /// External ID (e.g., Slack message timestamp)
    pub external_id: String,
    /// Channel or location (e.g., Slack channel ID)
    pub channel: String,
    /// When the message was posted
    pub posted_at: DateTime<Utc>,
    /// When to poll next (for scheduler)
    pub next_poll_at: Instant,
    /// Number of polls performed
    pub poll_count: u32,
}

// ============================================================================
// Poll Result
// ============================================================================

/// Result of polling for an approval decision.
///
/// Implements: REQ-GOV-003/§6.4
///
/// This is distinct from `governance::ApprovalDecision` because it captures
/// additional information about how the decision was detected (reaction vs reply).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollResult {
    /// The decision (approved or rejected)
    pub decision: PollDecision,
    /// Who made the decision (user identifier/name)
    pub decided_by: String,
    /// When the decision was detected
    pub decided_at: DateTime<Utc>,
    /// How the decision was detected
    pub method: DecisionMethod,
}

/// The approval decision detected by polling.
///
/// Implements: REQ-GOV-003/§6.4
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PollDecision {
    /// Request was approved
    Approved,
    /// Request was rejected
    Rejected,
}

/// How the decision was detected.
///
/// Implements: REQ-GOV-003/§6.4
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionMethod {
    /// Decision via emoji reaction
    Reaction {
        /// The emoji used (e.g., "+1", "-1")
        emoji: String,
    },
    /// Decision via reply message
    Reply {
        /// The reply text
        text: String,
    },
}

impl DecisionMethod {
    /// Returns a human-readable description of the method.
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            Self::Reaction { emoji } => format!(":{emoji}: reaction"),
            Self::Reply { text } => {
                let preview: String = text.chars().take(30).collect();
                if text.len() > 30 {
                    format!("reply \"{preview}...\"")
                } else {
                    format!("reply \"{preview}\"")
                }
            }
        }
    }
}

// ============================================================================
// Adapter Errors
// ============================================================================

/// Errors from approval adapters.
///
/// Implements: REQ-GOV-003/§6.5
#[derive(Debug, Error, Clone)]
pub enum AdapterError {
    /// Failed to post approval request
    #[error("Failed to post approval request: {reason}")]
    PostFailed {
        /// Reason for failure
        reason: String,
        /// Whether the operation can be retried
        retriable: bool,
    },

    /// Failed to poll for decision
    #[error("Failed to poll for decision: {reason}")]
    PollFailed {
        /// Reason for failure
        reason: String,
        /// Whether the operation can be retried
        retriable: bool,
    },

    /// Rate limited by external API
    #[error("Rate limited, retry after {retry_after:?}")]
    RateLimited {
        /// Duration to wait before retrying
        retry_after: Duration,
    },

    /// Invalid or expired authentication token
    #[error("Invalid or expired authentication token")]
    InvalidToken,

    /// Channel not found
    #[error("Channel not found: {channel}")]
    ChannelNotFound {
        /// The channel that was not found
        channel: String,
    },

    /// Message not found (may have been deleted)
    #[error("Message not found: {ts}")]
    MessageNotFound {
        /// The message timestamp that was not found
        ts: String,
    },
}

impl AdapterError {
    /// Returns whether this error is retriable.
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::PostFailed { retriable, .. } | Self::PollFailed { retriable, .. } => *retriable,
            Self::RateLimited { .. } => true,
            Self::InvalidToken | Self::ChannelNotFound { .. } | Self::MessageNotFound { .. } => {
                false
            }
        }
    }
}

// ============================================================================
// Approval Adapter Trait
// ============================================================================

/// Adapter for external approval systems.
///
/// Implements: REQ-GOV-003/§6.4
///
/// This trait abstracts over different approval backends (Slack, Teams, etc.).
/// Each adapter handles:
/// - Posting approval request messages
/// - Polling for approval decisions
/// - Cancelling pending approvals
#[async_trait]
pub trait ApprovalAdapter: Send + Sync {
    /// Post an approval request to the external system.
    ///
    /// Implements: REQ-GOV-003/F-001
    ///
    /// # Arguments
    ///
    /// * `request` - The approval request to post
    ///
    /// # Returns
    ///
    /// A reference that can be used to poll for the decision.
    ///
    /// # Errors
    ///
    /// Returns `AdapterError` if posting fails.
    async fn post_approval_request(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalReference, AdapterError>;

    /// Poll for a decision on a pending approval.
    ///
    /// Implements: REQ-GOV-003/F-003
    ///
    /// # Arguments
    ///
    /// * `reference` - The reference returned from `post_approval_request`
    ///
    /// # Returns
    ///
    /// - `Ok(Some(result))` - Decision detected
    /// - `Ok(None)` - Still pending
    /// - `Err(e)` - Polling failed
    async fn poll_for_decision(
        &self,
        reference: &ApprovalReference,
    ) -> Result<Option<PollResult>, AdapterError>;

    /// Cancel a pending approval (best-effort).
    ///
    /// Implements: REQ-GOV-003/F-005
    ///
    /// This is best-effort—failures are logged but not propagated.
    /// Typically updates the message to show it's cancelled.
    ///
    /// # Arguments
    ///
    /// * `reference` - The reference to cancel
    async fn cancel_approval(&self, reference: &ApprovalReference) -> Result<(), AdapterError>;

    /// Returns the adapter name for logging and metrics.
    fn name(&self) -> &'static str;
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the polling scheduler.
///
/// Implements: REQ-GOV-003/§5.1
#[derive(Debug, Clone)]
pub struct PollingConfig {
    /// Base polling interval
    pub base_interval: Duration,
    /// Maximum polling interval (after backoff)
    pub max_interval: Duration,
    /// Maximum concurrent polls
    pub max_concurrent: usize,
    /// How long an approval is valid after being granted
    pub approval_valid_for: Duration,
    /// Rate limit for API calls (requests per second)
    pub rate_limit_per_sec: f64,
}

impl Default for PollingConfig {
    fn default() -> Self {
        Self {
            base_interval: Duration::from_secs(5),
            max_interval: Duration::from_secs(30),
            max_concurrent: 100,
            approval_valid_for: Duration::from_secs(60),
            rate_limit_per_sec: 1.0,
        }
    }
}

impl PollingConfig {
    /// Load configuration from environment variables.
    ///
    /// Implements: REQ-GOV-003/§5.1
    ///
    /// # Environment Variables
    ///
    /// - `THOUGHTGATE_APPROVAL_POLL_INTERVAL_SECS` - Base poll interval (default: 5)
    /// - `THOUGHTGATE_APPROVAL_POLL_MAX_INTERVAL_SECS` - Max poll interval (default: 30)
    /// - `THOUGHTGATE_MAX_CONCURRENT_POLLS` - Max concurrent polls (default: 100)
    /// - `THOUGHTGATE_SLACK_RATE_LIMIT_PER_SEC` - API rate limit (default: 1)
    #[must_use]
    pub fn from_env() -> Self {
        let base_interval = std::env::var("THOUGHTGATE_APPROVAL_POLL_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(5));

        let max_interval = std::env::var("THOUGHTGATE_APPROVAL_POLL_MAX_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(30));

        let max_concurrent = std::env::var("THOUGHTGATE_MAX_CONCURRENT_POLLS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100);

        let rate_limit_per_sec = std::env::var("THOUGHTGATE_SLACK_RATE_LIMIT_PER_SEC")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.0);

        Self {
            base_interval,
            max_interval,
            max_concurrent,
            approval_valid_for: Duration::from_secs(60),
            rate_limit_per_sec,
        }
    }

    /// Calculate backoff interval for the given poll count.
    ///
    /// Implements: REQ-GOV-003/F-002.3
    ///
    /// Uses exponential backoff: base * 2^min(poll_count, 3)
    /// Capped at max_interval.
    #[must_use]
    pub fn backoff_interval(&self, poll_count: u32) -> Duration {
        // 5s, 10s, 20s, 30s (max)
        let factor = 2u32.pow(poll_count.min(3));
        let interval = self.base_interval.saturating_mul(factor);
        interval.min(self.max_interval)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decision_method_description() {
        let reaction = DecisionMethod::Reaction {
            emoji: "+1".to_string(),
        };
        assert_eq!(reaction.description(), ":+1: reaction");

        let short_reply = DecisionMethod::Reply {
            text: "approved".to_string(),
        };
        assert_eq!(short_reply.description(), "reply \"approved\"");

        let long_reply = DecisionMethod::Reply {
            text: "This is a very long reply that should be truncated".to_string(),
        };
        assert!(long_reply.description().ends_with("...\""));
    }

    #[test]
    fn test_adapter_error_retriable() {
        assert!(
            AdapterError::PostFailed {
                reason: "timeout".to_string(),
                retriable: true
            }
            .is_retriable()
        );

        assert!(
            !AdapterError::PostFailed {
                reason: "invalid".to_string(),
                retriable: false
            }
            .is_retriable()
        );

        assert!(
            AdapterError::RateLimited {
                retry_after: Duration::from_secs(60)
            }
            .is_retriable()
        );

        assert!(!AdapterError::InvalidToken.is_retriable());
        assert!(
            !AdapterError::ChannelNotFound {
                channel: "#foo".to_string()
            }
            .is_retriable()
        );
    }

    #[test]
    fn test_polling_config_default() {
        let config = PollingConfig::default();
        assert_eq!(config.base_interval, Duration::from_secs(5));
        assert_eq!(config.max_interval, Duration::from_secs(30));
        assert_eq!(config.max_concurrent, 100);
    }

    #[test]
    fn test_polling_config_backoff() {
        let config = PollingConfig::default();

        // 5s * 2^0 = 5s
        assert_eq!(config.backoff_interval(0), Duration::from_secs(5));
        // 5s * 2^1 = 10s
        assert_eq!(config.backoff_interval(1), Duration::from_secs(10));
        // 5s * 2^2 = 20s
        assert_eq!(config.backoff_interval(2), Duration::from_secs(20));
        // 5s * 2^3 = 40s, capped to 30s
        assert_eq!(config.backoff_interval(3), Duration::from_secs(30));
        // Further polls stay at max
        assert_eq!(config.backoff_interval(10), Duration::from_secs(30));
    }
}
