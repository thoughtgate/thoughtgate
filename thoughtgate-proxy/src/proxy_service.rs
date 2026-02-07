//! Core proxy service implementation.
//!
//! # Overview
//!
//! ProxyService is the unified entry point for all HTTP traffic in ThoughtGate.
//! It discriminates between MCP and regular HTTP traffic:
//!
//! - **MCP Traffic** (POST /mcp/v1 with application/json):
//!   - Buffered for inspection and policy evaluation
//!   - Routed through McpHandler for Cedar policy + task handling
//!
//! - **HTTP Traffic** (everything else):
//!   - Zero-copy streaming passthrough to upstream
//!   - No inspection or buffering overhead
//!
//! # Request Flow
//!
//! ```text
//! Request<Incoming> ──► discriminate_traffic()
//!                              │
//!         ┌────────────────────┴────────────────────┐
//!         │                                         │
//!   TrafficType::Mcp                         TrafficType::Http
//!   (POST /mcp/v1 with JSON)                 (everything else)
//!         │                                         │
//!         ▼                                         ▼
//!   handle_mcp_request()                     handle_http_request()
//!         │                                         │
//!   Buffer body → McpHandler                 Zero-copy streaming
//! ```
//!
//! # Traceability
//! - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy)
//! - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)

use crate::error::{ProxyError, ProxyResult};
use crate::mcp_handler::McpHandler;
use crate::proxy_config::ProxyConfig;
use crate::traffic::{TrafficType, discriminate_traffic};
use bytes::Bytes;
use futures_util::StreamExt;
use http::Uri;
use http_body_util::{BodyExt, BodyStream, Full, Limited, StreamBody};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode, header};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;
use std::sync::Arc;
use tower::Service;
use tracing::{debug, error, info, warn};

/// Type alias for the client's streaming body type.
type ClientBody =
    http_body_util::combinators::BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

/// Type alias for the unified response body type.
///
/// MCP responses use `Full<Bytes>` (buffered), HTTP responses use streaming.
/// Both are boxed for a unified return type.
pub type UnifiedBody = http_body_util::combinators::BoxBody<Bytes, ProxyError>;

/// Main proxy service that handles HTTP and HTTPS requests with full TLS support.
///
/// This service implements:
/// - **MCP Traffic**: Buffered inspection via `McpHandler` (Cedar policy + task handling)
/// - **HTTP Traffic**: Zero-copy streaming via `BodyStream` to minimize latency
///
/// It supports both forward proxy (HTTP_PROXY/HTTPS_PROXY) and
/// reverse proxy (UPSTREAM_URL) modes.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy)
/// - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
pub struct ProxyService {
    /// HTTPS-capable client for upstream connections with streaming support.
    client: Client<HttpsConnector<HttpConnector>, ClientBody>,
    /// Optional fixed upstream URL for reverse proxy mode
    upstream_url: Option<String>,
    /// Runtime configuration
    config: ProxyConfig,
    /// MCP request handler (optional - for MCP traffic routing)
    mcp_handler: Option<Arc<McpHandler>>,
}

impl Clone for ProxyService {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            upstream_url: self.upstream_url.clone(),
            config: self.config.clone(),
            mcp_handler: self.mcp_handler.clone(),
        }
    }
}

impl ProxyService {
    /// Create a new proxy service with optional upstream URL for reverse proxy mode.
    ///
    /// # Errors
    ///
    /// Returns `ProxyError::Connection` if:
    /// - TLS crypto provider installation fails
    /// - Native TLS root certificates cannot be loaded
    pub fn new_with_upstream(upstream_url: Option<String>) -> ProxyResult<Self> {
        Self::new_with_config(upstream_url, ProxyConfig::from_env())
    }

    /// Create a new proxy service with explicit configuration.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-001 Section 3.2 (Network Optimization)
    ///
    /// # Errors
    ///
    /// Returns `ProxyError::Connection` if:
    /// - TLS crypto provider installation fails
    /// - Native TLS root certificates cannot be loaded
    pub fn new_with_config(upstream_url: Option<String>, config: ProxyConfig) -> ProxyResult<Self> {
        // Install default crypto provider for rustls (required for TLS to work).
        // Uses OnceLock to ensure this is called exactly once and the result
        // is captured for error reporting without panicking.
        static RUSTLS_INIT: std::sync::OnceLock<Result<(), ()>> = std::sync::OnceLock::new();
        let init_result = RUSTLS_INIT.get_or_init(|| {
            rustls::crypto::ring::default_provider()
                .install_default()
                .map_err(|_| ())
        });
        if init_result.is_err() {
            return Err(ProxyError::Connection(
                "Failed to install rustls crypto provider".into(),
            ));
        }

        // Create HTTP connector with TCP_NODELAY enabled for upstream connections
        // Implements: REQ-CORE-001 F-001 (Latency - TCP_NODELAY on both legs)
        let mut http_connector = HttpConnector::new();
        http_connector.set_nodelay(config.tcp_nodelay);

        // Build HTTPS connector with native OS certificate store
        let https_connector = HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|e| ProxyError::Connection(format!("Failed to load native TLS roots: {}", e)))?
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(http_connector);

