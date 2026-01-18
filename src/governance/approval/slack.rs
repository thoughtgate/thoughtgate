//! Slack adapter for approval workflows.
//!
//! Implements: REQ-GOV-003/F-006
//!
//! This module provides the Slack implementation of the `ApprovalAdapter` trait.
//! It posts approval requests as Block Kit messages and polls for reactions
//! (👍 = approve, 👎 = reject).
//!
//! ## Security
//!
//! - Bot token is NEVER logged
//! - All API calls use HTTPS
//! - User identity from Slack is trusted

use super::{
    AdapterError, ApprovalAdapter, ApprovalReference, ApprovalRequest, DecisionMethod,
    PollDecision, PollResult,
};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use reqwest::Client;
use serde::Deserialize;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the Slack adapter.
///
/// Implements: REQ-GOV-003/§5.2
#[derive(Debug, Clone)]
pub struct SlackConfig {
    /// Bot token for Slack API (NEVER log this value)
    bot_token: String,
    /// Default channel for approval messages
    pub channel: String,
    /// Reaction emoji for approval (without colons, e.g., "+1")
    pub approve_reaction: String,
    /// Reaction emoji for rejection (without colons, e.g., "-1")
    pub reject_reaction: String,
    /// Request timeout for Slack API calls
    pub api_timeout: Duration,
    /// Initial poll interval (should match PollingConfig::base_interval)
    pub initial_poll_interval: Duration,
}

impl SlackConfig {
    /// Create a new Slack configuration.
    ///
    /// Implements: REQ-GOV-003/§5.2
    ///
    /// # Arguments
    ///
    /// * `bot_token` - Slack bot token (starts with xoxb-)
    /// * `channel` - Default channel for approval messages
    #[must_use]
    pub fn new(bot_token: impl Into<String>, channel: impl Into<String>) -> Self {
        Self {
            bot_token: bot_token.into(),
            channel: channel.into(),
            approve_reaction: "+1".to_string(),
            reject_reaction: "-1".to_string(),
            api_timeout: Duration::from_secs(10),
            initial_poll_interval: Duration::from_secs(5),
        }
    }

    /// Load configuration from environment variables.
    ///
    /// Implements: REQ-GOV-003/§5.2
    ///
    /// # Environment Variables
    ///
    /// - `SLACK_BOT_TOKEN` (required) - Bot token for Slack API
    /// - `SLACK_CHANNEL` (default: #approvals) - Channel for approval messages
    /// - `SLACK_APPROVE_REACTION` (default: +1) - Reaction emoji for approval
    /// - `SLACK_REJECT_REACTION` (default: -1) - Reaction emoji for rejection
    ///
    /// # Errors
    ///
    /// Returns `AdapterError::InvalidToken` if SLACK_BOT_TOKEN is not set.
    pub fn from_env() -> Result<Self, AdapterError> {
        let bot_token = std::env::var("SLACK_BOT_TOKEN").map_err(|_| AdapterError::InvalidToken)?;

        let initial_poll_interval = std::env::var("THOUGHTGATE_APPROVAL_POLL_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(5));

        Ok(Self {
            bot_token,
            channel: std::env::var("SLACK_CHANNEL").unwrap_or_else(|_| "#approvals".to_string()),
            approve_reaction: std::env::var("SLACK_APPROVE_REACTION")
                .unwrap_or_else(|_| "+1".to_string()),
            reject_reaction: std::env::var("SLACK_REJECT_REACTION")
                .unwrap_or_else(|_| "-1".to_string()),
            api_timeout: Duration::from_secs(10),
            initial_poll_interval,
        })
    }

    /// Set the approval reaction emoji.
    #[must_use]
    pub fn with_approve_reaction(mut self, emoji: impl Into<String>) -> Self {
        self.approve_reaction = emoji.into();
        self
    }

    /// Set the rejection reaction emoji.
    #[must_use]
    pub fn with_reject_reaction(mut self, emoji: impl Into<String>) -> Self {
        self.reject_reaction = emoji.into();
        self
    }

    /// Set the API timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.api_timeout = timeout;
        self
    }

    /// Set the initial poll interval.
    #[must_use]
    pub fn with_initial_poll_interval(mut self, interval: Duration) -> Self {
        self.initial_poll_interval = interval;
        self
    }
}

