//! W3C Trace Context and Baggage propagation for HTTP and stdio transports.
//!
//! Provides extract/inject helpers for:
//! - HTTP headers (`traceparent`, `tracestate`, `baggage`)
//! - Stdio JSON-RPC `params._meta` fields (for Claude Desktop, Cursor, VS Code, Windsurf)
//!
//! Works with both `http::HeaderMap` and `reqwest::header::HeaderMap` (they are
//! the same type - reqwest re-exports from http crate).
//!
//! # Overview
//!
//! When a caller sends `traceparent`/`tracestate` headers, ThoughtGate's MCP spans
//! become children of the caller's trace. When no headers are present, ThoughtGate
//! becomes the trace root.
//!
//! # W3C Trace Context Format
//!
//! - `traceparent`: `{version}-{trace_id}-{span_id}-{trace_flags}`
//!   - version: "00" for W3C Trace Context v1
//!   - trace_id: 32 lowercase hex characters (16 bytes)
//!   - span_id: 16 lowercase hex characters (8 bytes)
//!   - trace_flags: 2 lowercase hex characters ("01" = sampled)
//!
//! - `tracestate`: comma-separated vendor-specific key-value pairs
//!   - ThoughtGate adds `thoughtgate={session_id}` for downstream correlation
//!
//! - `baggage`: W3C Baggage header for request-scoped key-value pairs
//!   - Carries business context (tenant ID, request ID) across service boundaries
//!
//! # Traceability
//! - Implements: REQ-OBS-002 §7.1 (Inbound Trace Context Extraction)
//! - Implements: REQ-OBS-002 §7.2 (Outbound Trace Context Injection)
//! - Implements: REQ-OBS-002 §7.3 (Stdio Transport Propagation)
//! - Implements: REQ-OBS-002 §7.4.3 (Business Context via Baggage)

use http::header::{HeaderMap, HeaderName, HeaderValue};
use opentelemetry::Context;
use opentelemetry::propagation::{
    Extractor, Injector, TextMapCompositePropagator, TextMapPropagator,
};
use opentelemetry::trace::TraceContextExt;
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};

// ─────────────────────────────────────────────────────────────────────────────
// Composite Propagator (TraceContext + Baggage)
// ─────────────────────────────────────────────────────────────────────────────

/// Creates a composite propagator that handles both W3C Trace Context and Baggage.
///
/// This propagator extracts/injects:
/// - `traceparent` and `tracestate` headers (trace correlation)
/// - `baggage` header (business context like tenant ID, request ID)
///
/// Per REQ-OBS-002 §7.4.3, baggage carries request-scoped metadata:
/// - `mcp.request.id`: Original MCP request correlation
/// - `thoughtgate.task.id`: ThoughtGate task identifier
/// - `thoughtgate.tenant`: Multi-tenant deployment tenant ID
///
/// Implements: REQ-OBS-002 §7.4.3
fn composite_propagator() -> TextMapCompositePropagator {
    TextMapCompositePropagator::new(vec![
        Box::new(TraceContextPropagator::new()),
        Box::new(BaggagePropagator::new()),
    ])
}

// ─────────────────────────────────────────────────────────────────────────────
// HTTP Header Extractor/Injector
// ─────────────────────────────────────────────────────────────────────────────

/// HTTP header extractor implementing OTel's Extractor trait.
///
/// Wraps an `http::HeaderMap` to extract W3C trace context headers.
///
/// # Example
///
/// ```ignore
/// let extractor = HeaderExtractor(&headers);
/// let context = propagator.extract(&extractor);
/// ```
pub struct HeaderExtractor<'a>(pub &'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    /// Get a header value by key.
    ///
    /// Returns `None` if the header is missing or cannot be parsed as UTF-8.
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.to_str().ok()
    }

    /// Get all header keys.
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// HTTP header injector implementing OTel's Injector trait.
///
/// Wraps a mutable `http::HeaderMap` to inject W3C trace context headers.
///
/// # Example
///
/// ```ignore
/// let mut injector = HeaderInjector(&mut headers);
/// propagator.inject_context(&context, &mut injector);
/// ```
pub struct HeaderInjector<'a>(pub &'a mut HeaderMap);

