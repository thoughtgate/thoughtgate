//! Mock MCP upstream server for integration testing.
//!
//! Provides a configurable mock server that responds to JSON-RPC requests
//! with preconfigured responses, delays, and failures.
//!
//! Note: Some methods are provided for future test expansion and may not
//! be used yet. They are marked with `#[allow(dead_code)]`.

#![allow(dead_code)]

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

/// Mock MCP upstream server for testing.
///
/// Allows configuring:
/// - Tool definitions (for tools/list)
/// - Responses per tool (for tools/call)
/// - Delays per tool (for timeout testing)
/// - Errors per tool (for error handling testing)
#[derive(Debug, Clone)]
pub struct MockUpstream {
    tools: Vec<ToolDefinition>,
    responses: HashMap<String, Value>,
    delays: HashMap<String, Duration>,
    errors: HashMap<String, MockError>,
}

/// A mock tool definition.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// A mock error response.
#[derive(Debug, Clone)]
pub struct MockError {
    pub code: i32,
    pub message: String,
}

/// Shared state for the mock server.
#[derive(Debug)]
struct MockState {
    tools: Vec<ToolDefinition>,
    responses: HashMap<String, Value>,
    delays: HashMap<String, Duration>,
    errors: HashMap<String, MockError>,
    request_count: RwLock<u32>,
    last_request: RwLock<Option<Value>>,
}

impl MockUpstream {
    /// Create a new mock upstream with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            responses: HashMap::new(),
            delays: HashMap::new(),
            errors: HashMap::new(),
        }
    }

    /// Add a tool with a successful response.
    #[must_use]
    pub fn with_tool(mut self, name: &str, description: &str, response: Value) -> Self {
        self.tools.push(ToolDefinition {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: json!({"type": "object"}),
        });
        self.responses.insert(name.to_string(), response);
        self
    }

    /// Add a delay for a specific tool.
    #[must_use]
    pub fn with_delay(mut self, name: &str, delay: Duration) -> Self {
        self.delays.insert(name.to_string(), delay);
        self
    }

    /// Add an error response for a specific tool.
    #[must_use]
    pub fn with_error(mut self, name: &str, code: i32, message: &str) -> Self {
        self.errors.insert(
            name.to_string(),
            MockError {
                code,
                message: message.to_string(),
            },
        );
        self
    }

    /// Start the mock server and return its address and handle.
    pub async fn start(self) -> (SocketAddr, MockServerHandle) {
        let state = Arc::new(MockState {
            tools: self.tools,
            responses: self.responses,
            delays: self.delays,
            errors: self.errors,
            request_count: RwLock::new(0),
            last_request: RwLock::new(None),
        });

        let app = Router::new()
            .route("/", post(handle_jsonrpc))
            .route("/mcp/v1", post(handle_jsonrpc))
            .with_state(state.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        (
            addr,
            MockServerHandle {
                state,
                _handle: handle,
            },
        )
    }
}

impl Default for MockUpstream {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle to the running mock server.
pub struct MockServerHandle {
    state: Arc<MockState>,
    _handle: JoinHandle<()>,
}

impl MockServerHandle {
    /// Get the number of requests received.
    pub async fn request_count(&self) -> u32 {
        *self.state.request_count.read().await
    }

    /// Get the last request received.
    pub async fn last_request(&self) -> Option<Value> {
        self.state.last_request.read().await.clone()
    }
}

/// Handle JSON-RPC requests to the mock upstream.
async fn handle_jsonrpc(
    State(state): State<Arc<MockState>>,
    _headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    // Parse request
    let request: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::OK,
                json!({
                    "jsonrpc": "2.0",
                    "error": {
                        "code": -32700,
                        "message": format!("Parse error: {}", e)
                    },
                    "id": null
                })
                .to_string(),
            );
        }
    };

    // Update state
    {
        let mut count = state.request_count.write().await;
        *count += 1;
    }
    {
        let mut last = state.last_request.write().await;
        *last = Some(request.clone());
    }

    let method = request["method"].as_str().unwrap_or("");
    let id = request["id"].clone();

    // Route by method
    let result = match method {
        "tools/list" => {
            let tools: Vec<Value> = state
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema
                    })
                })
                .collect();
            json!({ "tools": tools })
        }
        "tools/call" => {
            let tool_name = request["params"]["name"].as_str().unwrap_or("");

            // Check for configured delay
            if let Some(delay) = state.delays.get(tool_name) {
                tokio::time::sleep(*delay).await;
            }

            // Check for configured error
            if let Some(error) = state.errors.get(tool_name) {
                return (
                    StatusCode::OK,
                    json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": error.code,
                            "message": error.message
                        },
                        "id": id
                    })
                    .to_string(),
                );
            }

            // Return configured response or default
            let response = state
                .responses
                .get(tool_name)
                .cloned()
                .unwrap_or_else(|| json!({"result": "ok"}));

            json!({ "content": [{ "type": "text", "text": response.to_string() }] })
        }
        "initialize" => {
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "mock-upstream",
                    "version": "1.0.0"
                }
            })
        }
        _ => {
            return (
                StatusCode::OK,
                json!({
                    "jsonrpc": "2.0",
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {}", method)
                    },
                    "id": id
                })
                .to_string(),
            );
        }
    };

    (
        StatusCode::OK,
        json!({
            "jsonrpc": "2.0",
            "result": result,
            "id": id
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_upstream_tools_list() {
        let mock = MockUpstream::new()
            .with_tool("read_file", "Read a file", json!({"success": true}))
            .with_tool("delete_file", "Delete a file", json!({"deleted": true}));

        let (addr, handle) = mock.start().await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{}/mcp/v1", addr))
            .json(&json!({
                "jsonrpc": "2.0",
                "method": "tools/list",
                "id": 1
            }))
            .send()
            .await
            .unwrap();

        let body: Value = response.json().await.unwrap();
        assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 2);
        assert_eq!(handle.request_count().await, 1);
    }

    #[tokio::test]
    async fn test_mock_upstream_tools_call() {
        let mock = MockUpstream::new().with_tool("echo", "Echo back", json!({"echoed": "hello"}));

        let (addr, _handle) = mock.start().await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{}/mcp/v1", addr))
            .json(&json!({
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": {
                    "name": "echo",
                    "arguments": {"input": "hello"}
                },
                "id": 1
            }))
            .send()
            .await
            .unwrap();

        let body: Value = response.json().await.unwrap();
        assert!(
            body["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("echoed")
        );
    }

    #[tokio::test]
    async fn test_mock_upstream_with_error() {
        let mock = MockUpstream::new()
            .with_tool("failing", "Always fails", json!({}))
            .with_error("failing", -32000, "Simulated failure");

        let (addr, _handle) = mock.start().await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{}/mcp/v1", addr))
            .json(&json!({
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": {
                    "name": "failing",
                    "arguments": {}
                },
                "id": 1
            }))
            .send()
            .await
            .unwrap();

        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["code"], -32000);
        assert_eq!(body["error"]["message"], "Simulated failure");
    }
}
