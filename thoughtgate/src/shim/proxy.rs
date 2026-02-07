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

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout};
use tokio::process::Command;
use tokio::sync::{Mutex, watch};

use thoughtgate_core::StreamDirection;
use thoughtgate_core::governance::api::{
    ApprovalOutcome, GovernanceDecision, GovernanceEvaluateRequest, GovernanceEvaluateResponse,
    MessageType, TaskStatusResponse,
};
use thoughtgate_core::governance::service::HeartbeatRequest;
use thoughtgate_core::jsonrpc::{JsonRpcId, JsonRpcMessageKind};
use thoughtgate_core::profile::Profile;
use thoughtgate_core::telemetry::{
    McpMessageType, McpSpanData, ThoughtGateMetrics, extract_context_from_meta, finish_mcp_span,
    start_mcp_span,
};

use crate::error::{FramingError, StdioError};
use crate::shim::ndjson::{detect_smuggling, parse_stdio_message};
use crate::wrap::config_adapter::ShimOptions;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Initial backoff delay for governance /healthz polling (F-016a).
const HEALTHZ_INITIAL_BACKOFF_MS: u64 = 50;

/// Maximum backoff delay for governance /healthz polling.
const HEALTHZ_MAX_BACKOFF_MS: u64 = 2000;

/// Maximum number of /healthz poll attempts before giving up.
const HEALTHZ_MAX_ATTEMPTS: u32 = 20;

/// Heartbeat interval — shims send a heartbeat every 5 seconds when idle.
const HEARTBEAT_INTERVAL_SECS: u64 = 5;

/// Connect timeout for all governance HTTP requests (F-016b).
const CONNECT_TIMEOUT_MS: u64 = 500;

/// Request timeout for POST /governance/evaluate (F-016b).
const EVALUATE_TIMEOUT_MS: u64 = 2000;

/// Request timeout for POST /governance/heartbeat.
const HEARTBEAT_TIMEOUT_MS: u64 = 5000;

/// Default poll interval for approval status checks (ms).
const DEFAULT_APPROVAL_POLL_MS: u64 = 5000;

/// Minimum poll interval to prevent CPU spin from a misconfigured or
/// compromised governance service returning very low values.
const MIN_APPROVAL_POLL_MS: u64 = 100;

/// Maximum poll interval to prevent a compromised governance service from
/// stalling the shim indefinitely with very high values.
const MAX_APPROVAL_POLL_MS: u64 = 30_000;

/// Request timeout for GET /governance/task/{id} (ms).
const TASK_STATUS_TIMEOUT_MS: u64 = 5000;

/// Maximum number of poll cycles before giving up (safety net).
/// Real timeout is server-side TTL. 720 polls × 5s = 1 hour.
const MAX_POLL_CYCLES: u32 = 720;

// ─────────────────────────────────────────────────────────────────────────────
// Pure Helper Functions
// ─────────────────────────────────────────────────────────────────────────────

/// Check if a JSON-RPC method should bypass governance evaluation (F-017).
///
/// These messages are forwarded without calling POST /governance/evaluate:
/// - Protocol handshake: `initialize`, `initialized`
/// - Cancel/progress notifications
/// - Resource/tool/prompt list change notifications
/// - Ping/pong keepalive
///
/// Implements: REQ-CORE-008/F-017
fn is_passthrough(method: &str) -> bool {
    matches!(
        method,
        "initialize"
            | "initialized"
            | "notifications/cancelled"
            | "notifications/progress"
            | "notifications/resources/updated"
            | "notifications/resources/list_changed"
            | "notifications/tools/list_changed"
            | "notifications/prompts/list_changed"
            | "ping"
            | "pong"
    )
}

/// Format a JSON-RPC error response for a denied message.
///
/// Returns a complete NDJSON line (with trailing newline) containing a
/// JSON-RPC 2.0 error response with code -32003 (PolicyDenied) and
/// ThoughtGate denial details in the `data` field.
///
/// Implements: REQ-CORE-008/F-012
fn format_deny_response(
    id: &thoughtgate_core::jsonrpc::JsonRpcId,
    server_id: &str,
    policy_id: Option<&str>,
) -> String {
    let id_json = serde_json::to_value(id).unwrap_or(serde_json::Value::Null);
    let mut data = serde_json::json!({
        "server_id": server_id,
    });
    if let Some(pid) = policy_id {
        data["policy_id"] = serde_json::Value::String(pid.to_string());
    }
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id_json,
        "error": {
            "code": -32003,
            "message": "Denied by ThoughtGate policy",
            "data": data,
        }
    });
    // Static fallback: the json!() macro above should always serialize, but
    // guard against truly pathological id values. An empty string + '\n' would
    // be an unparseable NDJSON line for the agent.
    let mut line = serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32003,"message":"Denied by ThoughtGate policy"}}"#.to_string()
    });
    line.push('\n');
    line
}

