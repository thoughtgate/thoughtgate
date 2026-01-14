//! ThoughtGate - High-performance sidecar proxy for governing MCP and A2A agentic AI traffic.
//!
//! This proxy acts as a forward HTTP proxy that can be configured via HTTP_PROXY/HTTPS_PROXY
//! environment variables. It supports full HTTP/1.1 and HTTP/2, with zero-copy streaming
//! for low-latency AI traffic.
//!
//! # Traceability
//! - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy)
//! - Implements: REQ-CORE-005 (Operational Lifecycle)

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use clap::Parser;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use thoughtgate::error::ProxyError;
use thoughtgate::lifecycle::{DrainResult, LifecycleConfig, LifecycleManager, health_router};
use thoughtgate::logging_layer::LoggingLayer;
use thoughtgate::proxy_config::ProxyConfig;
use thoughtgate::proxy_service::ProxyService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, broadcast};
use tower::ServiceBuilder;
use tracing::{error, info, warn};

/// Configuration for the proxy server.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - configuration)
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Config {
    /// Port to listen on (default: 4141, or PROXY_PORT env var)
    #[arg(short, long, env = "PROXY_PORT", default_value = "4141")]
    port: u16,

    /// Bind address (default: 127.0.0.1)
    #[arg(short, long, default_value = "127.0.0.1")]
    bind: String,

    /// Optional upstream URL for reverse proxy mode (e.g., "http://backend:8080")
    /// When set, all requests are forwarded to this upstream instead of using the request's target
    #[arg(long, env = "UPSTREAM_URL")]
    upstream_url: Option<String>,

    /// Health endpoint port (default: same as main port via separate listener)
    #[arg(long, env = "THOUGHTGATE_HEALTH_PORT")]
    health_port: Option<u16>,
}