        let client = Client::builder(TokioExecutor::new())
            .http1_preserve_header_case(true)
            .http1_title_case_headers(true)
            .http1_allow_obsolete_multiline_headers_in_responses(true)
            .http2_keep_alive_while_idle(true)
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .build(https_connector);

        Ok(Self {
            client,
            upstream_url,
            config,
            mcp_handler: None,
        })
    }

    /// Set the MCP handler for this proxy service.
    ///
    /// When set, MCP traffic (POST /mcp/v1 with application/json) will be
    /// routed through this handler for Cedar policy evaluation and task handling.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
    pub fn with_mcp_handler(mut self, handler: Arc<McpHandler>) -> Self {
        self.mcp_handler = Some(handler);
        self
    }

    /// Get a reference to the proxy configuration.
    pub fn config(&self) -> &ProxyConfig {
        &self.config
    }

    /// Handle an incoming request, discriminating between MCP and HTTP traffic.
    ///
    /// This is the main entry point for all traffic. It:
    /// 1. Discriminates traffic type (MCP vs HTTP)
    /// 2. Routes MCP traffic through McpHandler (if configured)
    /// 3. Routes HTTP traffic through zero-copy streaming
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
    /// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy)
    pub async fn handle_request(
        &self,
        req: Request<Incoming>,
    ) -> ProxyResult<Response<UnifiedBody>> {
        let traffic_type = discriminate_traffic(&req);

        match traffic_type {
            TrafficType::Mcp => {
                if let Some(ref mcp_handler) = self.mcp_handler {
                    debug!(
                        method = %req.method(),
                        uri = %req.uri(),
                        "MCP traffic detected, routing to McpHandler"
                    );
                    self.handle_mcp_request(req, mcp_handler.clone()).await
                } else {
                    // No MCP handler configured, fall through to HTTP passthrough
                    debug!(
                        method = %req.method(),
                        uri = %req.uri(),
                        "MCP traffic detected but no handler configured, using HTTP passthrough"
                    );
                    self.handle_http_request(req).await
                }
            }
            TrafficType::Http => self.handle_http_request(req).await,
        }
    }

    /// Handle MCP traffic by buffering the body and passing to McpHandler.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003 (MCP Transport & Routing)
    /// - Implements: REQ-OBS-002 §7.1 (W3C Trace Context Extraction)
    async fn handle_mcp_request(
        &self,
        req: Request<Incoming>,
        mcp_handler: Arc<McpHandler>,
    ) -> ProxyResult<Response<UnifiedBody>> {
        // Extract W3C trace context BEFORE dropping headers.
        // This allows MCP spans to become children of the caller's trace.
        // Implements: REQ-OBS-002 §7.1
        let trace_context =
            thoughtgate_core::telemetry::extract_context_from_headers(req.headers());

        // Buffer the request body with stream-level size enforcement.
        // Using Limited prevents full memory allocation for oversized payloads —
        // the read is aborted as soon as cumulative bytes exceed the limit.
        let (_parts, body) = req.into_parts();
        let max_body_size = mcp_handler.max_body_size();
        let limited_body = Limited::new(body, max_body_size);

        let body_bytes = match limited_body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(e) => {
                warn!(
                    max = max_body_size,
                    "MCP request body exceeds size limit: {}", e
                );
                let body = Full::new(Bytes::from(format!(
                    r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32600,"message":"Request body exceeds maximum size of {} bytes"}}}}"#,
                    max_body_size
                )));
                // MCP Streamable HTTP transport requires HTTP 200 for all
                // JSON-RPC responses, including errors (the error is in the body).
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    // Full<Bytes> has Infallible error - convert using absurd pattern
                    .body(body.map_err(|e| match e {}).boxed())
                    .map_err(|e| ProxyError::Connection(e.to_string()));
            }
        };

        debug!(size = body_bytes.len(), "Collected MCP request body");

        // Handle the MCP request with trace context - returns (StatusCode, Bytes) directly.
        // This avoids double-buffering (Simplification #5).
        // The trace context enables W3C trace propagation (REQ-OBS-002 §7.1).
        let (status, response_bytes) = mcp_handler
            .handle_with_context(body_bytes, trace_context)
            .await;

        // Build unified response directly from bytes
        // Full<Bytes> has Infallible error - convert using absurd pattern
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Full::new(response_bytes).map_err(|e| match e {}).boxed())
            .map_err(|e| ProxyError::Connection(e.to_string()))
    }

    /// Handle an incoming HTTP request with zero-copy streaming.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-001 F-001 (Zero-Copy using bytes::Bytes and BodyStream)
    /// - Implements: REQ-CORE-001 F-002 (Fail-Fast Error Propagation)
    /// - Implements: REQ-CORE-001 F-003 (Transparency - preserve Content-Length/Transfer-Encoding)
    /// - Implements: REQ-CORE-001 F-004 (Protocol Upgrade Handling)
    pub async fn handle_http_request(
        &self,
        req: Request<Incoming>,
    ) -> ProxyResult<Response<UnifiedBody>> {
        // Extract target URI from request
        let target_uri = self.extract_target_uri(&req)?;

        // Check for protocol upgrade (REQ-CORE-001 F-004)
        let is_upgrade = is_upgrade_request(&req);
        let upgrade_protocol = if is_upgrade {
            get_upgrade_protocol(&req)
        } else {
            None
        };

        if is_upgrade {
            info!(
                method = %req.method(),
                uri = %req.uri(),
                target = %target_uri,
                upgrade_protocol = ?upgrade_protocol,
                "Proxying upgrade request"
            );
        } else {
            info!(
                method = %req.method(),
                uri = %req.uri(),
                target = %target_uri,
                "Proxying request"
            );
        }

        // Split request into parts and body
        let (parts, incoming_body) = req.into_parts();

        // Build upstream request
        let mut upstream_req = Request::builder()
            .method(parts.method.clone())
            .uri(&target_uri)
            .version(parts.version);

        // Copy headers (excluding hop-by-hop headers)
        let headers = upstream_req.headers_mut().ok_or_else(|| {
            error!("Failed to get mutable headers from request builder");
            ProxyError::Connection("Request builder in invalid state".to_string())
        })?;
        for (name_opt, value) in parts.headers {
            if let Some(name) = name_opt
                && !is_hop_by_hop_header(name.as_str())
            {
                headers.insert(name, value);
            }
        }

        // Convert Incoming body to zero-copy streaming body
        let body_stream = BodyStream::new(incoming_body);
        let mapped_stream = body_stream.map(|result| {
            result.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::other(format!("Body stream error: {}", e)))
            })
        });

        let stream_body = StreamBody::new(mapped_stream);
        let boxed_body: ClientBody = BodyExt::boxed(stream_body);

        let upstream_req = upstream_req.body(boxed_body).map_err(|e| {
            error!(error = %e, "Failed to build upstream request");
            ProxyError::Connection(format!("Failed to build request: {}", e))
        })?;

        // Send request and stream response (zero-copy, no buffering)
        // Map hyper errors to appropriate ProxyError variants (REQ-CORE-001 F-002)
        let upstream_res = self
            .client
            .request(upstream_req)
            .await
            .map_err(map_hyper_error)?;

        // REQ-CORE-001 F-005: TimeoutBody is now applied to response bodies below,
        // enforcing per-chunk (stream_read_timeout) and total (stream_total_timeout)
        // timeouts to prevent slow-read attacks on the Green Path.

        // Log if upgrade was successful (REQ-CORE-001 F-004)
        if is_upgrade && is_upgrade_response(&upstream_res) {
            info!(
                status = %upstream_res.status(),
                upgrade_protocol = ?upgrade_protocol,
                "Protocol upgrade successful - relying on hyper's internal handling"
            );

            // TODO(REQ-CORE-001 F-004): KNOWN LIMITATION - Upgrade Handling
            //
            // Current State: We detect upgrades and preserve necessary headers, but rely on
            // hyper's internal upgrade handling rather than explicitly using hyper::upgrade::on()
            // and tokio::io::copy_bidirectional().
            //
            // Why: The current architecture uses hyper_util::client::legacy::Client which abstracts
            // away the underlying connection. To properly implement "opaque TCP pipe" semantics per
            // REQ-CORE-001 F-004, we would need to:
            //
            // 1. Use hyper::upgrade::on() on both client request and upstream response
            // 2. Extract the underlying IO from both connections
            // 3. Use tokio::io::copy_bidirectional() for the relay
            //
            // This requires architectural changes:
            // - Replace Client API with manual connection pooling
            // - Track raw TcpStreams for both client and upstream
            // - Implement custom upgrade detection and handshake coordination
            //
            // Impact: For most upgrade scenarios (WebSocket, HTTP/2), hyper's internal handling
            // is sufficient. However, for strict "opaque TCP pipe" semantics and full control
            // over the bidirectional relay, explicit handling is preferred.
            //
            // Next Steps:
            // - Implement custom connection pool that exposes raw IO
            // - Add explicit upgrade handler in handle_connection()
            // - Add integration test that verifies bidirectional data flow
        }

        // Convert the Incoming body to UnifiedBody for type unification.
        // Wrap with TimeoutBody to enforce per-chunk and total stream timeouts,
        // preventing slow-read attacks on the Green Path (REQ-CORE-001 F-005).
        let (parts, body) = upstream_res.into_parts();

        let timeout_config = crate::timeout::TimeoutConfig::new(
            self.config.stream_read_timeout,
            self.config.stream_total_timeout,
        );
        let timeout_body = crate::timeout::TimeoutBody::new(body, timeout_config);

        let body_stream = BodyStream::new(timeout_body);
        let mapped_stream = body_stream.map(|result| {
            result.map_err(|e| ProxyError::Connection(format!("Body stream error: {}", e)))
        });
        let stream_body = StreamBody::new(mapped_stream);
        let boxed_body: UnifiedBody = BodyExt::boxed(stream_body);

        Ok(Response::from_parts(parts, boxed_body))
    }

    /// Extract target URI from request.
    // Made public for fuzzing to test URI extraction logic
    #[cfg(feature = "fuzzing")]
    pub fn extract_target_uri<B>(&self, req: &Request<B>) -> ProxyResult<Uri> {
        self.extract_target_uri_impl(req)
    }

    #[cfg(not(feature = "fuzzing"))]
    fn extract_target_uri<B>(&self, req: &Request<B>) -> ProxyResult<Uri> {
        self.extract_target_uri_impl(req)
    }

    fn extract_target_uri_impl<B>(&self, req: &Request<B>) -> ProxyResult<Uri> {
        // If upstream URL is configured (reverse proxy mode), use it
        if let Some(upstream) = &self.upstream_url {
            let path = req
                .uri()
                .path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or("/");
            let full_uri = format!("{}{}", upstream.trim_end_matches('/'), path);
            return full_uri.parse().map_err(|e| {
                ProxyError::InvalidUri(format!("Failed to parse upstream URI: {}", e))
            });
        }

        // Otherwise, use forward proxy mode
        let uri = req.uri();

        // Use absolute URI if present
        if uri.scheme().is_some() {
            return Ok(uri.clone());
        }

        // Otherwise construct from Host header
        if let Some(host) = req.headers().get("host") {
            let host_str = host
                .to_str()
                .map_err(|_| ProxyError::InvalidUri("Invalid Host header".to_string()))?;
            let scheme = if req.uri().scheme_str() == Some("https") {
                "https"
            } else {
                "http"
            };
            let full_uri = format!(
                "{}://{}{}",
                scheme,
                host_str,
                uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("")
            );
            return full_uri
                .parse()
                .map_err(|e| ProxyError::InvalidUri(format!("Failed to parse URI: {}", e)));
        }

        Err(ProxyError::InvalidUri(
            "Cannot determine target URI from request".to_string(),
        ))
    }
}

