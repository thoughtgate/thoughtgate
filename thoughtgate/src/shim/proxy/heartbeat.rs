//! Heartbeat and healthz polling for governance service connectivity.
//!
//! Implements: REQ-CORE-008/F-016, F-016a

use std::time::Duration;

use tokio::sync::watch;

use thoughtgate_core::governance::service::HeartbeatRequest;

use crate::error::StdioError;

use super::helpers::{
    HEALTHZ_INITIAL_BACKOFF_MS, HEALTHZ_MAX_ATTEMPTS, HEALTHZ_MAX_BACKOFF_MS,
    HEARTBEAT_INTERVAL_SECS, HEARTBEAT_TIMEOUT_MS,
};

// ─────────────────────────────────────────────────────────────────────────────
// Governance Healthz Polling (F-016a)
// ─────────────────────────────────────────────────────────────────────────────

/// Poll the governance service `/healthz` endpoint with exponential backoff.
///
/// The shim MUST wait for the governance service to become reachable before
/// entering the proxy loop. This prevents fail-closed denials during normal
/// startup races between shim instances and the governance service.
///
/// # Errors
///
/// Returns [`StdioError::GovernanceUnavailable`] if the governance service is
/// not reachable within the backoff window.
///
/// Implements: REQ-CORE-008/F-016a
pub(super) async fn poll_governance_healthz(
    client: &reqwest::Client,
    endpoint: &str,
) -> Result<(), StdioError> {
    let url = format!("{endpoint}/healthz");
    let mut delay_ms = HEALTHZ_INITIAL_BACKOFF_MS;

    for attempt in 1..=HEALTHZ_MAX_ATTEMPTS {
        match client
            .get(&url)
            .timeout(Duration::from_millis(HEALTHZ_MAX_BACKOFF_MS))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(attempt, "governance service is healthy");
                return Ok(());
            }
            Ok(resp) => {
                tracing::debug!(attempt, status = %resp.status(), "governance healthz non-200");
            }
            Err(e) => {
                tracing::debug!(attempt, error = %e, "governance healthz unreachable");
            }
        }

        if attempt < HEALTHZ_MAX_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            delay_ms = (delay_ms * 2).min(HEALTHZ_MAX_BACKOFF_MS);
        }
    }

    tracing::error!(
        attempts = HEALTHZ_MAX_ATTEMPTS,
        "governance service unavailable after readiness polling"
    );
    Err(StdioError::GovernanceUnavailable)
}

// ─────────────────────────────────────────────────────────────────────────────
// Heartbeat Task (F-016)
// ─────────────────────────────────────────────────────────────────────────────

/// Send periodic heartbeats to the governance service.
///
/// If the governance service responds with `shutdown: true`, or if any
/// connection error occurs, the shutdown signal is sent immediately.
///
/// **EC-STDIO-042:** Connection errors are treated as fatal — no retry,
/// no backoff. The governance service is gone; initiate shutdown.
///
/// Implements: REQ-CORE-008/F-016
pub(super) async fn heartbeat_loop(
    server_id: String,
    governance_endpoint: String,
    client: reqwest::Client,
    shutdown_tx: watch::Sender<bool>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let url = format!("{governance_endpoint}/governance/heartbeat");
    let mut interval = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));

    // Skip the first immediate tick (the proxy just started).
    interval.tick().await;

    loop {
        // Wait for next interval OR shutdown signal (whichever comes first).
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                tracing::debug!(server_id, "heartbeat: shutdown signal, stopping");
                break;
            }
            _ = interval.tick() => {}
        }

        let req = HeartbeatRequest {
            server_id: server_id.clone(),
        };

        match client
            .post(&url)
            .timeout(Duration::from_millis(HEARTBEAT_TIMEOUT_MS))
            .json(&req)
            .send()
            .await
        {
            Ok(resp) => {
                match resp
                    .json::<thoughtgate_core::governance::service::HeartbeatResponse>()
                    .await
                {
                    Ok(hb) => {
                        if hb.shutdown {
                            tracing::info!(server_id, "heartbeat: shutdown signal received");
                            let _ = shutdown_tx.send(true);
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(server_id, error = %e, "heartbeat: failed to parse response");
                    }
                }
            }
            Err(e) => {
                // EC-STDIO-042: Any connection error is fatal. No retry.
                tracing::error!(
                    server_id,
                    error = %e,
                    "heartbeat: connection error — governance service gone, initiating shutdown"
                );
                let _ = shutdown_tx.send(true);
                break;
            }
        }
    }
}
