//! W3C Trace Context serialization/deserialization helpers.
//!
//! This module provides utilities for serializing and deserializing OpenTelemetry
//! span contexts to/from W3C traceparent format. This is necessary for propagating
//! trace context across async boundaries like Slack message metadata.
//!
//! # Traceability
//! - Implements: REQ-OBS-002 §7.4 (Async HITL Boundary Propagation)
//! - Implements: REQ-OBS-002 §7.4.1 (Context Storage at Dispatch)
//! - Implements: REQ-OBS-002 §7.4.2 (Context Restoration at Callback)

use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};
use serde::{Deserialize, Serialize};
use tracing::warn;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// Serialized W3C trace context for storage in external systems (e.g., Slack metadata).
///
/// Format follows W3C Trace Context specification:
/// - `traceparent`: `{version}-{trace_id}-{span_id}-{trace_flags}`
///   - version: always "00" for current spec
///   - trace_id: 32 hex characters (16 bytes)
///   - span_id: 16 hex characters (8 bytes)
///   - trace_flags: 2 hex characters ("01" = sampled)
/// - `tracestate`: optional vendor-specific key-value pairs
///
/// Implements: REQ-OBS-002 §7.4.1
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SerializedTraceContext {
    /// W3C traceparent header value (e.g., "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
    pub traceparent: String,
    /// Optional W3C tracestate header value (vendor-specific key-value pairs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
}