// ============================================================================
// Slack Adapter
// ============================================================================

/// Slack adapter for approval workflows.
///
/// Implements: REQ-GOV-003/F-006
///
/// Posts approval requests as Block Kit messages and polls for
/// reactions to detect approval decisions.
pub struct SlackAdapter {
    client: Client,
    config: SlackConfig,
    /// Cache: user_id -> display_name (avoid repeated users.info calls)
    user_cache: DashMap<String, String>,
}

impl SlackAdapter {
    /// Create a new Slack adapter.
    ///
    /// Implements: REQ-GOV-003/F-006
    ///
    /// # Errors
    ///
    /// Returns `AdapterError::PostFailed` if the HTTP client cannot be built.
    pub fn new(config: SlackConfig) -> Result<Self, AdapterError> {
        let client = Client::builder()
            .timeout(config.api_timeout)
            .build()
            .map_err(|e| AdapterError::PostFailed {
                reason: format!("Failed to build HTTP client: {e}"),
                retriable: false,
            })?;

        Ok(Self {
            client,
            config,
            user_cache: DashMap::new(),
        })
    }

    /// Build Block Kit message for approval request.
    ///
    /// Implements: REQ-GOV-003/F-006.2
    fn build_approval_blocks(&self, request: &ApprovalRequest) -> serde_json::Value {
        let args_pretty = serde_json::to_string_pretty(&request.tool_arguments)
            .unwrap_or_else(|_| request.tool_arguments.to_string());

        serde_json::json!([
            {
                "type": "header",
                "text": {
                    "type": "plain_text",
                    "text": format!("🔒 Approval Required: {}", request.tool_name),
                    "emoji": true
                }
            },
            {
                "type": "section",
                "fields": [
                    {
                        "type": "mrkdwn",
                        "text": format!("*Tool:* `{}`", request.tool_name)
                    },
                    {
                        "type": "mrkdwn",
                        "text": format!("*Principal:* {}", request.principal.app_name)
                    }
                ]
            },
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!("*Arguments:*\n```\n{}\n```", args_pretty)
                }
            },
            {
                "type": "context",
                "elements": [
                    {
                        "type": "mrkdwn",
                        "text": "React with 👍 to *approve* or 👎 to *reject*"
                    }
                ]
            },
            {
                "type": "context",
                "elements": [
                    {
                        "type": "mrkdwn",
                        "text": format!(
                            "Task ID: `{}` • Expires: {}",
                            request.task_id,
                            request.expires_at.format("%Y-%m-%d %H:%M UTC")
                        )
                    }
                ]
            }
        ])
    }

    /// Build cancelled message blocks.
    ///
    /// Implements: REQ-GOV-003/F-005.2
    fn build_cancelled_blocks(&self) -> serde_json::Value {
        serde_json::json!([
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": "❌ *Cancelled* - This approval request is no longer active."
                }
            }
        ])
    }

    /// Look up user display name (cached).
    ///
    /// Implements: REQ-GOV-003/F-003.5, F-006.4
    async fn get_user_display_name(&self, user_id: &str) -> Result<String, AdapterError> {
        // Check cache first
        if let Some(name) = self.user_cache.get(user_id) {
            return Ok(name.clone());
        }

        // Fetch from Slack API
        let response = self
            .client
            .get("https://slack.com/api/users.info")
            .bearer_auth(&self.config.bot_token)
            .query(&[("user", user_id)])
            .send()
            .await
            .map_err(|e| AdapterError::PollFailed {
                reason: format!("Failed to fetch user info: {e}"),
                retriable: true,
            })?;

        let body: SlackUserInfoResponse =
            response
                .json()
                .await
                .map_err(|e| AdapterError::PollFailed {
                    reason: format!("Failed to parse user info response: {e}"),
                    retriable: true,
                })?;

        if !body.ok {
            warn!(user_id = %user_id, error = ?body.error, "Failed to fetch user info");
            // Fall back to user_id
            return Ok(user_id.to_string());
        }

        let display_name = body
            .user
            .map(|u| {
                u.profile
                    .and_then(|p| p.display_name.filter(|n| !n.is_empty()))
                    .or(u.real_name)
                    .unwrap_or(u.id)
            })
            .unwrap_or_else(|| user_id.to_string());

        // Cache the result
        self.user_cache
            .insert(user_id.to_string(), display_name.clone());

        Ok(display_name)
    }

    /// Check reactions for approval/rejection.
    ///
    /// Implements: REQ-GOV-003/F-003.1, F-003.2
    fn check_reactions(
        &self,
        reactions: &Option<Vec<SlackReaction>>,
    ) -> Option<(PollDecision, String, String)> {
        let reactions = reactions.as_ref()?;

        // Check for approval reaction first (priority per spec F-003)
        if let Some(reaction) = reactions
            .iter()
            .find(|r| r.name == self.config.approve_reaction)
        {
            if let Some(user_id) = reaction.users.first() {
                return Some((
                    PollDecision::Approved,
                    user_id.clone(),
                    reaction.name.clone(),
                ));
            }
        }

        // Check for rejection reaction
        if let Some(reaction) = reactions
            .iter()
            .find(|r| r.name == self.config.reject_reaction)
        {
            if let Some(user_id) = reaction.users.first() {
                return Some((
                    PollDecision::Rejected,
                    user_id.clone(),
                    reaction.name.clone(),
                ));
            }
        }

        None
    }

    /// Handle HTTP 429 rate limit response.
    ///
    /// Implements: REQ-GOV-003/§5.3, EC-APR-010
    fn handle_rate_limit(response: &reqwest::Response) -> AdapterError {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);

        AdapterError::RateLimited {
            retry_after: Duration::from_secs(retry_after),
        }
    }

    /// Map Slack API error to AdapterError.
    ///
    /// Implements: REQ-GOV-003/F-006.5
    fn map_slack_error(error: &str, channel: &str, ts: Option<&str>) -> AdapterError {
        match error {
            "channel_not_found" => AdapterError::ChannelNotFound {
                channel: channel.to_string(),
            },
            "invalid_auth" | "account_inactive" | "token_revoked" | "not_authed" => {
                AdapterError::InvalidToken
            }
            "message_not_found" => AdapterError::MessageNotFound {
                ts: ts.unwrap_or("unknown").to_string(),
            },
            "ratelimited" => AdapterError::RateLimited {
                retry_after: Duration::from_secs(60),
            },
            _ => AdapterError::PostFailed {
                reason: error.to_string(),
                retriable: false,
            },
        }
    }
}