/// Convert a [`FramingError`] variant to a metric label string.
///
/// Used as the `error_type` attribute for `framing_errors_total` counter.
fn framing_error_type(e: &FramingError) -> &'static str {
    match e {
        FramingError::MessageTooLarge { .. } => "message_too_large",
        FramingError::MalformedJson { .. } => "malformed_json",
        FramingError::MissingVersion => "missing_version",
        FramingError::UnsupportedVersion { .. } => "unsupported_version",
        FramingError::UnsupportedBatch => "unsupported_batch",
        FramingError::Io(_) => "io_error",
    }
}

/// Extract the method name from a parsed [`JsonRpcMessageKind`].
///
/// Returns the method for requests and notifications, or `"response"` for
/// responses (which have no method field).
///
/// Implements: REQ-CORE-008/F-013 (NDJSON Message Classification)
fn method_from_kind(kind: &JsonRpcMessageKind) -> &str {
    match kind {
        JsonRpcMessageKind::Request { method, .. } => method.as_str(),
        JsonRpcMessageKind::Notification { method } => method.as_str(),
        JsonRpcMessageKind::Response { .. } => "response",
    }
}

/// Derive the [`MessageType`] from a parsed [`JsonRpcMessageKind`].
///
/// Maps JSON-RPC message kinds to governance API message types for
/// evaluation requests.
///
/// Implements: REQ-CORE-008/F-016 (Governance Integration)
fn message_type_from_kind(kind: &JsonRpcMessageKind) -> MessageType {
    match kind {
        JsonRpcMessageKind::Request { .. } => MessageType::Request,
        JsonRpcMessageKind::Response { .. } => MessageType::Response,
        JsonRpcMessageKind::Notification { .. } => MessageType::Notification,
    }
}

/// Extract the JSON-RPC ID from a message kind, if present.
///
/// Returns `Some(id)` for requests and responses, `None` for notifications.
///
/// Implements: REQ-CORE-008/F-015 (Request ID Tracking)
fn id_from_kind(kind: &JsonRpcMessageKind) -> Option<JsonRpcId> {
    match kind {
        JsonRpcMessageKind::Request { id, .. } | JsonRpcMessageKind::Response { id } => {
            Some(id.clone())
        }
        JsonRpcMessageKind::Notification { .. } => None,
    }
}

/// Convert a JsonRpcId to a string for span attributes.
fn jsonrpc_id_to_string(id: &JsonRpcId) -> String {
    match id {
        JsonRpcId::Number(n) => n.to_string(),
        JsonRpcId::String(s) => s.clone(),
        JsonRpcId::Null => "null".to_string(),
    }
}

/// Extract the tool name from a parsed message's params, if it's a tools/call.
fn extract_tool_name(params: Option<&serde_json::Value>) -> Option<String> {
    params?.get("name")?.as_str().map(|s| s.to_string())
}

/// Rebuild a JSON-RPC message with new params.
///
/// Used to strip `_meta.traceparent` and `_meta.tracestate` before forwarding
/// to the upstream MCP server, as many servers use strict schema validation
/// and will reject unknown fields.
///
/// Implements: REQ-OBS-002 §7.3
fn rebuild_message_with_params(
    kind: &JsonRpcMessageKind,
    params: Option<&serde_json::Value>,
) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "jsonrpc".to_string(),
        serde_json::Value::String("2.0".to_string()),
    );

    match kind {
        JsonRpcMessageKind::Request { id, method } => {
            obj.insert(
                "id".to_string(),
                serde_json::to_value(id).unwrap_or(serde_json::Value::Null),
            );
            obj.insert(
                "method".to_string(),
                serde_json::Value::String(method.clone()),
            );
            if let Some(p) = params {
                obj.insert("params".to_string(), p.clone());
            }
        }
        JsonRpcMessageKind::Notification { method } => {
            obj.insert(
                "method".to_string(),
                serde_json::Value::String(method.clone()),
            );
            if let Some(p) = params {
                obj.insert("params".to_string(), p.clone());
            }
        }
        JsonRpcMessageKind::Response { id } => {
            obj.insert(
                "id".to_string(),
                serde_json::to_value(id).unwrap_or(serde_json::Value::Null),
            );
            // Responses use result/error, not params - but we handle them separately
        }
    }

    let mut json =
        serde_json::to_string(&serde_json::Value::Object(obj)).unwrap_or_else(|_| "{}".to_string());
    json.push('\n');
    json
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared Stdout Writer
// ─────────────────────────────────────────────────────────────────────────────

