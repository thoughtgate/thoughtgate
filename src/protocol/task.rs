//! SEP-1686 Task Types.
//!
//! Implements: REQ-CORE-007/§6.5 (Task ID Format), §6.6 (Task Response Formats)
//!
//! This module defines the core task types for SEP-1686 protocol compliance:
//! - `Sep1686TaskId`: Task identifier with `tg_` prefix for ThoughtGate ownership
//! - `Sep1686Status`: SEP-1686 compliant status enum
//! - `Sep1686TaskMetadata`: Task metadata for API responses

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// ============================================================================
// Task ID
// ============================================================================

/// SEP-1686 compliant task identifier.
///
/// Implements: REQ-CORE-007/§6.5
///
/// Format: `tg_<nanoid>` where nanoid is 21 alphanumeric characters.
/// Example: `tg_V1StGXR8_Z5jdHi6B-myT`
///
/// The `tg_` prefix indicates ThoughtGate ownership. Task IDs without this
/// prefix are assumed to be upstream-owned and should be passed through.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sep1686TaskId(String);

/// Prefix for ThoughtGate-owned task IDs.
pub const TASK_ID_PREFIX: &str = "tg_";

/// Length of the nanoid body (excluding prefix).
pub const TASK_ID_BODY_LENGTH: usize = 21;

impl Sep1686TaskId {
    /// Creates a new random task ID with `tg_` prefix.
    ///
    /// Implements: REQ-CORE-007/§6.5
    ///
    /// The generated ID uses nanoid with 21 characters, which provides
    /// ~1 billion IDs before 1% collision probability.
    #[must_use]
    pub fn new() -> Self {
        let body = nanoid::nanoid!(TASK_ID_BODY_LENGTH);
        Self(format!("{}{}", TASK_ID_PREFIX, body))
    }

    /// Creates a task ID from a raw string without validation.
    ///
    /// Use this for upstream task IDs that don't have the `tg_` prefix.
    #[must_use]
    pub fn from_raw(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns true if this is a ThoughtGate-owned task ID.
    ///
    /// Implements: REQ-CORE-007/F-006.3
    #[must_use]
    pub fn is_thoughtgate_owned(&self) -> bool {
        self.0.starts_with(TASK_ID_PREFIX)
    }

    /// Returns the raw string value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Sep1686TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Sep1686TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Sep1686TaskId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

impl Serialize for Sep1686TaskId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Sep1686TaskId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self(s))
    }
}

// ============================================================================
// Task Status
// ============================================================================

/// SEP-1686 compliant task status.
///
/// Implements: REQ-CORE-007/§6.6
///
/// These are the status values defined by SEP-1686:
/// - `working`: Task is actively being processed
/// - `input_required`: Task is waiting for external input (approval)
/// - `completed`: Task finished successfully
/// - `failed`: Task failed with an error
/// - `cancelled`: Task was cancelled by the client
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sep1686Status {
    /// Task is actively being processed.
    Working,
    /// Task is waiting for external input (e.g., human approval).
    InputRequired,
    /// Task completed successfully.
    Completed,
    /// Task failed with an error.
    Failed,
    /// Task was cancelled by the client.
    Cancelled,
}

impl Sep1686Status {
    /// Returns true if this is a terminal state.
    ///
    /// Terminal states indicate the task lifecycle has ended.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Returns the SEP-1686 wire format string.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::InputRequired => "input_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for Sep1686Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for Sep1686Status {
    type Err = ParseStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "working" => Ok(Self::Working),
            "input_required" => Ok(Self::InputRequired),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(ParseStatusError(s.to_string())),
        }
    }
}

impl Serialize for Sep1686Status {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Sep1686Status {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Error when parsing an invalid status string.
#[derive(Debug, Clone, thiserror::Error)]
#[error("Invalid SEP-1686 status: '{0}'")]
pub struct ParseStatusError(String);

// ============================================================================
// Task Metadata
// ============================================================================

/// SEP-1686 task metadata for API responses.
///
/// Implements: REQ-CORE-007/§6.6
///
/// This structure is returned by `tools/call` when task metadata is present,
/// and by `tasks/get` for status queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sep1686TaskMetadata {
    /// Unique task identifier.
    pub task_id: Sep1686TaskId,