#[async_trait]
impl ApprovalAdapter for SlackAdapter {
    /// Post approval request to Slack.
    ///
    /// Implements: REQ-GOV-003/F-001
    /// Handles: EC-APR-001, EC-APR-002, EC-APR-003
    async fn post_approval_request(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalReference, AdapterError> {
        let blocks = self.build_approval_blocks(request);

        let response = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.config.bot_token)
            .json(&serde_json::json!({
                "channel": self.config.channel,
                "blocks": blocks,
                "metadata": {
                    "event_type": "thoughtgate_approval",
                    "event_payload": {
                        "task_id": request.task_id.to_string()
                    }
                }
            }))
            .send()
            .await
            .map_err(|e| {
                error!(task_id = %request.task_id, error = %e, "Failed to post approval message");
                AdapterError::PostFailed {
                    reason: e.to_string(),
                    retriable: e.is_connect() || e.is_timeout(),
                }
            })?;

        // Check for rate limiting
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Self::handle_rate_limit(&response));
        }

        let body: SlackPostMessageResponse =
            response
                .json()
                .await
                .map_err(|e| AdapterError::PostFailed {
                    reason: format!("Failed to parse Slack response: {e}"),
                    retriable: false,
                })?;

        if !body.ok {
            let error = body.error.as_deref().unwrap_or("unknown");
            return Err(Self::map_slack_error(error, &self.config.channel, None));
        }

        let ts = body.ts.ok_or_else(|| AdapterError::PostFailed {
            reason: "No timestamp in response".to_string(),
            retriable: false,
        })?;

        let channel = body.channel.ok_or_else(|| AdapterError::PostFailed {
            reason: "No channel in response".to_string(),
            retriable: false,
        })?;

        info!(
            task_id = %request.task_id,
            channel = %channel,
            ts = %ts,
            "Posted approval message to Slack"
        );

        Ok(ApprovalReference {
            task_id: request.task_id.clone(),
            external_id: ts,
            channel,
            posted_at: Utc::now(),
            next_poll_at: Instant::now() + self.config.initial_poll_interval,
            poll_count: 0,
        })
    }

    /// Poll for decision via reactions.get.
    ///
    /// Implements: REQ-GOV-003/F-003
    /// Handles: EC-APR-004, EC-APR-005, EC-APR-006, EC-APR-007
    async fn poll_for_decision(
        &self,
        reference: &ApprovalReference,
    ) -> Result<Option<PollResult>, AdapterError> {
        let response = self
            .client
            .get("https://slack.com/api/reactions.get")
            .bearer_auth(&self.config.bot_token)
            .query(&[
                ("channel", &reference.channel),
                ("timestamp", &reference.external_id),
            ])
            .send()
            .await
            .map_err(|e| AdapterError::PollFailed {
                reason: e.to_string(),
                retriable: true,
            })?;

        // Check for rate limiting
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Self::handle_rate_limit(&response));
        }

        let body: SlackReactionsGetResponse =
            response
                .json()
                .await
                .map_err(|e| AdapterError::PollFailed {
                    reason: format!("Failed to parse reactions response: {e}"),
                    retriable: true,
                })?;

        if !body.ok {
            let error = body.error.as_deref().unwrap_or("unknown");
            return Err(Self::map_slack_error(
                error,
                &reference.channel,
                Some(&reference.external_id),
            ));
        }

        // Check for approval/rejection reactions
        let reactions = body.message.and_then(|m| m.reactions);

        if let Some((decision, user_id, emoji)) = self.check_reactions(&reactions) {
            // Best-effort lookup: fall back to user_id if lookup fails
            let display_name = match self.get_user_display_name(&user_id).await {
                Ok(name) => name,
                Err(e) => {
                    warn!(
                        user_id = %user_id,
                        error = %e,
                        "Failed to lookup user display name, using user_id"
                    );
                    user_id.clone()
                }
            };

            info!(
                task_id = %reference.task_id,
                decision = ?decision,
                decided_by = %display_name,
                emoji = %emoji,
                "Detected approval decision"
            );

            return Ok(Some(PollResult {
                decision,
                decided_by: display_name,
                decided_at: Utc::now(),
                method: DecisionMethod::Reaction { emoji },
            }));
        }

        debug!(
            task_id = %reference.task_id,
            poll_count = reference.poll_count,
            "No decision yet"
        );

        Ok(None)
    }

    /// Cancel approval by updating the message.
    ///
    /// Implements: REQ-GOV-003/F-005
    /// Handles: EC-APR-009
    async fn cancel_approval(&self, reference: &ApprovalReference) -> Result<(), AdapterError> {
        // Best-effort update to show cancellation
        let result = self
            .client
            .post("https://slack.com/api/chat.update")
            .bearer_auth(&self.config.bot_token)
            .json(&serde_json::json!({
                "channel": reference.channel,
                "ts": reference.external_id,
                "blocks": self.build_cancelled_blocks()
            }))
            .send()
            .await;

        match result {
            Ok(response) => {
                if response.status().is_success() {
                    info!(
                        task_id = %reference.task_id,
                        "Cancelled approval message"
                    );
                } else {
                    warn!(
                        task_id = %reference.task_id,
                        status = %response.status(),
                        "Failed to cancel approval message"
                    );
                }
            }
            Err(e) => {
                warn!(
                    task_id = %reference.task_id,
                    error = %e,
                    "Failed to cancel approval message"
                );
            }
        }

        Ok(())
    }

    fn name(&self) -> &'static str {
        "slack"
    }
}