impl Injector for HeaderInjector<'_> {
    /// Set a header value.
    ///
    /// Silently ignores invalid header names or values.
    fn set(&mut self, key: &str, value: String) {
        if let Ok(name) = HeaderName::from_bytes(key.as_bytes()) {
            if let Ok(val) = HeaderValue::from_str(&value) {
                self.0.insert(name, val);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Extract/Inject Functions
// ─────────────────────────────────────────────────────────────────────────────

/// Extract trace context and baggage from HTTP headers.
///
/// Parses headers per W3C specifications:
/// - `traceparent` and `tracestate` (W3C Trace Context)
/// - `baggage` (W3C Baggage)
///
/// Returns `Context::current()` if no valid traceparent is found (ThoughtGate
/// becomes the trace root).
///
/// # Arguments
///
/// * `headers` - HTTP headers containing trace context and baggage
///
/// # Returns
///
/// An OpenTelemetry `Context` with the extracted trace context and baggage.
/// If no valid traceparent is found, returns the current context (no remote parent).
///
/// # Example
///
/// ```ignore
/// let parent_context = extract_context_from_headers(req.headers());
/// let span = tracer.span_builder("my_span")
///     .start_with_context(&tracer, &parent_context);
/// ```
///
/// Implements: REQ-OBS-002 §7.1, §7.4.3
pub fn extract_context_from_headers(headers: &HeaderMap) -> Context {
    let propagator = composite_propagator();
    propagator.extract(&HeaderExtractor(headers))
}

/// Inject trace context and baggage into HTTP headers for upstream requests.
///
/// Adds headers to outbound requests for end-to-end distributed tracing:
/// - `traceparent` and `tracestate` (W3C Trace Context)
/// - `baggage` (W3C Baggage for business context)
///
/// Optionally augments tracestate with `thoughtgate={session_id}` for
/// downstream correlation.
///
/// # Arguments
///
/// * `cx` - The OpenTelemetry context containing the current span and baggage
/// * `headers` - Mutable HTTP headers to inject into
/// * `session_id` - Optional session identifier to add to tracestate
///
/// # Example
///
/// ```ignore
/// let mut headers = HeaderMap::new();
/// inject_context_into_headers(&Context::current(), &mut headers, Some("req-abc123"));
/// // headers now contains traceparent, tracestate, and baggage
/// ```
///
/// Implements: REQ-OBS-002 §7.2, §7.4.3
pub fn inject_context_into_headers(
    cx: &Context,
    headers: &mut HeaderMap,
    session_id: Option<&str>,
) {
    let propagator = composite_propagator();
    propagator.inject_context(cx, &mut HeaderInjector(headers));

    // Augment tracestate with thoughtgate session identifier
    if let Some(sid) = session_id {
        augment_tracestate(headers, sid);
    }
}

/// Augment tracestate header with thoughtgate session identifier.
///
/// Per W3C Trace Context spec:
/// - Prepend new entry (most recent first)
/// - Maintain maximum 32 entries
/// - Replace existing thoughtgate entry if present
///
/// # Arguments
///
/// * `headers` - Mutable HTTP headers containing tracestate
/// * `session_id` - Session identifier to add
fn augment_tracestate(headers: &mut HeaderMap, session_id: &str) {
    let tg_entry = format!("thoughtgate={}", session_id);
    let tracestate_key = HeaderName::from_static("tracestate");

    let new_value = match headers.get(&tracestate_key) {
        Some(existing) => {
            if let Ok(existing_str) = existing.to_str() {
                // Parse existing entries, remove any old thoughtgate entry
                let entries: Vec<&str> = existing_str
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty() && !s.starts_with("thoughtgate="))
                    .collect();

                // Prepend new entry, limit to 32 total (W3C spec maximum)
                let mut all_entries = vec![tg_entry.as_str()];
                all_entries.extend(entries.into_iter().take(31));
                all_entries.join(",")
            } else {
                tg_entry
            }
        }
        None => tg_entry,
    };

    if let Ok(val) = HeaderValue::from_str(&new_value) {
        headers.insert(tracestate_key, val);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Stdio Transport Propagation (REQ-OBS-002 §7.3)
// ─────────────────────────────────────────────────────────────────────────────

/// Extractor for JSON-RPC `params._meta` fields (stdio transport).
///
/// Implements the OTel `Extractor` trait for W3C trace context stored in
/// JSON-RPC params._meta fields, enabling trace propagation through stdio
/// transports used by Claude Desktop, Cursor, VS Code, and Windsurf.
struct MetaExtractor<'a>(&'a serde_json::Map<String, serde_json::Value>);

impl Extractor for MetaExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.as_str()
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// Injector for JSON-RPC result `_meta` fields (stdio transport).
///
/// Implements the OTel `Injector` trait for W3C trace context injection
/// into JSON-RPC response _meta fields.
struct MetaInjector<'a>(&'a mut serde_json::Map<String, serde_json::Value>);

impl Injector for MetaInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        self.0
            .insert(key.to_string(), serde_json::Value::String(value));
    }
}

