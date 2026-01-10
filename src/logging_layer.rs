//! Tower layer for structured request/response logging.
//!
//! # Traceability
//! - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - observability)

use http::HeaderMap;
use std::fmt;
use std::time::Instant;
use tower::Service;
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

/// Logging layer that wraps services and logs requests/responses.
///
/// This Tower layer provides structured JSON logging with:
/// - Request/response latency tracking
/// - Automatic redaction of sensitive headers
/// - Zero-allocation header sanitization (only at DEBUG level)
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - observability)
#[derive(Clone, Debug)]
pub struct LoggingLayer;

impl<S> tower::Layer<S> for LoggingLayer {
    type Service = LoggingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LoggingService { inner }
    }
}

/// Service wrapper that adds logging.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - observability)
#[derive(Clone, Debug)]
pub struct LoggingService<S> {
    inner: S,
}

impl<S, ReqBody, ResBody> Service<hyper::Request<ReqBody>> for LoggingService<S>
where
    S: Service<hyper::Request<ReqBody>, Response = hyper::Response<ResBody>>,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
{
    type Response = hyper::Response<ResBody>;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: hyper::Request<ReqBody>) -> Self::Future {
        let start = Instant::now();
        let method = req.method().clone();
        let uri = req.uri().clone();

        info!(
            method = %method,
            uri = %uri,
            direction = "inbound",
            "Request received"
        );

        // PERF(latency): Only sanitize headers at DEBUG level to avoid allocation overhead
        if tracing::enabled!(tracing::Level::DEBUG) {
            let version = req.version();
            let headers = sanitize_headers(req.headers());
            tracing::debug!(
                version = ?version,
                headers = ?headers,
                "Request details"
            );
        }

        let fut = self.inner.call(req);

        Box::pin(async move {
            let result = fut.await;
            let elapsed = start.elapsed();

            match &result {
                Ok(res) => {
                    let status = res.status();

                    info!(
                        method = %method,
                        uri = %uri,
                        status = %status.as_u16(),
                        latency_ms = elapsed.as_millis(),
                        direction = "outbound",
                        "Response sent"
                    );

                    // PERF(latency): Only sanitize headers and extract body info at DEBUG level
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        let res_version = res.version();
                        let res_headers = sanitize_headers(res.headers());
                        let body_info = get_body_info(res.headers());
                        tracing::debug!(
                            version = ?res_version,
                            headers = ?res_headers,
                            body_info = %body_info,
                            "Response details"
                        );
                    }
                }
                Err(_e) => {
                    warn!(
                        method = %method,
                        uri = %uri,
                        latency_ms = elapsed.as_millis(),
                        direction = "error",
                        "Request failed"
                    );
                }
            }

            result
        })
    }
}

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

/// Zero-allocation body info wrapper.
struct BodyInfo<'a>(&'a HeaderMap);

impl<'a> fmt::Display for BodyInfo<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(content_length) = self.0.get("content-length")
            && let Ok(len_str) = content_length.to_str()
        {
            return write!(f, "{} bytes", len_str);
        }

        if let Some(te) = self.0.get("transfer-encoding")
            && let Ok(te_str) = te.to_str()
            && te_str.contains("chunked")
        {
            return f.write_str("chunked/streaming");
        }

        if let Some(ct) = self.0.get("content-type")
            && let Ok(ct_str) = ct.to_str()
            && ct_str.contains("text/event-stream")
        {
            return f.write_str("SSE/streaming");
        }

        f.write_str("unknown")
    }
}

/// Extract body information from headers without reading the body.
#[inline]
fn get_body_info(headers: &HeaderMap) -> BodyInfo<'_> {
    BodyInfo(headers)
}
