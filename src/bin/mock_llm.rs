use axum::{
    routing::{post, get},
    Router,
    response::sse::{Event, Sse},
    extract::{State, Json},
};
use futures_util::stream::{self, Stream};
use std::{convert::Infallible, net::SocketAddr, time::Duration, sync::Arc};
use tokio::time::sleep;
use tokio::sync::Mutex;

/// Shared state for tracking all incoming requests (spy pattern)
type RequestHistory = Arc<Mutex<Vec<serde_json::Value>>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    
    // Initialize request history
    let history: RequestHistory = Arc::new(Mutex::new(Vec::new()));
    
    // Use standard OpenAI endpoint for compatibility
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_chat))
        .route("/_admin/history", get(admin_history))
        .route("/health", get(health_check))
        .with_state(history);
    
    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    tracing::info!("🤖 ThoughtGate Mock LLM listening on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await
        .map_err(|e| {
            tracing::error!("Failed to bind to {}: {}", addr, e);
            e
        })?;
    
    axum::serve(listener, app).await
        .map_err(|e| {
            tracing::error!("Server error: {}", e);
            e
        })?;
    
    Ok(())
}

/// Mock chat endpoint that captures requests and streams responses
async fn mock_chat(
    State(history): State<RequestHistory>,
    Json(payload): Json<serde_json::Value>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    tracing::info!("Received request, capturing payload...");
    
    // Capture the request in history (spy pattern)
    {
        let mut history = history.lock().await;
        history.push(payload.clone());
        tracing::debug!("Captured request #{}: {:?}", history.len(), payload);
    }
    
    // 1. Simulate "Think Time" (500ms TTFB)
    sleep(Duration::from_millis(500)).await;

    // 2. Simulate "Token Streaming" (50 tokens, 10ms intervals)
    let stream = stream::unfold(0, |i| async move {
        if i >= 50 { return None; }
        sleep(Duration::from_millis(10)).await;
        let data = format!("token_{}", i);
        Some((Ok(Event::default().data(data)), i + 1))
    });

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

/// Admin endpoint to retrieve captured request history
async fn admin_history(State(history): State<RequestHistory>) -> Json<Vec<serde_json::Value>> {
    let history = history.lock().await;
    tracing::info!("Admin history requested, returning {} captured requests", history.len());
    Json(history.clone())
}

/// Health check endpoint for readiness probes
async fn health_check() -> &'static str {
    "OK"
}
