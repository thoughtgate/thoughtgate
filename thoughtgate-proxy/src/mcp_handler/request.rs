//! MCP request processing: body parsing, single/batch dispatch.
//!
//! Implements: REQ-CORE-003/§10 (Request Handler Pattern)
//! Implements: REQ-OBS-002 §7.1 (W3C Trace Context Propagation)

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use bytes::Bytes;
use tracing::{debug, error, warn};

use thoughtgate_core::error::ThoughtGateError;
use thoughtgate_core::telemetry::{
    BoxedSpan, McpMessageType, McpSpanData, finish_mcp_span, start_mcp_span,
};
use thoughtgate_core::transport::jsonrpc::{
    JsonRpcResponse, McpRequest, ParsedRequests, parse_jsonrpc,
};

use super::McpState;
use super::helpers::{BufferGuard, error_bytes, extract_tool_name, json_bytes};

/// Handle POST /mcp/v1 requests (Axum handler for tests).
///
/// Implements: REQ-CORE-003/§10 (Request Handler Pattern)
#[cfg(test)]
pub(super) async fn handle_mcp_request(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<McpState>>,
    body: Bytes,
) -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;

    let (status, bytes) = handle_mcp_body_bytes(&state, body, None).await;
    (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}

/// Handle a buffered MCP request body, returning (StatusCode, Bytes).
///
/// This is the core MCP processing logic, used by both:
/// - `McpHandler::handle()` (direct invocation from ProxyService)
/// - `handle_mcp_request()` (Axum handler for standalone server)
///
/// Returns `(StatusCode, Bytes)` to avoid double-buffering when ProxyService
/// converts to UnifiedBody.
///
/// # Request Flow
///
/// 1. Check body size limit
/// 2. Acquire semaphore permit (EC-MCP-011)
/// 3. Parse JSON-RPC request(s)
/// 4. Route and handle each request
/// 5. Return response(s)
///
/// # Traceability
/// - Implements: REQ-CORE-003/§10 (Request Handler Pattern)
/// - Implements: REQ-OBS-002 §7.1 (W3C Trace Context Propagation)
pub(super) async fn handle_mcp_body_bytes(
    state: &McpState,
    body: Bytes,
    parent_context: Option<opentelemetry::Context>,
) -> (StatusCode, Bytes) {
    // Check body size limit (generate unique correlation ID per REQ-CORE-004)
    if body.len() > state.max_body_size {
        let correlation_id =
            thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
        let error = ThoughtGateError::InvalidRequest {
            details: format!(
                "Request body exceeds maximum size of {} bytes",
                state.max_body_size
            ),
        };
        return error_bytes(None, &error, &correlation_id);
    }

    // Track aggregate buffered bytes to prevent OOM.
    // Increment before processing; decrement when this request completes.
    let body_size = body.len();
    let prev = state.buffered_bytes.fetch_add(body_size, Ordering::AcqRel);
    if prev.saturating_add(body_size) > state.max_aggregate_buffer {
        state.buffered_bytes.fetch_sub(body_size, Ordering::Release);
        let correlation_id =
            thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
        warn!(
            correlation_id = %correlation_id,
            buffered = prev.saturating_add(body_size),
            limit = state.max_aggregate_buffer,
            "Aggregate buffer limit exceeded"
        );
        let error = ThoughtGateError::RateLimited {
            retry_after_secs: Some(1),
        };
        return error_bytes(None, &error, &correlation_id);
    }

    // Ensure we decrement buffered_bytes when this request completes.
    let _buffer_guard = BufferGuard {
        counter: &state.buffered_bytes,
        size: body_size,
    };

    // Parse JSON-RPC request(s) first to determine permit count
    // (generate unique correlation ID per REQ-CORE-004)
    let parsed = match parse_jsonrpc(&body) {
        Ok(p) => p,
        Err(e) => {
            let correlation_id =
                thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
            return error_bytes(None, &e, &correlation_id);
        }
    };

    // Determine how many permits to acquire: 1 for single, batch_size for batch.
    // This prevents a batch of N requests from consuming only 1 concurrency slot.
    // Also extract method name for payload size metrics (REQ-OBS-002 §6.2/MH-005).
    let (permit_count, inbound_method) = match &parsed {
        ParsedRequests::Single(req) => (1, req.method.clone()),
        ParsedRequests::Batch(requests) => {
            if requests.len() > state.max_batch_size {
                let correlation_id =
                    thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
                let error = ThoughtGateError::InvalidRequest {
                    details: format!(
                        "Batch size {} exceeds maximum of {}",
                        requests.len(),
                        state.max_batch_size
                    ),
                };
                return error_bytes(None, &error, &correlation_id);
            }
            // For batch, use "batch" as method label
            (requests.len().max(1) as u32, "batch".to_string())
        }
    };

    // Record inbound payload size (REQ-OBS-002 §6.2/MH-005)
    if let Some(ref metrics) = state.tg_metrics {
        metrics.record_payload_size("inbound", &inbound_method, body_size as f64);
    }

    // Try to acquire semaphore permits weighted by request count (EC-MCP-011)
    let _permit = match state.semaphore.clone().try_acquire_many_owned(permit_count) {
        Ok(permit) => permit,
        Err(_) => {
            let correlation_id =
                thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
            warn!(
                correlation_id = %correlation_id,
                permits_requested = permit_count,
                "Max concurrent requests reached, returning 503"
            );
            // Return HTTP 200 with JSON-RPC error per MCP spec.
            // MCP clients expect JSON-RPC error frames over HTTP 200, not HTTP 503.
            let error = ThoughtGateError::RateLimited {
                retry_after_secs: Some(1),
            };
            let jsonrpc_error = error.to_jsonrpc_error(&correlation_id);
            let response = JsonRpcResponse::error(None, jsonrpc_error);
            let bytes = serde_json::to_vec(&response)
                .map(Bytes::from)
                .unwrap_or_else(|_| {
                    Bytes::from_static(
                        br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32009,"message":"Rate limited"}}"#,
                    )
                });
            return (StatusCode::OK, bytes);
        }
    };

    let (status, response_bytes) = match parsed {
        ParsedRequests::Single(request) => {
            handle_single_request_bytes(state, request, parent_context.as_ref()).await
        }
        ParsedRequests::Batch(requests) => {
            handle_batch_request_bytes(state, requests, parent_context.as_ref()).await
        }
    };

    // Record outbound payload size (REQ-OBS-002 §6.2/MH-005)
    if let Some(ref metrics) = state.tg_metrics {
        metrics.record_payload_size("outbound", &inbound_method, response_bytes.len() as f64);
    }

    (status, response_bytes)
}