/// Main entry point for the ThoughtGate proxy.
///
/// Implements: REQ-CORE-005/F-001 (Startup Sequencing)
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Phase 1: Initialize observability
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli_config = Config::parse();
    let proxy_config = ProxyConfig::from_env();

    // Phase 2: Initialize lifecycle manager
    // Implements: REQ-CORE-005/F-001
    let lifecycle_config = LifecycleConfig::from_env();
    let lifecycle = Arc::new(LifecycleManager::new(lifecycle_config));

    // Initialize OpenTelemetry metrics (REQ-CORE-001 NFR-001)
    #[cfg(feature = "metrics")]
    {
        use opentelemetry::global;
        use opentelemetry_sdk::metrics::SdkMeterProvider;
        use thoughtgate::metrics;

        let exporter = opentelemetry_prometheus::exporter()
            .with_registry(prometheus::default_registry().clone())
            .build()?;

        let provider = SdkMeterProvider::builder().with_reader(exporter).build();
        global::set_meter_provider(provider.clone());

        let meter = global::meter("thoughtgate");
        metrics::init_metrics(&meter);

        // Spawn metrics endpoint server
        let metrics_port = proxy_config.metrics_port;
        tokio::spawn(async move {
            if let Err(e) = serve_metrics(metrics_port).await {
                error!(error = %e, "Metrics server error");
            }
        });

        info!(metrics_port = metrics_port, "Metrics endpoint started");
    }

    // Phase 3: Start health endpoint server
    // Implements: REQ-CORE-005/F-002, F-003
    let health_port = cli_config.health_port.unwrap_or_else(|| {
        // Validate that port + 1 won't overflow
        if cli_config.port == 65535 {
            warn!(
                main_port = cli_config.port,
                "Main port is 65535, health endpoint will use port 65534 instead of port+1"
            );
            65534
        } else {
            cli_config.port + 1
        }
    });
    let health_bind = cli_config.bind.clone();
    let health_lifecycle = lifecycle.clone();
    let health_shutdown = lifecycle.shutdown_token();
    tokio::spawn(async move {
        if let Err(e) =
            serve_health_endpoints(health_bind, health_port, health_lifecycle, health_shutdown)
                .await
        {
            error!(error = %e, "Health server error");
        }
    });
    info!(
        health_port = health_port,
        "Health endpoints started (/health, /ready)"
    );

    // Phase 4: Bind main listener
    let addr = format!("{}:{}", cli_config.bind, cli_config.port);
    let listener = TcpListener::bind(&addr).await?;

    info!(
        bind = %cli_config.bind,
        port = cli_config.port,
        health_port = health_port,
        drain_timeout_secs = lifecycle.config().drain_timeout.as_secs(),
        addr = %addr,
        tcp_nodelay = proxy_config.tcp_nodelay,
        tcp_keepalive_secs = proxy_config.tcp_keepalive_secs,
        max_concurrent_streams = proxy_config.max_concurrent_streams,
        socket_buffer_size = proxy_config.socket_buffer_size,
        "ThoughtGate Proxy starting"
    );

    // Phase 5: Create proxy service
    let proxy_service = Arc::new(ProxyService::new_with_config(
        cli_config.upstream_url.clone(),
        proxy_config.clone(),
    )?);
    let service_stack = ServiceBuilder::new()
        .layer(LoggingLayer)
        .service(proxy_service.as_ref().clone());

    // Setup signal handlers
    // Implements: REQ-CORE-005/F-004 (Signal Handling)
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    setup_signal_handlers(shutdown_tx.clone(), lifecycle.clone());

    let config_clone = proxy_config.clone();

    // Semaphore for concurrency limiting (REQ-CORE-001 Section 3.2)
    let semaphore = Arc::new(Semaphore::new(proxy_config.max_concurrent_streams));

    let mut shutdown_rx = shutdown_tx.subscribe();

    // Mark as ready
    // Implements: REQ-CORE-005/F-001
    lifecycle.mark_policies_loaded(); // Policies loaded (proxy mode)
    lifecycle.mark_task_store_initialized(); // Task store ready (proxy mode)
    lifecycle.update_upstream_health(true, None); // Assume healthy initially
    lifecycle.mark_ready();

    // Main accept loop
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, peer_addr)) => {
                        // Check if shutting down - reject new connections
                        // Implements: REQ-CORE-005/F-004.1, EC-OPS-006
                        let request_guard = match lifecycle.track_request() {
                            Some(guard) => guard,
                            None => {
                                // Shutting down, reject new connections
                                warn!(peer = %peer_addr, "Rejected connection: shutting down");
                                tokio::spawn(async move {
                                    let _ = send_503_shutdown_response(stream).await;
                                });
                                continue;
                            }
                        };

                        // Try to acquire semaphore permit (REQ-CORE-001 Section 3.2)
                        let permit = match semaphore.clone().try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                // Semaphore exhausted - return 503 immediately
                                warn!(
                                    peer = %peer_addr,
                                    max_streams = proxy_config.max_concurrent_streams,
                                    "Rejected connection: max concurrent streams reached"
                                );
                                drop(request_guard); // Release lifecycle tracking
                                tokio::spawn(async move {
                                    let _ = send_503_response(stream).await;
                                });
                                continue;
                            }
                        };

                        // Configure socket with optimized options
                        // Implements: REQ-CORE-001 Section 3.2 (Network Optimization)
                        if let Err(e) = configure_tcp_stream(&stream, &config_clone) {
                            error!(error = %e, "Failed to configure socket");
                        }

                        let service_stack = service_stack.clone();
                        let mut conn_shutdown_rx = shutdown_tx.subscribe();

                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(
                                stream,
                                peer_addr,
                                service_stack,
                                &mut conn_shutdown_rx,
                            )
                            .await
                            {
                                error!(error = %e, "Connection handling error");
                            }

                            // Explicit drops for clarity (both happen automatically at scope end)
                            drop(request_guard); // Decrements lifecycle request counter
                            drop(permit); // Release semaphore permit
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to accept connection");
                    }
                }
            }

            _ = shutdown_rx.recv() => {
                info!("Shutdown signal received, stopping new connections");
                break;
            }
        }
    }

    // Graceful shutdown sequence
    // Implements: REQ-CORE-005/F-004, F-005
    info!(
        active_requests = lifecycle.active_request_count(),
        drain_timeout_secs = lifecycle.config().drain_timeout.as_secs(),
        "Waiting for active requests to drain"
    );

    // Drain requests
    // Implements: REQ-CORE-005/F-005
    let drain_result = lifecycle.drain_requests().await;

    // Mark as stopped
    lifecycle.mark_stopped();

    // Exit with appropriate code
    // Implements: REQ-CORE-005/F-004.6
    match drain_result {
        DrainResult::Complete => {
            info!("All requests drained, shutting down cleanly");
            Ok(())
        }
        DrainResult::Timeout { remaining } => {
            // Return error instead of std::process::exit(1) to allow proper cleanup
            // The caller (main) will convert this to an exit code
            Err(format!(
                "Drain timeout exceeded with {} remaining requests",
                remaining
            )
            .into())
        }
    }
}

