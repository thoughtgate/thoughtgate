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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thoughtgate::admin::AdminServer;
use thoughtgate::config::{self, Version, find_config_file, load_and_validate};
use thoughtgate::error::ProxyError;
use thoughtgate::lifecycle::{DrainResult, LifecycleConfig, LifecycleManager};
use thoughtgate::logging_layer::logging_layer;
use thoughtgate::ports::{admin_port, inbound_port, outbound_port};
use thoughtgate::proxy_config::ProxyConfig;
use thoughtgate::proxy_service::ProxyService;
use thoughtgate::transport::{
    McpHandler, McpHandlerConfig, UpstreamClient, UpstreamConfig, create_governance_components,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tower::ServiceBuilder;
use tracing::{debug, error, info, warn};

/// Configuration for the proxy server.
///
/// # Envoy-style 3-Port Model
///
/// | Port | Env Variable | Default | Purpose |
/// |------|--------------|---------|---------|
/// | 7467 | THOUGHTGATE_OUTBOUND_PORT | 7467 | Client requests → upstream (main proxy) |
/// | 7468 | (reserved) | 7468 | Future inbound (dummy socket, not wired) |
/// | 7469 | THOUGHTGATE_ADMIN_PORT | 7469 | Health checks, metrics, admin API |
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - configuration)
/// - Implements: REQ-CORE-005/§5.1 (Network Configuration)
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Config {
    /// Bind address (default: 0.0.0.0)
    #[arg(short, long, default_value = "0.0.0.0")]
    bind: String,

    /// Optional upstream URL for reverse proxy mode (e.g., "http://backend:8080")
    /// When set, all requests are forwarded to this upstream instead of using the request's target
    #[arg(long, env = "UPSTREAM_URL")]
    upstream_url: Option<String>,

    /// Path to YAML configuration file for governance.
    /// Enables the 4-gate governance model (visibility, rules, Cedar policy, approval).
    /// If not specified, searches: THOUGHTGATE_CONFIG env, /etc/thoughtgate/config.yaml, ./config.yaml
    #[arg(long, env = "THOUGHTGATE_CONFIG")]
    config: Option<PathBuf>,
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

    // Record startup deadline for timeout enforcement
    // Implements: REQ-CORE-005/F-001 (startup timeout)
    let startup_deadline = std::time::Instant::now() + lifecycle.config().startup_timeout;

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
    }

    // Phase 3: Create unified shutdown token
    // Implements: REQ-CORE-005/F-004 (Unified Shutdown)
    let shutdown = CancellationToken::new();

    // Phase 4: Start admin server on dedicated port
    // Implements: REQ-CORE-005/F-002, F-003, REQ-CORE-005/§5.1
    let admin_port_val = admin_port();
    let admin_shutdown = shutdown.clone();
    let admin_lifecycle = lifecycle.clone();
    tokio::spawn(async move {
        let admin_server = AdminServer::with_config(
            admin_lifecycle,
            thoughtgate::admin::AdminServerConfig {
                port: admin_port_val,
                bind_addr: "0.0.0.0".to_string(),
            },
        );
        if let Err(e) = admin_server.run(admin_shutdown).await {
            error!(error = %e, "Admin server error");
        }
    });
    info!(
        admin_port = admin_port_val,
        "Admin server started (/health, /ready, /metrics)"
    );

    // Reserve inbound port (7468) - dummy socket, not wired to anything
    // This reserves the port for future callback/webhook functionality.
    // IMPORTANT: This listener must be kept alive (not dropped) to hold the port.
    // The #[allow(unused)] silences the warning while keeping the socket open.
    let inbound_port_val = inbound_port();
    #[allow(unused)]
    let inbound_listener = TcpListener::bind(format!("0.0.0.0:{}", inbound_port_val)).await?;
    info!(
        inbound_port = inbound_port_val,
        "Inbound port reserved (not wired)"
    );

    // Phase 5: Bind main listener (outbound port)
    let outbound_port_val = outbound_port();
    let addr = format!("{}:{}", cli_config.bind, outbound_port_val);
    let listener = TcpListener::bind(&addr).await?;

    info!(
        bind = %cli_config.bind,
        outbound_port = outbound_port_val,
        inbound_port = inbound_port_val,
        admin_port = admin_port_val,
        drain_timeout_secs = lifecycle.config().drain_timeout.as_secs(),
        addr = %addr,
        tcp_nodelay = proxy_config.tcp_nodelay,
        tcp_keepalive_secs = proxy_config.tcp_keepalive_secs,
        max_concurrent_streams = proxy_config.max_concurrent_streams,
        socket_buffer_size = proxy_config.socket_buffer_size,
        "ThoughtGate Proxy starting (Envoy-style 3-port model)"
    );

    // Phase 6: Load YAML config and create governance components
    // Implements: REQ-GOV-002 (Governance Pipeline)
    let yaml_config: Option<config::Config> = if let Some(ref path) = cli_config.config {
        let path_display = path.display().to_string();
        info!(path = %path_display, "Loading configuration file");
        let (config, result) = load_and_validate(path, Version::V0_2)?;
        for warning in &result.warnings {
            warn!(warning = %warning, "Configuration warning");
        }
        Some(config)
    } else {
        // Try to find config in default locations
        match find_config_file(None) {
            Ok(found_path) => {
                let path_display = found_path.display().to_string();
                info!(path = %path_display, "Found configuration file");
                let (config, result) = load_and_validate(&found_path, Version::V0_2)?;
                for warning in &result.warnings {
                    warn!(warning = %warning, "Configuration warning");
                }
                Some(config)
            }
            Err(_) => {
                debug!("No configuration file found, using passthrough mode");
                None
            }
        }
    };

    // Create MCP handler with governance if config exists
    let mcp_handler: Option<Arc<McpHandler>> = if let Some(ref config) = yaml_config {
        // Create upstream client for MCP handler
        let upstream_config = UpstreamConfig::from_env().map_err(|e| {
            format!("MCP governance requires THOUGHTGATE_UPSTREAM environment variable: {e}")
        })?;
        let upstream = Arc::new(UpstreamClient::new(upstream_config)?);

        // Create governance components (TaskHandler, CedarEngine, ApprovalEngine)
        // IMPORTANT: The TaskHandler contains the shared TaskStore that ApprovalEngine uses
        let (task_handler, cedar_engine, approval_engine) =
            create_governance_components(upstream.clone(), Some(config), shutdown.clone()).await?;

        // Create MCP handler with full governance
        // Use the same TaskStore that ApprovalEngine uses for task coordination
        let handler = McpHandler::with_governance(
            upstream,
            cedar_engine,
            task_handler.shared_store(), // Share TaskStore with ApprovalEngine
            McpHandlerConfig::from_env(),
            Some(Arc::new(config.clone())),
            approval_engine,
        );

        info!(
            requires_approval = config.requires_approval_engine(),
            rules_count = config.governance.rules.len(),
            "MCP governance enabled (4-gate model)"
        );

        Some(Arc::new(handler))
    } else {
        info!("MCP governance disabled (no config file)");
        None
    };

    // Phase 7: Create proxy service
    let mut proxy_service =
        ProxyService::new_with_config(cli_config.upstream_url.clone(), proxy_config.clone())?;

    // Wire MCP handler if governance is enabled
    if let Some(handler) = mcp_handler {
        proxy_service = proxy_service.with_mcp_handler(handler);
    }

    let proxy_service = Arc::new(proxy_service);
    let service_stack = ServiceBuilder::new()
        .layer(logging_layer())
        .service(proxy_service.as_ref().clone());

    // Setup signal handlers with unified shutdown token
    // Implements: REQ-CORE-005/F-004 (Signal Handling)
    setup_signal_handlers(shutdown.clone(), lifecycle.clone());

    let config_clone = proxy_config.clone();

    // Semaphore for concurrency limiting (REQ-CORE-001 Section 3.2)
    let semaphore = Arc::new(Semaphore::new(proxy_config.max_concurrent_streams));

    // Phase 8: Upstream connectivity check and health checker
    // Implements: REQ-CORE-005/F-001.4, F-008
    let health_check_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Failed to create health check client: {}", e))?;

    if let Some(ref upstream_url) = cli_config.upstream_url {
        // Check startup upstream connectivity if required
        // Implements: REQ-CORE-005/F-001.4
        if lifecycle.config().require_upstream_at_startup {
            info!(upstream = %upstream_url, "Verifying upstream connectivity at startup");
            match health_check_client.head(upstream_url).send().await {
                Ok(resp) if resp.status().is_success() || resp.status().is_redirection() => {
                    lifecycle.update_upstream_health(true, None);
                    info!(
                        upstream = %upstream_url,
                        status = %resp.status(),
                        "Upstream connectivity verified"
                    );
                }
                Ok(resp) => {
                    return Err(format!(
                        "Upstream {} returned non-success status {} at startup",
                        upstream_url,
                        resp.status()
                    )
                    .into());
                }
                Err(e) => {
                    return Err(format!(
                        "Cannot connect to upstream {} at startup: {}",
                        upstream_url, e
                    )
                    .into());
                }
            }
        } else {
            // Mark upstream as healthy initially (will be updated by health checker)
            lifecycle.update_upstream_health(true, None);
        }

        // Spawn background upstream health checker
        // Implements: REQ-CORE-005/F-008
        lifecycle.spawn_upstream_health_checker(upstream_url.clone(), health_check_client);
        info!(
            upstream = %upstream_url,
            interval_secs = lifecycle.config().upstream_health_interval.as_secs(),
            "Background upstream health checker started"
        );
    } else {
        // No upstream configured - mark as healthy (forward proxy mode)
        lifecycle.update_upstream_health(true, None);
    }

    // Check startup timeout before marking ready
    // Implements: REQ-CORE-005/F-001 (startup timeout enforcement)
    if std::time::Instant::now() > startup_deadline {
        return Err(format!(
            "Startup timeout exceeded ({}s)",
            lifecycle.config().startup_timeout.as_secs()
        )
        .into());
    }

    // Mark as ready
    // Implements: REQ-CORE-005/F-001
    lifecycle.mark_config_loaded(); // Configuration loaded and validated
    lifecycle.mark_approval_store_initialized(); // Approval store ready
    lifecycle.mark_ready();

    // Per-request timeout for proxy connections
    // Prevents indefinitely hanging connections from leaking resources
    let request_timeout = Duration::from_secs(
        std::env::var("THOUGHTGATE_REQUEST_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300),
    );
    info!(
        timeout_secs = request_timeout.as_secs(),
        "Per-request timeout configured"
    );

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
                        let conn_shutdown = shutdown.clone();

                        tokio::spawn(async move {
                            match tokio::time::timeout(
                                request_timeout,
                                handle_connection(
                                    stream,
                                    peer_addr,
                                    service_stack,
                                    conn_shutdown,
                                ),
                            )
                            .await
                            {
                                Ok(Ok(())) => {}
                                Ok(Err(e)) => {
                                    error!(error = %e, "Connection handling error");
                                }
                                Err(_elapsed) => {
                                    warn!(
                                        peer = %peer_addr,
                                        timeout_secs = request_timeout.as_secs(),
                                        "Connection timed out, dropping"
                                    );
                                }
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

            _ = shutdown.cancelled() => {
                info!("Shutdown signal received, stopping new connections");
                break;
            }
        }
    }

    // Graceful shutdown sequence
    // Implements: REQ-CORE-005/F-004, F-005
    // Note: admin_shutdown is the same token as shutdown, already cancelled by signal handler

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
/// Uses a unified CancellationToken for all shutdown coordination.
///
/// - SIGINT (Ctrl+C): Begin graceful shutdown
/// - SIGTERM: Begin graceful shutdown
/// - SIGQUIT: Immediate shutdown (no drain)
fn setup_signal_handlers(shutdown: CancellationToken, lifecycle: Arc<LifecycleManager>) {
    // SIGINT handler
    let shutdown_sigint = shutdown.clone();
    let lifecycle_sigint = lifecycle.clone();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                info!("Received SIGINT (Ctrl+C), initiating graceful shutdown");
                lifecycle_sigint.begin_shutdown();
                shutdown_sigint.cancel();
            }
            Err(e) => {
                error!(error = %e, "Failed to listen for SIGINT");
            }
        }
    });

    // SIGTERM handler (Unix only)
    #[cfg(unix)]
    {
        let shutdown_sigterm = shutdown.clone();
        let lifecycle_sigterm = lifecycle.clone();
        tokio::spawn(async move {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut sigterm) => {
                    sigterm.recv().await;
                    info!("Received SIGTERM, initiating graceful shutdown");
                    lifecycle_sigterm.begin_shutdown();
                    shutdown_sigterm.cancel();
                }
                Err(e) => {
                    error!(error = %e, "Failed to listen for SIGTERM");
                }
            }
        });
    }

    // SIGQUIT handler (Unix only) - Immediate shutdown without draining
    // Implements: REQ-CORE-005/§5.2, EC-OPS-013
    #[cfg(unix)]
    {
        let lifecycle_sigquit = lifecycle;
        tokio::spawn(async move {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::quit()) {
                Ok(mut sigquit) => {
                    sigquit.recv().await;
                    warn!(
                        active_requests = lifecycle_sigquit.active_request_count(),
                        "Received SIGQUIT, immediate shutdown (no drain)"
                    );
                    lifecycle_sigquit.mark_stopped();
                    // Intentional process::exit: SIGQUIT demands immediate termination
                    // without drain. This cannot return through main() because the
                    // signal handler runs in a spawned task.
                    std::process::exit(1);
                }
                Err(e) => {
                    error!(error = %e, "Failed to listen for SIGQUIT");
                }
            }
        });
    }

    // Prevent unused variable warning on non-Unix
    #[cfg(not(unix))]
    let _ = (shutdown, lifecycle);
}

/// Handle a single connection with HTTP protocol.
///
/// # Traceability
/// - Implements: REQ-CORE-001 F-001 (TCP_NODELAY enforcement)
/// - Implements: REQ-CORE-002 (Conditional Termination - CONNECT rejection)
async fn handle_connection<S, B>(
    mut stream: TcpStream,
    _peer_addr: SocketAddr,
    service: S,
    shutdown: CancellationToken,
) -> Result<(), ProxyError>
where
    S: tower::Service<Request<Incoming>, Response = Response<B>, Error = ProxyError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: http_body::Body<Data = bytes::Bytes> + Send + Sync + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
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
                    // Convert body error to boxed error for hyper compatibility.
                    Ok(response.map(|body| {
                        body.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })
                            .boxed()
                    }))
                }
                Err(e) => {
                    error!(error = %e, "Service error");
                    // Use to_response() to map error to proper HTTP status code
                    // Full<Bytes> has Infallible error - convert using absurd pattern
                    Ok(e.to_response()
                        .map(|body| body.map_err(|e| match e {}).boxed()))
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
        _ = shutdown.cancelled() => {
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