    /// Current task status.
    pub status: Sep1686Status,

    /// Optional human-readable status message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,

    /// Suggested poll interval in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_interval: Option<u64>,
}

impl Sep1686TaskMetadata {
    /// Creates new task metadata.
    #[must_use]
    pub fn new(task_id: Sep1686TaskId, status: Sep1686Status) -> Self {
        Self {
            task_id,
            status,
            status_message: None,
            poll_interval: None,
        }
    }

    /// Sets the status message.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.status_message = Some(message.into());
        self
    }

    /// Sets the poll interval in milliseconds.
    #[must_use]
    pub fn with_poll_interval(mut self, interval_ms: u64) -> Self {
        self.poll_interval = Some(interval_ms);
        self
    }
}

// ============================================================================
// Task Result
// ============================================================================

/// Result of a completed task.
///
/// Implements: REQ-CORE-007/§6.6 (`tasks/result` response)
///
/// Contains either the successful tool result or an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sep1686TaskResult {
    /// The tool call result content.
    pub content: serde_json::Value,

    /// Whether this is an error result.
    #[serde(default)]
    pub is_error: bool,
}

impl Sep1686TaskResult {
    /// Creates a successful result.
    #[must_use]
    pub fn success(content: serde_json::Value) -> Self {
        Self {
            content,
            is_error: false,
        }
    }

    /// Creates an error result.
    #[must_use]
    pub fn error(content: serde_json::Value) -> Self {
        Self {
            content,
            is_error: true,
        }
    }
}

// ============================================================================
// Task List Entry
// ============================================================================

/// Entry in a task list response.
///
/// Implements: REQ-CORE-007/§6.6 (`tasks/list` response)
///
/// A condensed view of task metadata for list operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sep1686TaskListEntry {
    /// Unique task identifier.
    pub task_id: Sep1686TaskId,

    /// Current task status.
    pub status: Sep1686Status,

    /// When the task was created.
    pub created_at: DateTime<Utc>,

    /// The tool that was called.
    pub tool_name: String,
}

impl Sep1686TaskListEntry {
    /// Creates a new task list entry.
    #[must_use]
    pub fn new(
        task_id: Sep1686TaskId,
        status: Sep1686Status,
        created_at: DateTime<Utc>,
        tool_name: impl Into<String>,
    ) -> Self {
        Self {
            task_id,
            status,
            created_at,
            tool_name: tool_name.into(),
        }
    }
}

// ============================================================================
// Task Request Metadata
// ============================================================================

/// Task metadata included in `tools/call` requests.
///
/// Implements: REQ-CORE-007/§5.1 (Task Metadata Structure)
///
/// When a client sends a `tools/call` with task metadata, it opts into
/// async execution mode.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Sep1686TaskRequest {
    /// Time-to-live in milliseconds for the task.
    #[serde(default)]
    pub ttl: Option<u64>,
}

impl Sep1686TaskRequest {
    /// Creates task request metadata with the given TTL.
    #[must_use]
    pub fn with_ttl(ttl_ms: u64) -> Self {
        Self { ttl: Some(ttl_ms) }
    }