/// Setup signal handlers for graceful shutdown.
///
/// Implements: REQ-CORE-005/§5.2 (Signal Handling)
///
/// - SIGINT (Ctrl+C): Begin graceful shutdown
/// - SIGTERM: Begin graceful shutdown
fn setup_signal_handlers(shutdown_tx: broadcast::Sender<()>, lifecycle: Arc<LifecycleManager>) {
    // SIGINT handler
    let shutdown_tx_sigint = shutdown_tx.clone();
    let lifecycle_sigint = lifecycle.clone();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                info!("Received SIGINT (Ctrl+C), initiating graceful shutdown");
                lifecycle_sigint.begin_shutdown();
                let _ = shutdown_tx_sigint.send(());
            }
            Err(e) => {
                error!(error = %e, "Failed to listen for SIGINT");
            }
        }
    });

    // SIGTERM handler (Unix only)
    #[cfg(unix)]
    {
        let shutdown_tx_sigterm = shutdown_tx;
        let lifecycle_sigterm = lifecycle;
        tokio::spawn(async move {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut sigterm) => {
                    sigterm.recv().await;
                    info!("Received SIGTERM, initiating graceful shutdown");
                    lifecycle_sigterm.begin_shutdown();
                    let _ = shutdown_tx_sigterm.send(());
                }
                Err(e) => {
                    error!(error = %e, "Failed to listen for SIGTERM");
                }
            }
        });
    }
}

/// Serve health endpoints on a dedicated port.
///
/// Implements: REQ-CORE-005/F-002, F-003
async fn serve_health_endpoints(
    bind: String,
    port: u16,
    lifecycle: Arc<LifecycleManager>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = health_router(lifecycle);
    let addr = format!("{}:{}", bind, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.cancelled().await;
        })
        .await?;

    Ok(())
}

/// Handle a single connection with HTTP protocol.
///
/// # Traceability
/// - Implements: REQ-CORE-001 F-001 (TCP_NODELAY enforcement)
/// - Implements: REQ-CORE-002 (Conditional Termination - CONNECT rejection)
async fn handle_connection<S>(
    mut stream: TcpStream,
    _peer_addr: SocketAddr,
    service: S,
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> Result<(), ProxyError>
where
    S: tower::Service<Request<Incoming>, Response = Response<Incoming>, Error = ProxyError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    // Peek to detect CONNECT method and reject it immediately
    // PERF(latency): Zero-copy peek avoids buffering overhead
    let mut peek_buf = [0u8; 7];
    if let Ok(n) = stream.peek(&mut peek_buf).await
        && n >= 7
        && &peek_buf[..7] == b"CONNECT"
    {
        use tokio::io::AsyncWriteExt;

        warn!("Rejected CONNECT request - ThoughtGate is a termination proxy");

        let body = "405 Method Not Allowed\n\n\
                 ThoughtGate is a termination proxy for AI governance.\n\
                 It requires visibility into request headers and bodies.\n\n\
                 Send plain HTTP requests to http://127.0.0.1:4141 instead.";
        let content_length = body.len();
        let response = format!(
            "HTTP/1.1 405 Method Not Allowed\r\n\
                 Content-Type: text/plain\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {}",
            content_length, body
        );

        let _ = stream.write_all(response.as_bytes()).await;
        return Ok(());
    }

    let io = TokioIo::new(stream);

    let svc_fn = hyper::service::service_fn(move |req| {
        let mut svc = service.clone();
        async move {
            // Convert ProxyError to proper HTTP response with correct status codes
            // Implements: REQ-CORE-001 F-002 (Fail-Fast Error Propagation)
            // Implements: REQ-CORE-002 F-002 (Fail-Closed State)
            let result: Result<_, std::convert::Infallible> = match svc.call(req).await {
                Ok(response) => {
                    // Box the successful response body
                    Ok(response.map(|body| body.boxed()))
                }
                Err(e) => {
                    error!(error = %e, "Service error");
                    // Use to_response() to map error to proper HTTP status code
                    // Map Infallible error type to hyper::Error to match Incoming body
                    Ok(e.to_response().map(|body| {
                        body.map_err(|never: std::convert::Infallible| match never {})
                            .boxed()
                    }))
                }
            };
            result
        }
    });

    let executor = hyper_util::rt::TokioExecutor::new();
    let builder = auto::Builder::new(executor);
    let conn = builder.serve_connection_with_upgrades(io, svc_fn);

    tokio::pin!(conn);

    tokio::select! {
        result = &mut conn => {
            if let Err(e) = result {
                error!(error = %e, "Connection error");
            }
        }
        _ = shutdown_rx.recv() => {
            info!("Shutdown signal received, gracefully closing connection");
            conn.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(Duration::from_secs(5), conn).await;
        }
    }

    Ok(())
}

