//! SEP-1686 Capability Advertisement Types.
//!
//! Implements: REQ-CORE-007/F-001 (Capability Injection)
//!
//! This module provides:
//! - Capability cache for upstream detection
//! - Capability extraction and injection helpers for `initialize` responses

use std::sync::atomic::{AtomicBool, Ordering};

// ============================================================================
// Capability Cache
// ============================================================================

/// Cached state from initialize handshake.
///
/// Implements: REQ-CORE-007/§6.0
///
/// Thread-safe cache for upstream capability detection results.
/// All fields are lock-free atomics to avoid blocking the Tokio runtime.
#[derive(Debug)]
pub struct CapabilityCache {
    /// Whether upstream MCP server supports SEP-1686 tasks.
    upstream_supports_tasks: AtomicBool,

    /// Whether upstream MCP server supports SSE task notifications.
    upstream_supports_task_sse: AtomicBool,

    /// Whether the initialize handshake has been completed at least once.
    has_initialized: AtomicBool,
}

impl CapabilityCache {
    /// Creates a new capability cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            upstream_supports_tasks: AtomicBool::new(false),
            upstream_supports_task_sse: AtomicBool::new(false),
            has_initialized: AtomicBool::new(false),
        }
    }

    /// Sets whether upstream supports tasks.
    ///
    /// Implements: REQ-CORE-007/F-001.2
    pub fn set_upstream_supports_tasks(&self, supports: bool) {
        self.upstream_supports_tasks
            .store(supports, Ordering::SeqCst);
        self.has_initialized.store(true, Ordering::Release);
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

    /// Returns whether the upstream has been initialized at least once.
    #[must_use]
    pub fn has_initialized(&self) -> bool {
        self.has_initialized.load(Ordering::Acquire)
    }

    /// Invalidates the cache (e.g., on upstream reconnect).
    pub fn invalidate(&self) {
        self.upstream_supports_tasks.store(false, Ordering::SeqCst);
        self.upstream_supports_task_sse
            .store(false, Ordering::SeqCst);
        self.has_initialized.store(false, Ordering::Release);
    }
}

