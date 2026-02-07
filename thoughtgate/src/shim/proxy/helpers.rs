//! Pure helper functions, constants, shared types, and I/O utilities.
//!
//! Implements: REQ-CORE-008/F-012, F-013, F-015, F-016, F-017
//! Implements: REQ-OBS-002 §7.3

use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, Stdout};
use tokio::sync::{Mutex, watch};

use thoughtgate_core::governance::api::{ApprovalOutcome, MessageType, TaskStatusResponse};
use thoughtgate_core::jsonrpc::{JsonRpcId, JsonRpcMessageKind};
use thoughtgate_core::profile::Profile;

use crate::error::FramingError;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Initial backoff delay for governance /healthz polling (F-016a).
pub(super) const HEALTHZ_INITIAL_BACKOFF_MS: u64 = 50;

/// Maximum backoff delay for governance /healthz polling.
pub(super) const HEALTHZ_MAX_BACKOFF_MS: u64 = 2000;

/// Maximum number of /healthz poll attempts before giving up.
pub(super) const HEALTHZ_MAX_ATTEMPTS: u32 = 20;

/// Heartbeat interval — shims send a heartbeat every 5 seconds when idle.
pub(super) const HEARTBEAT_INTERVAL_SECS: u64 = 5;

/// Connect timeout for all governance HTTP requests (F-016b).
pub(super) const CONNECT_TIMEOUT_MS: u64 = 500;

/// Request timeout for POST /governance/evaluate (F-016b).
pub(super) const EVALUATE_TIMEOUT_MS: u64 = 2000;

/// Request timeout for POST /governance/heartbeat.
pub(super) const HEARTBEAT_TIMEOUT_MS: u64 = 5000;

/// Default poll interval for approval status checks (ms).
pub(super) const DEFAULT_APPROVAL_POLL_MS: u64 = 5000;

/// Minimum poll interval to prevent CPU spin from a misconfigured or
/// compromised governance service returning very low values.
pub(super) const MIN_APPROVAL_POLL_MS: u64 = 100;

/// Maximum poll interval to prevent a compromised governance service from
/// stalling the shim indefinitely with very high values.
pub(super) const MAX_APPROVAL_POLL_MS: u64 = 30_000;

/// Request timeout for GET /governance/task/{id} (ms).
pub(super) const TASK_STATUS_TIMEOUT_MS: u64 = 5000;

/// Maximum number of poll cycles before giving up (safety net).
/// Real timeout is server-side TTL. 720 polls × 5s = 1 hour.
pub(super) const MAX_POLL_CYCLES: u32 = 720;

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
pub(super) fn is_passthrough(method: &str) -> bool {
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
/// JSON-RPC 2.0 error response with code -32003 (PolicyDenied).
/// Delegates to core's `error_response_string` for consistent wire format.
///
/// Implements: REQ-CORE-008/F-012
pub(super) fn format_deny_response(
    id: &JsonRpcId,
    server_id: &str,
    policy_id: Option<&str>,
) -> String {
    let error = thoughtgate_core::error::ThoughtGateError::PolicyDenied {
        tool: String::new(),
        policy_id: policy_id.map(String::from),
        reason: None,
    };
    let mut line =
        thoughtgate_core::transport::jsonrpc::error_response_string(id, &error, server_id);
    line.push('\n');
    line
}

/// Convert a [`FramingError`] variant to a metric label string.
///
/// Used as the `error_type` attribute for `framing_errors_total` counter.
pub(super) fn framing_error_type(e: &FramingError) -> &'static str {
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
pub(super) fn method_from_kind(kind: &JsonRpcMessageKind) -> &str {
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
pub(super) fn message_type_from_kind(kind: &JsonRpcMessageKind) -> MessageType {
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
pub(super) fn id_from_kind(kind: &JsonRpcMessageKind) -> Option<JsonRpcId> {
    match kind {
        JsonRpcMessageKind::Request { id, .. } | JsonRpcMessageKind::Response { id } => {
            Some(id.clone())
        }
        JsonRpcMessageKind::Notification { .. } => None,
    }
}

/// Convert a JsonRpcId to a string for span attributes.
///
/// Delegates to the `Display` impl on `JsonRpcId`.
pub(super) fn jsonrpc_id_to_string(id: &JsonRpcId) -> String {
    id.to_string()
}

/// Extract the tool name from a parsed message's params, if it's a tools/call.
pub(super) fn extract_tool_name(params: Option<&serde_json::Value>) -> Option<String> {
    params?.get("name")?.as_str().map(|s| s.to_string())
}

/// Rebuild a JSON-RPC message with new params.
///
/// Used to strip `_meta.traceparent` and `_meta.tracestate` before forwarding
/// to the upstream MCP server, as many servers use strict schema validation
/// and will reject unknown fields.
///
/// Implements: REQ-OBS-002 §7.3
pub(super) fn rebuild_message_with_params(
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
pub(super) async fn write_stdout(
    stdout: &Mutex<Stdout>,
    data: &[u8],
) -> Result<(), std::io::Error> {
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
pub(super) async fn bounded_read_line<R: tokio::io::AsyncBufRead + Unpin>(
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
// Approval Polling Types
// ─────────────────────────────────────────────────────────────────────────────

/// Result of polling the task status endpoint for an approval decision.
#[derive(Debug)]
pub(super) enum ApprovalPollResult {
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
pub(super) struct PendingRequest {
    pub(super) method: String,
    pub(super) sent_at: Instant,
}

/// Bidirectional request ID tracking for audit and orphan detection.
///
/// Implements: REQ-CORE-008/F-015
#[derive(Debug, Default)]
pub(super) struct PendingRequests {
    /// Agent→server requests awaiting server responses.
    pub(super) outbound: std::collections::HashMap<String, PendingRequest>,
    /// Server→agent requests awaiting agent responses.
    pub(super) inbound: std::collections::HashMap<String, PendingRequest>,
}

/// Poll `GET /governance/task/{task_id}` until a terminal decision is reached.
///
/// This function blocks the agent→server task for this message, which is correct
/// for stdio — the MCP protocol is sequential per-connection. The server→agent
/// task and heartbeat continue concurrently.
///
/// Implements: REQ-CORE-008/F-016
pub(super) async fn poll_approval_status(
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