    /// Returns the TTL as a Duration, or None if not specified.
    #[must_use]
    pub fn ttl_duration(&self) -> Option<std::time::Duration> {
        self.ttl.map(std::time::Duration::from_millis)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // TaskId Tests
    // ========================================================================

    /// Tests that new task IDs have the correct format.
    ///
    /// Verifies: REQ-CORE-007/§6.5
    #[test]
    fn test_task_id_format() {
        let id = Sep1686TaskId::new();
        let s = id.as_str();

        // Must start with tg_ prefix
        assert!(
            s.starts_with(TASK_ID_PREFIX),
            "ID should start with 'tg_': {}",
            s
        );

        // Total length should be prefix + body
        assert_eq!(
            s.len(),
            TASK_ID_PREFIX.len() + TASK_ID_BODY_LENGTH,
            "ID length should be {}: {}",
            TASK_ID_PREFIX.len() + TASK_ID_BODY_LENGTH,
            s
        );

        // Should be alphanumeric with allowed special chars
        let body = &s[TASK_ID_PREFIX.len()..];
        assert!(
            body.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "Body should be alphanumeric: {}",
            body
        );
    }

    /// Tests task ID ownership detection.
    ///
    /// Verifies: REQ-CORE-007/F-006.3
    #[test]
    fn test_task_id_ownership() {
        let tg_id = Sep1686TaskId::new();
        assert!(tg_id.is_thoughtgate_owned());

        let upstream_id = Sep1686TaskId::from_raw("abc-123-def");
        assert!(!upstream_id.is_thoughtgate_owned());

        let upstream_uuid = Sep1686TaskId::from_raw("550e8400-e29b-41d4-a716-446655440000");
        assert!(!upstream_uuid.is_thoughtgate_owned());
    }

    /// Tests task ID serialization round-trip.
    #[test]
    fn test_task_id_serialization() {
        let id = Sep1686TaskId::new();
        let json = serde_json::to_string(&id).unwrap();

        // Should serialize as a plain string
        assert!(json.starts_with('"'));
        assert!(json.ends_with('"'));
        assert!(json.contains(TASK_ID_PREFIX));

        // Round-trip
        let parsed: Sep1686TaskId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, parsed);
    }

    /// Tests task ID uniqueness.
    #[test]
    fn test_task_id_uniqueness() {
        let ids: Vec<Sep1686TaskId> = (0..100).map(|_| Sep1686TaskId::new()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len(), "All IDs should be unique");
    }

    // ========================================================================
    // TaskStatus Tests
    // ========================================================================

    /// Tests status serialization matches SEP-1686.
    ///
    /// Verifies: REQ-CORE-007/§6.6
    #[test]
    fn test_status_serialization() {
        assert_eq!(
            serde_json::to_string(&Sep1686Status::Working).unwrap(),
            "\"working\""
        );
        assert_eq!(
            serde_json::to_string(&Sep1686Status::InputRequired).unwrap(),
            "\"input_required\""
        );
        assert_eq!(
            serde_json::to_string(&Sep1686Status::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&Sep1686Status::Failed).unwrap(),
            "\"failed\""
        );
        assert_eq!(
            serde_json::to_string(&Sep1686Status::Cancelled).unwrap(),
            "\"cancelled\""
        );
    }

    /// Tests status deserialization.
    #[test]
    fn test_status_deserialization() {
        assert_eq!(
            serde_json::from_str::<Sep1686Status>("\"working\"").unwrap(),
            Sep1686Status::Working
        );
        assert_eq!(
            serde_json::from_str::<Sep1686Status>("\"input_required\"").unwrap(),
            Sep1686Status::InputRequired
        );
        assert_eq!(
            serde_json::from_str::<Sep1686Status>("\"completed\"").unwrap(),
            Sep1686Status::Completed
        );
        assert_eq!(
            serde_json::from_str::<Sep1686Status>("\"failed\"").unwrap(),
            Sep1686Status::Failed
        );
        assert_eq!(
            serde_json::from_str::<Sep1686Status>("\"cancelled\"").unwrap(),
            Sep1686Status::Cancelled
        );
    }

    /// Tests invalid status deserialization.
    #[test]
    fn test_status_deserialization_invalid() {
        let result = serde_json::from_str::<Sep1686Status>("\"invalid\"");
        assert!(result.is_err());
    }

    /// Tests terminal status detection.
    #[test]
    fn test_status_is_terminal() {
        assert!(!Sep1686Status::Working.is_terminal());
        assert!(!Sep1686Status::InputRequired.is_terminal());
        assert!(Sep1686Status::Completed.is_terminal());
        assert!(Sep1686Status::Failed.is_terminal());
        assert!(Sep1686Status::Cancelled.is_terminal());
    }