impl Service<Request<Incoming>> for ProxyService {
    type Response = Response<UnifiedBody>;
    type Error = ProxyError;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move { service.handle_request(req).await })
    }
}

/// Check if a header is a hop-by-hop header that shouldn't be forwarded.
///
/// Note: `transfer-encoding` and `upgrade` are NOT filtered per REQ-CORE-001 F-003 and F-004.
/// As a transparent streaming proxy, we must preserve chunked encoding information.
/// Upgrade headers are preserved to enable WebSocket and HTTP/2 upgrades.
///
/// # Traceability
/// - Implements: REQ-CORE-001 F-003 (Transparency - preserve Transfer-Encoding)
/// - Implements: REQ-CORE-001 F-004 (Protocol Upgrade Handling)
#[cfg(feature = "fuzzing")]
pub fn is_hop_by_hop_header(name: &str) -> bool {
    is_hop_by_hop_header_impl(name)
}

#[cfg(not(feature = "fuzzing"))]
fn is_hop_by_hop_header(name: &str) -> bool {
    is_hop_by_hop_header_impl(name)
}

fn is_hop_by_hop_header_impl(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "keep-alive" | "proxy-authenticate" | "proxy-authorization" | "te" | "trailers" // NOTE: "connection", "upgrade", and "transfer-encoding" intentionally NOT filtered
                                                                                        // to support WebSocket upgrades (F-004) and transparent streaming (F-003)
    )
}

