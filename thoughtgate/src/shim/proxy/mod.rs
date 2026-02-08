//! Bidirectional stdio proxy with governance evaluation and trace context propagation.
//!
//! Implements: REQ-CORE-008 §7.4 (F-011 through F-017), §7.5 (F-018)
//! Implements: REQ-OBS-002 §7.3 (Stdio Transport Trace Context Propagation)
//!
//! This module contains the main shim proxy function [`run_shim`] which spawns
//! an MCP server child process, proxies stdin/stdout bidirectionally with
//! governance evaluation on the agent→server path, sends periodic heartbeats,
//! and handles graceful shutdown.
//!
//! # Trace Context Propagation
//!
//! The shim extracts W3C trace context from `params._meta.traceparent` and
//! `params._meta.tracestate` fields in incoming JSON-RPC messages. This enables
//! distributed tracing through stdio-based MCP clients (Claude Desktop, Cursor,
//! VS Code, Windsurf). The trace context is:
//!
//! 1. Extracted from the incoming message's `_meta` field
//! 2. Forwarded to the governance service via HTTP headers
//! 3. Stripped from the message before forwarding to the MCP server
//!
//! This design ensures upstream MCP servers with strict schema validation don't
//! reject requests containing unknown fields.

mod agent_to_server;
mod heartbeat;
mod helpers;
mod server_to_agent;
mod shutdown;

use std::sync::Arc;
use std::time::Duration;

use tokio::io::Stdout;
use tokio::process::Command;
use tokio::sync::{Mutex, watch};

use thoughtgate_core::config::Config;
use thoughtgate_core::telemetry::ThoughtGateMetrics;

use crate::error::StdioError;
use crate::wrap::config_adapter::ShimOptions;

use agent_to_server::agent_to_server;
use heartbeat::{heartbeat_loop, poll_governance_healthz};
use helpers::{CONNECT_TIMEOUT_MS, PendingRequests};
use server_to_agent::server_to_agent;
use shutdown::shutdown_server;

// ─────────────────────────────────────────────────────────────────────────────
// run_shim — Main Shim Orchestrator
// ─────────────────────────────────────────────────────────────────────────────

