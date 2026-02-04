//! Integration tests for W3C Trace Context propagation.
//!
//! Tests that verify:
//! 1. Inbound traceparent/tracestate headers are extracted and used as parent context
//! 2. MCP spans become children of the caller's trace when traceparent is present
//! 3. Outbound requests to upstream include traceparent/tracestate headers
//!
//! Implements: REQ-OBS-002 §7.1, §7.2

use axum::{Router, body::Bytes, extract::State, http::HeaderMap, routing::post};
use opentelemetry::trace::{SpanId, TraceContextExt, TraceFlags, TraceId, TracerProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use serial_test::serial;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use uuid::Uuid;

use thoughtgate_core::transport::UpstreamForwarder;

/// Captured headers from upstream request.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // body is captured for debugging but not always read
struct CapturedRequest {
    headers: Vec<(String, String)>,
    body: Option<Value>,
}

/// State for the trace-capturing mock upstream.
#[derive(Debug, Default)]
struct TraceCaptureState {
    captured: RwLock<Vec<CapturedRequest>>,
}

/// Mock upstream that captures trace headers from inbound requests.
async fn trace_capturing_handler(
    State(state): State<Arc<TraceCaptureState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (axum::http::StatusCode, String) {
    // Capture all headers
    let captured_headers: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    // Parse body
    let body_json: Option<Value> = serde_json::from_slice(&body).ok();

    // Store captured request
    {
        let mut captured = state.captured.write().await;
        captured.push(CapturedRequest {
            headers: captured_headers,
            body: body_json.clone(),
        });
    }

    // Return a valid JSON-RPC response
    let id = body_json
        .as_ref()
        .and_then(|b| b.get("id"))
        .cloned()
        .unwrap_or(json!(null));

    (
        axum::http::StatusCode::OK,
        json!({
            "jsonrpc": "2.0",
            "result": {"status": "ok"},
            "id": id
        })
        .to_string(),
    )
}

/// Start a trace-capturing mock upstream.
async fn start_trace_capture_upstream() -> (SocketAddr, Arc<TraceCaptureState>) {
    let state = Arc::new(TraceCaptureState::default());
    let state_clone = state.clone();

    let app = Router::new()
        .route("/mcp/v1", post(trace_capturing_handler))
        .with_state(state_clone);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server time to start
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    (addr, state)
}

#[tokio::test]
async fn test_trace_context_injected_to_upstream() {
    // Start trace-capturing mock upstream
    let (upstream_addr, capture_state) = start_trace_capture_upstream().await;

    // Create upstream client
    let upstream_config = thoughtgate_core::transport::upstream::UpstreamConfig {
        base_url: format!("http://{}", upstream_addr),
        ..Default::default()
    };
    let upstream_client =
        thoughtgate_core::transport::upstream::UpstreamClient::new(upstream_config)
            .expect("Failed to create upstream client");

    // Create a span and attach it to context (simulating what mcp_handler does)
    let provider = SdkTracerProvider::builder().build();
    let tracer = provider.tracer("test");

    use opentelemetry::trace::Tracer;
    let span = tracer.start("test-mcp-span");
    let span_context = opentelemetry::trace::Span::span_context(&span).clone();
    let cx = opentelemetry::Context::current_with_span(span);
    let _guard = cx.attach();

    // Create a test request
    let request = thoughtgate_core::transport::jsonrpc::McpRequest {
        method: "tools/call".to_string(),
        params: Some(Arc::new(json!({
            "name": "test_tool",
            "arguments": {}
        }))),
        id: Some(thoughtgate_core::transport::jsonrpc::JsonRpcId::Number(1)),
        correlation_id: Uuid::new_v4(),
        task_metadata: None,
        received_at: Instant::now(),
    };

    // Forward request to upstream
    let _result = upstream_client.forward(&request).await;

    // Give request time to be captured
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Verify captured headers
    let captured = capture_state.captured.read().await;
    assert!(
        !captured.is_empty(),
        "Should have captured at least one request"
    );

    let captured_req = &captured[0];

    // Find traceparent header
    let traceparent = captured_req
        .headers
        .iter()
        .find(|(k, _)| k == "traceparent")
        .map(|(_, v)| v.as_str());

    assert!(
        traceparent.is_some(),
        "Upstream request should have traceparent header. Headers: {:?}",
        captured_req.headers
    );

    let traceparent_value = traceparent.unwrap();

    // Verify traceparent format: 00-{trace_id}-{span_id}-{flags}
    let parts: Vec<&str> = traceparent_value.split('-').collect();
    assert_eq!(parts.len(), 4, "traceparent should have 4 parts");
    assert_eq!(parts[0], "00", "version should be 00");
    assert_eq!(parts[1].len(), 32, "trace_id should be 32 hex chars");
    assert_eq!(parts[2].len(), 16, "span_id should be 16 hex chars");
    assert_eq!(parts[3].len(), 2, "flags should be 2 hex chars");

    // Verify trace_id matches the span we created
    assert_eq!(
        parts[1],
        format!("{}", span_context.trace_id()),
        "traceparent trace_id should match our span's trace_id"
    );

    // Find tracestate header
    let tracestate = captured_req
        .headers
        .iter()
        .find(|(k, _)| k == "tracestate")
        .map(|(_, v)| v.as_str());

    assert!(
        tracestate.is_some(),
        "Upstream request should have tracestate header"
    );

    let tracestate_value = tracestate.unwrap();
    assert!(
        tracestate_value.contains("thoughtgate="),
        "tracestate should contain thoughtgate entry"
    );
}

#[tokio::test]
async fn test_extract_context_from_headers() {
    // Test the extraction function directly
    let mut headers = http::header::HeaderMap::new();
    headers.insert(
        "traceparent",
        http::header::HeaderValue::from_static(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        ),
    );

    let context = thoughtgate_core::telemetry::extract_context_from_headers(&headers);

    // The context should have the remote span context
    let span = context.span();
    let span_context = span.span_context();

    assert!(
        span_context.is_valid(),
        "Extracted context should have valid span context"
    );
    assert!(
        span_context.is_remote(),
        "Extracted context should be marked as remote"
    );

    // Verify the trace and span IDs match
    assert_eq!(
        span_context.trace_id(),
        TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap()
    );
    assert_eq!(
        span_context.span_id(),
        SpanId::from_hex("00f067aa0ba902b7").unwrap()
    );
    assert_eq!(span_context.trace_flags(), TraceFlags::SAMPLED);
}

#[tokio::test]
async fn test_extract_context_missing_traceparent() {
    // When no traceparent is present, context should not have remote span
    let headers = http::header::HeaderMap::new();

    let context = thoughtgate_core::telemetry::extract_context_from_headers(&headers);

    let span = context.span();
    let span_context = span.span_context();

    // Should not be a valid remote context
    assert!(
        !span_context.is_remote() || !span_context.is_valid(),
        "Missing traceparent should result in no remote context"
    );
}

#[tokio::test]
async fn test_inject_context_into_headers() {
    // Create a span context to inject
    let provider = SdkTracerProvider::builder().build();
    let tracer = provider.tracer("test");

    use opentelemetry::trace::Tracer;
    let span = tracer.start("test-span");
    let cx = opentelemetry::Context::current_with_span(span);

    let mut headers = http::header::HeaderMap::new();
    thoughtgate_core::telemetry::inject_context_into_headers(
        &cx,
        &mut headers,
        Some("session-123"),
    );

    // Verify headers were added
    assert!(
        headers.contains_key("traceparent"),
        "Should have traceparent"
    );
    assert!(headers.contains_key("tracestate"), "Should have tracestate");

    let tracestate = headers.get("tracestate").unwrap().to_str().unwrap();
    assert!(
        tracestate.contains("thoughtgate=session-123"),
        "tracestate should contain session ID"
    );
}

#[tokio::test]
#[serial]
async fn test_mcp_span_inherits_parent_trace_id() {
    use opentelemetry::trace::TraceState;
    use opentelemetry_sdk::trace::InMemorySpanExporter;

    // Set up in-memory exporter
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());

    // Create a parent context (simulating extracted from headers)
    let parent_trace_id = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap();
    let parent_span_id = SpanId::from_hex("00f067aa0ba902b7").unwrap();
    let parent_ctx = opentelemetry::trace::SpanContext::new(
        parent_trace_id,
        parent_span_id,
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );
    let parent_context = opentelemetry::Context::new().with_remote_span_context(parent_ctx);

    // Start MCP span with parent context
    let span_data = thoughtgate_core::telemetry::McpSpanData {
        method: "tools/call",
        message_type: thoughtgate_core::telemetry::McpMessageType::Request,
        message_id: Some("123".to_string()),
        correlation_id: "test-inherit",
        tool_name: Some("test_tool"),
        parent_context: Some(&parent_context),
    };

    let mut span = thoughtgate_core::telemetry::start_mcp_span(&span_data);
    thoughtgate_core::telemetry::finish_mcp_span(&mut span, false, None, None);
    drop(span);

    // Flush and get spans
    provider.force_flush().expect("flush should succeed");
    let spans = exporter.get_finished_spans().expect("should get spans");
    assert_eq!(spans.len(), 1);

    let finished = &spans[0];

    // MCP span should inherit the parent's trace_id
    assert_eq!(
        finished.span_context.trace_id(),
        parent_trace_id,
        "MCP span should inherit parent trace_id"
    );

    // MCP span should have the parent_span_id set to the remote parent
    assert_eq!(
        finished.parent_span_id, parent_span_id,
        "MCP span should have parent_span_id pointing to remote parent"
    );
}

#[tokio::test]
#[serial]
async fn test_new_trace_when_no_parent() {
    use opentelemetry_sdk::trace::InMemorySpanExporter;

    // Set up in-memory exporter
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());

    // Start MCP span without parent context (no traceparent header)
    let span_data = thoughtgate_core::telemetry::McpSpanData {
        method: "tools/call",
        message_type: thoughtgate_core::telemetry::McpMessageType::Request,
        message_id: Some("456".to_string()),
        correlation_id: "test-no-parent",
        tool_name: None,
        parent_context: None,
    };

    let mut span = thoughtgate_core::telemetry::start_mcp_span(&span_data);
    thoughtgate_core::telemetry::finish_mcp_span(&mut span, false, None, None);
    drop(span);

    // Flush and get spans
    provider.force_flush().expect("flush should succeed");
    let spans = exporter.get_finished_spans().expect("should get spans");
    assert_eq!(spans.len(), 1);

    let finished = &spans[0];

    // Span should have a valid trace_id (auto-generated)
    assert_ne!(
        finished.span_context.trace_id(),
        TraceId::INVALID,
        "Root span should have valid trace_id"
    );

    // Span should have no parent (is root)
    assert_eq!(
        finished.parent_span_id,
        SpanId::INVALID,
        "Root span should have no parent_span_id"
    );
}