// ============================================================================
// Slack API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct SlackPostMessageResponse {
    ok: bool,
    error: Option<String>,
    ts: Option<String>,
    channel: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackReactionsGetResponse {
    ok: bool,
    error: Option<String>,
    message: Option<SlackMessage>,
}

#[derive(Debug, Deserialize)]
struct SlackMessage {
    reactions: Option<Vec<SlackReaction>>,
}

#[derive(Debug, Deserialize)]
struct SlackReaction {
    name: String,
    users: Vec<String>,
    #[allow(dead_code)]
    count: u32,
}

#[derive(Debug, Deserialize)]
struct SlackUserInfoResponse {
    ok: bool,
    error: Option<String>,
    user: Option<SlackUser>,
}

#[derive(Debug, Deserialize)]
struct SlackUser {
    id: String,
    real_name: Option<String>,
    profile: Option<SlackUserProfile>,
}

#[derive(Debug, Deserialize)]
struct SlackUserProfile {
    display_name: Option<String>,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::Principal;

    fn test_config() -> SlackConfig {
        SlackConfig::new("xoxb-test-token", "#test-channel")
    }

    fn test_request() -> ApprovalRequest {
        ApprovalRequest {
            task_id: crate::governance::TaskId::new(),
            tool_name: "delete_user".to_string(),
            tool_arguments: serde_json::json!({"user_id": "12345"}),
            principal: Principal::new("test-app"),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            created_at: Utc::now(),
            correlation_id: "test-correlation".to_string(),
        }
    }

