//! Mock MCP Server for benchmarking ThoughtGate proxy.
//!
//! A minimal MCP JSON-RPC server that responds to requests with minimal latency.
//! Used for benchmarking proxy overhead without upstream processing delays.
//!
//! # Environment Variables
//!
//! - `MOCK_MCP_PORT`: Listen port (default: 9999)
//! - `MOCK_MCP_DELAY_MS`: Response delay in milliseconds (default: 0)
//!
//! # Usage
//!
//! ```bash
//! # Start with defaults (port 9999, no delay)
//! cargo run --bin mock_mcp
//!
//! # Start with custom port and delay
//! MOCK_MCP_PORT=8888 MOCK_MCP_DELAY_MS=10 cargo run --bin mock_mcp
//!
//! # Test with curl
//! curl -X POST http://localhost:9999/mcp/v1 \
//!   -H "Content-Type: application/json" \
//!   -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"test"}}'
//! ```

use axum::{
    Json, Router,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::sleep;

/// JSON-RPC 2.0 request structure.
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Fields are parsed for validation but not all used in response
struct JsonRpcRequest {
    jsonrpc: String,
    id: serde_json::Value,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// JSON-RPC 2.0 response structure.
#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: serde_json::Value,
    result: serde_json::Value,
}

/// Handle MCP JSON-RPC requests.
///
/// Returns a mock response with configurable delay for benchmarking.
async fn handle_mcp(Json(req): Json<JsonRpcRequest>) -> Json<JsonRpcResponse> {
    // Apply configurable delay (default: 0ms for benchmarking)
    let delay_ms: u64 = std::env::var("MOCK_MCP_DELAY_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    if delay_ms > 0 {
        sleep(Duration::from_millis(delay_ms)).await;
    }

    // Return a valid MCP-style response
    Json(JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: req.id,
        result: serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("mock response for method: {}", req.method)
            }]
        }),
    })
}

/// Health check endpoint.
async fn health() -> &'static str {
    "OK"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse port from environment
    let port: u16 = std::env::var("MOCK_MCP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9999);

    // Build router with MCP and health endpoints
    let app = Router::new()
        .route("/mcp/v1", post(handle_mcp))
        .route("/", post(handle_mcp)) // Root path also valid for MCP
        .route("/health", get(health));

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("Mock MCP server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
