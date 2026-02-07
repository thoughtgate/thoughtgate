//! Graceful shutdown sequence for server child processes.
//!
//! Implements: REQ-CORE-008/F-018, F-021

use std::sync::Arc;

use thoughtgate_core::telemetry::ThoughtGateMetrics;

use crate::error::StdioError;

/// Execute the graceful shutdown sequence for a server child process.
///
/// 1. Close stdin (already dropped by aborting agent→server task)
/// 2. Wait stdin_close_grace for server to exit
/// 3. Send SIGTERM to process group (Unix)
/// 4. Wait sigterm_grace
/// 5. Send SIGKILL
/// 6. Collect exit code via wait() (F-021: zombie prevention)
///
/// Implements: REQ-CORE-008/F-018, F-021
pub(super) async fn shutdown_server(
    server_id: &str,
    child: &mut tokio::process::Child,
    metrics: &Option<Arc<ThoughtGateMetrics>>,
    shutdown_req: &crate::shim::lifecycle::ShutdownRequest,
) -> Result<i32, StdioError> {
    tracing::info!(
        server_id,
        state = "shutting_down",
        "initiating graceful shutdown"
    );

    // Step 2: Wait for server to exit after stdin is closed.
    match tokio::time::timeout(shutdown_req.stdin_close_grace, child.wait()).await {
        Ok(Ok(status)) => {
            let code = status.code().unwrap_or(-1);
            tracing::info!(
                server_id,
                code,
                state = "stopped",
                "server exited after stdin close"
            );
            if let Some(m) = metrics {
                m.decrement_stdio_active_servers();
                m.set_stdio_server_state(server_id, "exited");
            }
            return Ok(code);
        }
        Ok(Err(e)) => {
            tracing::error!(server_id, error = %e, "wait failed after stdin close");
        }
        Err(_) => {
            tracing::info!(server_id, "server did not exit within stdin_close_grace");
        }
    }

    // Step 3: Send SIGTERM to process group (Unix only).
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;
        if let Some(pid) = child.id() {
            tracing::info!(server_id, pid, "sending SIGTERM to process group");
            if let Err(e) = killpg(Pid::from_raw(pid as i32), Signal::SIGTERM) {
                tracing::warn!(server_id, pid, error = ?e, "killpg SIGTERM failed");
            }
        }
    }

    // Step 4: Wait for server to exit after SIGTERM.
    match tokio::time::timeout(shutdown_req.sigterm_grace, child.wait()).await {
        Ok(Ok(status)) => {
            let code = status.code().unwrap_or(-1);
            tracing::info!(
                server_id,
                code,
                state = "stopped",
                "server exited after SIGTERM"
            );
            if let Some(m) = metrics {
                m.decrement_stdio_active_servers();
                m.set_stdio_server_state(server_id, "signalled");
            }
            return Ok(code);
        }
        Ok(Err(e)) => {
            tracing::error!(server_id, error = %e, "wait failed after SIGTERM");
        }
        Err(_) => {
            tracing::warn!(server_id, "server did not exit within sigterm_grace");
        }
    }

    // Step 5: SIGKILL as last resort.
    tracing::warn!(server_id, "sending SIGKILL");
    if let Err(e) = child.kill().await {
        tracing::error!(server_id, error = %e, "SIGKILL failed");
    }

    // Step 6: Collect exit code (F-021: zombie prevention).
    let status = child.wait().await.map_err(StdioError::StdioIo)?;
    let code = status.code().unwrap_or(-1);
    tracing::info!(
        server_id,
        code,
        state = "stopped",
        "server exited after SIGKILL"
    );

    if let Some(m) = metrics {
        m.decrement_stdio_active_servers();
        m.set_stdio_server_state(server_id, "signalled");
    }
    Ok(code)
}