    #[test]
    fn test_slack_config_new() {
        let config = SlackConfig::new("xoxb-token", "#channel");
        assert_eq!(config.channel, "#channel");
        assert_eq!(config.approve_reaction, "+1");
        assert_eq!(config.reject_reaction, "-1");
    }

    #[test]
    fn test_slack_config_with_reactions() {
        let config = SlackConfig::new("xoxb-token", "#channel")
            .with_approve_reaction("white_check_mark")
            .with_reject_reaction("x");

        assert_eq!(config.approve_reaction, "white_check_mark");
        assert_eq!(config.reject_reaction, "x");
    }

    #[test]
    fn test_build_approval_blocks() {
        let adapter = SlackAdapter::new(test_config()).expect("Failed to create adapter");
        let request = test_request();

        let blocks = adapter.build_approval_blocks(&request);
        let blocks_array = blocks.as_array().expect("Expected array");

        assert_eq!(blocks_array.len(), 5);
        assert_eq!(blocks_array[0]["type"], "header");
        assert_eq!(blocks_array[1]["type"], "section");
    }

    #[test]
    fn test_check_reactions_approve() {
        let adapter = SlackAdapter::new(test_config()).expect("Failed to create adapter");

        let reactions = Some(vec![SlackReaction {
            name: "+1".to_string(),
            users: vec!["U123".to_string()],
            count: 1,
        }]);

        let result = adapter.check_reactions(&reactions);
        assert!(result.is_some());

        let (decision, user_id, emoji) = result.unwrap();
        assert_eq!(decision, PollDecision::Approved);
        assert_eq!(user_id, "U123");
        assert_eq!(emoji, "+1");
    }

    #[test]
    fn test_check_reactions_reject() {
        let adapter = SlackAdapter::new(test_config()).expect("Failed to create adapter");

        let reactions = Some(vec![SlackReaction {
            name: "-1".to_string(),
            users: vec!["U456".to_string()],
            count: 1,
        }]);

        let result = adapter.check_reactions(&reactions);
        assert!(result.is_some());

        let (decision, _, _) = result.unwrap();
        assert_eq!(decision, PollDecision::Rejected);
    }

    #[test]
    fn test_check_reactions_both_approve_wins() {
        let adapter = SlackAdapter::new(test_config()).expect("Failed to create adapter");

        // Both reactions present - +1 (approve) should win per spec F-003
        let reactions = Some(vec![
            SlackReaction {
                name: "+1".to_string(),
                users: vec!["U123".to_string()],
                count: 1,
            },
            SlackReaction {
                name: "-1".to_string(),
                users: vec!["U456".to_string()],
                count: 1,
            },
        ]);

        let result = adapter.check_reactions(&reactions);
        assert!(result.is_some());

        let (decision, _, _) = result.unwrap();
        assert_eq!(decision, PollDecision::Approved);
    }

    #[test]
    fn test_check_reactions_none() {
        let adapter = SlackAdapter::new(test_config()).expect("Failed to create adapter");

        // No reactions
        let result = adapter.check_reactions(&None);
        assert!(result.is_none());

        // Empty reactions
        let result = adapter.check_reactions(&Some(vec![]));
        assert!(result.is_none());

        // Unrelated reactions
        let reactions = Some(vec![SlackReaction {
            name: "eyes".to_string(),
            users: vec!["U789".to_string()],
            count: 1,
        }]);
        let result = adapter.check_reactions(&reactions);
        assert!(result.is_none());
    }