/// Atomically write data to the shared agent stdout and flush.
///
/// Both `agent_to_server` (deny responses) and `server_to_agent` (forwarded
/// data) write to the same stdout fd. This helper ensures writes are serialized
/// through a `Mutex` so NDJSON lines are never interleaved.
async fn write_stdout(stdout: &Mutex<Stdout>, data: &[u8]) -> Result<(), std::io::Error> {
    let mut guard = stdout.lock().await;
    guard.write_all(data).await?;
    guard.flush().await
}

// ─────────────────────────────────────────────────────────────────────────────
// Bounded Line Reading (DoS Protection)
// ─────────────────────────────────────────────────────────────────────────────

/// Read a single line from an async buffered reader, enforcing a byte limit.
///
/// Unlike bare `read_line`, this function will not allocate unbounded memory
/// if the peer sends a continuous stream of bytes without a newline delimiter.
/// If the accumulated bytes exceed `max_bytes` before a newline is found, the
/// buffer is drained and a `FramingError::MessageTooLarge` is returned.
///
/// Raw bytes are accumulated into a `Vec<u8>` to avoid corrupting multi-byte
/// UTF-8 characters that straddle internal buffer boundaries. The caller
/// converts to `String` after the full line is assembled.
///
/// # Returns
///
/// - `Ok(n)` where `n > 0`: a complete line was read into `buf`
/// - `Ok(0)`: EOF reached
/// - `Err(FramingError::MessageTooLarge)`: line exceeded `max_bytes` without newline
/// - `Err(FramingError::Io)`: underlying I/O error
///
/// Implements: REQ-CORE-008 §5.2 (message size limit)
async fn bounded_read_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> Result<usize, FramingError> {
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf().await.map_err(FramingError::Io)?;

        // EOF — return what we have (or 0 if nothing).
        if available.is_empty() {
            return Ok(total);
        }

        // Scan for newline in the available buffer.
        match available.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                // Found newline at `pos`. Consume up to and including it.
                let to_consume = pos + 1;
                if total + to_consume > max_bytes {
                    // Drain the oversized data so the reader isn't stuck.
                    reader.consume(to_consume);
                    return Err(FramingError::MessageTooLarge { max_bytes });
                }

                buf.extend_from_slice(&available[..to_consume]);
                total += to_consume;
                reader.consume(to_consume);
                return Ok(total);
            }
            None => {
                // No newline yet — consume all available bytes.
                let len = available.len();
                if total + len > max_bytes {
                    // Drain all available data and continue draining until newline or EOF.
                    reader.consume(len);
                    drain_until_newline(reader).await;
                    return Err(FramingError::MessageTooLarge { max_bytes });
                }

                buf.extend_from_slice(available);
                total += len;
                reader.consume(len);
            }
        }
    }
}

