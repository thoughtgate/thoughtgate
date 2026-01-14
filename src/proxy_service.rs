//! Core proxy service implementation.
//!
//! # v0.1 Status: DEFERRED
//!
//! This module implements the Green Path (zero-copy streaming) which is **deferred**
//! to v0.2+. In v0.1, requests are forwarded directly without streaming optimizations.
//!
//! The streaming infrastructure is retained for when LLM token streaming is needed.
//!
//! # When to Activate (v0.2+)
//!
//! - When streaming LLM responses (SSE token streams)
//! - When large file transfers need zero-copy optimization
//! - When response inspection during streaming is required
//!
//! # Traceability
//! - Deferred: REQ-CORE-001 (Zero-Copy Peeking Strategy)

use crate::error::{ProxyError, ProxyResult};
use crate::proxy_config::ProxyConfig;
use bytes::Bytes;
use futures_util::StreamExt;
use http::Uri;
use http_body_util::{BodyStream, StreamBody};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;
use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};
use std::net::SocketAddr;
use std::time::Duration;
use tower::Service;
use tracing::{error, info};

/// Type alias for the client's streaming body type.
type ClientBody =
    http_body_util::combinators::BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

/// Main proxy service that handles HTTP and HTTPS requests with full TLS support.
///
/// This service implements zero-copy streaming via `BodyStream` to minimize latency
/// and memory overhead. It supports both forward proxy (HTTP_PROXY/HTTPS_PROXY) and
/// reverse proxy (UPSTREAM_URL) modes.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy)
pub struct ProxyService {
    /// HTTPS-capable client for upstream connections with streaming support.
    client: Client<HttpsConnector<HttpConnector>, ClientBody>,
    /// Optional fixed upstream URL for reverse proxy mode
    upstream_url: Option<String>,
    /// Runtime configuration
    config: ProxyConfig,
}

impl Clone for ProxyService {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            upstream_url: self.upstream_url.clone(),
            config: self.config.clone(),
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
        // Install default crypto provider for rustls (required for TLS to work)
        let _ = rustls::crypto::ring::default_provider().install_default();

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
            .pool_max_idle_per_host(32)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .build(https_connector);

        Ok(Self {
            client,
            upstream_url,
            config,
        })
    }

    /// Get a reference to the proxy configuration.
    pub fn config(&self) -> &ProxyConfig {
        &self.config
    }

    /// Handle an incoming HTTP request with zero-copy streaming.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-001 F-001 (Zero-Copy using bytes::Bytes and BodyStream)
    /// - Implements: REQ-CORE-001 F-002 (Fail-Fast Error Propagation)
    /// - Implements: REQ-CORE-001 F-003 (Transparency - preserve Content-Length/Transfer-Encoding)
    /// - Implements: REQ-CORE-001 F-004 (Protocol Upgrade Handling)
    pub async fn handle_request(&self, req: Request<Incoming>) -> ProxyResult<Response<Incoming>> {
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
        use http_body_util::BodyExt;
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

        // TODO(REQ-CORE-001 F-005): KNOWN LIMITATION - Timeout Wrapping
        //
        // Current State: TimeoutBody and ProxyBody wrappers exist but are not applied
        // to the Green Path response bodies.
        //
        // Why: The current architecture returns `Response<Incoming>` directly from the
        // client. Wrapping the body would change the return type to
        // `Response<ProxyBody<TimeoutBody<Incoming>>>`, which would require:
        // 1. Type erasure via BoxBody (adds allocation overhead)
        // 2. Changes to Service trait implementation
        // 3. Updates to all call sites in main.rs
        //
        // Impact: The proxy is vulnerable to slow-read attacks on the Green Path.
        // Chunks can be delayed indefinitely without triggering timeouts.
        //
        // Remediation Path:
        // 1. Change return type to use BoxBody for type erasure
        // 2. Apply TimeoutBody wrapper with config from ProxyConfig
        // 3. Apply ProxyBody wrapper for metrics and cancellation
        // 4. Update Service trait and main.rs to handle boxed bodies
        //
        // Note: The Amber Path (BufferedForwarder) already has timeout protection
        // via tokio::time::timeout wrapping the entire buffering operation.

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

        Ok(upstream_res)
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
    type Response = Response<Incoming>;
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

/// Extract host from URI.
#[allow(dead_code)]
fn extract_host(uri: &Uri) -> ProxyResult<String> {
    uri.host()
        .ok_or_else(|| ProxyError::InvalidUri("URI missing host".to_string()))
        .map(|s| s.to_string())
}

/// Extract port from URI, defaulting to 80 for HTTP or 443 for HTTPS.
#[allow(dead_code)]
fn extract_port(uri: &Uri) -> ProxyResult<u16> {
    if let Some(port) = uri.port_u16() {
        Ok(port)
    } else {
        match uri.scheme_str() {
            Some("https") => Ok(443),
            Some("http") => Ok(80),
            _ => Err(ProxyError::InvalidUri(
                "Cannot determine port from URI".to_string(),
            )),
        }
    }
}

/// Configure socket with optimized options for zero-copy streaming.
///
/// # Traceability
/// - Implements: REQ-CORE-001 Section 3.2 (Network Optimization)
///
/// # Socket Options
/// - TCP_NODELAY: Disable Nagle's algorithm for low-latency
/// - SO_KEEPALIVE: Keep connections alive during idle periods
/// - SO_RCVBUF / SO_SNDBUF: Set buffer sizes for throughput
pub fn configure_socket(socket: &Socket, config: &ProxyConfig) -> ProxyResult<()> {
    // Set TCP_NODELAY (disable Nagle's algorithm)
    socket
        .set_nodelay(config.tcp_nodelay)
        .map_err(|e| ProxyError::Connection(format!("Failed to set TCP_NODELAY: {}", e)))?;

    // Set TCP keepalive
    let keepalive = TcpKeepalive::new().with_time(Duration::from_secs(config.tcp_keepalive_secs));
    socket
        .set_tcp_keepalive(&keepalive)
        .map_err(|e| ProxyError::Connection(format!("Failed to set SO_KEEPALIVE: {}", e)))?;

    // Set socket buffer sizes
    socket
        .set_recv_buffer_size(config.socket_buffer_size)
        .map_err(|e| ProxyError::Connection(format!("Failed to set SO_RCVBUF: {}", e)))?;

    socket
        .set_send_buffer_size(config.socket_buffer_size)
        .map_err(|e| ProxyError::Connection(format!("Failed to set SO_SNDBUF: {}", e)))?;

    Ok(())
}

/// Create a configured TCP socket for the given address.
///
/// # Traceability
/// - Implements: REQ-CORE-001 Section 3.2 (Network Optimization)
pub fn create_configured_socket(addr: &SocketAddr, config: &ProxyConfig) -> ProxyResult<Socket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };

    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
        .map_err(|e| ProxyError::Connection(format!("Failed to create socket: {}", e)))?;

    configure_socket(&socket, config)?;

    Ok(socket)
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
}