/// Handle a single JSON-RPC request, returning (StatusCode, Bytes).
///
/// Implements the v0.2 4-gate model:
/// - Gate 1: Visibility (ExposeConfig filtering)
/// - Gate 2: Governance Rules (YAML rule matching)
/// - Gate 3: Cedar Policy (when action: policy)
/// - Gate 4: Approval Workflow (when action: approve)
///
/// # Traceability
/// - Implements: REQ-CORE-003/F-002 (Method Routing)
async fn handle_single_request_bytes(
    state: &McpState,
    request: McpRequest,
    parent_context: Option<&opentelemetry::Context>,
) -> (StatusCode, Bytes) {
    let correlation_id = request.correlation_id.to_string();
    let id = request.id.clone();
    let is_notification = request.is_notification();
    let method = request.method.clone();
    let request_start = std::time::Instant::now();

    // Extract tool name for tools/call requests (REQ-OBS-002 §5.1.1)
    // Borrows from request.params (Arc<Value>) — must convert to owned before request is moved
    let tool_name: Option<String> = extract_tool_name(&request).map(String::from);

    // Start MCP span with optional parent context (REQ-OBS-002 §5.1, §7.1)
    // When parent_context is provided, the span becomes a child of the caller's trace.
    let span_data = McpSpanData {
        method: &method,
        message_type: if is_notification {
            McpMessageType::Notification
        } else {
            McpMessageType::Request
        },
        message_id: id.as_ref().map(|id| id.to_string()),
        correlation_id: &correlation_id,
        tool_name: tool_name.as_deref(),
        session_id: None, // TODO: extract from MCP handshake when available
        parent_context,
    };
    let mut mcp_span: BoxedSpan = start_mcp_span(&span_data);

    debug!(
        correlation_id = %correlation_id,
        method = %request.method,
        is_notification = is_notification,
        is_task_augmented = request.is_task_augmented(),
        "Processing single request"
    );

    // Route the request through shared routing logic.
    // Note: Trace context propagation to upstream uses Context::current() in forward_once().
    // The MCP span is part of the correct trace because we used parent_context when
    // creating it via start_mcp_span(). For outbound propagation to upstream, the
    // global propagator was installed in init_telemetry() and will inject the trace
    // context based on the current span.
    //
    // IMPORTANT: ContextGuard is !Send and cannot be held across .await points.
    // The trace context for upstream injection works because:
    // 1. The MCP span was created with the correct parent context
    // 2. The global propagator extracts context from the current span
    // 3. forward_once() calls Context::current() which finds the span
    let result = super::route_request(state, request).await;

    // Determine error info for span (REQ-OBS-002 §5.1.1)
    let (is_error, error_code, error_type): (bool, Option<i32>, Option<String>) = match &result {
        Ok(_) => (false, None, None),
        Err(e) => (
            true,
            Some(e.to_jsonrpc_code()),
            Some(e.error_type_name().to_string()),
        ),
    };

    // Finish span with result attributes
    finish_mcp_span(&mut mcp_span, is_error, error_code, error_type.as_deref());
    drop(mcp_span);

    // Record prometheus-client metrics (REQ-OBS-002 §6.1, §6.2)
    let outcome = if result.is_ok() { "success" } else { "error" };
    if let Some(ref metrics) = state.tg_metrics {
        let duration_ms = request_start.elapsed().as_secs_f64() * 1000.0;
        metrics.record_request(&method, tool_name.as_deref(), outcome);
        metrics.record_request_duration(&method, tool_name.as_deref(), duration_ms);

        if let Err(ref e) = result {
            metrics.record_error(e.error_type_name(), &method);
        }
    }

    // Handle notification - no response (empty body with 204)
    if is_notification {
        if let Err(e) = result {
            error!(
                correlation_id = %correlation_id,
                error = %e,
                "Notification processing failed"
            );
        }
        return (StatusCode::NO_CONTENT, Bytes::new());
    }

    // Return response
    match result {
        Ok(response) => json_bytes(&response),
        Err(e) => error_bytes(id, &e, &correlation_id),
    }
}

