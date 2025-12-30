//! Core proxy service implementation.

use crate::error::{ProxyError, ProxyResult};
use http::{StatusCode, Uri};
use http_body_util::{BodyStream, StreamBody, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use bytes::Bytes;
use futures_util::StreamExt;
use tower::Service;
use tracing::{error, info, warn};

/// Type alias for the client's streaming body type.
type ClientBody = http_body_util::combinators::BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

/// Main proxy service that handles HTTP and HTTPS requests with full TLS support.
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
    /// Create a new proxy service with default HTTP client configuration.
    pub fn new() -> Self {
        Self::new_with_upstream(None)
    }

    /// Create a new proxy service with optional upstream URL for reverse proxy mode.
    pub fn new_with_upstream(upstream_url: Option<String>) -> Self {
        // Build HTTPS connector with native OS certificate store
        let https_connector = HttpsConnectorBuilder::new()
            .with_native_roots()
            .expect("Failed to load native TLS roots")
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        let client = Client::builder(TokioExecutor::new())
            .http1_preserve_header_case(true)
            .http1_title_case_headers(true)
            .http1_allow_obsolete_multiline_headers_in_responses(true)
            .http2_keep_alive_while_idle(true)
            .pool_max_idle_per_host(32)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .build(https_connector);
        
        Self { 
            client,
            upstream_url,
        }
    }

    /// Handle an incoming HTTP request with zero-copy streaming.
    pub async fn handle_request(
        &self,
        req: Request<Incoming>,
    ) -> ProxyResult<Response<Incoming>> {
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
        let headers = upstream_req.headers_mut().unwrap();
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
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Body stream error: {}", e),
                ))
            })
        });

        let stream_body = StreamBody::new(mapped_stream);
        use http_body_util::BodyExt;
        let boxed_body: ClientBody = BodyExt::boxed(stream_body);

        let upstream_req = upstream_req.body(boxed_body)
            .map_err(|e| {
                error!(error = %e, "Failed to build upstream request");
                ProxyError::Connection(format!("Failed to build request: {}", e))
            })?;

        // Send request and stream response (zero-copy, no buffering)
        let upstream_res = self.client
            .request(upstream_req)
            .await
            .map_err(|e| {
                error!(error = %e, "Upstream client error");
                ProxyError::Connection(format!("Client error: {}", e))
            })?;

        Ok(upstream_res)
    }

    /// Extract target URI from request.
    fn extract_target_uri(&self, req: &Request<Incoming>) -> ProxyResult<Uri> {
        // If upstream URL is configured (reverse proxy mode), use it
        if let Some(upstream) = &self.upstream_url {
            let path = req.uri().path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or("/");
            let full_uri = format!("{}{}", upstream.trim_end_matches('/'), path);
            return full_uri
                .parse()
                .map_err(|e| ProxyError::InvalidUri(format!("Failed to parse upstream URI: {}", e)));
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
            let full_uri = format!("{}://{}{}", scheme, host_str, uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(""));
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
        Box::pin(async move {
            service.handle_request(req).await
        })
    }
}

/// Check if a header is a hop-by-hop header that shouldn't be forwarded.
fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "connection" | "keep-alive" | "proxy-authenticate" | "proxy-authorization" | "te" | "trailers" | "transfer-encoding" | "upgrade"
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