/// Drain bytes from a reader until a newline or EOF is reached.
///
/// Used after detecting an oversized line to skip the remainder of the
/// offending message so the reader is positioned at the start of the next line.
/// A 30-second timeout prevents hanging on slow or stalled peers.
async fn drain_until_newline<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) {
    let drain = async {
        loop {
            match reader.fill_buf().await {
                Ok([]) => return, // EOF
                Ok(buf) => {
                    if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                        let consume = pos + 1;
                        reader.consume(consume);
                        return;
                    }
                    let len = buf.len();
                    reader.consume(len);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "IO error while draining oversized message");
                    return;
                }
            }
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(30), drain)
        .await
        .is_err()
    {
        tracing::warn!("drain_until_newline timed out after 30s");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Approval Polling (PendingApproval)
// ─────────────────────────────────────────────────────────────────────────────

/// Result of polling the task status endpoint for an approval decision.
#[derive(Debug)]
enum ApprovalPollResult {
    /// Approval granted — shim should forward the original message.
    Approved,
    /// Approval explicitly rejected by a human reviewer.
    Rejected(Option<String>),
    /// Task expired before a decision was made.
    Expired,
    /// Task was cancelled.
    Cancelled,
    /// Poll cycle limit reached without a decision.
    Timeout,
    /// Unrecoverable HTTP error during polling.
    Error,
    /// Shutdown signal received during polling.
    Shutdown,
}

/// Metadata for an outstanding (in-flight) JSON-RPC request.
///
/// Implements: REQ-CORE-008/F-015
#[derive(Debug, Clone)]
struct PendingRequest {
    method: String,
    sent_at: Instant,
}

/// Bidirectional request ID tracking for audit and orphan detection.
///
/// Implements: REQ-CORE-008/F-015
#[derive(Debug, Default)]
struct PendingRequests {
    /// Agent→server requests awaiting server responses.
    outbound: std::collections::HashMap<String, PendingRequest>,
    /// Server→agent requests awaiting agent responses.
    inbound: std::collections::HashMap<String, PendingRequest>,
}

/// Poll `GET /governance/task/{task_id}` until a terminal decision is reached.
///
/// This function blocks the agent→server task for this message, which is correct
/// for stdio — the MCP protocol is sequential per-connection. The server→agent
/// task and heartbeat continue concurrently.
///
/// Implements: REQ-CORE-008/F-016
async fn poll_approval_status(
    client: &reqwest::Client,
    task_url: &str,
    poll_interval: Duration,
    shutdown_rx: &mut watch::Receiver<bool>,
    profile: Profile,
) -> ApprovalPollResult {
    for cycle in 0..MAX_POLL_CYCLES {
        // Check shutdown between polls.
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                return ApprovalPollResult::Shutdown;
            }
            _ = tokio::time::sleep(poll_interval) => {}
        }

        // GET task status.
        let resp = match client
            .get(task_url)
            .timeout(Duration::from_millis(TASK_STATUS_TIMEOUT_MS))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                if profile == Profile::Production {
                    tracing::warn!(cycle, error = %e, "approval poll: HTTP error");
                    return ApprovalPollResult::Error;
                }
                // Dev mode: retry on transient errors (debug level to reduce noise).
                tracing::debug!(cycle, error = %e, "approval poll: HTTP error (dev retry)");
                continue;
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(cycle, status = %resp.status(), "approval poll: non-200");
            continue;
        }

        let status: TaskStatusResponse = match resp.json().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(cycle, error = %e, "approval poll: parse error");
                continue;
            }
        };

        match status.decision {
            Some(ApprovalOutcome::Approved) => return ApprovalPollResult::Approved,
            Some(ApprovalOutcome::Rejected) => {
                return ApprovalPollResult::Rejected(status.reason);
            }
            Some(ApprovalOutcome::Expired) => return ApprovalPollResult::Expired,
            Some(ApprovalOutcome::Cancelled) => return ApprovalPollResult::Cancelled,
            None => {
                // Still pending — continue polling.
                tracing::debug!(cycle, "approval poll: still pending");
            }
        }
    }

    tracing::warn!(
        max_cycles = MAX_POLL_CYCLES,
        "approval poll: cycle limit reached"
    );
    ApprovalPollResult::Timeout
}

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
async fn poll_governance_healthz(
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
// Main Proxy Function
// ─────────────────────────────────────────────────────────────────────────────

/// Run the shim proxy for a single MCP server.
///
/// Spawns the server child process, establishes bidirectional stdio proxying
/// with governance evaluation on the agent→server path, sends periodic
/// heartbeats, and handles graceful shutdown.
///
/// # Arguments
///
/// * `opts` - Shim configuration (server_id, governance endpoint, profile).
/// * `metrics` - Optional metrics collector (use `None` for tests).
/// * `server_command` - The MCP server command to spawn.
/// * `server_args` - Arguments for the server command.
///
/// # Returns
///
/// The server's exit code (0 for clean shutdown).
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

        tokio::spawn(async move {
            server_to_agent(
                server_id,
                metrics,
                child_stdout,
                agent_stdout,
                &mut shutdown_rx,
                pending,
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
// Agent → Server Task (F-012 outbound)
// ─────────────────────────────────────────────────────────────────────────────

/// Read from agent stdin, evaluate via governance, forward to server stdin.
///
/// Implements: REQ-CORE-008/F-012 (agent→server), F-013, F-014, F-015, F-016, F-017
#[allow(clippy::too_many_arguments)]
async fn agent_to_server(
    server_id: String,
    governance_endpoint: String,
    profile: Profile,
    client: reqwest::Client,
    metrics: Option<Arc<ThoughtGateMetrics>>,
    mut child_stdin: tokio::process::ChildStdin,
    agent_stdout: Arc<Mutex<Stdout>>,
    shutdown_rx: &mut watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
    pending: Arc<tokio::sync::Mutex<PendingRequests>>,
) -> Result<(), StdioError> {
    let agent_stdin = tokio::io::stdin();
    let mut reader = BufReader::new(agent_stdin);
    let mut raw_buf = Vec::new();
    let evaluate_url = format!("{governance_endpoint}/governance/evaluate");

    loop {
        raw_buf.clear();

        let bytes_read = tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                tracing::debug!(server_id, "agent→server: shutdown signal received");
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
                            "agent→server: message exceeded size limit, skipping"
                        );
                        continue;
                    }
                    Err(FramingError::Io(e)) => {
                        return Err(StdioError::Framing {
                            server_id: server_id.clone(),
                            direction: StreamDirection::AgentToServer,
                            source: FramingError::Io(e),
                        });
                    }
                    Err(e) => {
                        return Err(StdioError::Framing {
                            server_id: server_id.clone(),
                            direction: StreamDirection::AgentToServer,
                            source: e,
                        });
                    }
                }
            }
        };

        // EOF — agent closed stdin.
        if bytes_read == 0 {
            tracing::debug!(server_id, "agent stdin EOF");
            break;
        }

        // Validate UTF-8 strictly: lossy conversion would silently replace
        // invalid bytes with U+FFFD, corrupting JSON-RPC message content.
        let buf = match String::from_utf8(raw_buf.clone()) {
            Ok(s) => s,
            Err(_) => {
                if let Some(ref m) = metrics {
                    m.record_stdio_framing_error(&server_id, "invalid_utf8");
                }
                tracing::warn!(
                    server_id,
                    len = raw_buf.len(),
                    "agent→server: invalid UTF-8, skipping"
                );
                continue;
            }
        };

        // Skip empty lines (lenient parsing per §10.2).
        if buf.trim().is_empty() {
            continue;
        }

        // F-014: Smuggling detection on raw bytes (before parsing).
        let smuggled = detect_smuggling(&raw_buf);

        // F-013: Parse the NDJSON line.
        let msg = match parse_stdio_message(&buf) {
            Ok(msg) => msg,
            Err(e) => {
                if let Some(ref m) = metrics {
                    m.record_stdio_framing_error(&server_id, framing_error_type(&e));
                }
                tracing::warn!(server_id, error = %e, "agent→server framing error, skipping");
                continue;
            }
        };

        // Handle smuggling per profile (F-014).
        if smuggled {
            if let Some(ref m) = metrics {
                m.record_stdio_framing_error(&server_id, "smuggling");
            }
            if profile == Profile::Production {
                tracing::warn!(server_id, "smuggling detected, rejecting message");
                continue;
            }
            tracing::warn!(
                server_id,
                "smuggling detected, WOULD_REJECT (development mode)"
            );
        }

        let method = method_from_kind(&msg.kind).to_string();

        // NFR-002: Record every parsed message.
        if let Some(ref m) = metrics {
            m.record_stdio_message(&server_id, "agent_to_server", &method);
        }

        // F-017: Passthrough messages bypass governance.
        if is_passthrough(&method) {
            child_stdin
                .write_all(&raw_buf)
                .await
                .map_err(|e| StdioError::Framing {
                    server_id: server_id.clone(),
                    direction: StreamDirection::AgentToServer,
                    source: FramingError::Io(e),
                })?;
            child_stdin.flush().await.map_err(|e| StdioError::Framing {
                server_id: server_id.clone(),
                direction: StreamDirection::AgentToServer,
                source: FramingError::Io(e),
            })?;
            continue;
        }

        // REQ-OBS-002 §7.3: Extract trace context from _meta for stdio transport.
        // This enables distributed tracing through stdio-based MCP clients
        // (Claude Desktop, Cursor, VS Code, Windsurf).
        // Default: strip trace fields (propagate_upstream: false) to avoid breaking
        // strict schema servers. Set propagate_upstream: true in config for trace-aware upstreams.
        let trace_ctx = extract_context_from_meta(msg.params.as_ref(), false);
        let stripped_params = trace_ctx.stripped_params.clone();

        // Build the message to forward (with _meta.traceparent stripped if present).
        // Only rebuild if we actually extracted trace context to avoid unnecessary overhead.
        let forward_bytes: Vec<u8> = if trace_ctx.had_trace_context {
            rebuild_message_with_params(&msg.kind, stripped_params.as_ref()).into_bytes()
        } else {
            raw_buf.clone()
        };

        if trace_ctx.had_trace_context {
            tracing::debug!(
                server_id,
                method,
                "extracted trace context from _meta, stripped for upstream"
            );
        }

        // REQ-OBS-002 §5.1: Create MCP span for this message
        let correlation_id = uuid::Uuid::new_v4().to_string();
        let tool_name = extract_tool_name(msg.params.as_ref());
        let span_data = McpSpanData {
            method: &method,
            message_type: match &msg.kind {
                JsonRpcMessageKind::Request { .. } => McpMessageType::Request,
                JsonRpcMessageKind::Notification { .. } => McpMessageType::Notification,
                JsonRpcMessageKind::Response { .. } => McpMessageType::Request, // Responses are handled in server_to_agent
            },
            message_id: id_from_kind(&msg.kind).map(|id| jsonrpc_id_to_string(&id)),
            correlation_id: &correlation_id,
            tool_name: tool_name.as_deref(),
            session_id: None, // stdio shim does not track MCP sessions
            parent_context: if trace_ctx.had_trace_context {
                Some(&trace_ctx.context)
            } else {
                None
            },
        };
        let mut mcp_span = start_mcp_span(&span_data);

        // F-016: Governance evaluation via HTTP.
        // Use stripped params to avoid sending trace context in the body.
        let eval_req = GovernanceEvaluateRequest {
            server_id: server_id.clone(),
            direction: StreamDirection::AgentToServer,
            method: method.clone(),
            id: id_from_kind(&msg.kind),
            params: stripped_params,
            message_type: message_type_from_kind(&msg.kind),
            profile,
        };

        // Build HTTP request, adding traceparent/tracestate headers if extracted.
        let mut request = client
            .post(&evaluate_url)
            .timeout(Duration::from_millis(EVALUATE_TIMEOUT_MS))
            .json(&eval_req);

        // Propagate trace context via HTTP headers to governance service.
        if trace_ctx.had_trace_context {
            use opentelemetry::propagation::TextMapPropagator;
            use opentelemetry_sdk::propagation::TraceContextPropagator;

            let propagator = TraceContextPropagator::new();
            let mut carrier = std::collections::HashMap::new();
            propagator.inject_context(&trace_ctx.context, &mut carrier);

            if let Some(tp) = carrier.get("traceparent") {
                request = request.header("traceparent", tp);
            }
            if let Some(ts) = carrier.get("tracestate") {
                request = request.header("tracestate", ts);
            }
        }

        let eval_result = request.send().await;

        let eval_resp: GovernanceEvaluateResponse = match eval_result {
            Ok(resp) => match resp.json().await {
                Ok(body) => body,
                Err(e) => {
                    tracing::error!(server_id, error = %e, "failed to parse governance response");
                    // Fail-closed in production: send deny so agent sees an error
                    // rather than a silently dropped request.
                    if profile == Profile::Production {
                        if let Some(ref m) = metrics {
                            m.record_stdio_governance_decision(
                                &server_id,
                                "deny",
                                &format!("{profile:?}"),
                            );
                        }
                        if let Some(id) = id_from_kind(&msg.kind) {
                            let deny_line = format_deny_response(&id, &server_id, None);
                            write_stdout(&agent_stdout, deny_line.as_bytes())
                                .await
                                .map_err(StdioError::StdioIo)?;
                        }
                        // REQ-OBS-002 §5.1: Finish MCP span (governance parse error)
                        finish_mcp_span(
                            &mut mcp_span,
                            true,
                            Some(-32603),
                            Some("governance_parse_error"),
                        );
                        continue;
                    }
                    tracing::warn!(
                        server_id,
                        "WOULD_DENY: governance parse error, forwarding in dev"
                    );
                    GovernanceEvaluateResponse {
                        decision: GovernanceDecision::Forward,
                        task_id: None,
                        policy_id: None,
                        reason: None,
                        poll_interval_ms: None,
                        shutdown: false,
                        deny_source: None,
                    }
                }
            },
            Err(e) => {
                tracing::error!(server_id, error = %e, "governance evaluate request failed");
                // Fail-closed in production: send deny so agent sees an error
                // rather than a silently dropped request.
                if profile == Profile::Production {
                    if let Some(ref m) = metrics {
                        m.record_stdio_governance_decision(
                            &server_id,
                            "deny",
                            &format!("{profile:?}"),
                        );
                    }
                    if let Some(id) = id_from_kind(&msg.kind) {
                        let deny_line = format_deny_response(&id, &server_id, None);
                        write_stdout(&agent_stdout, deny_line.as_bytes())
                            .await
                            .map_err(StdioError::StdioIo)?;
                    }
                    // REQ-OBS-002 §5.1: Finish MCP span (governance request failed)
                    finish_mcp_span(
                        &mut mcp_span,
                        true,
                        Some(-32000),
                        Some("governance_unavailable"),
                    );
                    continue;
                }
                tracing::warn!(
                    server_id,
                    "WOULD_DENY: governance timeout, forwarding in dev"
                );
                GovernanceEvaluateResponse {
                    decision: GovernanceDecision::Forward,
                    task_id: None,
                    policy_id: None,
                    reason: None,
                    poll_interval_ms: None,
                    shutdown: false,
                    deny_source: None,
                }
            }
        };

        // NFR-002: Record governance decision.
        let decision_str = match eval_resp.decision {
            GovernanceDecision::Forward => "forward",
            GovernanceDecision::Deny => "deny",
            GovernanceDecision::PendingApproval => "pending_approval",
        };
        if let Some(ref m) = metrics {
            m.record_stdio_governance_decision(&server_id, decision_str, &format!("{profile:?}"));
        }

        // NFR-002: Structured audit log entry.
        {
            let tool_name = msg
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("-");
            let message_id = id_from_kind(&msg.kind)
                .map(|id| serde_json::to_string(&id).unwrap_or_default())
                .unwrap_or_else(|| "-".to_string());
            tracing::info!(
                component = "shim",
                server_id = %server_id,
                direction = "agent_to_server",
                method = %method,
                tool = %tool_name,
                message_id = %message_id,
                governance_decision = %decision_str,
                profile = ?profile,
                "governance audit"
            );
        }

        // Check for shutdown signal piggy-backed on evaluate response.
        if eval_resp.shutdown {
            tracing::info!(server_id, "shutdown signal in evaluate response");
            let _ = shutdown_tx.send(true);
        }

        match eval_resp.decision {
            GovernanceDecision::Forward => {
                // Forward with _meta.traceparent stripped (REQ-OBS-002 §7.3).
                child_stdin
                    .write_all(&forward_bytes)
                    .await
                    .map_err(|e| StdioError::Framing {
                        server_id: server_id.clone(),
                        direction: StreamDirection::AgentToServer,
                        source: FramingError::Io(e),
                    })?;
                child_stdin.flush().await.map_err(|e| StdioError::Framing {
                    server_id: server_id.clone(),
                    direction: StreamDirection::AgentToServer,
                    source: FramingError::Io(e),
                })?;

                // F-015: Track outbound request IDs.
                if let Some(id) = id_from_kind(&msg.kind) {
                    let key = serde_json::to_string(&id).unwrap_or_default();
                    pending.lock().await.outbound.insert(
                        key,
                        PendingRequest {
                            method: method.clone(),
                            sent_at: Instant::now(),
                        },
                    );
                }

                // REQ-OBS-002 §5.1: Finish MCP span (success)
                finish_mcp_span(&mut mcp_span, false, None, None);
            }
            GovernanceDecision::Deny => {
                if let Some(id) = id_from_kind(&msg.kind) {
                    let deny_line =
                        format_deny_response(&id, &server_id, eval_resp.policy_id.as_deref());
                    write_stdout(&agent_stdout, deny_line.as_bytes())
                        .await
                        .map_err(StdioError::StdioIo)?;
                }
                // Notifications have no id — silently drop denials for them.

                // REQ-OBS-002 §5.1: Finish MCP span (policy denied)
                finish_mcp_span(&mut mcp_span, true, Some(-32003), Some("policy_denied"));
            }
            GovernanceDecision::PendingApproval => {
                // Poll the governance task status endpoint until a decision
                // is reached or the operation times out / shuts down.
                let task_id = match eval_resp.task_id.as_deref() {
                    Some(id) => id,
                    None => {
                        tracing::error!(server_id, "PendingApproval without task_id — denying");
                        if let Some(id) = id_from_kind(&msg.kind) {
                            let deny_line = format_deny_response(&id, &server_id, None);
                            write_stdout(&agent_stdout, deny_line.as_bytes())
                                .await
                                .map_err(StdioError::StdioIo)?;
                        }
                        // REQ-OBS-002 §5.1: Finish MCP span (internal error - no task_id)
                        finish_mcp_span(&mut mcp_span, true, Some(-32603), Some("internal_error"));
                        continue;
                    }
                };

                let poll_interval = Duration::from_millis(
                    eval_resp
                        .poll_interval_ms
                        .unwrap_or(DEFAULT_APPROVAL_POLL_MS)
                        .clamp(MIN_APPROVAL_POLL_MS, MAX_APPROVAL_POLL_MS),
                );
                let task_url = format!("{governance_endpoint}/governance/task/{task_id}");

                tracing::info!(
                    server_id,
                    task_id,
                    method,
                    poll_ms = poll_interval.as_millis() as u64,
                    "PendingApproval — starting poll loop"
                );

                // NFR-002: Track approval latency.
                let approval_start = Instant::now();
                let outcome =
                    poll_approval_status(&client, &task_url, poll_interval, shutdown_rx, profile)
                        .await;
                if let Some(ref m) = metrics {
                    m.record_stdio_approval_latency(&server_id, approval_start.elapsed());
                }

                match outcome {
                    ApprovalPollResult::Approved => {
                        tracing::info!(server_id, task_id, "approval granted — forwarding");
                        // Forward with _meta.traceparent stripped (REQ-OBS-002 §7.3).
                        child_stdin.write_all(&forward_bytes).await.map_err(|e| {
                            StdioError::Framing {
                                server_id: server_id.clone(),
                                direction: StreamDirection::AgentToServer,
                                source: FramingError::Io(e),
                            }
                        })?;
                        child_stdin.flush().await.map_err(|e| StdioError::Framing {
                            server_id: server_id.clone(),
                            direction: StreamDirection::AgentToServer,
                            source: FramingError::Io(e),
                        })?;

                        // F-015: Track outbound request IDs (after approval).
                        if let Some(id) = id_from_kind(&msg.kind) {
                            let key = serde_json::to_string(&id).unwrap_or_default();
                            pending.lock().await.outbound.insert(
                                key,
                                PendingRequest {
                                    method: method.clone(),
                                    sent_at: Instant::now(),
                                },
                            );
                        }

                        // REQ-OBS-002 §5.1: Finish MCP span (approval granted)
                        finish_mcp_span(&mut mcp_span, false, None, None);
                    }
                    ApprovalPollResult::Rejected(reason) => {
                        tracing::info!(
                            server_id,
                            task_id,
                            ?reason,
                            "approval rejected — sending deny"
                        );
                        if let Some(id) = id_from_kind(&msg.kind) {
                            let deny_line = format_deny_response(
                                &id,
                                &server_id,
                                eval_resp.policy_id.as_deref(),
                            );
                            write_stdout(&agent_stdout, deny_line.as_bytes())
                                .await
                                .map_err(StdioError::StdioIo)?;
                        }

                        // REQ-OBS-002 §5.1: Finish MCP span (approval rejected)
                        finish_mcp_span(
                            &mut mcp_span,
                            true,
                            Some(-32007),
                            Some("approval_rejected"),
                        );
                    }
                    ApprovalPollResult::Expired | ApprovalPollResult::Cancelled => {
                        tracing::info!(
                            server_id,
                            task_id,
                            result = ?outcome,
                            "approval expired/cancelled — sending deny"
                        );
                        if let Some(id) = id_from_kind(&msg.kind) {
                            let deny_line = format_deny_response(
                                &id,
                                &server_id,
                                eval_resp.policy_id.as_deref(),
                            );
                            write_stdout(&agent_stdout, deny_line.as_bytes())
                                .await
                                .map_err(StdioError::StdioIo)?;
                        }

                        // REQ-OBS-002 §5.1: Finish MCP span (expired/cancelled)
                        let error_type = match outcome {
                            ApprovalPollResult::Expired => "approval_expired",
                            _ => "approval_cancelled",
                        };
                        finish_mcp_span(&mut mcp_span, true, Some(-32005), Some(error_type));
                    }
                    ApprovalPollResult::Timeout | ApprovalPollResult::Error => {
                        tracing::warn!(
                            server_id,
                            task_id,
                            result = ?outcome,
                            "approval poll failed — sending deny"
                        );
                        if let Some(id) = id_from_kind(&msg.kind) {
                            let deny_line = format_deny_response(
                                &id,
                                &server_id,
                                eval_resp.policy_id.as_deref(),
                            );
                            write_stdout(&agent_stdout, deny_line.as_bytes())
                                .await
                                .map_err(StdioError::StdioIo)?;
                        }

                        // REQ-OBS-002 §5.1: Finish MCP span (timeout/error)
                        let error_type = match outcome {
                            ApprovalPollResult::Timeout => "approval_timeout",
                            _ => "approval_error",
                        };
                        finish_mcp_span(&mut mcp_span, true, Some(-32008), Some(error_type));
                    }
                    ApprovalPollResult::Shutdown => {
                        tracing::info!(
                            server_id,
                            task_id,
                            "shutdown during approval poll — breaking"
                        );
                        // REQ-OBS-002 §5.1: Finish MCP span (shutdown)
                        finish_mcp_span(&mut mcp_span, true, Some(-32013), Some("shutdown"));
                        break;
                    }
                }
            }
        }
    }

    // Dropping child_stdin closes the server's stdin pipe.
    drop(child_stdin);
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Server → Agent Task (F-012 inbound)
// ─────────────────────────────────────────────────────────────────────────────

