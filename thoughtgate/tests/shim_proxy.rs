//! Integration tests for the shim proxy.
//!
//! These tests exercise the full shim proxy pipeline: governance service ↔
//! shim proxy ↔ child MCP server process.
//!
//! Uses `start_governance_service(0, ...)` from `thoughtgate-core` to spin up
//! a real governance HTTP service, and `cat` as a simple echo server.
//!
//! These tests spawn Unix commands (`cat`, `sleep`, `false`) and are gated on
//! `cfg(unix)` — the CLI wrapper depends on Unix signals and process semantics.
#![cfg(unix)]

use std::sync::Arc;

use thoughtgate::shim::proxy::run_shim;
use thoughtgate::wrap::config_adapter::ShimOptions;
use thoughtgate_core::governance::service::{GovernanceServiceState, start_governance_service};
use thoughtgate_core::profile::Profile;

/// Start a test governance service on an ephemeral port.
async fn start_test_governance() -> (u16, Arc<GovernanceServiceState>) {
    let state = Arc::new(GovernanceServiceState::new());
    let (port, _handle) = start_governance_service(0, state.clone())
        .await
        .expect("failed to start governance service");
    (port, state)
}

/// Build ShimOptions for a test run.
fn test_opts(server_id: &str, port: u16) -> ShimOptions {
    ShimOptions {
        server_id: server_id.to_string(),
        governance_endpoint: format!("http://127.0.0.1:{port}"),
        profile: Profile::Production,
    }
}

/// EC-STDIO-010: Server process crashes mid-session.
#[tokio::test(flavor = "multi_thread")]
async fn test_server_crash_handled_gracefully() {
    // `false` exits immediately with code 1. The shim should detect this
    // and return without hanging.
    let (port, _state) = start_test_governance().await;
    let opts = test_opts("crash-server", port);
    let metrics = None;

    let result = run_shim(opts, metrics, "false".to_string(), vec![]).await;

    // The shim should return an exit code (1 from `false`).
    match result {
        Ok(code) => {
            assert_ne!(code, 0, "false should exit with non-zero code");
        }
        Err(e) => {
            // Some errors are acceptable for a crashing server.
            panic!("unexpected error: {e}");
        }
    }
}

/// EC-STDIO-036: Governance service not ready when shim starts.
#[tokio::test(flavor = "multi_thread")]
async fn test_governance_unavailable_returns_error() {
    // Point at a port where nothing is listening.
    let opts = ShimOptions {
        server_id: "test-server".to_string(),
        governance_endpoint: "http://127.0.0.1:1".to_string(), // Port 1 — nothing listening
        profile: Profile::Production,
    };
    let metrics = None;

    let result = run_shim(opts, metrics, "cat".to_string(), vec![]).await;

    match result {
        Err(thoughtgate::error::StdioError::GovernanceUnavailable) => {
            // Expected — governance healthz polling failed.
        }
        other => {
            panic!("expected GovernanceUnavailable, got: {other:?}");
        }
    }
}

/// EC-STDIO-009: Server process fails to start (command not found).
#[tokio::test(flavor = "multi_thread")]
async fn test_server_spawn_failure() {
    let (port, _state) = start_test_governance().await;
    let opts = test_opts("bad-server", port);
    let metrics = None;

    // Try to spawn a command that doesn't exist.
    let result = run_shim(
        opts,
        metrics,
        "nonexistent-command-that-does-not-exist-12345".to_string(),
        vec![],
    )
    .await;

    match result {
        Err(thoughtgate::error::StdioError::ServerSpawnError { server_id, .. }) => {
            assert_eq!(server_id, "bad-server");
        }
        other => {
            panic!("expected ServerSpawnError, got: {other:?}");
        }
    }
}

/// EC-STDIO-041, EC-STDIO-042: Shutdown via governance heartbeat.
#[tokio::test(flavor = "multi_thread")]
async fn test_shutdown_via_governance() {
    // Start governance, trigger shutdown immediately, then start a shim.
    // The shim should detect shutdown via heartbeat and exit.
    let (port, state) = start_test_governance().await;
    state.trigger_shutdown();

    let opts = test_opts("shutdown-server", port);
    let metrics = None;

    // `sleep 60` would normally run for 60 seconds, but the shim should
    // detect the shutdown flag from the governance service and terminate it.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        run_shim(opts, metrics, "sleep".to_string(), vec!["60".to_string()]),
    )
    .await;

    match result {
        Ok(Ok(_code)) => {
            // Shim exited — shutdown was detected.
        }
        Ok(Err(e)) => {
            // Some shutdown-related errors are acceptable.
            tracing::info!("shim exited with error during shutdown test: {e}");
        }
        Err(_) => {
            panic!("shim did not exit within 15 seconds after governance shutdown");
        }
    }
}
