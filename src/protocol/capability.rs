//! SEP-1686 Capability Advertisement Types.
//!
//! Implements: REQ-CORE-007/F-001 (Capability Injection), F-002 (Tool Annotation)
//!
//! This module defines types for:
//! - Task capability declaration during `initialize`
//! - Tool annotation values for `taskSupport`
//! - Capability cache for upstream detection

use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

// ============================================================================
// Task Capability
// ============================================================================

/// Task capability declaration for `initialize` response.
///
/// Implements: REQ-CORE-007/§5.1
///
/// ```json
/// {
///   "capabilities": {
///     "tasks": {
///       "requests": {
///         "tools/call": true
///       }
///     }
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskCapability {
    /// Methods that support task mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requests: Option<TaskCapabilityRequests>,
}

impl TaskCapability {
    /// Creates a new task capability with tools/call support.
    #[must_use]
    pub fn new() -> Self {
        Self {
            requests: Some(TaskCapabilityRequests {
                tools_call: Some(true),
            }),
        }
    }

    /// Returns true if tools/call supports tasks.
    #[must_use]
    pub fn supports_tools_call(&self) -> bool {
        self.requests
            .as_ref()
            .and_then(|r| r.tools_call)
            .unwrap_or(false)
    }
}

/// Methods that support task mode.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskCapabilityRequests {
    /// Whether `tools/call` supports task metadata.
    #[serde(rename = "tools/call", skip_serializing_if = "Option::is_none")]
    pub tools_call: Option<bool>,
}

// ============================================================================
// Task Notifications Capability
// ============================================================================

/// Task notification capability for SSE support.
///
/// Implements: REQ-CORE-007/§6.1
///
/// ```json
/// {
///   "capabilities": {
///     "notifications": {
///       "tasks": {
///         "status": true
///       }
///     }
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskNotificationCapability {
    /// Whether task status notifications are supported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<bool>,
}

impl TaskNotificationCapability {
    /// Creates a new notification capability with status support.
    #[must_use]
    pub fn with_status() -> Self {
        Self { status: Some(true) }
    }

    /// Returns true if status notifications are supported.
    #[must_use]
    pub fn supports_status(&self) -> bool {
        self.status.unwrap_or(false)
    }
}

// ============================================================================
// Tool Annotation
// ============================================================================

/// Tool annotation for task support.
///
/// Implements: REQ-CORE-007/§5.1 (Tool Annotation Values)
///
/// | Value | Meaning | Client Behavior |
/// |-------|---------|-----------------|
/// | `forbidden` | Tool cannot be called with task metadata | Must NOT include `task` field |
/// | `optional` | Tool supports both sync and async | May include `task` field |
/// | `required` | Tool must be called with task metadata | Must include `task` field |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskSupportAnnotation {
    /// Tool cannot be called with task metadata.
    Forbidden,
    /// Tool supports both sync and async execution.
    Optional,
    /// Tool must be called with task metadata.
    Required,
}

impl TaskSupportAnnotation {
    /// Returns the wire format string.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::Optional => "optional",
            Self::Required => "required",
        }
    }
}

impl std::fmt::Display for TaskSupportAnnotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for TaskSupportAnnotation {
    type Err = ParseAnnotationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "forbidden" => Ok(Self::Forbidden),
            "optional" => Ok(Self::Optional),
            "required" => Ok(Self::Required),
            _ => Err(ParseAnnotationError(s.to_string())),
        }
    }
}

impl Serialize for TaskSupportAnnotation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TaskSupportAnnotation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Error when parsing an invalid annotation string.
#[derive(Debug, Clone, thiserror::Error)]
#[error("Invalid task support annotation: '{0}'")]
pub struct ParseAnnotationError(String);

// ============================================================================
// Tool Annotations
// ============================================================================

/// Tool annotations structure for tools/list response.
///
/// Implements: REQ-CORE-007/§6.2
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    /// Task support annotation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_support: Option<TaskSupportAnnotation>,

    /// Other annotations (preserved from upstream).
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl ToolAnnotations {
    /// Creates new annotations with task support.
    #[must_use]
    pub fn with_task_support(annotation: TaskSupportAnnotation) -> Self {
        Self {
            task_support: Some(annotation),
            other: Default::default(),
        }
    }

    /// Sets the task support annotation.
    #[must_use]
    pub fn set_task_support(mut self, annotation: TaskSupportAnnotation) -> Self {
        self.task_support = Some(annotation);
        self
    }
}

