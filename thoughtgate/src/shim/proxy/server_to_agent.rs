//! Server → Agent task: read from server stdout, forward to agent stdout.
//!
//! Implements: REQ-CORE-008/F-012 (server→agent), F-015

use std::sync::Arc;
use std::time::Instant;

use tokio::io::{BufReader, Stdout};
use tokio::sync::{Mutex, watch};

use thoughtgate_core::StreamDirection;
use thoughtgate_core::config::Config;
use thoughtgate_core::jsonrpc::JsonRpcMessageKind;
use thoughtgate_core::telemetry::ThoughtGateMetrics;

use crate::error::{FramingError, StdioError};
use crate::shim::ndjson::{detect_smuggling, parse_stdio_message};

use super::helpers::{
    PendingRequest, PendingRequests, bounded_read_line, filter_list_response, framing_error_type,
    is_list_method, method_from_kind, write_stdout,
};

/// Read from server stdout, forward to agent stdout.
///
/// In v0.3, server→agent messages are forwarded unconditionally after local
/// audit logging. No governance HTTP call is made for inbound messages.
///
/// Implements: REQ-CORE-008/F-012 (server→agent), F-015
pub(super) async fn server_to_agent(
    server_id: String,
    metrics: Option<Arc<ThoughtGateMetrics>>,
    child_stdout: tokio::process::ChildStdout,
    agent_stdout: Arc<Mutex<Stdout>>,
    shutdown_rx: &mut watch::Receiver<bool>,
    pending: Arc<tokio::sync::Mutex<PendingRequests>>,
    config: Option<Arc<Config>>,
) -> Result<(), StdioError> {
    let mut reader = BufReader::new(child_stdout);
    let mut raw_buf = Vec::new();

    loop {
        raw_buf.clear();

        let bytes_read = tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                tracing::debug!(server_id, "server→agent: shutdown signal received");
                break;
            }
            result = bounded_read_line(
                &mut reader,
                &mut raw_buf,
                crate::shim::ndjson::MAX_MESSAGE_BYTES,
            ) => {
                match result {
                    Ok(n) => n,
                    Err(FramingError::MessageTooLarge { .. }) => {
                        if let Some(ref m) = metrics {
                            m.record_stdio_framing_error(&server_id, "message_too_large");
                        }
                        tracing::warn!(
                            server_id,
                            "server→agent: message exceeded size limit, skipping"
                        );
                        continue;
                    }
                    Err(FramingError::Io(e)) => {
                        return Err(StdioError::Framing {
                            server_id: server_id.clone(),
                            direction: StreamDirection::ServerToAgent,
                            source: FramingError::Io(e),
                        });
                    }
                    Err(e) => {
                        return Err(StdioError::Framing {
                            server_id: server_id.clone(),
                            direction: StreamDirection::ServerToAgent,
                            source: e,
                        });
                    }
                }
            }
        };

        // EOF — server closed stdout (server exited).
        if bytes_read == 0 {
            tracing::debug!(server_id, "server stdout EOF");
            break;
        }

        // Validate UTF-8: invalid bytes would corrupt audit logs and metrics.
        // The raw bytes are always forwarded regardless of validation.
        let buf = match String::from_utf8(raw_buf.clone()) {
            Ok(s) => s,
            Err(_) => {
                if let Some(ref m) = metrics {
                    m.record_stdio_framing_error(&server_id, "invalid_utf8");
                }
                tracing::warn!(
                    server_id,
                    len = raw_buf.len(),
                    "server→agent: invalid UTF-8, forwarding raw"
                );
                write_stdout(&agent_stdout, &raw_buf)
                    .await
                    .map_err(StdioError::StdioIo)?;
                continue;
            }
        };

        // Skip empty lines.
        if buf.trim().is_empty() {
            continue;
        }

        // F-014: Smuggling detection on raw bytes (log only for server→agent in v0.3).
        if detect_smuggling(&raw_buf) {
            if let Some(ref m) = metrics {
                m.record_stdio_framing_error(&server_id, "smuggling");
            }
            tracing::warn!(
                server_id,
                "smuggling detected in server→agent message (logged only)"
            );
        }

        // Parse for metrics/audit — but always forward the raw line.
        match parse_stdio_message(&buf) {
            Ok(msg) => {
                let method = method_from_kind(&msg.kind);
                // NFR-002: Record every parsed message.
                if let Some(ref m) = metrics {
                    m.record_stdio_message(&server_id, "server_to_agent", method);
                }

                // NFR-002: Structured audit log for server→agent.
                tracing::info!(
                    component = "shim",
                    server_id = %server_id,
                    direction = "server_to_agent",
                    method = %method,
                    governance_decision = "passthrough",
                    "audit: server response forwarded"
                );

                // F-015: Correlate responses to outbound requests; track inbound requests.
                match &msg.kind {
                    JsonRpcMessageKind::Response { id } => {
                        let key = serde_json::to_string(id).unwrap_or_default();
                        let matched = pending.lock().await.outbound.remove(&key);
                        if let Some(ref req) = matched {
                            tracing::debug!(
                                server_id,
                                method = %req.method,
                                latency_us = req.sent_at.elapsed().as_micros() as u64,
                                "response matched outbound request"
                            );

                            // Gate 1: Filter list responses by visibility.
                            // Apply the same filtering the HTTP proxy uses so
                            // agents only see tools/resources/prompts they are
                            // allowed to access.
                            if is_list_method(&req.method) {
                                if let Some(ref cfg) = config {
                                    if let Some(filtered_line) =
                                        filter_list_response(&buf, &req.method, cfg, &server_id)
                                    {
                                        tracing::debug!(
                                            server_id,
                                            method = %req.method,
                                            "filtered list response by visibility"
                                        );
                                        write_stdout(&agent_stdout, filtered_line.as_bytes())
                                            .await
                                            .map_err(StdioError::StdioIo)?;
                                        continue;
                                    }
                                    // Parsing failed — fall through to forward raw.
                                }
                            }
                        }
                    }
                    JsonRpcMessageKind::Request { id, method: m } => {
                        let key = serde_json::to_string(id).unwrap_or_default();
                        pending.lock().await.inbound.insert(
                            key,
                            PendingRequest {
                                method: m.clone(),
                                sent_at: Instant::now(),
                            },
                        );
                    }
                    _ => {}
                }
            }
            Err(e) => {
                // NFR-002: Record framing error.
                if let Some(ref m) = metrics {
                    m.record_stdio_framing_error(&server_id, framing_error_type(&e));
                }
                tracing::warn!(
                    server_id,
                    error = %e,
                    "server→agent framing error (forwarding raw line)"
                );
            }
        }

        // Always forward raw bytes to agent stdout.
        write_stdout(&agent_stdout, &raw_buf)
            .await
            .map_err(StdioError::StdioIo)?;
    }

    Ok(())
}
