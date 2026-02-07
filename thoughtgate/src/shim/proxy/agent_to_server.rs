//! Agent → Server task: read from agent stdin, evaluate via governance, forward to server stdin.
//!
//! Implements: REQ-CORE-008/F-012 (agent→server), F-013, F-014, F-015, F-016, F-017
//! Implements: REQ-OBS-002 §7.3

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncWriteExt, BufReader, Stdout};
use tokio::sync::{Mutex, watch};

use thoughtgate_core::StreamDirection;
use thoughtgate_core::governance::api::{
    GovernanceDecision, GovernanceEvaluateRequest, GovernanceEvaluateResponse,
};
use thoughtgate_core::jsonrpc::JsonRpcMessageKind;
use thoughtgate_core::profile::Profile;
use thoughtgate_core::telemetry::{
    McpMessageType, McpSpanData, ThoughtGateMetrics, extract_context_from_meta, finish_mcp_span,
    start_mcp_span,
};

use crate::error::{FramingError, StdioError};
use crate::shim::ndjson::{detect_smuggling, parse_stdio_message};

use super::helpers::{
    ApprovalPollResult, DEFAULT_APPROVAL_POLL_MS, EVALUATE_TIMEOUT_MS, MAX_APPROVAL_POLL_MS,
    MIN_APPROVAL_POLL_MS, PendingRequest, PendingRequests, bounded_read_line, extract_tool_name,
    format_deny_response, framing_error_type, id_from_kind, is_passthrough, jsonrpc_id_to_string,
    message_type_from_kind, method_from_kind, poll_approval_status, rebuild_message_with_params,
    write_stdout,
};

/// Read from agent stdin, evaluate via governance, forward to server stdin.
///
/// Implements: REQ-CORE-008/F-012 (agent→server), F-013, F-014, F-015, F-016, F-017
#[allow(clippy::too_many_arguments)]
pub(super) async fn agent_to_server(
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
        let trace_ctx = extract_context_from_meta(msg.params.as_ref(), false);
        let stripped_params = trace_ctx.stripped_params.clone();

        // Build the message to forward (with _meta.traceparent stripped if present).
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
                JsonRpcMessageKind::Response { .. } => McpMessageType::Request,
            },
            message_id: id_from_kind(&msg.kind).map(|id| jsonrpc_id_to_string(&id)),
            correlation_id: &correlation_id,
            tool_name: tool_name.as_deref(),
            session_id: None,
            parent_context: if trace_ctx.had_trace_context {
                Some(&trace_ctx.context)
            } else {
                None
            },
        };
        let mut mcp_span = start_mcp_span(&span_data);

        // F-016: Governance evaluation via HTTP.
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

                finish_mcp_span(&mut mcp_span, true, Some(-32003), Some("policy_denied"));
            }
            GovernanceDecision::PendingApproval => {
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