// ============================================================================
// Capability Cache
// ============================================================================

/// Cached state from initialize handshake.
///
/// Implements: REQ-CORE-007/§6.0
///
/// Thread-safe cache for upstream capability detection results.
#[derive(Debug)]
pub struct CapabilityCache {
    /// Whether upstream MCP server supports SEP-1686 tasks.
    upstream_supports_tasks: AtomicBool,

    /// Whether upstream MCP server supports SSE task notifications.
    upstream_supports_task_sse: AtomicBool,

    /// Timestamp of last initialize (for cache invalidation on reconnect).
    last_initialize: RwLock<Option<Instant>>,
}

impl CapabilityCache {
    /// Creates a new capability cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            upstream_supports_tasks: AtomicBool::new(false),
            upstream_supports_task_sse: AtomicBool::new(false),
            last_initialize: RwLock::new(None),
        }
    }

    /// Sets whether upstream supports tasks.
    ///
    /// Implements: REQ-CORE-007/F-001.2
    pub fn set_upstream_supports_tasks(&self, supports: bool) {
        self.upstream_supports_tasks
            .store(supports, Ordering::SeqCst);
        // Use poison-safe write to avoid panics if another thread panicked while holding the lock
        *self
            .last_initialize
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
    }

    /// Sets whether upstream supports SSE task notifications.
    ///
    /// Implements: REQ-CORE-007/F-001.3
    pub fn set_upstream_supports_task_sse(&self, supports: bool) {
        self.upstream_supports_task_sse
            .store(supports, Ordering::SeqCst);
    }

    /// Returns whether upstream supports tasks.
    ///
    /// Implements: REQ-CORE-007/§5.4
    #[must_use]
    pub fn upstream_supports_tasks(&self) -> bool {
        self.upstream_supports_tasks.load(Ordering::SeqCst)
    }

    /// Returns whether upstream supports SSE task notifications.
    #[must_use]
    pub fn upstream_supports_task_sse(&self) -> bool {
        self.upstream_supports_task_sse.load(Ordering::SeqCst)
    }

    /// Returns the time of the last initialize, if any.
    #[must_use]
    pub fn last_initialize(&self) -> Option<Instant> {
        // Use poison-safe read to avoid panics if another thread panicked while holding the lock
        *self
            .last_initialize
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Invalidates the cache (e.g., on upstream reconnect).
    pub fn invalidate(&self) {
        self.upstream_supports_tasks.store(false, Ordering::SeqCst);
        self.upstream_supports_task_sse
            .store(false, Ordering::SeqCst);
        // Use poison-safe write to avoid panics if another thread panicked while holding the lock
        *self
            .last_initialize
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

impl Default for CapabilityCache {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Task Validation Result
// ============================================================================

/// Result of validating task metadata against tool annotation.
///
/// Implements: REQ-CORE-007/§6.3
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskValidationResult {
    /// Task metadata present and valid.
    Valid {
        /// Validated TTL duration.
        ttl: std::time::Duration,
    },

    /// Task metadata missing but tool annotation is "required".
    MissingRequired {
        /// The tool name.
        tool: String,
    },

    /// Task metadata present but tool annotation is "forbidden".
    ForbiddenPresent {
        /// The tool name.
        tool: String,
    },

    /// No task metadata, tool allows sync execution.
    NotRequested,
}

impl TaskValidationResult {
    /// Returns true if the validation passed (Valid or NotRequested).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid { .. } | Self::NotRequested)
    }

    /// Returns true if this results in async execution.
    #[must_use]
    pub fn is_async(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // TaskCapability Tests
    // ========================================================================

    #[test]
    fn test_task_capability_serialization() {
        let cap = TaskCapability::new();
        let json = serde_json::to_string(&cap).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["requests"]["tools/call"], true);
    }

    #[test]
    fn test_task_capability_supports_tools_call() {
        let cap = TaskCapability::new();
        assert!(cap.supports_tools_call());

        let empty = TaskCapability::default();
        assert!(!empty.supports_tools_call());
    }

    // ========================================================================
    // TaskSupportAnnotation Tests
    // ========================================================================

    #[test]
    fn test_annotation_serialization() {
        assert_eq!(
            serde_json::to_string(&TaskSupportAnnotation::Forbidden).unwrap(),
            "\"forbidden\""
        );
        assert_eq!(
            serde_json::to_string(&TaskSupportAnnotation::Optional).unwrap(),
            "\"optional\""
        );
        assert_eq!(
            serde_json::to_string(&TaskSupportAnnotation::Required).unwrap(),
            "\"required\""
        );
    }

    #[test]
    fn test_annotation_deserialization() {
        assert_eq!(
            serde_json::from_str::<TaskSupportAnnotation>("\"forbidden\"").unwrap(),
            TaskSupportAnnotation::Forbidden
        );
        assert_eq!(
            serde_json::from_str::<TaskSupportAnnotation>("\"optional\"").unwrap(),
            TaskSupportAnnotation::Optional
        );
        assert_eq!(
            serde_json::from_str::<TaskSupportAnnotation>("\"required\"").unwrap(),
            TaskSupportAnnotation::Required
        );
    }

    #[test]
    fn test_annotation_invalid() {
        let result = serde_json::from_str::<TaskSupportAnnotation>("\"invalid\"");
        assert!(result.is_err());
    }

    // ========================================================================
    // ToolAnnotations Tests
    // ========================================================================

    #[test]
    fn test_tool_annotations_serialization() {
        let annotations = ToolAnnotations::with_task_support(TaskSupportAnnotation::Required);
        let json = serde_json::to_string(&annotations).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["taskSupport"], "required");
    }

    #[test]
    fn test_tool_annotations_with_other_fields() {
        let mut other = serde_json::Map::new();
        other.insert("readOnlyHint".to_string(), serde_json::json!(true));

        let annotations = ToolAnnotations {
            task_support: Some(TaskSupportAnnotation::Optional),
            other,
        };

        let json = serde_json::to_string(&annotations).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["taskSupport"], "optional");
        assert_eq!(parsed["readOnlyHint"], true);
    }

    // ========================================================================
    // CapabilityCache Tests
    // ========================================================================

    #[test]
    fn test_capability_cache_defaults() {
        let cache = CapabilityCache::new();
        assert!(!cache.upstream_supports_tasks());
        assert!(!cache.upstream_supports_task_sse());
        assert!(cache.last_initialize().is_none());
    }

    #[test]
    fn test_capability_cache_set_and_get() {
        let cache = CapabilityCache::new();

        cache.set_upstream_supports_tasks(true);
        assert!(cache.upstream_supports_tasks());
        assert!(cache.last_initialize().is_some());

        cache.set_upstream_supports_task_sse(true);
        assert!(cache.upstream_supports_task_sse());
    }

    #[test]
    fn test_capability_cache_invalidate() {
        let cache = CapabilityCache::new();
        cache.set_upstream_supports_tasks(true);
        cache.set_upstream_supports_task_sse(true);

        cache.invalidate();

        assert!(!cache.upstream_supports_tasks());
        assert!(!cache.upstream_supports_task_sse());
        assert!(cache.last_initialize().is_none());
    }

    // ========================================================================
    // TaskValidationResult Tests
    // ========================================================================

    #[test]
    fn test_validation_result_is_valid() {
        assert!(
            TaskValidationResult::Valid {
                ttl: std::time::Duration::from_secs(60)
            }
            .is_valid()
        );
        assert!(TaskValidationResult::NotRequested.is_valid());
        assert!(
            !TaskValidationResult::MissingRequired {
                tool: "test".to_string()
            }
            .is_valid()
        );
        assert!(
            !TaskValidationResult::ForbiddenPresent {
                tool: "test".to_string()
            }
            .is_valid()
        );
    }

    #[test]
    fn test_validation_result_is_async() {
        assert!(
            TaskValidationResult::Valid {
                ttl: std::time::Duration::from_secs(60)
            }
            .is_async()
        );
        assert!(!TaskValidationResult::NotRequested.is_async());
        assert!(
            !TaskValidationResult::MissingRequired {
                tool: "test".to_string()
            }
            .is_async()
        );
    }

    // ========================================================================
    // TaskNotificationCapability Tests
    // ========================================================================

    #[test]
    fn test_notification_capability_serialization() {
        let cap = TaskNotificationCapability::with_status();
        let json = serde_json::to_string(&cap).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["status"], true);
    }

    #[test]
    fn test_notification_capability_supports_status() {
        let cap = TaskNotificationCapability::with_status();
        assert!(cap.supports_status());

        let empty = TaskNotificationCapability::default();
        assert!(!empty.supports_status());
    }
}
