//! ThoughtGate - High-performance sidecar proxy for governing MCP and A2A agentic AI traffic.
//!
//! This proxy acts as a forward HTTP proxy that can be configured via HTTP_PROXY/HTTPS_PROXY
//! environment variables. It supports full HTTP/1.1 and HTTP/2, including CONNECT tunneling
//! for HTTPS connections.

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
#[derive(Parser, Debug)]
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
}

/// Connection tracker for graceful shutdown.
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

    let proxy_service = ProxyService::new();
    let service_stack = ServiceBuilder::new()
        .layer(LoggingLayer)
        .service(proxy_service);

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

        if start.elapsed().as_secs() % 5 == 0 {
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

/// Handle a single connection, detecting CONNECT vs normal HTTP.
async fn handle_connection<S>(
    stream: TcpStream,
    _peer_addr: SocketAddr,
    service: S,
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> Result<(), ProxyError>
where
    S: tower::Service<
            Request<Incoming>,
            Response = Response<Incoming>,
            Error = ProxyError,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    let mut peek_buf = [0u8; 7];
    let peek_result = {
        
        stream.peek(&mut peek_buf).await
    };

    match peek_result {
        Ok(n) if n >= 7 && &peek_buf[..7] == b"CONNECT" => {
            handle_connect(stream, _peer_addr).await
        }
        _ => {
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
    }
}

/// Handle CONNECT method for HTTPS tunneling.
async fn handle_connect(
    mut stream: TcpStream,
    _peer_addr: SocketAddr,
) -> Result<(), ProxyError> {
    use tokio::io::AsyncReadExt;
    
    // Read CONNECT request using buffering to avoid byte-by-byte syscalls
    const BUFFER_SIZE: usize = 4096;
    let mut buffer = Vec::with_capacity(BUFFER_SIZE);
    let mut read_buf = [0u8; BUFFER_SIZE];
    
    loop {
        let n = stream.read(&mut read_buf).await?;
        if n == 0 {
            return Err(ProxyError::Connection(
                "Connection closed before CONNECT request complete".to_string()
            ));
        }
        
        buffer.extend_from_slice(&read_buf[..n]);
        
        if buffer.len() >= 4 {
            let search_start = buffer.len().saturating_sub(n + 3);
            if let Some(_pos) = buffer[search_start..]
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            {
                break;
            }
        }
        
        if buffer.len() > 8192 {
            return Err(ProxyError::InvalidUri(
                "CONNECT request too large".to_string()
            ));
        }
    }

    let request_str = String::from_utf8_lossy(&buffer);
    let authority = request_str
        .lines()
        .next()
        .and_then(|line| {
            line.split_whitespace()
                .nth(1)
                .map(|s| s.to_string())
        })
        .ok_or_else(|| ProxyError::InvalidUri("Invalid CONNECT request".to_string()))?;

    let proxy_service = ProxyService::new();
    proxy_service.handle_connect(_peer_addr, stream, &authority).await
}

