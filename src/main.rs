//! ThoughtGate - High-performance sidecar proxy for governing MCP and A2A agentic AI traffic.
//!
//! This proxy acts as a forward HTTP proxy that can be configured via HTTP_PROXY/HTTPS_PROXY
//! environment variables. It supports full HTTP/1.1 and HTTP/2, with zero-copy streaming
//! for low-latency AI traffic.
//!
//! # Traceability
//! - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy)

mod error;
mod logging_layer;
mod proxy_service;

use clap::Parser;
use error::ProxyError;
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto;
use logging_layer::LoggingLayer;
use proxy_service::ProxyService;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::time::sleep;
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

    /// Graceful shutdown timeout in seconds (default: 30)
    #[arg(long, env = "SHUTDOWN_TIMEOUT", default_value = "30")]
    shutdown_timeout: u64,

    /// Optional upstream URL for reverse proxy mode (e.g., "http://backend:8080")
    /// When set, all requests are forwarded to this upstream instead of using the request's target
    #[arg(long, env = "UPSTREAM_URL")]
    upstream_url: Option<String>,
}

/// Connection tracker for graceful shutdown.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - connection management)
#[derive(Clone)]
struct ConnectionTracker {
    active_connections: Arc<AtomicUsize>,
}

impl ConnectionTracker {
    fn new() -> Self {
        Self {
            active_connections: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn increment(&self) {
        self.active_connections.fetch_add(1, Ordering::SeqCst);
    }

    fn decrement(&self) {
        self.active_connections.fetch_sub(1, Ordering::SeqCst);
    }

    fn count(&self) -> usize {
        self.active_connections.load(Ordering::SeqCst)
    }
}

/// Main entry point for the ThoughtGate proxy.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::parse();
    let addr = format!("{}:{}", config.bind, config.port);
    let listener = TcpListener::bind(&addr).await?;

    info!(
        bind = %config.bind,
        port = config.port,
        shutdown_timeout = config.shutdown_timeout,
        addr = %addr,
        "ThoughtGate Proxy starting"
    );

    let proxy_service = Arc::new(ProxyService::new_with_upstream(
        config.upstream_url.clone(),
    )?);
    let service_stack = ServiceBuilder::new()
        .layer(LoggingLayer)
        .service(proxy_service.as_ref().clone());

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let shutdown_tx_clone = shutdown_tx.clone();
    let connection_tracker = ConnectionTracker::new();
    let tracker_clone = connection_tracker.clone();

    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                info!("Received SIGINT (Ctrl+C), initiating graceful shutdown");
                let _ = shutdown_tx_clone.send(());
            }
            Err(e) => {
                error!(error = %e, "Failed to listen for SIGINT");
            }
        }
    });

    #[cfg(unix)]
    {
        let shutdown_tx_sigterm = shutdown_tx.clone();
        tokio::spawn(async move {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut sigterm) => {
                    sigterm.recv().await;
                    info!("Received SIGTERM, initiating graceful shutdown");
                    let _ = shutdown_tx_sigterm.send(());
                }
                Err(e) => {
                    error!(error = %e, "Failed to listen for SIGTERM");
                }
            }
        });
    }

    let mut shutdown_rx = shutdown_tx.subscribe();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, peer_addr)) => {
                        // Disable Nagle's algorithm for low-latency streaming
                        // Implements: REQ-CORE-001 F-001 (TCP_NODELAY enforcement)
                        if let Err(e) = stream.set_nodelay(true) {
                            error!(error = %e, "Failed to set TCP_NODELAY");
                        }

                        let service_stack = service_stack.clone();
                        let mut conn_shutdown_rx = shutdown_tx.subscribe();
                        let tracker = connection_tracker.clone();

                        tracker.increment();

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

                            tracker.decrement();
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

    info!(
        active_connections = tracker_clone.count(),
        timeout_seconds = config.shutdown_timeout,
        "Waiting for active connections to drain"
    );

    let shutdown_deadline = Duration::from_secs(config.shutdown_timeout);
    let start = std::time::Instant::now();

    while tracker_clone.count() > 0 {
        if start.elapsed() >= shutdown_deadline {
            warn!(
                active_connections = tracker_clone.count(),
                "Shutdown timeout reached, forcing exit"
            );
            break;
        }

        sleep(Duration::from_millis(100)).await;

        if start.elapsed().as_secs().is_multiple_of(5) {
            info!(
                active_connections = tracker_clone.count(),
                elapsed_seconds = start.elapsed().as_secs(),
                "Still draining connections..."
            );
        }
    }

    if tracker_clone.count() == 0 {
        info!("All connections drained, shutting down cleanly");
    }

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
    if let Ok(n) = stream.peek(&mut peek_buf).await {
        if n >= 7 && &peek_buf[..7] == b"CONNECT" {
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
    }

    let io = TokioIo::new(stream);

    let svc_fn = hyper::service::service_fn(move |req| {
        let mut svc = service.clone();
        async move {
            svc.call(req).await.map_err(|e| {
                error!(error = %e, "Service error");
                format!("{}", e)
            })
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