/// Check if a request is attempting a protocol upgrade.
///
/// # Traceability
/// - Implements: REQ-CORE-001 F-004 (Protocol Upgrade Handling)
pub fn is_upgrade_request<B>(req: &Request<B>) -> bool {
    req.headers()
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase().contains("upgrade"))
        .unwrap_or(false)
        && req.headers().contains_key("upgrade")
}

/// Get the upgrade protocol from request headers.
///
/// # Traceability
/// - Implements: REQ-CORE-001 F-004 (Protocol Upgrade Handling)
pub fn get_upgrade_protocol<B>(req: &Request<B>) -> Option<String> {
    req.headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase())
}

/// Check if a response indicates a successful protocol upgrade.
///
/// # Traceability
/// - Implements: REQ-CORE-001 F-004 (Protocol Upgrade Handling)
pub fn is_upgrade_response<B>(res: &Response<B>) -> bool {
    res.status() == hyper::StatusCode::SWITCHING_PROTOCOLS
}

/// Map hyper_util client errors to appropriate ProxyError variants.
///
/// # Traceability
/// - Implements: REQ-CORE-001 F-002 (Fail-Fast Error Propagation)
///
/// # Error Mapping
/// - Connection refused -> `ProxyError::ConnectionRefused` (502)
/// - Timeout -> `ProxyError::Timeout` (504)
/// - Other errors -> `ProxyError::Connection` (502)
fn map_hyper_error(e: hyper_util::client::legacy::Error) -> ProxyError {
    use tracing::warn;

    let error_msg = e.to_string().to_lowercase();

    // Check for connection refused
    if error_msg.contains("connection refused") {
        warn!(error = %e, "Upstream connection refused");
        return ProxyError::ConnectionRefused(format!("Upstream refused connection: {}", e));
    }

    // Check for timeout
    if error_msg.contains("timeout") || error_msg.contains("timed out") {
        warn!(error = %e, "Upstream timeout");
        return ProxyError::Timeout(format!("Upstream timeout: {}", e));
    }

    // Check for connection errors
    if error_msg.contains("connection") || error_msg.contains("connect") {
        warn!(error = %e, "Upstream connection failed");
        return ProxyError::Connection(format!("Failed to connect to upstream: {}", e));
    }

    // Check for client disconnection
    if error_msg.contains("closed") || error_msg.contains("canceled") || error_msg.contains("reset")
    {
        warn!(error = %e, "Client disconnected");
        return ProxyError::ClientDisconnect;
    }

    // Default to generic connection error
    warn!(error = %e, "Upstream error");
    ProxyError::Connection(format!("Upstream error: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traffic::{TrafficType, discriminate_traffic};
    use http::{HeaderMap, Method, Version};

    /// Test URI extraction in reverse proxy mode
    #[test]
    fn test_uri_extraction_reverse_proxy() {
        let service = ProxyService::new_with_upstream(Some("https://api.example.com".to_string()))
            .expect("Failed to create proxy service");

        // Create a test request
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/chat/completions")
            .header("host", "localhost:3000")
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        let target_uri = service.extract_target_uri(&req).unwrap();

        assert_eq!(
            target_uri.to_string(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    /// Test URI extraction in forward proxy mode
    #[test]
    fn test_uri_extraction_forward_proxy() {
        let service =
            ProxyService::new_with_upstream(None).expect("Failed to create proxy service");

        // Test with absolute URI
        let req = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/path")
            .body(())
            .unwrap();

        let target_uri = service.extract_target_uri(&req).unwrap();

        assert_eq!(target_uri.to_string(), "https://example.com/path");
    }

    /// Test URI extraction with Host header
    #[test]
    fn test_uri_extraction_with_host_header() {
        let service =
            ProxyService::new_with_upstream(None).expect("Failed to create proxy service");

        // Test with relative URI + Host header
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/endpoint")
            .header("host", "api.example.com")
            .body(())
            .unwrap();

        let target_uri = service.extract_target_uri(&req).unwrap();

        assert_eq!(
            target_uri.to_string(),
            "http://api.example.com/api/endpoint"
        );
    }

    /// Test hop-by-hop header filtering
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-001 F-003 (Transparency - Transfer-Encoding NOT filtered)
    /// - Implements: REQ-CORE-001 F-004 (Protocol Upgrade - Upgrade headers NOT filtered)
    #[test]
    fn test_hop_by_hop_headers() {
        // These ARE hop-by-hop and should be filtered
        assert!(is_hop_by_hop_header("keep-alive"));
        assert!(is_hop_by_hop_header("Keep-Alive"));
        assert!(is_hop_by_hop_header("proxy-authorization"));
        assert!(is_hop_by_hop_header("Proxy-Authorization"));

        // These are NOT hop-by-hop and should NOT be filtered
        assert!(!is_hop_by_hop_header("content-type"));
        assert!(!is_hop_by_hop_header("authorization"));
        assert!(!is_hop_by_hop_header("user-agent"));

        // CRITICAL: transfer-encoding must NOT be filtered (REQ-CORE-001 F-003)
        assert!(!is_hop_by_hop_header("transfer-encoding"));
        assert!(!is_hop_by_hop_header("Transfer-Encoding"));

        // CRITICAL: connection and upgrade must NOT be filtered (REQ-CORE-001 F-004)
        // These are needed for WebSocket and HTTP/2 upgrades
        assert!(!is_hop_by_hop_header("connection"));
        assert!(!is_hop_by_hop_header("Connection"));
        assert!(!is_hop_by_hop_header("upgrade"));
        assert!(!is_hop_by_hop_header("Upgrade"));
    }

    /// Snapshot test: Verify request transformation logic
    #[test]
    fn test_integrity_snapshot() {
        use serde_json::json;

        // Test Case 1: Reverse proxy mode with complex headers
        let service =
            ProxyService::new_with_upstream(Some("https://upstream.example.com".to_string()))
                .expect("Failed to create proxy service");

        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/chat/completions?param=value")
            .version(Version::HTTP_11)
            .header("host", "localhost:3000")
            .header("content-type", "application/json")
            .header("authorization", "Bearer secret-token")
            .header("user-agent", "test-client/1.0")
            .header("x-custom-header", "custom-value")
            .header("connection", "keep-alive") // NOT filtered (F-004 upgrade support)
            .header("transfer-encoding", "chunked") // NOT filtered (F-003 transparency)
            .header("keep-alive", "timeout=60") // Should be filtered
            .body(())
            .unwrap();

        let target_uri = service.extract_target_uri(&req).unwrap();

        // Collect headers that would be forwarded (excluding hop-by-hop)
        let mut forwarded_headers = HeaderMap::new();
        for (name, value) in req.headers() {
            if !is_hop_by_hop_header(name.as_str()) {
                forwarded_headers.insert(name.clone(), value.clone());
            }
        }

        let snapshot_data = json!({
            "test_case": "reverse_proxy_with_filtering",
            "method": req.method().to_string(),
            "version": format!("{:?}", req.version()),
            "target_uri": target_uri.to_string(),
            "forwarded_headers": forwarded_headers.iter()
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect::<std::collections::BTreeMap<_, _>>(),
        });

        insta::assert_json_snapshot!(snapshot_data);

        // Test Case 2: Forward proxy mode with absolute URI
        let service_forward =
            ProxyService::new_with_upstream(None).expect("Failed to create proxy service");

        let req_forward = Request::builder()
            .method(Method::GET)
            .uri("https://api.openai.com/v1/models")
            .header("authorization", "Bearer token")
            .header("proxy-authorization", "Basic xyz") // Should be filtered
            .body(())
            .unwrap();

        let target_uri_forward = service_forward.extract_target_uri(&req_forward).unwrap();

        let mut forwarded_headers_forward = HeaderMap::new();
        for (name, value) in req_forward.headers() {
            if !is_hop_by_hop_header(name.as_str()) {
                forwarded_headers_forward.insert(name.clone(), value.clone());
            }
        }

        let snapshot_data_forward = json!({
            "test_case": "forward_proxy_absolute_uri",
            "method": req_forward.method().to_string(),
            "version": format!("{:?}", req_forward.version()),
            "target_uri": target_uri_forward.to_string(),
            "forwarded_headers": forwarded_headers_forward.iter()
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect::<std::collections::BTreeMap<_, _>>(),
        });

        insta::assert_json_snapshot!(snapshot_data_forward);
    }

    /// Test URI extraction with path and query parameters
    #[test]
    fn test_uri_with_query_params() {
        let service = ProxyService::new_with_upstream(Some("https://api.example.com".to_string()))
            .expect("Failed to create proxy service");

        let req = Request::builder()
            .method(Method::GET)
            .uri("/search?q=test&limit=10")
            .body(())
            .unwrap();

        let target_uri = service.extract_target_uri(&req).unwrap();

        assert_eq!(
            target_uri.to_string(),
            "https://api.example.com/search?q=test&limit=10"
        );
    }

    /// Test error handling for missing URI information
    #[test]
    fn test_uri_extraction_error() {
        let service =
            ProxyService::new_with_upstream(None).expect("Failed to create proxy service");

        // Request with no absolute URI and no Host header
        let req = Request::builder()
            .method(Method::GET)
            .uri("/relative/path")
            .body(())
            .unwrap();

        let result = service.extract_target_uri(&req);

        assert!(result.is_err());
        match result {
            Err(ProxyError::InvalidUri(_)) => {} // Expected
            _ => panic!("Expected InvalidUri error"),
        }
    }

    // =========================================================================
    // Traffic Discrimination Tests (REQ-CORE-003/F-002)
    // =========================================================================

    /// Test that MCP traffic (POST /mcp/v1 with JSON) is detected correctly.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
    #[test]
    fn test_traffic_discrimination_mcp_traffic() {
        // Standard MCP request to /mcp/v1
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v1")
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Mcp);
    }

    /// Test that MCP traffic to root path is detected correctly.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
    #[test]
    fn test_traffic_discrimination_mcp_root_path() {
        // MCP request to root path (for simple deployments)
        let req = Request::builder()
            .method(Method::POST)
            .uri("/")
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Mcp);
    }

    /// Test that HTTP traffic (non-MCP) is detected correctly.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
    #[test]
    fn test_traffic_discrimination_http_traffic() {
        // GET request (not MCP)
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/status")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
    }

    /// Test that POST to non-MCP paths is HTTP traffic.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
    #[test]
    fn test_traffic_discrimination_post_non_mcp_path() {
        // POST to non-MCP path
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/data")
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
    }

    /// Test that POST to /mcp/v1 without JSON content-type is HTTP traffic.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
    #[test]
    fn test_traffic_discrimination_mcp_path_wrong_content_type() {
        // POST to /mcp/v1 but with wrong content-type
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v1")
            .header("content-type", "text/plain")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
    }

    /// Test that POST to /mcp/v1 without content-type header is HTTP traffic.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
    #[test]
    fn test_traffic_discrimination_mcp_path_no_content_type() {
        // POST to /mcp/v1 but missing content-type
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v1")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
    }

    /// Test MCP traffic with content-type charset is still detected.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
    #[test]
    fn test_traffic_discrimination_mcp_content_type_with_charset() {
        // POST to /mcp/v1 with charset in content-type
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v1")
            .header("content-type", "application/json; charset=utf-8")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Mcp);
    }

    // =========================================================================
    // MCP Request Handling Tests (handle_mcp_request)
    // =========================================================================

    mod mcp_request_tests {
        use super::*;
        use crate::mcp_handler::{McpHandler, McpHandlerConfig};
        use bytes::Bytes;
        use std::sync::Arc;
        use thoughtgate_core::error::ThoughtGateError;
        use thoughtgate_core::governance::TaskStore;
        use thoughtgate_core::policy::engine::CedarEngine;
        use thoughtgate_core::transport::jsonrpc::{JsonRpcResponse, McpRequest};
        use thoughtgate_core::transport::upstream::UpstreamForwarder;

        /// Mock upstream that returns a simple JSON-RPC response.
        struct MockUpstream;

        #[async_trait::async_trait]
        impl UpstreamForwarder for MockUpstream {
            async fn forward(
                &self,
                request: &McpRequest,
            ) -> Result<JsonRpcResponse, ThoughtGateError> {
                Ok(JsonRpcResponse::success(
                    request.id.clone(),
                    serde_json::json!({"mock": "response"}),
                ))
            }

            async fn forward_batch(
                &self,
                requests: &[McpRequest],
            ) -> Result<Vec<JsonRpcResponse>, ThoughtGateError> {
                Ok(requests
                    .iter()
                    .filter(|r| !r.is_notification())
                    .map(|r| {
                        JsonRpcResponse::success(
                            r.id.clone(),
                            serde_json::json!({"mock": "response"}),
                        )
                    })
                    .collect())
            }
        }

        fn create_test_mcp_handler() -> Arc<McpHandler> {
            let task_store = Arc::new(TaskStore::with_defaults());
            let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));
            let config = McpHandlerConfig::default();

            Arc::new(McpHandler::new(
                Arc::new(MockUpstream),
                cedar_engine,
                task_store,
                config,
            ))
        }

        fn create_test_mcp_handler_with_size_limit(max_size: usize) -> Arc<McpHandler> {
            let task_store = Arc::new(TaskStore::with_defaults());
            let cedar_engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));
            let config = McpHandlerConfig {
                max_body_size: max_size,
                ..Default::default()
            };

            Arc::new(McpHandler::new(
                Arc::new(MockUpstream),
                cedar_engine,
                task_store,
                config,
            ))
        }

        /// Test MCP handler processes valid JSON-RPC request.
        ///
        /// # Traceability
        /// - Implements: REQ-CORE-003 (MCP Transport & Routing)
        #[tokio::test]
        async fn test_mcp_handler_valid_request() {
            let handler = create_test_mcp_handler();

            let body = Bytes::from(r#"{"jsonrpc":"2.0","id":1,"method":"test"}"#);
            let (status, response) = handler.handle(body).await;

            assert_eq!(status, StatusCode::OK);
            let response_str = String::from_utf8(response.to_vec()).unwrap();
            assert!(response_str.contains("\"jsonrpc\":\"2.0\""));
        }

        /// Test MCP handler returns JSON-RPC error for oversized body.
        ///
        /// McpHandler has its own size check that returns -32600 (Invalid Request)
        /// when the body exceeds max_body_size.
        ///
        /// # Traceability
        /// - Implements: REQ-CORE-003 (MCP Transport & Routing)
        #[tokio::test]
        async fn test_mcp_handler_size_limit_enforcement() {
            // Create handler with 100 byte limit
            let handler = create_test_mcp_handler_with_size_limit(100);

            // Create body larger than limit
            let large_body = Bytes::from(vec![b'x'; 200]);
            let (status, response) = handler.handle(large_body).await;

            // JSON-RPC errors return HTTP 200 per spec; the error is in the body
            assert_eq!(status, StatusCode::OK);
            let response_str = String::from_utf8(response.to_vec()).unwrap();
            // Invalid Request (-32600) for oversized body
            assert!(
                response_str.contains("-32600"),
                "Expected invalid request error -32600, got: {}",
                response_str
            );
            assert!(response_str.contains("exceeds maximum size"));
        }

        /// Test MCP handler returns proper JSON-RPC error for invalid JSON.
        ///
        /// Per JSON-RPC 2.0 spec, parse errors return HTTP 200 with error in body.
        ///
        /// # Traceability
        /// - Implements: REQ-CORE-003 (MCP Transport & Routing)
        #[tokio::test]
        async fn test_mcp_handler_invalid_json() {
            let handler = create_test_mcp_handler();

            let body = Bytes::from("not valid json");
            let (status, response) = handler.handle(body).await;

            // JSON-RPC errors return HTTP 200 per spec; the error is in the body
            assert_eq!(status, StatusCode::OK);
            let response_str = String::from_utf8(response.to_vec()).unwrap();
            assert!(response_str.contains("-32700")); // Parse error
        }

        /// Test MCP handler handles notifications (no id).
        ///
        /// # Traceability
        /// - Implements: REQ-CORE-003 (MCP Transport & Routing)
        #[tokio::test]
        async fn test_mcp_handler_notification() {
            let handler = create_test_mcp_handler();

            // Notification has no "id" field
            let body = Bytes::from(r#"{"jsonrpc":"2.0","method":"test"}"#);
            let (status, _response) = handler.handle(body).await;

            assert_eq!(status, StatusCode::NO_CONTENT);
        }

        /// Test MCP handler handles batch requests.
        ///
        /// # Traceability
        /// - Implements: REQ-CORE-003 (MCP Transport & Routing)
        #[tokio::test]
        async fn test_mcp_handler_batch_request() {
            let handler = create_test_mcp_handler();

            let body = Bytes::from(
                r#"[{"jsonrpc":"2.0","id":1,"method":"test"},{"jsonrpc":"2.0","id":2,"method":"test2"}]"#,
            );
            let (status, response) = handler.handle(body).await;

            assert_eq!(status, StatusCode::OK);
            let response_str = String::from_utf8(response.to_vec()).unwrap();
            // Batch response should be an array
            assert!(response_str.starts_with('['));
        }

        /// Test ProxyService size limit check in handle_mcp_request path.
        ///
        /// This tests the size limit enforcement that happens before
        /// the body is passed to McpHandler.
        ///
        /// # Traceability
        /// - Implements: REQ-CORE-003 (MCP Transport & Routing)
        #[test]
        fn test_proxy_service_mcp_handler_size_limit_config() {
            let handler = create_test_mcp_handler_with_size_limit(512);
            assert_eq!(handler.max_body_size(), 512);

            let default_handler = create_test_mcp_handler();
            // Default is 1MB
            assert_eq!(default_handler.max_body_size(), 1024 * 1024);
        }
    }
}