/// Configure a TcpStream with optimized socket options.
///
/// # Traceability
/// - Implements: REQ-CORE-001 Section 3.2 (Network Optimization)
fn configure_tcp_stream(stream: &TcpStream, config: &ProxyConfig) -> std::io::Result<()> {
    // Set TCP_NODELAY
    stream.set_nodelay(config.tcp_nodelay)?;

    // Convert to socket2::Socket for advanced options
    let socket = socket2::SockRef::from(stream);

    // Set TCP keepalive
    let keepalive =
        socket2::TcpKeepalive::new().with_time(Duration::from_secs(config.tcp_keepalive_secs));
    socket.set_tcp_keepalive(&keepalive)?;

    // Set socket buffer sizes
    socket.set_recv_buffer_size(config.socket_buffer_size)?;
    socket.set_send_buffer_size(config.socket_buffer_size)?;

    Ok(())
}

/// Send a 503 Service Unavailable response when semaphore is exhausted.
///
/// # Traceability
/// - Implements: REQ-CORE-001 Section 3.2 (Concurrency Limit)
async fn send_503_response(mut stream: TcpStream) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let body = "503 Service Unavailable\n\n\
                ThoughtGate has reached its maximum concurrent stream limit.\n\
                Please retry your request in a moment.";
    let content_length = body.len();
    let response = format!(
        "HTTP/1.1 503 Service Unavailable\r\n\
         Content-Type: text/plain\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Retry-After: 1\r\n\
         \r\n\
         {}",
        content_length, body
    );

    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Send a 503 Service Unavailable response during shutdown.
///
/// Implements: REQ-CORE-005/F-004.3
async fn send_503_shutdown_response(mut stream: TcpStream) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let body = "503 Service Unavailable\n\n\
                ThoughtGate is shutting down.\n\
                Please retry your request on another instance.";
    let content_length = body.len();
    let response = format!(
        "HTTP/1.1 503 Service Unavailable\r\n\
         Content-Type: text/plain\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Retry-After: 5\r\n\
         \r\n\
         {}",
        content_length, body
    );

    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Serve Prometheus metrics endpoint.
///
/// # Traceability
/// - Implements: REQ-CORE-001 NFR-001 (Observability - Metrics Endpoint)
#[cfg(feature = "metrics")]
async fn serve_metrics(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    use axum::{Router, response::IntoResponse, routing::get};

    async fn metrics_handler() -> impl IntoResponse {
        use prometheus::{Encoder, TextEncoder};

        let metrics = prometheus::default_registry().gather();
        let encoder = TextEncoder::new();
        let mut buffer = Vec::new();
        if let Err(e) = encoder.encode(&metrics, &mut buffer) {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to encode metrics: {}", e),
            )
                .into_response();
        }
        (
            axum::http::StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            buffer,
        )
            .into_response()
    }

    let app = Router::new().route("/metrics", get(metrics_handler));

    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    info!(addr = %addr, "Metrics server listening");

    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use thoughtgate::lifecycle::LifecycleConfig;

    #[test]
    fn test_lifecycle_integration() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        assert!(!lifecycle.is_ready());
        assert!(!lifecycle.is_shutting_down());

        lifecycle.mark_ready();
        assert!(lifecycle.is_ready());

        // Can track requests when ready
        let guard = lifecycle.track_request();
        assert!(guard.is_some());
        assert_eq!(lifecycle.active_request_count(), 1);
        drop(guard);
        assert_eq!(lifecycle.active_request_count(), 0);

        // Begin shutdown
        lifecycle.begin_shutdown();
        assert!(lifecycle.is_shutting_down());

        // Cannot track new requests during shutdown
        let guard = lifecycle.track_request();
        assert!(guard.is_none());
    }

    #[test]
    fn test_request_guard_panic_safety() {
        use std::panic::AssertUnwindSafe;

        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_ready();

        assert_eq!(lifecycle.active_request_count(), 0);

        // Simulate a panic in a request handler
        let lifecycle_clone = Arc::clone(&lifecycle);
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = lifecycle_clone.track_request();
            assert_eq!(lifecycle_clone.active_request_count(), 1);
            panic!("Simulated panic in request handler");
        }));

        assert!(result.is_err());

        // Even after panic, counter should be decremented
        assert_eq!(lifecycle.active_request_count(), 0);
    }

    #[tokio::test]
    async fn test_request_guard_async_panic_safety() {
        let lifecycle = Arc::new(LifecycleManager::new(LifecycleConfig::default()));
        lifecycle.mark_ready();

        assert_eq!(lifecycle.active_request_count(), 0);

        // Simulate what happens in the actual code: spawn a task that panics
        let lifecycle_clone = lifecycle.clone();
        let handle = tokio::spawn(async move {
            let _guard = lifecycle_clone.track_request();
            // Simulate some async work
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            panic!("Simulated async panic");
        });

        // Wait for the task to complete (it will panic)
        let result = handle.await;
        assert!(result.is_err());

        // Counter should be back to 0 even after panic
        assert_eq!(lifecycle.active_request_count(), 0);
    }
}