/// Run the bidirectional stdio proxy for a single MCP server child process.
///
/// This function:
/// 1. Spawns the server child process (F-011)
/// 2. Polls governance `/healthz` to ensure readiness (F-016a)
/// 3. Spawns three concurrent tasks: agent→server, server→agent, heartbeat
/// 4. Waits for any task to complete or a shutdown signal
/// 5. Runs the graceful shutdown sequence (F-018)
///
/// # Errors
///
/// Returns [`StdioError`] on spawn failure, governance unavailability, or
/// unrecoverable IO errors.
///
/// Implements: REQ-CORE-008/F-011, F-012, F-016, F-018
pub async fn run_shim(
    opts: ShimOptions,
    metrics: Option<Arc<ThoughtGateMetrics>>,
    server_command: String,
    server_args: Vec<String>,
) -> Result<i32, StdioError> {
    let server_id = opts.server_id.clone();
    let governance_endpoint = opts.governance_endpoint.clone();
    let profile = opts.profile;

    // ── F-011: Spawn server child process ────────────────────────────────
    let mut cmd = Command::new(&server_command);
    cmd.args(&server_args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        // Scrub ThoughtGate internals so the MCP server cannot detect
        // governance or spoof its identity via inherited env vars.
        .env_remove("THOUGHTGATE_ACTIVE")
        .env_remove("THOUGHTGATE_SERVER_ID")
        .env_remove("THOUGHTGATE_GOVERNANCE_ENDPOINT");

    #[cfg(unix)]
    cmd.process_group(0);

    if let Some(ref m) = metrics {
        m.set_stdio_server_state(&server_id, "starting");
    }

    let mut child = cmd.spawn().map_err(|e| {
        if let Some(ref m) = metrics {
            m.set_stdio_server_state(&server_id, "failed_to_start");
        }
        StdioError::ServerSpawnError {
            server_id: server_id.clone(),
            reason: e.to_string(),
        }
    })?;

    if let Some(ref m) = metrics {
        m.increment_stdio_active_servers();
        m.set_stdio_server_state(&server_id, "running");
    }
    tracing::info!(server_id, server_command, "server process spawned");

    // ── Build reqwest client ─────────────────────────────────────────────
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(CONNECT_TIMEOUT_MS))
        .build()
        .map_err(|e| StdioError::ServerSpawnError {
            server_id: server_id.clone(),
            reason: format!("failed to build HTTP client: {e}"),
        })?;

    // ── Load ThoughtGate config for list response filtering ─────────────
    let tg_config: Option<Arc<Config>> =
        std::env::var("THOUGHTGATE_CONFIG")
            .ok()
            .and_then(|path_str| {
                let path = std::path::Path::new(&path_str);
                if !path.exists() {
                    tracing::debug!(path = %path_str, "THOUGHTGATE_CONFIG path not found");
                    return None;
                }
                match thoughtgate_core::config::load_and_validate(
                    path,
                    thoughtgate_core::config::Version::V0_2,
                ) {
                    Ok((config, _)) => {
                        tracing::debug!(path = %path_str, "loaded config for list filtering");
                        Some(Arc::new(config))
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "failed to load config for list filtering");
                        None
                    }
                }
            });

    // ── F-016a: Poll governance /healthz ─────────────────────────────────
    poll_governance_healthz(&client, &governance_endpoint).await?;

    // ── F-015: Bidirectional request ID tracking ─────────────────────────
    let pending = Arc::new(tokio::sync::Mutex::new(PendingRequests::default()));

    // ── Set up channels and IO handles ───────────────────────────────────
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| StdioError::ServerSpawnError {
            server_id: server_id.clone(),
            reason: "failed to capture server stdin".to_string(),
        })?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| StdioError::ServerSpawnError {
            server_id: server_id.clone(),
            reason: "failed to capture server stdout".to_string(),
        })?;

    // ── Spawn concurrent tasks ───────────────────────────────────────────

    // Shared stdout handle — both tasks write NDJSON to the agent's stdout.
    // Serialized through a Mutex to prevent interleaved lines.
    let agent_stdout: Arc<Mutex<Stdout>> = Arc::new(Mutex::new(tokio::io::stdout()));

    // Task 1: Agent → Server
    let a2s_handle = {
        let server_id = server_id.clone();
        let governance_endpoint = governance_endpoint.clone();
        let client = client.clone();
        let metrics = metrics.clone();
        let mut shutdown_rx = shutdown_rx.clone();
        let shutdown_tx = shutdown_tx.clone();
        let pending = pending.clone();
        let agent_stdout = agent_stdout.clone();

        tokio::spawn(async move {
            agent_to_server(
                server_id,
                governance_endpoint,
                profile,
                client,
                metrics,
                child_stdin,
                agent_stdout,
                &mut shutdown_rx,
                shutdown_tx,
                pending,
            )
            .await
        })
    };

    // Task 2: Server → Agent
    let s2a_handle = {
        let server_id = server_id.clone();
        let metrics = metrics.clone();
        let mut shutdown_rx = shutdown_rx.clone();
        let pending = pending.clone();
        let agent_stdout = agent_stdout.clone();
        let tg_config = tg_config.clone();

        tokio::spawn(async move {
            server_to_agent(
                server_id,
                metrics,
                child_stdout,
                agent_stdout,
                &mut shutdown_rx,
                pending,
                tg_config,
            )
            .await
        })
    };

    // Task 3: Heartbeat
    let hb_handle = {
        let server_id = server_id.clone();
        let governance_endpoint = governance_endpoint.clone();
        let client = client.clone();
        let shutdown_tx = shutdown_tx.clone();
        let shutdown_rx = shutdown_rx.clone();

        tokio::spawn(async move {
            heartbeat_loop(
                server_id,
                governance_endpoint,
                client,
                shutdown_tx,
                shutdown_rx,
            )
            .await;
        })
    };

    // ── Select on task completion ────────────────────────────────────────
    let mut shutdown_rx_main = shutdown_rx.clone();

    tokio::select! {
        result = a2s_handle => {
            match result {
                Ok(Ok(())) => tracing::info!(server_id, "agent→server stream closed (stdin EOF)"),
                Ok(Err(ref e)) => tracing::error!(server_id, error = %e, "agent→server task failed"),
                Err(ref e) => tracing::error!(server_id, error = %e, "agent→server task panicked"),
            }
        }
        result = s2a_handle => {
            match result {
                Ok(Ok(())) => tracing::info!(server_id, "server→agent stream closed (server stdout EOF)"),
                Ok(Err(ref e)) => tracing::error!(server_id, error = %e, "server→agent task failed"),
                Err(ref e) => tracing::error!(server_id, error = %e, "server→agent task panicked"),
            }
        }
        status = child.wait() => {
            match status {
                Ok(ref s) => tracing::info!(server_id, ?s, "server process exited"),
                Err(ref e) => tracing::error!(server_id, error = %e, "failed to wait on server process"),
            }
        }
        _ = shutdown_rx_main.changed() => {
            tracing::info!(server_id, "governance-initiated shutdown");
        }
    }

    // ── F-018: Graceful shutdown sequence ────────────────────────────────
    let shutdown_req = crate::shim::lifecycle::ShutdownRequest::default();
    let exit_code = shutdown_server(&server_id, &mut child, &metrics, &shutdown_req).await;

    // Abort remaining tasks (best-effort).
    // The heartbeat task handle is not in scope from select! so we abort it here.
    hb_handle.abort();

    // F-015: Log orphaned requests at shutdown.
    {
        let map = pending.lock().await;
        if !map.outbound.is_empty() {
            tracing::warn!(
                server_id,
                count = map.outbound.len(),
                "orphaned outbound requests at shutdown"
            );
        }
        if !map.inbound.is_empty() {
            tracing::warn!(
                server_id,
                count = map.inbound.len(),
                "orphaned inbound requests at shutdown"
            );
        }
    }

    exit_code
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use thoughtgate_core::governance::api::MessageType;
    use thoughtgate_core::jsonrpc::{JsonRpcId, JsonRpcMessageKind};

    use crate::error::FramingError;

    use super::helpers::{
        PendingRequest, PendingRequests, format_deny_response, framing_error_type, id_from_kind,
        is_passthrough, message_type_from_kind, method_from_kind, rebuild_message_with_params,
    };

    // ── F-017 Passthrough Tests ──────────────────────────────────────────

    #[test]
    fn test_is_passthrough_initialize() {
        assert!(is_passthrough("initialize"));
        assert!(is_passthrough("initialized"));
    }

    #[test]
    fn test_is_passthrough_tools_call_not_passthrough() {
        assert!(!is_passthrough("tools/call"));
        assert!(!is_passthrough("tools/list"));
        assert!(!is_passthrough("resources/read"));
        assert!(!is_passthrough("prompts/get"));
    }

    /// EC-STDIO-020: Passthrough whitelist includes server→agent notifications.
    #[test]
    fn test_is_passthrough_notifications() {
        assert!(is_passthrough("notifications/cancelled"));
        assert!(is_passthrough("notifications/progress"));
        assert!(is_passthrough("notifications/resources/updated"));
        assert!(is_passthrough("notifications/resources/list_changed"));
        assert!(is_passthrough("notifications/tools/list_changed"));
        assert!(is_passthrough("notifications/prompts/list_changed"));
    }

    #[test]
    fn test_is_passthrough_ping_pong() {
        assert!(is_passthrough("ping"));
        assert!(is_passthrough("pong"));
    }

    #[test]
    fn test_is_passthrough_unknown_method() {
        assert!(!is_passthrough("custom/method"));
        assert!(!is_passthrough(""));
        assert!(!is_passthrough("sampling/createMessage"));
    }

    // ── Deny Response Formatting Tests ───────────────────────────────────

    #[test]
    fn test_format_deny_response_number_id() {
        let line = format_deny_response(&JsonRpcId::Number(42), "filesystem", Some("pol-001"));
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 42);
        assert_eq!(parsed["error"]["code"], -32003);
        // Core error format uses correlation_id (our server_id) and error_type
        assert_eq!(parsed["error"]["data"]["correlation_id"], "filesystem");
        assert_eq!(parsed["error"]["data"]["error_type"], "policy_denied");
    }

    #[test]
    fn test_format_deny_response_string_id() {
        let line = format_deny_response(&JsonRpcId::String("req-abc".to_string()), "github", None);
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["id"], "req-abc");
        assert_eq!(parsed["error"]["data"]["correlation_id"], "github");
    }

    #[test]
    fn test_format_deny_response_null_id() {
        let line = format_deny_response(&JsonRpcId::Null, "sqlite", Some("pol-002"));
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert!(parsed["id"].is_null());
        assert_eq!(parsed["error"]["code"], -32003);
    }

    #[test]
    fn test_format_deny_response_ends_with_newline() {
        let line = format_deny_response(&JsonRpcId::Number(1), "test", None);
        assert!(line.ends_with('\n'));
        // Should be a single line (NDJSON).
        assert_eq!(line.matches('\n').count(), 1);
    }

    // ── framing_error_type Tests ─────────────────────────────────────────

    #[test]
    fn test_framing_error_type_labels() {
        assert_eq!(
            framing_error_type(&FramingError::MessageTooLarge { max_bytes: 100 }),
            "message_too_large"
        );
        assert_eq!(
            framing_error_type(&FramingError::MalformedJson {
                reason: "bad".to_string()
            }),
            "malformed_json"
        );
        assert_eq!(
            framing_error_type(&FramingError::MissingVersion),
            "missing_version"
        );
        assert_eq!(
            framing_error_type(&FramingError::UnsupportedVersion {
                version: "1.0".to_string()
            }),
            "unsupported_version"
        );
        assert_eq!(
            framing_error_type(&FramingError::UnsupportedBatch),
            "unsupported_batch"
        );
    }

    // ── Helper Function Tests ────────────────────────────────────────────

    #[test]
    fn test_method_from_kind() {
        assert_eq!(
            method_from_kind(&JsonRpcMessageKind::Request {
                id: JsonRpcId::Number(1),
                method: "tools/call".to_string(),
            }),
            "tools/call"
        );
        assert_eq!(
            method_from_kind(&JsonRpcMessageKind::Notification {
                method: "initialized".to_string(),
            }),
            "initialized"
        );
        assert_eq!(
            method_from_kind(&JsonRpcMessageKind::Response {
                id: JsonRpcId::Number(1),
            }),
            "response"
        );
    }

    #[test]
    fn test_message_type_from_kind() {
        assert!(matches!(
            message_type_from_kind(&JsonRpcMessageKind::Request {
                id: JsonRpcId::Number(1),
                method: "x".to_string(),
            }),
            MessageType::Request
        ));
        assert!(matches!(
            message_type_from_kind(&JsonRpcMessageKind::Response {
                id: JsonRpcId::Number(1),
            }),
            MessageType::Response
        ));
        assert!(matches!(
            message_type_from_kind(&JsonRpcMessageKind::Notification {
                method: "x".to_string(),
            }),
            MessageType::Notification
        ));
    }

    #[test]
    fn test_id_from_kind() {
        assert_eq!(
            id_from_kind(&JsonRpcMessageKind::Request {
                id: JsonRpcId::Number(42),
                method: "x".to_string(),
            }),
            Some(JsonRpcId::Number(42))
        );
        assert_eq!(
            id_from_kind(&JsonRpcMessageKind::Response {
                id: JsonRpcId::String("abc".to_string()),
            }),
            Some(JsonRpcId::String("abc".to_string()))
        );
        assert_eq!(
            id_from_kind(&JsonRpcMessageKind::Notification {
                method: "x".to_string(),
            }),
            None
        );
    }

    // ── F-015 PendingRequests Tests ─────────────────────────────────────

    #[test]
    fn test_pending_requests_insert_remove() {
        let mut pending = PendingRequests::default();
        pending.outbound.insert(
            "42".to_string(),
            PendingRequest {
                method: "tools/call".to_string(),
                sent_at: Instant::now(),
            },
        );
        assert_eq!(pending.outbound.len(), 1);
        assert!(pending.outbound.remove("42").is_some());
        assert!(pending.outbound.is_empty());
    }

    #[test]
    fn test_pending_requests_independent_maps() {
        let mut pending = PendingRequests::default();
        pending.outbound.insert(
            "1".to_string(),
            PendingRequest {
                method: "tools/call".to_string(),
                sent_at: Instant::now(),
            },
        );
        pending.inbound.insert(
            "1".to_string(),
            PendingRequest {
                method: "sampling/createMessage".to_string(),
                sent_at: Instant::now(),
            },
        );
        assert_eq!(pending.outbound.len(), 1);
        assert_eq!(pending.inbound.len(), 1);
    }

    /// EC-STDIO-019: Server-initiated requests (sampling/createMessage) are not passthrough.
    #[test]
    fn test_sampling_not_passthrough_ec019() {
        assert!(!is_passthrough("sampling/createMessage"));
    }

    // ── REQ-OBS-002 §7.3 Trace Context Propagation Tests ────────────────

    /// TC-OBS2-006: Verify rebuild_message_with_params produces valid NDJSON.
    #[test]
    fn test_rebuild_message_request() {
        let kind = JsonRpcMessageKind::Request {
            id: JsonRpcId::Number(1),
            method: "tools/call".to_string(),
        };
        let params = serde_json::json!({
            "name": "test_tool",
            "arguments": { "a": 1 }
        });

        let rebuilt = rebuild_message_with_params(&kind, Some(&params));
        assert!(rebuilt.ends_with('\n'), "should end with newline");

        let parsed: serde_json::Value = serde_json::from_str(rebuilt.trim()).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["method"], "tools/call");
        assert_eq!(parsed["params"]["name"], "test_tool");
    }

    #[test]
    fn test_rebuild_message_notification() {
        let kind = JsonRpcMessageKind::Notification {
            method: "initialized".to_string(),
        };

        let rebuilt = rebuild_message_with_params(&kind, None);
        let parsed: serde_json::Value = serde_json::from_str(rebuilt.trim()).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["method"], "initialized");
        assert!(parsed.get("id").is_none());
        assert!(parsed.get("params").is_none());
    }

    #[test]
    fn test_rebuild_message_strips_meta() {
        use thoughtgate_core::telemetry::extract_context_from_meta;

        let kind = JsonRpcMessageKind::Request {
            id: JsonRpcId::Number(42),
            method: "tools/call".to_string(),
        };
        let params = serde_json::json!({
            "_meta": {
                "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
            },
            "name": "calculator"
        });

        // Extract trace context (which strips _meta.traceparent when propagate_upstream=false)
        let trace_ctx = extract_context_from_meta(Some(&params), false);
        assert!(trace_ctx.had_trace_context);

        // Rebuild with stripped params
        let rebuilt = rebuild_message_with_params(&kind, trace_ctx.stripped_params.as_ref());
        let parsed: serde_json::Value = serde_json::from_str(rebuilt.trim()).unwrap();

        // Verify _meta.traceparent is gone but other params preserved
        assert_eq!(parsed["params"]["name"], "calculator");
        assert!(
            parsed["params"].get("_meta").is_none(),
            "empty _meta should be removed"
        );
    }
}