/// Result of extracting trace context from stdio `_meta` field.
///
/// Includes the extracted OpenTelemetry context and the modified params
/// with `_meta.traceparent`/`_meta.tracestate` stripped for upstream forwarding.
#[derive(Debug)]
pub struct StdioTraceContext {
    /// The extracted OpenTelemetry context (may be empty if no traceparent).
    pub context: Context,

    /// The params with `_meta.traceparent` and `_meta.tracestate` removed.
    /// If `_meta` becomes empty after stripping, it is also removed.
    pub stripped_params: Option<serde_json::Value>,

    /// Whether a valid trace context was extracted.
    pub had_trace_context: bool,
}

/// Extract trace context and baggage from JSON-RPC `params._meta` field (stdio transport).
///
/// Per REQ-OBS-002 §7.3, stdio transports use `params._meta` fields for trace
/// propagation since there are no HTTP headers:
/// - `_meta.traceparent` and `_meta.tracestate` (W3C Trace Context)
/// - `_meta.baggage` (W3C Baggage for business context)
///
/// This function:
/// 1. Extracts trace context and baggage from `params._meta` if present
/// 2. Creates an OpenTelemetry context from the extracted data
/// 3. Optionally strips trace fields from params to avoid breaking strict
///    upstream MCP servers that reject unknown fields
///
/// # Arguments
///
/// * `params` - The JSON-RPC params value (may or may not have `_meta`)
/// * `propagate_upstream` - If true, preserve trace fields in `stripped_params`.
///   If false (default), strip `_meta.traceparent`/`_meta.tracestate`/`_meta.baggage`.
///
/// # Returns
///
/// A [`StdioTraceContext`] containing the extracted context and optionally stripped params.
///
/// # Example
///
/// ```ignore
/// let params = json!({
///     "_meta": {
///         "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
///         "baggage": "mcp.request.id=req-123"
///     },
///     "name": "calculator",
///     "arguments": { "a": 1 }
/// });
///
/// // Default: strip trace fields
/// let result = extract_context_from_meta(Some(&params), false);
/// assert!(result.had_trace_context);
/// // result.stripped_params has _meta.traceparent and _meta.baggage removed
///
/// // Propagate: preserve trace fields for trace-aware upstreams
/// let result = extract_context_from_meta(Some(&params), true);
/// // result.stripped_params preserves _meta.traceparent and _meta.baggage
/// ```
///
/// Implements: REQ-OBS-002 §7.3, §7.3.1, §7.4.3
pub fn extract_context_from_meta(
    params: Option<&serde_json::Value>,
    propagate_upstream: bool,
) -> StdioTraceContext {
    let propagator = composite_propagator();

    // Handle None params
    let Some(params) = params else {
        return StdioTraceContext {
            context: Context::current(),
            stripped_params: None,
            had_trace_context: false,
        };
    };

    // Get _meta object if present
    let Some(meta) = params.get("_meta").and_then(|m| m.as_object()) else {
        return StdioTraceContext {
            context: Context::current(),
            stripped_params: Some(params.clone()),
            had_trace_context: false,
        };
    };

    // Extract trace context
    let context = propagator.extract(&MetaExtractor(meta));

    // Check if we actually extracted a valid trace context
    let span_context = context.span().span_context().clone();
    let had_trace_context = span_context.is_valid() && span_context.is_remote();

    // Conditionally strip trace context fields from _meta
    // If propagate_upstream is true, preserve trace fields for trace-aware upstreams
    let stripped_params = if propagate_upstream {
        // Preserve _meta as-is (trace-aware upstream)
        Some(params.clone())
    } else {
        // Strip traceparent, tracestate, and baggage from _meta
        let mut stripped_meta = meta.clone();
        stripped_meta.remove("traceparent");
        stripped_meta.remove("tracestate");
        stripped_meta.remove("baggage");

        // Rebuild params with stripped _meta (or without _meta if empty)
        if let Some(obj) = params.as_object() {
            let mut new_obj = obj.clone();
            if stripped_meta.is_empty() {
                new_obj.remove("_meta");
            } else {
                new_obj.insert(
                    "_meta".to_string(),
                    serde_json::Value::Object(stripped_meta),
                );
            }
            Some(serde_json::Value::Object(new_obj))
        } else {
            Some(params.clone())
        }
    };

    StdioTraceContext {
        context,
        stripped_params,
        had_trace_context,
    }
}