impl Default for CapabilityCache {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Capability Extraction and Injection Helpers
// ============================================================================

/// Extract upstream task support from an initialize response.
///
/// Implements: REQ-CORE-007/F-001.2 (Upstream Capability Detection)
///
/// Checks for `result.capabilities.tasks.requests["tools/call"]` in the
/// initialize response to determine if upstream supports SEP-1686 tasks.
///
/// # Arguments
///
/// * `result` - The `result` field from an initialize JSON-RPC response
///
/// # Returns
///
/// `true` if upstream advertises task support for tools/call, `false` otherwise.
#[must_use]
pub fn extract_upstream_task_support(result: &serde_json::Value) -> bool {
    result
        .get("capabilities")
        .and_then(|c| c.get("tasks"))
        .and_then(|t| t.get("requests"))
        .and_then(|r| r.get("tools/call"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Extract upstream SSE notification support from an initialize response.
///
/// Implements: REQ-CORE-007/F-001.3 (Upstream Capability Detection)
///
/// Checks for `result.capabilities.notifications.tasks.status` in the
/// initialize response to determine if upstream supports SSE task notifications.
///
/// # Arguments
///
/// * `result` - The `result` field from an initialize JSON-RPC response
///
/// # Returns
///
/// `true` if upstream advertises SSE task status notifications, `false` otherwise.
#[must_use]
pub fn extract_upstream_sse_support(result: &serde_json::Value) -> bool {
    result
        .get("capabilities")
        .and_then(|c| c.get("notifications"))
        .and_then(|n| n.get("tasks"))
        .and_then(|t| t.get("status"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Inject ThoughtGate's task capability into an initialize response.
///
/// Implements: REQ-CORE-007/F-001.1 (Capability Injection)
///
/// Sets `result.capabilities.tasks.requests["tools/call"] = true` to advertise
/// that ThoughtGate supports SEP-1686 task-augmented tool calls.
///
/// This function creates missing intermediate objects as needed:
/// - `capabilities` object if missing
/// - `tasks` object if missing
/// - `requests` object if missing
///
/// # Arguments
///
/// * `result` - Mutable reference to the `result` field from an initialize response
pub fn inject_task_capability(result: &mut serde_json::Value) {
    let capabilities = result
        .as_object_mut()
        .map(|obj| {
            obj.entry("capabilities")
                .or_insert_with(|| serde_json::json!({}))
        })
        .and_then(|c| c.as_object_mut());

    if let Some(capabilities) = capabilities {
        let tasks = capabilities
            .entry("tasks")
            .or_insert_with(|| serde_json::json!({}));

        if let Some(tasks) = tasks.as_object_mut() {
            let requests = tasks
                .entry("requests")
                .or_insert_with(|| serde_json::json!({}));

            if let Some(requests) = requests.as_object_mut() {
                requests.insert("tools/call".to_string(), serde_json::json!(true));
            }
        }
    }
}

/// Inject SSE notification capability into an initialize response.
///
/// Implements: REQ-CORE-007/F-001.4 (Conditional SSE Advertisement)
///
/// Sets `result.capabilities.notifications.tasks.status = true` to advertise
/// that ThoughtGate supports SSE task status notifications.
///
/// This should only be called if upstream also supports SSE notifications,
/// to avoid advertising capabilities that cannot be fulfilled.
///
/// # Arguments
///
/// * `result` - Mutable reference to the `result` field from an initialize response
pub fn inject_sse_capability(result: &mut serde_json::Value) {
    let capabilities = result
        .as_object_mut()
        .map(|obj| {
            obj.entry("capabilities")
                .or_insert_with(|| serde_json::json!({}))
        })
        .and_then(|c| c.as_object_mut());

    if let Some(capabilities) = capabilities {
        let notifications = capabilities
            .entry("notifications")
            .or_insert_with(|| serde_json::json!({}));

        if let Some(notifications) = notifications.as_object_mut() {
            let tasks = notifications
                .entry("tasks")
                .or_insert_with(|| serde_json::json!({}));

            if let Some(tasks) = tasks.as_object_mut() {
                tasks.insert("status".to_string(), serde_json::json!(true));
            }
        }
    }
}

/// Strip SSE notification capability from an initialize response.
///
/// Used in v0.2 to ensure ThoughtGate does not advertise SSE capability
/// that it cannot fulfill (no SSE endpoint implemented yet).
///
/// This removes `result.capabilities.notifications.tasks.status` if present,
/// and cleans up empty parent objects.
///
/// # Arguments
///
/// * `result` - Mutable reference to the `result` field from an initialize response
pub fn strip_sse_capability(result: &mut serde_json::Value) {
    // Navigate to capabilities.notifications.tasks and remove "status"
    if let Some(capabilities) = result
        .get_mut("capabilities")
        .and_then(|c| c.as_object_mut())
    {
        if let Some(notifications) = capabilities
            .get_mut("notifications")
            .and_then(|n| n.as_object_mut())
        {
            if let Some(tasks) = notifications
                .get_mut("tasks")
                .and_then(|t| t.as_object_mut())
            {
                tasks.remove("status");

                // Clean up empty "tasks" object
                if tasks.is_empty() {
                    notifications.remove("tasks");
                }
            }

            // Clean up empty "notifications" object
            if notifications.is_empty() {
                capabilities.remove("notifications");
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // CapabilityCache Tests
    // ========================================================================

    #[test]
    fn test_capability_cache_defaults() {
        let cache = CapabilityCache::new();
        assert!(!cache.upstream_supports_tasks());
        assert!(!cache.upstream_supports_task_sse());
        assert!(!cache.has_initialized());
    }

    #[test]
    fn test_capability_cache_set_and_get() {
        let cache = CapabilityCache::new();

        cache.set_upstream_supports_tasks(true);
        assert!(cache.upstream_supports_tasks());
        assert!(cache.has_initialized());

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
        assert!(!cache.has_initialized());
    }

    // ========================================================================
    // Capability Extraction Tests
    // ========================================================================

    #[test]
    fn test_extract_upstream_task_support_present() {
        let result = serde_json::json!({
            "capabilities": {
                "tasks": {
                    "requests": {
                        "tools/call": true
                    }
                }
            }
        });
        assert!(super::extract_upstream_task_support(&result));
    }

    #[test]
    fn test_extract_upstream_task_support_false() {
        let result = serde_json::json!({
            "capabilities": {
                "tasks": {
                    "requests": {
                        "tools/call": false
                    }
                }
            }
        });
        assert!(!super::extract_upstream_task_support(&result));
    }

    #[test]
    fn test_extract_upstream_task_support_absent() {
        let result = serde_json::json!({
            "capabilities": {
                "tools": {}
            }
        });
        assert!(!super::extract_upstream_task_support(&result));
    }

    #[test]
    fn test_extract_upstream_task_support_empty() {
        let result = serde_json::json!({});
        assert!(!super::extract_upstream_task_support(&result));
    }

    #[test]
    fn test_extract_upstream_sse_support_present() {
        let result = serde_json::json!({
            "capabilities": {
                "notifications": {
                    "tasks": {
                        "status": true
                    }
                }
            }
        });
        assert!(super::extract_upstream_sse_support(&result));
    }

    #[test]
    fn test_extract_upstream_sse_support_absent() {
        let result = serde_json::json!({
            "capabilities": {
                "notifications": {}
            }
        });
        assert!(!super::extract_upstream_sse_support(&result));
    }

    // ========================================================================
    // Capability Injection Tests
    // ========================================================================

    #[test]
    fn test_inject_task_capability_empty() {
        let mut result = serde_json::json!({});
        super::inject_task_capability(&mut result);

        assert_eq!(
            result["capabilities"]["tasks"]["requests"]["tools/call"],
            true
        );
    }

    #[test]
    fn test_inject_task_capability_preserves_existing() {
        let mut result = serde_json::json!({
            "capabilities": {
                "tools": { "listChanged": true }
            },
            "serverInfo": { "name": "test" }
        });
        super::inject_task_capability(&mut result);

        // Task capability injected
        assert_eq!(
            result["capabilities"]["tasks"]["requests"]["tools/call"],
            true
        );
        // Existing capabilities preserved
        assert_eq!(result["capabilities"]["tools"]["listChanged"], true);
        assert_eq!(result["serverInfo"]["name"], "test");
    }

    #[test]
    fn test_inject_task_capability_overwrites_false() {
        let mut result = serde_json::json!({
            "capabilities": {
                "tasks": {
                    "requests": {
                        "tools/call": false
                    }
                }
            }
        });
        super::inject_task_capability(&mut result);

        assert_eq!(
            result["capabilities"]["tasks"]["requests"]["tools/call"],
            true
        );
    }

    #[test]
    fn test_inject_sse_capability() {
        let mut result = serde_json::json!({});
        super::inject_sse_capability(&mut result);

        assert_eq!(
            result["capabilities"]["notifications"]["tasks"]["status"],
            true
        );
    }

    #[test]
    fn test_inject_sse_capability_preserves_existing() {
        let mut result = serde_json::json!({
            "capabilities": {
                "notifications": {
                    "progress": true
                }
            }
        });
        super::inject_sse_capability(&mut result);

        // SSE capability injected
        assert_eq!(
            result["capabilities"]["notifications"]["tasks"]["status"],
            true
        );
        // Existing notifications preserved
        assert_eq!(result["capabilities"]["notifications"]["progress"], true);
    }

    #[test]
    fn test_inject_both_capabilities() {
        let mut result = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": {
                "name": "upstream-server",
                "version": "1.0.0"
            }
        });

        super::inject_task_capability(&mut result);
        super::inject_sse_capability(&mut result);

        // Both capabilities present
        assert_eq!(
            result["capabilities"]["tasks"]["requests"]["tools/call"],
            true
        );
        assert_eq!(
            result["capabilities"]["notifications"]["tasks"]["status"],
            true
        );
        // Original fields preserved
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "upstream-server");
    }

    // ========================================================================
    // Strip SSE Capability Tests
    // ========================================================================

    #[test]
    fn test_strip_sse_capability() {
        let mut result = serde_json::json!({
            "capabilities": {
                "notifications": {
                    "tasks": {
                        "status": true
                    }
                }
            }
        });

        super::strip_sse_capability(&mut result);

        // SSE should be removed
        assert!(result["capabilities"]["notifications"]["tasks"]["status"].is_null());
        // Empty objects should be cleaned up
        assert!(result["capabilities"]["notifications"].is_null());
    }

    #[test]
    fn test_strip_sse_capability_preserves_other_notifications() {
        let mut result = serde_json::json!({
            "capabilities": {
                "notifications": {
                    "tasks": {
                        "status": true
                    },
                    "progress": true
                }
            }
        });

        super::strip_sse_capability(&mut result);

        // SSE should be removed
        assert!(result["capabilities"]["notifications"]["tasks"].is_null());
        // Other notifications preserved
        assert_eq!(result["capabilities"]["notifications"]["progress"], true);
    }

    #[test]
    fn test_strip_sse_capability_no_sse_present() {
        let mut result = serde_json::json!({
            "capabilities": {
                "tasks": {
                    "requests": {
                        "tools/call": true
                    }
                }
            }
        });

        // Should not panic or modify other capabilities
        super::strip_sse_capability(&mut result);

        // Task capability unchanged
        assert_eq!(
            result["capabilities"]["tasks"]["requests"]["tools/call"],
            true
        );
    }

    #[test]
    fn test_strip_sse_capability_empty_result() {
        let mut result = serde_json::json!({});

        // Should not panic on empty object
        super::strip_sse_capability(&mut result);

        assert!(result.as_object().unwrap().is_empty());
    }
}