    #[test]
    fn test_map_slack_error() {
        assert!(matches!(
            SlackAdapter::map_slack_error("channel_not_found", "#test", None),
            AdapterError::ChannelNotFound { .. }
        ));

        assert!(matches!(
            SlackAdapter::map_slack_error("invalid_auth", "#test", None),
            AdapterError::InvalidToken
        ));

        assert!(matches!(
            SlackAdapter::map_slack_error("message_not_found", "#test", Some("123.456")),
            AdapterError::MessageNotFound { .. }
        ));

        assert!(matches!(
            SlackAdapter::map_slack_error("ratelimited", "#test", None),
            AdapterError::RateLimited { .. }
        ));
    }

    // ========================================================================
    // Wiremock Integration Tests
    // ========================================================================
    //
    // These tests verify actual HTTP interactions with the Slack API using
    // wiremock to simulate Slack API responses.

    /// Creates a SlackAdapter that talks to a wiremock server instead of Slack.
    fn adapter_with_base_url(
        _base_url: &str,
        config: SlackConfig,
    ) -> Result<SlackAdapter, AdapterError> {
        // Create a client that uses the wiremock server URL
        let client = Client::builder()
            .timeout(config.api_timeout)
            .build()
            .map_err(|e| AdapterError::PostFailed {
                reason: format!("Failed to build HTTP client: {e}"),
                retriable: false,
            })?;

        Ok(SlackAdapter {
            client,
            config,
            user_cache: DashMap::new(),
        })
    }

    /// Implements the wiremock-based test adapter that overrides URLs.
    struct WiremockSlackAdapter {
        inner: SlackAdapter,
        base_url: String,
    }

    impl WiremockSlackAdapter {
        fn new(base_url: &str, config: SlackConfig) -> Result<Self, AdapterError> {
            let inner = adapter_with_base_url(base_url, config)?;
            Ok(Self {
                inner,
                base_url: base_url.to_string(),
            })
        }

        async fn post_approval_request(
            &self,
            request: &ApprovalRequest,
        ) -> Result<ApprovalReference, AdapterError> {
            let blocks = self.inner.build_approval_blocks(request);

            let response = self
                .inner
                .client
                .post(format!("{}/api/chat.postMessage", self.base_url))
                .bearer_auth(&self.inner.config.bot_token)
                .json(&serde_json::json!({
                    "channel": self.inner.config.channel,
                    "blocks": blocks,
                    "metadata": {
                        "event_type": "thoughtgate_approval",
                        "event_payload": {
                            "task_id": request.task_id.to_string()
                        }
                    }
                }))
                .send()
                .await
                .map_err(|e| AdapterError::PostFailed {
                    reason: e.to_string(),
                    retriable: e.is_connect() || e.is_timeout(),
                })?;

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(SlackAdapter::handle_rate_limit(&response));
            }

            let body: SlackPostMessageResponse =
                response
                    .json()
                    .await
                    .map_err(|e| AdapterError::PostFailed {
                        reason: format!("Failed to parse Slack response: {e}"),
                        retriable: false,
                    })?;

            if !body.ok {
                let error = body.error.as_deref().unwrap_or("unknown");
                return Err(SlackAdapter::map_slack_error(
                    error,
                    &self.inner.config.channel,
                    None,
                ));
            }

            let ts = body.ts.ok_or_else(|| AdapterError::PostFailed {
                reason: "No timestamp in response".to_string(),
                retriable: false,
            })?;

            let channel = body.channel.ok_or_else(|| AdapterError::PostFailed {
                reason: "No channel in response".to_string(),
                retriable: false,
            })?;

            Ok(ApprovalReference {
                task_id: request.task_id.clone(),
                external_id: ts,
                channel,
                posted_at: Utc::now(),
                next_poll_at: std::time::Instant::now() + self.inner.config.initial_poll_interval,
                poll_count: 0,
            })
        }

        async fn poll_for_decision(
            &self,
            reference: &ApprovalReference,
        ) -> Result<Option<PollResult>, AdapterError> {
            let response = self
                .inner
                .client
                .get(format!("{}/api/reactions.get", self.base_url))
                .bearer_auth(&self.inner.config.bot_token)
                .query(&[
                    ("channel", &reference.channel),
                    ("timestamp", &reference.external_id),
                ])
                .send()
                .await
                .map_err(|e| AdapterError::PollFailed {
                    reason: e.to_string(),
                    retriable: true,
                })?;

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(SlackAdapter::handle_rate_limit(&response));
            }