/// Result of deserializing a trace context.
///
/// Implements: REQ-OBS-002 §7.4.2
#[derive(Debug)]
pub struct DeserializedContext {
    /// The parsed trace ID (16 bytes)
    pub trace_id: TraceId,
    /// The parsed span ID (8 bytes)
    pub span_id: SpanId,
    /// The parsed trace flags
    pub trace_flags: TraceFlags,
    /// The parsed trace state (if any)
    pub trace_state: TraceState,
    /// Whether context was successfully recovered (false if corrupted/missing)
    pub recovered: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Serialization
// ─────────────────────────────────────────────────────────────────────────────

/// Serialize a SpanContext to W3C traceparent format for storage.
///
/// Converts an OpenTelemetry SpanContext into a serializable format that can
/// be stored in external systems like Slack's private_metadata field.
///
/// # Arguments
///
/// * `ctx` - The OpenTelemetry SpanContext to serialize
///
/// # Returns
///
/// A `SerializedTraceContext` containing the W3C-formatted traceparent string.
///
/// # Example
///
/// ```ignore
/// let span_ctx = span.span_context();
/// let serialized = serialize_span_context(&span_ctx);
/// // Store serialized.traceparent in Slack metadata
/// ```
///
/// Implements: REQ-OBS-002 §7.4.1
pub fn serialize_span_context(ctx: &SpanContext) -> SerializedTraceContext {
    // Format: {version}-{trace_id}-{span_id}-{trace_flags}
    // - version: "00" (W3C Trace Context version 1)
    // - trace_id: 32 lowercase hex characters
    // - span_id: 16 lowercase hex characters
    // - trace_flags: 2 lowercase hex characters (01 = sampled)
    let traceparent = format!(
        "00-{}-{}-{:02x}",
        ctx.trace_id(),
        ctx.span_id(),
        ctx.trace_flags().to_u8()
    );

    let tracestate = if ctx.trace_state().header().is_empty() {
        None
    } else {
        Some(ctx.trace_state().header().to_string())
    };

    SerializedTraceContext {
        traceparent,
        tracestate,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Deserialization
// ─────────────────────────────────────────────────────────────────────────────

/// Deserialize a W3C traceparent string back to its components.
///
/// Parses a serialized trace context and extracts the trace ID, span ID,
/// and trace flags. Returns None if the format is invalid.
///
/// # Arguments
///
/// * `serialized` - The SerializedTraceContext to parse
///
/// # Returns
///
/// - `Some(DeserializedContext)` with `recovered=true` if parsing succeeded
/// - `Some(DeserializedContext)` with `recovered=false` if parsing failed (new trace started)
///
/// # Graceful Degradation
///
/// Per REQ-OBS-002 §7.4.2, if the trace context is corrupted or missing,
/// this function generates a new trace ID and returns it with `recovered=false`.
/// This ensures the approval workflow completes even if trace continuity is lost.
///
/// # Example
///
/// ```ignore
/// let serialized = SerializedTraceContext {
///     traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
///     tracestate: None,
/// };
/// if let Some(ctx) = deserialize_span_context(&serialized) {
///     if ctx.recovered {
///         // Successfully restored original trace
///     } else {
///         // Started new trace due to corruption
///     }
/// }
/// ```
///
/// Implements: REQ-OBS-002 §7.4.2
pub fn deserialize_span_context(serialized: &SerializedTraceContext) -> DeserializedContext {
    match parse_traceparent(&serialized.traceparent) {
        Some((trace_id, span_id, trace_flags)) => {
            let trace_state = serialized
                .tracestate
                .as_ref()
                .and_then(|ts| TraceState::from_key_value(parse_tracestate(ts)).ok())
                .unwrap_or_default();

            DeserializedContext {
                trace_id,
                span_id,
                trace_flags,
                trace_state,
                recovered: true,
            }
        }
        None => {
            warn!(
                traceparent = %serialized.traceparent,
                "Failed to parse trace context, starting new trace"
            );
            // Generate new trace context for graceful degradation
            DeserializedContext {
                trace_id: generate_trace_id(),
                span_id: generate_span_id(),
                trace_flags: TraceFlags::SAMPLED,
                trace_state: TraceState::default(),
                recovered: false,
            }
        }
    }
}

/// Parse a W3C traceparent string into its components.
///
/// # Format
///
/// `{version}-{trace_id}-{span_id}-{trace_flags}`
/// - version: 2 hex chars (must be "00")
/// - trace_id: 32 hex chars (16 bytes)
/// - span_id: 16 hex chars (8 bytes)
/// - trace_flags: 2 hex chars
///
/// # Returns
///
/// `Some((TraceId, SpanId, TraceFlags))` if valid, `None` otherwise.
fn parse_traceparent(traceparent: &str) -> Option<(TraceId, SpanId, TraceFlags)> {
    let parts: Vec<&str> = traceparent.split('-').collect();
    if parts.len() != 4 {
        return None;
    }

    // Version must be "00"
    if parts[0] != "00" {
        return None;
    }

    // Parse trace_id (32 hex chars = 16 bytes)
    if parts[1].len() != 32 {
        return None;
    }
    let trace_id_bytes = hex_to_bytes(parts[1])?;
    if trace_id_bytes.len() != 16 {
        return None;
    }
    let trace_id = TraceId::from_bytes(trace_id_bytes.try_into().ok()?);

    // Reject invalid (all-zero) trace ID
    if trace_id == TraceId::INVALID {
        return None;
    }

    // Parse span_id (16 hex chars = 8 bytes)
    if parts[2].len() != 16 {
        return None;
    }
    let span_id_bytes = hex_to_bytes(parts[2])?;
    if span_id_bytes.len() != 8 {
        return None;
    }
    let span_id = SpanId::from_bytes(span_id_bytes.try_into().ok()?);

    // Reject invalid (all-zero) span ID
    if span_id == SpanId::INVALID {
        return None;
    }

    // Parse trace_flags (2 hex chars = 1 byte)
    if parts[3].len() != 2 {
        return None;
    }
    let flags_byte = u8::from_str_radix(parts[3], 16).ok()?;
    let trace_flags = TraceFlags::new(flags_byte);

    Some((trace_id, span_id, trace_flags))
}

/// Parse W3C tracestate header into key-value pairs.
///
/// Format: `key1=value1,key2=value2`
fn parse_tracestate(tracestate: &str) -> Vec<(String, String)> {
    tracestate
        .split(',')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?.trim().to_string();
            let value = parts.next()?.trim().to_string();
            if key.is_empty() {
                None
            } else {
                Some((key, value))
            }
        })
        .collect()
}

/// Convert a hex string to bytes.
fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }

    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// Generate a new random trace ID using cryptographically secure randomness.
///
/// Uses `rand::thread_rng()` which provides a thread-local CSPRNG seeded from
/// the operating system's entropy source. This ensures globally unique trace IDs
/// per W3C Trace Context specification requirements.
///
/// Implements: REQ-OBS-002 §7.4.2 (graceful degradation with new trace)
fn generate_trace_id() -> TraceId {
    use rand::Rng;
    TraceId::from_bytes(rand::rng().random())
}

