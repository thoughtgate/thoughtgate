//! Core proxy service implementation.

use crate::error::{ProxyError, ProxyResult};
use http::Uri;
use http_body_util::{BodyStream, StreamBody};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use bytes::Bytes;
use futures_util::StreamExt;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{self, AsyncWriteExt, split};
use tokio::net::TcpStream;
use tower::Service;
use tracing::{debug, error, info, warn};

/// Type alias for the client's streaming body type.
type ClientBody = http_body_util::combinators::BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

/// Main proxy service that handles HTTP requests and CONNECT tunneling.
pub struct ProxyService {
    /// Arc-wrapped HTTP client for upstream connections with streaming support.
    /// Type-erased to avoid exposing connector implementation details.
    client: Arc<dyn Fn(Request<ClientBody>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response<Incoming>, hyper_util::client::legacy::Error>> + Send>> + Send + Sync>,
}

impl Clone for ProxyService {
    fn clone(&self) -> Self {
        Self {
            client: Arc::clone(&self.client),
        }
    }
}

impl ProxyService {
    /// Create a new proxy service with default HTTP client configuration.
    pub fn new() -> Self {
        let client = Client::builder(TokioExecutor::new())
            .http1_preserve_header_case(true)
            .http1_title_case_headers(true)
            .http1_allow_obsolete_multiline_headers_in_responses(true)
            .http2_keep_alive_while_idle(true)
            .build_http::<ClientBody>();
        
        // Wrap client in Arc for cloning support
        let client_arc = Arc::new(client);
        let client_fn: Arc<dyn Fn(Request<ClientBody>) -> _ + Send + Sync> = Arc::new(move |req: Request<ClientBody>| {
            let client = Arc::clone(&client_arc);
            Box::pin(async move {
                (*client).request(req).await
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response<Incoming>, hyper_util::client::legacy::Error>> + Send>>
        });
        
        Self { client: client_fn }
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
        let fut = (self.client)(upstream_req);
        let upstream_res = fut
            .await
            .map_err(|e| {
                error!(error = %e, "Upstream client error");
                ProxyError::Connection(format!("Client error: {}", e))
            })?;

        Ok(upstream_res)
    }

    /// Extract target URI from request.
    fn extract_target_uri(&self, req: &Request<Incoming>) -> ProxyResult<Uri> {
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

    /// Handle CONNECT method for HTTPS tunneling.
    pub async fn handle_connect(
        &self,
        _addr: SocketAddr,
        mut stream: TcpStream,
        authority: &str,
    ) -> ProxyResult<()> {
        info!(
            authority = authority,
            "CONNECT request received"
        );

        // Parse authority (host:port)
        let (host, port) = parse_authority(authority)?;
        let target = format!("{}:{}", host, port);

        // Connect to upstream
        let upstream = TcpStream::connect(&target)
            .await
            .map_err(|e| {
                error!(
                    target = %target,
                    error = %e,
                    "Failed to connect to upstream"
                );
                ProxyError::Connection(format!("Failed to connect to {}: {}", target, e))
            })?;

        info!(
            target = %target,
            "Connected to upstream, starting tunnel"
        );

        // Send 200 Connection Established
        let response = "HTTP/1.1 200 Connection Established\r\n\r\n";
        stream
            .write_all(response.as_bytes())
            .await
            .map_err(|e| ProxyError::Io(e))?;

        // Bidirectional tunnel using owned stream halves
        let (mut client_read, mut client_write) = split(stream);
        let (mut upstream_read, mut upstream_write) = split(upstream);

        let client_to_upstream = tokio::spawn(async move {
            let copied = io::copy(&mut client_read, &mut upstream_write).await;
            if let Err(e) = copied {
                warn!(error = %e, "Client->Upstream copy error");
            } else {
                debug!(bytes = copied.unwrap_or(0), "Client->Upstream copy completed");
            }
        });

        let upstream_to_client = tokio::spawn(async move {
            let copied = io::copy(&mut upstream_read, &mut client_write).await;
            if let Err(e) = copied {
                warn!(error = %e, "Upstream->Client copy error");
            } else {
                debug!(bytes = copied.unwrap_or(0), "Upstream->Client copy completed");
            }
        });

        // Wait for either direction to complete
        tokio::select! {
            _ = client_to_upstream => {
                debug!("Client->Upstream tunnel closed");
            }
            _ = upstream_to_client => {
                debug!("Upstream->Client tunnel closed");
            }
        }

        info!(authority = authority, "CONNECT tunnel closed");
        Ok(())
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

/// Parse authority string (host:port) into host and port.
fn parse_authority(authority: &str) -> ProxyResult<(String, u16)> {
    if let Some(colon_pos) = authority.rfind(':') {
        let host = authority[..colon_pos].to_string();
        let port_str = &authority[colon_pos + 1..];
        let port = port_str
            .parse::<u16>()
            .map_err(|_| ProxyError::InvalidUri(format!("Invalid port: {}", port_str)))?;
        Ok((host, port))
    } else {
        Ok((authority.to_string(), 443))
    }
}

