//! W3C Trace Context propagation for HTTP transport.
//!
//! Provides extract/inject helpers for traceparent and tracestate HTTP headers.
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
//! # Traceability
//! - Implements: REQ-OBS-002 §7.1 (Inbound Trace Context Extraction)
//! - Implements: REQ-OBS-002 §7.2 (Outbound Trace Context Injection)

use http::header::{HeaderMap, HeaderName, HeaderValue};
use opentelemetry::Context;
use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator};
use opentelemetry_sdk::propagation::TraceContextPropagator;

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

/// Extract trace context from HTTP headers.
///
/// Parses `traceparent` and `tracestate` headers per W3C Trace Context spec.
/// Returns `Context::current()` if no valid traceparent is found (ThoughtGate
/// becomes the trace root).
///
/// # Arguments
///
/// * `headers` - HTTP headers containing trace context
///
/// # Returns
///
/// An OpenTelemetry `Context` with the extracted trace context. If no valid
/// traceparent is found, returns the current context (no remote parent).
///
/// # Example
///
/// ```ignore
/// let parent_context = extract_context_from_headers(req.headers());
/// let span = tracer.span_builder("my_span")
///     .start_with_context(&tracer, &parent_context);
/// ```
///
/// Implements: REQ-OBS-002 §7.1
pub fn extract_context_from_headers(headers: &HeaderMap) -> Context {
    let propagator = TraceContextPropagator::new();
    propagator.extract(&HeaderExtractor(headers))
}

/// Inject trace context into HTTP headers for upstream requests.
///
/// Adds `traceparent` and `tracestate` headers to outbound requests,
/// enabling end-to-end distributed tracing across service boundaries.
///
/// Optionally augments tracestate with `thoughtgate={session_id}` for
/// downstream correlation.
///
/// # Arguments
///
/// * `cx` - The OpenTelemetry context containing the current span
/// * `headers` - Mutable HTTP headers to inject into
/// * `session_id` - Optional session identifier to add to tracestate
///
/// # Example
///
/// ```ignore
/// let mut headers = HeaderMap::new();
/// inject_context_into_headers(&Context::current(), &mut headers, Some("req-abc123"));
/// // headers now contains traceparent and tracestate
/// ```
///
/// Implements: REQ-OBS-002 §7.2
pub fn inject_context_into_headers(
    cx: &Context,
    headers: &mut HeaderMap,
    session_id: Option<&str>,
) {
    let propagator = TraceContextPropagator::new();
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
}
