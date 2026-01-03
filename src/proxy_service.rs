//! Core proxy service implementation.
//!
//! # Traceability
//! - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy)

use crate::error::{ProxyError, ProxyResult};
use bytes::Bytes;
use futures_util::StreamExt;
use http::Uri;
use http_body_util::{BodyStream, StreamBody};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
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
}

impl Clone for ProxyService {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            upstream_url: self.upstream_url.clone(),
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
        // Install default crypto provider for rustls (required for TLS to work)
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Create HTTP connector with TCP_NODELAY enabled for upstream connections
        // Implements: REQ-CORE-001 F-001 (Latency - TCP_NODELAY on both legs)
        let mut http_connector = HttpConnector::new();
        http_connector.set_nodelay(true);

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
        })
    }

    /// Handle an incoming HTTP request with zero-copy streaming.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-001 F-002 (Zero-Copy using bytes::Bytes and BodyStream)
    /// - Implements: REQ-CORE-001 F-003 (Transparency - preserve Content-Length/Transfer-Encoding)
    pub async fn handle_request(&self, req: Request<Incoming>) -> ProxyResult<Response<Incoming>> {
        // Extract target URI from request
        let target_uri = self.extract_target_uri(&req)?;

        info!(
            method = %req.method(),
            uri = %req.uri(),
            target = %target_uri,
            "Proxying request"
        );

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
            if let Some(name) = name_opt {
                if !is_hop_by_hop_header(name.as_str()) {
                    headers.insert(name, value);
                }
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
        let upstream_res = self.client.request(upstream_req).await.map_err(|e| {
            error!(error = %e, "Upstream client error");
            ProxyError::Connection(format!("Client error: {}", e))
        })?;

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
/// Note: `transfer-encoding` is NOT filtered per REQ-CORE-001 F-003.
/// As a transparent streaming proxy, we must preserve chunked encoding information.
/// Only connection management headers are filtered.
///
/// # Traceability
/// - Implements: REQ-CORE-001 F-003 (Transparency - preserve Transfer-Encoding)
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
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "upgrade" // NOTE: "transfer-encoding" intentionally NOT filtered for transparent streaming
    )
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
    #[test]
    fn test_hop_by_hop_headers() {
        // These ARE hop-by-hop and should be filtered
        assert!(is_hop_by_hop_header("connection"));
        assert!(is_hop_by_hop_header("Connection"));
        assert!(is_hop_by_hop_header("keep-alive"));
        assert!(is_hop_by_hop_header("proxy-authorization"));
        assert!(is_hop_by_hop_header("upgrade"));

        // These are NOT hop-by-hop and should NOT be filtered
        assert!(!is_hop_by_hop_header("content-type"));
        assert!(!is_hop_by_hop_header("authorization"));
        assert!(!is_hop_by_hop_header("user-agent"));

        // CRITICAL: transfer-encoding must NOT be filtered (REQ-CORE-001 F-003)
        assert!(!is_hop_by_hop_header("transfer-encoding"));
        assert!(!is_hop_by_hop_header("Transfer-Encoding"));
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
            .header("connection", "keep-alive") // Should be filtered
            .header("transfer-encoding", "chunked") // Should be filtered
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
                .collect::<std::collections::HashMap<_, _>>(),
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
                .collect::<std::collections::HashMap<_, _>>(),
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