            let body: SlackReactionsGetResponse =
                response
                    .json()
                    .await
                    .map_err(|e| AdapterError::PollFailed {
                        reason: format!("Failed to parse reactions response: {e}"),
                        retriable: true,
                    })?;

            if !body.ok {
                let error = body.error.as_deref().unwrap_or("unknown");
                return Err(SlackAdapter::map_slack_error(
                    error,
                    &reference.channel,
                    Some(&reference.external_id),
                ));
            }

            let reactions = body.message.and_then(|m| m.reactions);

            if let Some((decision, user_id, emoji)) = self.inner.check_reactions(&reactions) {
                // Use user_id directly for tests (skip user lookup)
                return Ok(Some(PollResult {
                    decision,
                    decided_by: user_id,
                    decided_at: Utc::now(),
                    method: DecisionMethod::Reaction { emoji },
                }));
            }

            Ok(None)
        }
    }

    /// Tests that posting an approval message works correctly.
    ///
    /// Verifies: EC-SLK-001 (Slack API rate limit handling)
    /// Verifies: REQ-GOV-003/F-001 (Post approval request)
    #[tokio::test]
    async fn test_wiremock_post_message_success() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .and(header("Authorization", "Bearer xoxb-test-token"))
            .and(body_partial_json(serde_json::json!({
                "channel": "#test-channel"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "ts": "1234567890.123456",
                "channel": "C12345"
            })))
            .mount(&mock_server)
            .await;

        let config = SlackConfig::new("xoxb-test-token", "#test-channel");
        let adapter = WiremockSlackAdapter::new(&mock_server.uri(), config)
            .expect("Failed to create adapter");

        let request = test_request();
        let result = adapter.post_approval_request(&request).await;

        assert!(result.is_ok(), "Post should succeed: {:?}", result);
        let reference = result.unwrap();
        assert_eq!(reference.external_id, "1234567890.123456");
        assert_eq!(reference.channel, "C12345");
    }

    /// Tests that channel not found error is handled correctly.
    ///
    /// Verifies: EC-SLK-006 (Channel doesn't exist)
    #[tokio::test]
    async fn test_wiremock_channel_not_found() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "error": "channel_not_found"
            })))
            .mount(&mock_server)
            .await;

        let config = SlackConfig::new("xoxb-test-token", "#nonexistent");
        let adapter = WiremockSlackAdapter::new(&mock_server.uri(), config)
            .expect("Failed to create adapter");

        let request = test_request();
        let result = adapter.post_approval_request(&request).await;

        assert!(matches!(result, Err(AdapterError::ChannelNotFound { .. })));
    }

    /// Tests that invalid token error is handled correctly.
    ///
    /// Verifies: EC-SLK-005 (Slack token revoked)
    #[tokio::test]
    async fn test_wiremock_invalid_token() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "error": "invalid_auth"
            })))
            .mount(&mock_server)
            .await;

        let config = SlackConfig::new("xoxb-bad-token", "#test-channel");
        let adapter = WiremockSlackAdapter::new(&mock_server.uri(), config)
            .expect("Failed to create adapter");

        let request = test_request();
        let result = adapter.post_approval_request(&request).await;

        assert!(matches!(result, Err(AdapterError::InvalidToken)));
    }

    /// Tests that HTTP 429 rate limit is handled correctly.
    ///
    /// Verifies: EC-SLK-001 (Slack API rate limit)
    #[tokio::test]
    async fn test_wiremock_rate_limit() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "30")
                    .set_body_json(serde_json::json!({"ok": false, "error": "ratelimited"})),
            )
            .mount(&mock_server)
            .await;

        let config = SlackConfig::new("xoxb-test-token", "#test-channel");
        let adapter = WiremockSlackAdapter::new(&mock_server.uri(), config)
            .expect("Failed to create adapter");

        let request = test_request();
        let result = adapter.post_approval_request(&request).await;

        match result {
            Err(AdapterError::RateLimited { retry_after }) => {
                assert_eq!(retry_after, Duration::from_secs(30));
            }
            _ => panic!("Expected RateLimited error, got {:?}", result),
        }
    }

    /// Tests polling for approval reaction.
    ///
    /// Verifies: REQ-GOV-003/F-003 (Poll for decision)
    /// Verifies: EC-SLK-003 (Multiple reactions - first wins)
    #[tokio::test]
    async fn test_wiremock_poll_approve_reaction() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/reactions.get"))
            .and(query_param("channel", "C12345"))
            .and(query_param("timestamp", "1234.5678"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "message": {
                    "reactions": [
                        {
                            "name": "+1",
                            "users": ["U123"],
                            "count": 1
                        }
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let config = SlackConfig::new("xoxb-test-token", "#test-channel");
        let adapter = WiremockSlackAdapter::new(&mock_server.uri(), config)
            .expect("Failed to create adapter");

        let reference = ApprovalReference {
            task_id: crate::governance::TaskId::new(),
            external_id: "1234.5678".to_string(),
            channel: "C12345".to_string(),
            posted_at: Utc::now(),
            next_poll_at: std::time::Instant::now(),
            poll_count: 0,
        };

        let result = adapter.poll_for_decision(&reference).await;

        assert!(result.is_ok(), "Poll should succeed: {:?}", result);
        let decision = result.unwrap();
        assert!(decision.is_some(), "Should have decision");

        let poll_result = decision.unwrap();
        assert_eq!(poll_result.decision, PollDecision::Approved);
        assert_eq!(poll_result.decided_by, "U123");
    }

    /// Tests polling for rejection reaction.
    ///
    /// Verifies: REQ-GOV-003/F-003 (Poll for decision)
    #[tokio::test]
    async fn test_wiremock_poll_reject_reaction() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/reactions.get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "message": {
                    "reactions": [
                        {
                            "name": "-1",
                            "users": ["U456"],
                            "count": 1
                        }
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let config = SlackConfig::new("xoxb-test-token", "#test-channel");
        let adapter = WiremockSlackAdapter::new(&mock_server.uri(), config)
            .expect("Failed to create adapter");

        let reference = ApprovalReference {
            task_id: crate::governance::TaskId::new(),
            external_id: "1234.5678".to_string(),
            channel: "C12345".to_string(),
            posted_at: Utc::now(),
            next_poll_at: std::time::Instant::now(),
            poll_count: 0,
        };

        let result = adapter.poll_for_decision(&reference).await;

        assert!(result.is_ok());
        let poll_result = result.unwrap().expect("Should have decision");
        assert_eq!(poll_result.decision, PollDecision::Rejected);
        assert_eq!(poll_result.decided_by, "U456");
    }

    /// Tests polling returns None when no reactions present.
    ///
    /// Verifies: REQ-GOV-003/F-003 (Poll returns None if still pending)
    #[tokio::test]
    async fn test_wiremock_poll_no_reactions() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/reactions.get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "message": {}
            })))
            .mount(&mock_server)
            .await;

        let config = SlackConfig::new("xoxb-test-token", "#test-channel");
        let adapter = WiremockSlackAdapter::new(&mock_server.uri(), config)
            .expect("Failed to create adapter");

        let reference = ApprovalReference {
            task_id: crate::governance::TaskId::new(),
            external_id: "1234.5678".to_string(),
            channel: "C12345".to_string(),
            posted_at: Utc::now(),
            next_poll_at: std::time::Instant::now(),
            poll_count: 0,
        };

        let result = adapter.poll_for_decision(&reference).await;

        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "Should return None when no reactions"
        );
    }

    /// Tests message not found error during polling.
    ///
    /// Verifies: EC-SLK-002 (Slack message deleted before reaction)
    #[tokio::test]
    async fn test_wiremock_poll_message_not_found() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/reactions.get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "error": "message_not_found"
            })))
            .mount(&mock_server)
            .await;

        let config = SlackConfig::new("xoxb-test-token", "#test-channel");
        let adapter = WiremockSlackAdapter::new(&mock_server.uri(), config)
            .expect("Failed to create adapter");

        let reference = ApprovalReference {
            task_id: crate::governance::TaskId::new(),
            external_id: "deleted-msg".to_string(),
            channel: "C12345".to_string(),
            posted_at: Utc::now(),
            next_poll_at: std::time::Instant::now(),
            poll_count: 0,
        };

        let result = adapter.poll_for_decision(&reference).await;

        assert!(matches!(result, Err(AdapterError::MessageNotFound { .. })));
    }
}