    // ========================================================================
    // TaskMetadata Tests
    // ========================================================================

    /// Tests task metadata serialization matches SEP-1686.
    ///
    /// Verifies: REQ-CORE-007/§6.6
    #[test]
    fn test_task_metadata_serialization() {
        let metadata = Sep1686TaskMetadata::new(
            Sep1686TaskId::from_raw("tg_test123456789012345"),
            Sep1686Status::InputRequired,
        )
        .with_message("Awaiting approval")
        .with_poll_interval(5000);

        let json = serde_json::to_string(&metadata).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["taskId"], "tg_test123456789012345");
        assert_eq!(parsed["status"], "input_required");
        assert_eq!(parsed["statusMessage"], "Awaiting approval");
        assert_eq!(parsed["pollInterval"], 5000);
    }

    /// Tests task metadata without optional fields.
    #[test]
    fn test_task_metadata_minimal() {
        let metadata =
            Sep1686TaskMetadata::new(Sep1686TaskId::from_raw("tg_abc"), Sep1686Status::Working);

        let json = serde_json::to_string(&metadata).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["taskId"], "tg_abc");
        assert_eq!(parsed["status"], "working");
        assert!(parsed.get("statusMessage").is_none());
        assert!(parsed.get("pollInterval").is_none());
    }

    /// Tests task metadata round-trip.
    #[test]
    fn test_task_metadata_roundtrip() {
        let original = Sep1686TaskMetadata::new(Sep1686TaskId::new(), Sep1686Status::Completed)
            .with_message("Done")
            .with_poll_interval(1000);

        let json = serde_json::to_string(&original).unwrap();
        let parsed: Sep1686TaskMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(original.task_id, parsed.task_id);
        assert_eq!(original.status, parsed.status);
        assert_eq!(original.status_message, parsed.status_message);
        assert_eq!(original.poll_interval, parsed.poll_interval);
    }

    // ========================================================================
    // TaskResult Tests
    // ========================================================================

    /// Tests task result serialization.
    #[test]
    fn test_task_result_success() {
        let result = Sep1686TaskResult::success(serde_json::json!({"data": "value"}));

        let json = serde_json::to_string(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["content"]["data"], "value");
        assert_eq!(parsed["isError"], false);
    }

    /// Tests task result error.
    #[test]
    fn test_task_result_error() {
        let result = Sep1686TaskResult::error(serde_json::json!({"error": "Something went wrong"}));

        let json = serde_json::to_string(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["content"]["error"], "Something went wrong");
        assert_eq!(parsed["isError"], true);
    }

    // ========================================================================
    // TaskListEntry Tests
    // ========================================================================

    /// Tests task list entry serialization.
    #[test]
    fn test_task_list_entry_serialization() {
        let entry = Sep1686TaskListEntry::new(
            Sep1686TaskId::from_raw("tg_test123"),
            Sep1686Status::InputRequired,
            chrono::Utc::now(),
            "delete_user",
        );

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["taskId"], "tg_test123");
        assert_eq!(parsed["status"], "input_required");
        assert_eq!(parsed["toolName"], "delete_user");
        assert!(parsed["createdAt"].is_string());
    }

    // ========================================================================
    // TaskRequest Tests
    // ========================================================================

    /// Tests task request metadata.
    #[test]
    fn test_task_request_with_ttl() {
        let req = Sep1686TaskRequest::with_ttl(600000);
        assert_eq!(
            req.ttl_duration(),
            Some(std::time::Duration::from_millis(600000))
        );

        let json = serde_json::to_string(&req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ttl"], 600000);
    }

    /// Tests task request without TTL.
    #[test]
    fn test_task_request_default() {
        let req = Sep1686TaskRequest::default();
        assert_eq!(req.ttl_duration(), None);
    }
}
