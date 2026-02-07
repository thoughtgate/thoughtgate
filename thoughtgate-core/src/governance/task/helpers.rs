//! Task helper functions: hashing, canonical JSON, poll interval.
//!
//! Implements: REQ-GOV-001/F-002

use sha2::{Digest, Sha256};
use std::time::Duration;

use super::types::ToolCallRequest;

// ============================================================================
// Helper Functions
// ============================================================================

/// Computes a SHA256 hash of the request for integrity verification.
///
/// Implements: REQ-GOV-001/F-002.3
///
/// The hash includes the MCP method, tool name, and arguments, intentionally
/// excluding the `mcp_request_id`. This is because:
/// - The request ID is transport-layer metadata for JSON-RPC correlation
/// - The hash verifies the *semantic content* of what was approved
/// - Two requests with identical method+tool+arguments perform the same action
///   regardless of their request IDs
/// - Including `method` prevents collisions between different MCP operations
///   (e.g., `tools/call` vs `resources/read`) with the same name/arguments
///
/// Arguments are serialized using canonical JSON (sorted keys) to ensure
/// deterministic hashing regardless of key insertion order. This is necessary
/// because `serde_json/preserve_order` is enabled transitively by
/// `cedar-policy-core`, making `Value::to_string()` insertion-order dependent.
/// Returns an error if arguments exceed maximum nesting depth.
///
/// Implements: REQ-GOV-001/F-002
pub fn hash_request(request: &ToolCallRequest) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(request.method.as_bytes());
    hasher.update(request.name.as_bytes());
    hasher.update(canonical_json(&request.arguments)?.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

/// Maximum recursion depth for canonical JSON serialization.
/// Matches `MAX_JSON_DEPTH` in `policy/engine.rs` for consistency.
const MAX_CANONICAL_JSON_DEPTH: usize = 64;

/// Produces a canonical JSON string with object keys sorted alphabetically.
///
/// This ensures deterministic serialization regardless of the key insertion
/// order used by `serde_json::Value` (which depends on whether `preserve_order`
/// is enabled via feature flags).
///
/// Depth is capped at [`MAX_CANONICAL_JSON_DEPTH`] to prevent stack overflow
/// from malicious deeply nested payloads. Returns an error if depth is exceeded
/// rather than silently producing a sentinel that could cause hash collisions.
pub(super) fn canonical_json(value: &serde_json::Value) -> Result<String, String> {
    canonical_json_inner(value, 0)
}

fn canonical_json_inner(value: &serde_json::Value, depth: usize) -> Result<String, String> {
    if depth > MAX_CANONICAL_JSON_DEPTH {
        return Err(format!(
            "JSON nesting depth exceeds maximum of {MAX_CANONICAL_JSON_DEPTH}"
        ));
    }

    match value {
        serde_json::Value::Object(map) => {
            let mut sorted: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            sorted.sort_by_key(|(k, _)| *k);
            let mut entries = Vec::with_capacity(sorted.len());
            for (k, v) in sorted {
                let key_str = serde_json::to_string(k).unwrap_or_default();
                entries.push(format!(
                    "{}:{}",
                    key_str,
                    canonical_json_inner(v, depth + 1)?
                ));
            }
            Ok(format!("{{{}}}", entries.join(",")))
        }
        serde_json::Value::Array(arr) => {
            let mut items = Vec::with_capacity(arr.len());
            for v in arr {
                items.push(canonical_json_inner(v, depth + 1)?);
            }
            Ok(format!("[{}]", items.join(",")))
        }
        // Leaf values (strings, numbers, bools, null) serialize deterministically
        other => Ok(serde_json::to_string(other).unwrap_or_default()),
    }
}

/// Computes the suggested poll interval based on remaining TTL.
///
/// Implements: REQ-GOV-001/F-002.7
///
/// More frequent polling as expiration approaches:
/// - Last minute: poll every 2s
/// - Last 5 min: poll every 5s
/// - Last 15 min: poll every 10s
/// - Otherwise: poll every 30s
pub(super) fn compute_poll_interval(remaining_ttl: Duration) -> Duration {
    let secs = remaining_ttl.as_secs();
    match secs {
        0..=60 => Duration::from_secs(2),
        61..=300 => Duration::from_secs(5),
        301..=900 => Duration::from_secs(10),
        _ => Duration::from_secs(30),
    }
}
