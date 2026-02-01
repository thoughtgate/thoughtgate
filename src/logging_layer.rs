//! Tower layer for structured request/response logging.
//!
//! Uses `tower_http::trace::TraceLayer` for the middleware plumbing, with
//! custom callbacks for ThoughtGate-specific header redaction.
//!
//! # Traceability
//! - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - observability)

use http::HeaderMap;
use std::fmt;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

/// Headers that are redacted from logs for security.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - security)
#[cfg(feature = "fuzzing")]
pub const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "x-api-key",
    "x-auth-token",
    "proxy-authorization",
    "set-cookie",
];

#[cfg(not(feature = "fuzzing"))]
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "x-api-key",
    "x-auth-token",
    "proxy-authorization",
    "set-cookie",
];

/// Create the logging/tracing layer using `tower-http`.
///
/// Replaces the previous hand-rolled Tower `Service` impl with
/// `TraceLayer::new_for_http()` plus custom callbacks for request/response
/// logging and header redaction.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - observability)
pub fn logging_layer() -> TraceLayer<
    tower_http::classify::SharedClassifier<tower_http::classify::ServerErrorsAsFailures>,
    CorrelationMakeSpan,
    OnRequestLogger,
    OnResponseLogger,
    tower_http::trace::DefaultOnBodyChunk,
    tower_http::trace::DefaultOnEos,
    OnFailureLogger,
> {
    TraceLayer::new_for_http()
        .make_span_with(CorrelationMakeSpan)
        .on_request(OnRequestLogger)
        .on_response(OnResponseLogger)
        .on_failure(OnFailureLogger)
}

/// Custom span creator that attaches a correlation ID to every request span.
///
/// Extracts `x-request-id` from the request headers if present, otherwise
/// generates one using `fast_correlation_id()`. This ensures every log line
/// within a request's lifecycle carries a `request_id` field for correlation.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Observability - request correlation)
#[derive(Clone, Debug)]
pub struct CorrelationMakeSpan;

impl<B> tower_http::trace::MakeSpan<B> for CorrelationMakeSpan {
    fn make_span(&mut self, request: &hyper::Request<B>) -> tracing::Span {
        let request_id = request
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned())
            .unwrap_or_else(|| crate::transport::jsonrpc::fast_correlation_id().to_string());

        tracing::info_span!(
            "request",
            method = %request.method(),
            uri = %request.uri(),
            version = ?request.version(),
            request_id = %request_id,
        )
    }
}

/// Custom on-request callback that logs method, URI, and optionally headers.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - observability)
#[derive(Clone, Debug)]
pub struct OnRequestLogger;

impl<B> tower_http::trace::OnRequest<B> for OnRequestLogger {
    fn on_request(&mut self, request: &hyper::Request<B>, _span: &tracing::Span) {
        info!(
            method = %request.method(),
            uri = %request.uri(),
            direction = "inbound",
            "Request received"
        );

        // PERF(latency): Only sanitize headers at DEBUG level to avoid allocation overhead
        if tracing::enabled!(tracing::Level::DEBUG) {
            let version = request.version();
            let headers = sanitize_headers(request.headers());
            tracing::debug!(
                version = ?version,
                headers = ?headers,
                "Request details"
            );
        }
    }
}

/// Custom on-response callback that logs status, latency, and optionally headers.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - observability)
#[derive(Clone, Debug)]
pub struct OnResponseLogger;

impl<B> tower_http::trace::OnResponse<B> for OnResponseLogger {
    fn on_response(
        self,
        response: &hyper::Response<B>,
        latency: std::time::Duration,
        _span: &tracing::Span,
    ) {
        info!(
            status = %response.status().as_u16(),
            latency_ms = latency.as_millis(),
            direction = "outbound",
            "Response sent"
        );

        // PERF(latency): Only sanitize headers at DEBUG level
        if tracing::enabled!(tracing::Level::DEBUG) {
            let res_version = response.version();
            let res_headers = sanitize_headers(response.headers());
            tracing::debug!(
                version = ?res_version,
                headers = ?res_headers,
                "Response details"
            );
        }
    }
}

/// Custom on-failure callback that logs service errors.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - observability)
#[derive(Clone, Debug)]
pub struct OnFailureLogger;

impl tower_http::trace::OnFailure<tower_http::classify::ServerErrorsFailureClass>
    for OnFailureLogger
{
    fn on_failure(
        &mut self,
        failure: tower_http::classify::ServerErrorsFailureClass,
        latency: std::time::Duration,
        _span: &tracing::Span,
    ) {
        warn!(
            classification = %failure,
            latency_ms = latency.as_millis(),
            direction = "error",
            "Request failed"
        );
    }
}

// ============================================================================
// Header Redaction (kept for fuzz target compatibility)
// ============================================================================

/// Zero-allocation wrapper for sanitized headers.
#[cfg(feature = "fuzzing")]
pub struct SanitizedHeaders<'a>(pub &'a HeaderMap);

#[cfg(not(feature = "fuzzing"))]
struct SanitizedHeaders<'a>(&'a HeaderMap);

impl<'a> fmt::Debug for SanitizedHeaders<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();

        // Limit header count to prevent DoS via excessive formatting
        const MAX_HEADERS_TO_LOG: usize = 50;

        for (idx, (name, value)) in self.0.iter().enumerate() {
            // Safety: prevent unbounded formatting
            if idx >= MAX_HEADERS_TO_LOG {
                map.entry(&"...", &format!("({} more headers)", self.0.len() - idx));
                break;
            }

            let name_str = name.as_str();

            // SAFETY: HTTP header names are case-insensitive (RFC 7230 Section 3.2)
            // Use zero-allocation case-insensitive comparison to prevent header value leakage
            let is_sensitive = SENSITIVE_HEADERS
                .iter()
                .any(|&sensitive| name_str.eq_ignore_ascii_case(sensitive));

            if is_sensitive {
                // Redact sensitive headers
                map.entry(&name_str, &"[REDACTED]");
            } else {
                // Handle both UTF-8 and non-UTF-8 header values
                match value.to_str() {
                    Ok(val_str) => {
                        // Limit individual header value length
                        const MAX_VALUE_LEN: usize = 1024;
                        if val_str.len() <= MAX_VALUE_LEN {
                            map.entry(&name_str, &val_str);
                        } else {
                            map.entry(
                                &name_str,
                                &format!(
                                    "{}... ({} bytes)",
                                    &val_str[..MAX_VALUE_LEN],
                                    val_str.len()
                                ),
                            );
                        }
                    }
                    Err(_) => {
                        // Header contains non-UTF8 bytes, show as binary
                        map.entry(&name_str, &format!("<binary: {} bytes>", value.len()));
                    }
                }
            }
        }

        map.finish()
    }
}

/// Create a zero-allocation sanitized headers wrapper.
#[inline]
#[cfg(feature = "fuzzing")]
pub fn sanitize_headers(headers: &HeaderMap) -> SanitizedHeaders<'_> {
    SanitizedHeaders(headers)
}

#[inline]
#[cfg(not(feature = "fuzzing"))]
fn sanitize_headers(headers: &HeaderMap) -> SanitizedHeaders<'_> {
    SanitizedHeaders(headers)
}