/// Handle a batch JSON-RPC request, returning (StatusCode, Bytes).
///
/// Implements: REQ-CORE-003/F-007 (Batch Request Handling)
///
/// # Design Note: Batch Concurrency
///
/// Batch items are processed concurrently via `buffer_unordered` with a cap
/// of 16 in-flight items. This is safe because:
///
/// - **Independent routing**: Each item routes through `route_request()`
///   independently. Approval decisions are per-request, not per-batch.
/// - **Bounded concurrency**: The `.min(16)` cap prevents a single batch
///   from monopolizing the upstream connection pool, even when
///   `max_batch_size` allows up to 100 items.
/// - **Response ordering**: JSON-RPC 2.0 §6 specifies batch responses may
///   be returned in any order — clients match by `id`.
async fn handle_batch_request_bytes(
    state: &McpState,
    items: Vec<thoughtgate_core::transport::jsonrpc::BatchItem>,
    parent_context: Option<&opentelemetry::Context>,
) -> (StatusCode, Bytes) {
    use futures_util::stream::{self, StreamExt};
    use opentelemetry::trace::{Span, SpanKind, Tracer};
    use thoughtgate_core::transport::jsonrpc::BatchItem;

    // Create batch-level span for observability (REQ-OBS-002 §5.1).
    // ContextGuard is !Send so we cannot attach the span across async
    // boundaries, but the span itself tracks batch-level metadata.
    let tracer = opentelemetry::global::tracer("thoughtgate");
    let batch_size = items.len();
    let mut batch_span = {
        let builder = tracer
            .span_builder(format!("jsonrpc.batch[{batch_size}]"))
            .with_kind(SpanKind::Server)
            .with_attributes(vec![opentelemetry::KeyValue::new(
                "mcp.batch.size",
                batch_size as i64,
            )]);
        match parent_context {
            Some(ctx) => builder.start_with_context(&tracer, ctx),
            None => builder.start(&tracer),
        }
    };

    // Process batch items concurrently using buffer_unordered.
    // JSON-RPC batch spec allows responses in any order (matched by id).
    // EC-MCP-006: Handle mixed valid/invalid items
    let batch_concurrency = items.len().min(16); // Cap concurrent items

    let responses: Vec<Option<JsonRpcResponse>> = stream::iter(items)
        .map(|item| async move {
            match item {
                BatchItem::Invalid { id, error } => {
                    // EC-MCP-006: Include error response for invalid items
                    let correlation_id =
                        thoughtgate_core::transport::jsonrpc::fast_correlation_id().to_string();
                    Some(JsonRpcResponse::error(
                        id,
                        error.to_jsonrpc_error(&correlation_id),
                    ))
                }
                BatchItem::Valid(request) => {
                    let is_notification = request.is_notification();
                    let id = request.id.clone();
                    let correlation_id = request.correlation_id.to_string();

                    // Route through shared routing logic
                    let result = super::route_request(state, request).await;

                    // F-007.4: Notifications don't produce response entries
                    if is_notification {
                        if let Err(e) = result {
                            error!(
                                correlation_id = %correlation_id,
                                error = %e,
                                "Notification in batch failed"
                            );
                        }
                        return None;
                    }

                    let response = match result {
                        Ok(r) => r,
                        Err(e) => JsonRpcResponse::error(id, e.to_jsonrpc_error(&correlation_id)),
                    };
                    Some(response)
                }
            }
        })
        .buffer_unordered(batch_concurrency)
        .collect()
        .await;

    // Filter out None entries (notifications)
    let responses: Vec<JsonRpcResponse> = responses.into_iter().flatten().collect();

    // Record error count on the batch span.
    let error_count = responses.iter().filter(|r| r.error.is_some()).count();
    if error_count > 0 {
        batch_span.set_attribute(opentelemetry::KeyValue::new(
            "mcp.batch.error_count",
            error_count as i64,
        ));
    }
    batch_span.end();

    // Return batch response
    if responses.is_empty() {
        // All were notifications — per JSON-RPC 2.0 §6: "The client MUST NOT
        // expect the server to return any Response for a Batch that only
        // contains Notification objects." Return 204 No Content.
        return (StatusCode::NO_CONTENT, Bytes::new());
    }

    // Serialize batch response
    match serde_json::to_vec(&responses) {
        Ok(bytes) => (StatusCode::OK, Bytes::from(bytes)),
        Err(e) => {
            error!(error = %e, "Failed to serialize batch response");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Bytes::from_static(
                    br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error: failed to serialize response"}}"#,
                ),
            )
        }
    }
}