/// Read from server stdout, forward to agent stdout.
///
/// In v0.3, server→agent messages are forwarded unconditionally after local
/// audit logging. No governance HTTP call is made for inbound messages.
///
/// Implements: REQ-CORE-008/F-012 (server→agent), F-015
async fn server_to_agent(
    server_id: String,
    metrics: Option<Arc<ThoughtGateMetrics>>,
    child_stdout: tokio::process::ChildStdout,
    agent_stdout: Arc<Mutex<Stdout>>,
    shutdown_rx: &mut watch::Receiver<bool>,
    pending: Arc<tokio::sync::Mutex<PendingRequests>>,
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
                        if let Some(req) = matched {
                            tracing::debug!(
                                server_id,
                                method = %req.method,
                                latency_us = req.sent_at.elapsed().as_micros() as u64,
                                "response matched outbound request"
                            );
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
async fn heartbeat_loop(
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

// ─────────────────────────────────────────────────────────────────────────────
// Shutdown Sequence (F-018)
// ─────────────────────────────────────────────────────────────────────────────

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
async fn shutdown_server(
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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use thoughtgate_core::jsonrpc::JsonRpcId;

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
        assert_eq!(parsed["error"]["message"], "Denied by ThoughtGate policy");
        assert_eq!(parsed["error"]["data"]["server_id"], "filesystem");
        assert_eq!(parsed["error"]["data"]["policy_id"], "pol-001");
    }

    #[test]
    fn test_format_deny_response_string_id() {
        let line = format_deny_response(&JsonRpcId::String("req-abc".to_string()), "github", None);
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["id"], "req-abc");
        assert_eq!(parsed["error"]["data"]["server_id"], "github");
        assert!(parsed["error"]["data"]["policy_id"].is_null());
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