/// Inject trace context and baggage into JSON-RPC response `result._meta` field.
///
/// Per REQ-OBS-002 §7.3.2, when returning a response over stdio, ThoughtGate
/// injects trace context and baggage into the response's `result._meta` field:
/// - `_meta.traceparent` and `_meta.tracestate` (W3C Trace Context)
/// - `_meta.baggage` (W3C Baggage for business context)
///
/// # Arguments
///
/// * `cx` - The OpenTelemetry context containing the current span and baggage
/// * `result` - Mutable JSON-RPC result value to inject into
/// * `session_id` - Optional session identifier to add to tracestate
///
/// # Example
///
/// ```ignore
/// let mut result = json!({ "content": [...] });
/// inject_context_into_meta(&Context::current(), &mut result, Some("session-123"));
/// // result now has _meta.traceparent, _meta.tracestate, and _meta.baggage
/// ```
///
/// Implements: REQ-OBS-002 §7.3.2, §7.4.3
pub fn inject_context_into_meta(
    cx: &Context,
    result: &mut serde_json::Value,
    session_id: Option<&str>,
) {
    let propagator = composite_propagator();

    // Ensure result is an object
    let obj = match result.as_object_mut() {
        Some(o) => o,
        None => return,
    };

    // Get or create _meta
    let meta = obj
        .entry("_meta")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

    let meta_obj = match meta.as_object_mut() {
        Some(o) => o,
        None => return,
    };

    // Inject trace context
    propagator.inject_context(cx, &mut MetaInjector(meta_obj));

    // Augment tracestate with thoughtgate session identifier
    if let Some(sid) = session_id {
        if let Some(existing_ts) = meta_obj.get("tracestate").and_then(|v| v.as_str()) {
            // Parse existing entries, remove any old thoughtgate entry, prepend new
            let entries: Vec<&str> = existing_ts
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty() && !s.starts_with("thoughtgate="))
                .collect();
            let tg_entry = format!("thoughtgate={}", sid);
            let mut all_entries = vec![tg_entry.as_str()];
            all_entries.extend(entries.into_iter().take(31));
            meta_obj.insert(
                "tracestate".to_string(),
                serde_json::Value::String(all_entries.join(",")),
            );
        } else {
            meta_obj.insert(
                "tracestate".to_string(),
                serde_json::Value::String(format!("thoughtgate={}", sid)),
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::{SpanId, TraceContextExt, TraceFlags, TraceId, TracerProvider};

    #[test]
    fn test_extract_valid_traceparent() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );

        let context = extract_context_from_headers(&headers);
        let span = context.span();
        let span_context = span.span_context();

        assert!(span_context.is_valid());
        assert!(span_context.is_remote());
        assert_eq!(
            span_context.trace_id(),
            TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap()
        );
        assert_eq!(
            span_context.span_id(),
            SpanId::from_hex("00f067aa0ba902b7").unwrap()
        );
        assert_eq!(span_context.trace_flags(), TraceFlags::SAMPLED);
    }

    #[test]
    fn test_extract_with_tracestate() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );
        headers.insert(
            "tracestate",
            HeaderValue::from_static("vendor=value,other=data"),
        );

        let context = extract_context_from_headers(&headers);
        let span = context.span();
        let span_context = span.span_context();

        assert!(span_context.is_valid());
        // tracestate is preserved in span context
        let header = span_context.trace_state().header();
        assert!(header.contains("vendor=value"));
        assert!(header.contains("other=data"));
    }

    #[test]
    fn test_extract_missing_traceparent_returns_current_context() {
        let headers = HeaderMap::new();

        let context = extract_context_from_headers(&headers);
        let span = context.span();
        let span_context = span.span_context();

        // No remote context - span context should be invalid (no active span)
        assert!(!span_context.is_valid() || !span_context.is_remote());
    }

    #[test]
    fn test_extract_invalid_traceparent() {
        let mut headers = HeaderMap::new();
        headers.insert("traceparent", HeaderValue::from_static("invalid-format"));

        let context = extract_context_from_headers(&headers);
        let span = context.span();
        let span_context = span.span_context();

        // Invalid traceparent should result in no remote context
        assert!(!span_context.is_remote());
    }

    #[test]
    fn test_inject_context() {
        use opentelemetry_sdk::trace::SdkTracerProvider;

        // Create a real span context with a tracer
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("test");

        // Start a span to get a valid context
        use opentelemetry::trace::Tracer;
        let span = tracer.start("test-span");
        let cx = Context::current_with_span(span);

        let mut headers = HeaderMap::new();
        inject_context_into_headers(&cx, &mut headers, None);

        // Should have traceparent header
        assert!(headers.contains_key("traceparent"));

        let traceparent = headers.get("traceparent").unwrap().to_str().unwrap();
        // Format: 00-{trace_id}-{span_id}-{flags}
        assert!(traceparent.starts_with("00-"));
        assert_eq!(traceparent.len(), 55); // 2 + 1 + 32 + 1 + 16 + 1 + 2
    }

    #[test]
    fn test_inject_with_session_id() {
        use opentelemetry_sdk::trace::SdkTracerProvider;

        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("test");

        use opentelemetry::trace::Tracer;
        let span = tracer.start("test-span");
        let cx = Context::current_with_span(span);

        let mut headers = HeaderMap::new();
        inject_context_into_headers(&cx, &mut headers, Some("req-xyz789"));

        // Should have tracestate with thoughtgate entry
        assert!(headers.contains_key("tracestate"));

        let tracestate = headers.get("tracestate").unwrap().to_str().unwrap();
        assert!(tracestate.contains("thoughtgate=req-xyz789"));
    }

    #[test]
    fn test_augment_tracestate_empty() {
        let mut headers = HeaderMap::new();
        augment_tracestate(&mut headers, "session-123");

        let tracestate = headers.get("tracestate").unwrap().to_str().unwrap();
        assert_eq!(tracestate, "thoughtgate=session-123");
    }

    #[test]
    fn test_augment_tracestate_existing() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "tracestate",
            HeaderValue::from_static("vendor=value,other=data"),
        );

        augment_tracestate(&mut headers, "session-456");

        let tracestate = headers.get("tracestate").unwrap().to_str().unwrap();
        // thoughtgate should be prepended
        assert!(tracestate.starts_with("thoughtgate=session-456,"));
        assert!(tracestate.contains("vendor=value"));
        assert!(tracestate.contains("other=data"));
    }

    #[test]
    fn test_augment_tracestate_replaces_existing_thoughtgate() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "tracestate",
            HeaderValue::from_static("thoughtgate=old-session,vendor=value"),
        );

        augment_tracestate(&mut headers, "new-session");

        let tracestate = headers.get("tracestate").unwrap().to_str().unwrap();
        // Old thoughtgate entry should be replaced
        assert!(!tracestate.contains("old-session"));
        assert!(tracestate.starts_with("thoughtgate=new-session"));
        assert!(tracestate.contains("vendor=value"));
    }

    #[test]
    fn test_augment_tracestate_max_entries() {
        // Create a tracestate with 35 entries (exceeds 32 limit)
        let entries: Vec<String> = (0..35).map(|i| format!("vendor{}=value{}", i, i)).collect();
        let long_tracestate = entries.join(",");

        let mut headers = HeaderMap::new();
        headers.insert(
            "tracestate",
            HeaderValue::from_str(&long_tracestate).unwrap(),
        );

        augment_tracestate(&mut headers, "session-max");

        let tracestate = headers.get("tracestate").unwrap().to_str().unwrap();
        // Should have at most 32 entries (1 thoughtgate + 31 vendor)
        let entry_count = tracestate.split(',').count();
        assert!(
            entry_count <= 32,
            "Expected max 32 entries, got {}",
            entry_count
        );
        assert!(tracestate.starts_with("thoughtgate=session-max"));
    }

    #[test]
    fn test_header_extractor_keys() {
        let mut headers = HeaderMap::new();
        headers.insert("traceparent", HeaderValue::from_static("value1"));
        headers.insert("tracestate", HeaderValue::from_static("value2"));
        headers.insert("content-type", HeaderValue::from_static("application/json"));

        let extractor = HeaderExtractor(&headers);
        let keys = extractor.keys();

        assert!(keys.contains(&"traceparent"));
        assert!(keys.contains(&"tracestate"));
        assert!(keys.contains(&"content-type"));
    }

    #[test]
    fn test_header_extractor_get_missing() {
        let headers = HeaderMap::new();
        let extractor = HeaderExtractor(&headers);

        assert!(extractor.get("nonexistent").is_none());
    }

    #[test]
    fn test_header_injector_invalid_value() {
        let mut headers = HeaderMap::new();
        let mut injector = HeaderInjector(&mut headers);

        // Try to inject a value with invalid characters (should be silently ignored)
        injector.set("test-header", "value\nwith\nnewlines".to_string());

        // Should not be inserted due to invalid value
        assert!(!headers.contains_key("test-header"));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Stdio Transport Tests (REQ-OBS-002 §7.3)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_extract_from_meta_valid_traceparent() {
        use serde_json::json;

        let params = json!({
            "_meta": {
                "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
            },
            "name": "calculator",
            "arguments": { "a": 1 }
        });

        let result = extract_context_from_meta(Some(&params), false);

        assert!(result.had_trace_context);

        // Verify extracted context
        let span = result.context.span();
        let span_context = span.span_context();
        assert!(span_context.is_valid());
        assert!(span_context.is_remote());
        assert_eq!(
            span_context.trace_id(),
            TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap()
        );
        assert_eq!(
            span_context.span_id(),
            SpanId::from_hex("00f067aa0ba902b7").unwrap()
        );

        // Verify traceparent was stripped
        let stripped = result.stripped_params.unwrap();
        assert!(stripped.get("_meta").is_none() || stripped["_meta"].get("traceparent").is_none());
        // Other params preserved
        assert_eq!(stripped["name"], "calculator");
        assert_eq!(stripped["arguments"]["a"], 1);
    }

    #[test]
    fn test_extract_from_meta_with_tracestate() {
        use serde_json::json;

        let params = json!({
            "_meta": {
                "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                "tracestate": "vendor=value,other=data"
            },
            "name": "test"
        });

        let result = extract_context_from_meta(Some(&params), false);

        assert!(result.had_trace_context);

        // Verify tracestate preserved in context
        let span_context = result.context.span().span_context().clone();
        let header = span_context.trace_state().header();
        assert!(header.contains("vendor=value"));

        // Verify both traceparent and tracestate stripped
        let stripped = result.stripped_params.unwrap();
        assert!(stripped.get("_meta").is_none());
    }

    #[test]
    fn test_extract_from_meta_no_meta() {
        use serde_json::json;

        let params = json!({
            "name": "calculator",
            "arguments": { "a": 1 }
        });

        let result = extract_context_from_meta(Some(&params), false);

        assert!(!result.had_trace_context);

        // Params unchanged
        let stripped = result.stripped_params.unwrap();
        assert_eq!(stripped, params);
    }

    #[test]
    fn test_extract_from_meta_none_params() {
        let result = extract_context_from_meta(None, false);

        assert!(!result.had_trace_context);
        assert!(result.stripped_params.is_none());
    }

    #[test]
    fn test_extract_from_meta_preserves_other_meta_fields() {
        use serde_json::json;

        let params = json!({
            "_meta": {
                "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                "custom_field": "preserved"
            },
            "name": "test"
        });

        let result = extract_context_from_meta(Some(&params), false);

        assert!(result.had_trace_context);

        // Custom field should be preserved
        let stripped = result.stripped_params.unwrap();
        assert!(stripped.get("_meta").is_some());
        assert_eq!(stripped["_meta"]["custom_field"], "preserved");
        assert!(stripped["_meta"].get("traceparent").is_none());
    }

    #[test]
    fn test_extract_from_meta_invalid_traceparent() {
        use serde_json::json;

        let params = json!({
            "_meta": {
                "traceparent": "invalid-format"
            },
            "name": "test"
        });

        let result = extract_context_from_meta(Some(&params), false);

        // Invalid traceparent should not create a remote context
        assert!(!result.had_trace_context);
    }

    #[test]
    fn test_extract_from_meta_propagate_upstream_preserves_trace_fields() {
        use serde_json::json;

        let params = json!({
            "_meta": {
                "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                "tracestate": "vendor=value"
            },
            "name": "test"
        });

        // With propagate_upstream=true, trace fields should be preserved
        let result = extract_context_from_meta(Some(&params), true);

        assert!(result.had_trace_context);

        // Trace fields should NOT be stripped
        let stripped = result.stripped_params.unwrap();
        assert!(stripped.get("_meta").is_some());
        assert_eq!(
            stripped["_meta"]["traceparent"],
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        );
        assert_eq!(stripped["_meta"]["tracestate"], "vendor=value");
    }

    #[test]
    fn test_inject_into_meta_basic() {
        use opentelemetry::trace::Tracer;
        use opentelemetry_sdk::trace::SdkTracerProvider;
        use serde_json::json;

        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("test");
        let span = tracer.start("test-span");
        let cx = Context::current_with_span(span);

        let mut result = json!({
            "content": [{ "type": "text", "text": "hello" }]
        });

        inject_context_into_meta(&cx, &mut result, None);

        // Should have _meta.traceparent
        assert!(result.get("_meta").is_some());
        let traceparent = result["_meta"]["traceparent"].as_str().unwrap();
        assert!(traceparent.starts_with("00-"));
        assert_eq!(traceparent.len(), 55);
    }

    #[test]
    fn test_inject_into_meta_with_session_id() {
        use opentelemetry::trace::Tracer;
        use opentelemetry_sdk::trace::SdkTracerProvider;
        use serde_json::json;

        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("test");
        let span = tracer.start("test-span");
        let cx = Context::current_with_span(span);

        let mut result = json!({ "content": [] });

        inject_context_into_meta(&cx, &mut result, Some("session-xyz"));

        // Should have tracestate with thoughtgate entry
        let tracestate = result["_meta"]["tracestate"].as_str().unwrap();
        assert!(tracestate.contains("thoughtgate=session-xyz"));
    }

    #[test]
    fn test_inject_into_meta_preserves_existing_meta() {
        use opentelemetry::trace::Tracer;
        use opentelemetry_sdk::trace::SdkTracerProvider;
        use serde_json::json;

        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("test");
        let span = tracer.start("test-span");
        let cx = Context::current_with_span(span);

        let mut result = json!({
            "_meta": { "existing": "value" },
            "content": []
        });

        inject_context_into_meta(&cx, &mut result, None);

        // Existing _meta field preserved
        assert_eq!(result["_meta"]["existing"], "value");
        // traceparent added
        assert!(result["_meta"]["traceparent"].is_string());
    }
}