/// Generate a new random span ID using cryptographically secure randomness.
///
/// Uses `rand::thread_rng()` which provides a thread-local CSPRNG seeded from
/// the operating system's entropy source.
///
/// Implements: REQ-OBS-002 §7.4.2 (graceful degradation with new trace)
fn generate_span_id() -> SpanId {
    use rand::Rng;
    SpanId::from_bytes(rand::rng().random())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_and_deserialize_roundtrip() {
        // Create a known SpanContext
        let trace_id = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap();
        let span_id = SpanId::from_hex("00f067aa0ba902b7").unwrap();
        let trace_flags = TraceFlags::SAMPLED;
        let trace_state = TraceState::default();

        let ctx = SpanContext::new(trace_id, span_id, trace_flags, true, trace_state);

        // Serialize
        let serialized = serialize_span_context(&ctx);
        assert_eq!(
            serialized.traceparent,
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        );

        // Deserialize
        let deserialized = deserialize_span_context(&serialized);
        assert!(deserialized.recovered);
        assert_eq!(deserialized.trace_id, trace_id);
        assert_eq!(deserialized.span_id, span_id);
        assert_eq!(deserialized.trace_flags, trace_flags);
    }

    #[test]
    fn test_serialize_with_tracestate() {
        let trace_id = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap();
        let span_id = SpanId::from_hex("00f067aa0ba902b7").unwrap();
        let trace_state =
            TraceState::from_key_value(vec![("vendor".to_string(), "value".to_string())]).unwrap();

        let ctx = SpanContext::new(trace_id, span_id, TraceFlags::SAMPLED, true, trace_state);

        let serialized = serialize_span_context(&ctx);
        assert!(serialized.tracestate.is_some());
        assert!(serialized.tracestate.unwrap().contains("vendor=value"));
    }

    #[test]
    fn test_deserialize_invalid_traceparent_graceful_degradation() {
        let invalid_contexts = vec![
            SerializedTraceContext {
                traceparent: "invalid".to_string(),
                tracestate: None,
            },
            SerializedTraceContext {
                traceparent: "00-tooshort-00f067aa0ba902b7-01".to_string(),
                tracestate: None,
            },
            SerializedTraceContext {
                traceparent: "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(), // wrong version
                tracestate: None,
            },
            SerializedTraceContext {
                traceparent: "00-00000000000000000000000000000000-00f067aa0ba902b7-01".to_string(), // invalid trace_id
                tracestate: None,
            },
            SerializedTraceContext {
                traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01".to_string(), // invalid span_id
                tracestate: None,
            },
        ];

        for invalid in invalid_contexts {
            let result = deserialize_span_context(&invalid);
            assert!(
                !result.recovered,
                "Should not recover from invalid context: {}",
                invalid.traceparent
            );
            // Should still produce valid (new) IDs
            assert_ne!(result.trace_id, TraceId::INVALID);
            assert_ne!(result.span_id, SpanId::INVALID);
        }
    }

    #[test]
    fn test_deserialize_with_tracestate() {
        let serialized = SerializedTraceContext {
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
            tracestate: Some("thoughtgate=session-xyz,vendor=value".to_string()),
        };

        let result = deserialize_span_context(&serialized);
        assert!(result.recovered);
        // TraceState is parsed correctly
        assert!(!result.trace_state.header().is_empty());
    }

    #[test]
    fn test_parse_traceparent_valid() {
        let result = parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
        assert!(result.is_some());

        let (trace_id, span_id, flags) = result.unwrap();
        assert_eq!(
            trace_id,
            TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap()
        );
        assert_eq!(span_id, SpanId::from_hex("00f067aa0ba902b7").unwrap());
        assert_eq!(flags, TraceFlags::SAMPLED);
    }

    #[test]
    fn test_parse_traceparent_unsampled() {
        let result = parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00");
        assert!(result.is_some());

        let (_, _, flags) = result.unwrap();
        assert_eq!(flags, TraceFlags::default()); // Not sampled
    }

    #[test]
    fn test_parse_tracestate() {
        let pairs = parse_tracestate("key1=value1,key2=value2");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("key1".to_string(), "value1".to_string()));
        assert_eq!(pairs[1], ("key2".to_string(), "value2".to_string()));
    }

    #[test]
    fn test_parse_tracestate_with_spaces() {
        let pairs = parse_tracestate(" key1 = value1 , key2 = value2 ");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("key1".to_string(), "value1".to_string()));
    }

    #[test]
    fn test_hex_to_bytes() {
        assert_eq!(hex_to_bytes("00"), Some(vec![0u8]));
        assert_eq!(hex_to_bytes("ff"), Some(vec![255u8]));
        assert_eq!(hex_to_bytes("0102"), Some(vec![1u8, 2u8]));
        assert_eq!(hex_to_bytes(""), Some(vec![]));
        assert_eq!(hex_to_bytes("0"), None); // Odd length
        assert_eq!(hex_to_bytes("gg"), None); // Invalid hex
    }

    #[test]
    fn test_serialized_trace_context_serde() {
        let ctx = SerializedTraceContext {
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
            tracestate: Some("key=value".to_string()),
        };

        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains("traceparent"));
        assert!(json.contains("tracestate"));

        let deserialized: SerializedTraceContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, ctx);
    }

    #[test]
    fn test_serialized_trace_context_serde_no_tracestate() {
        let ctx = SerializedTraceContext {
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
            tracestate: None,
        };

        let json = serde_json::to_string(&ctx).unwrap();
        // tracestate should be omitted when None
        assert!(!json.contains("tracestate"));

        let deserialized: SerializedTraceContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, ctx);
    }
}
