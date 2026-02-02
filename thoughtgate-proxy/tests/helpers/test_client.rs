//! JSON-RPC test client for integration testing.
//!
//! Provides a high-level client for making MCP requests to ThoughtGate.
//!
//! Note: Some methods are provided for future test expansion and may not
//! be used yet. They are marked with `#[allow(dead_code)]`.

#![allow(dead_code)]

use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;

/// Test client for making JSON-RPC requests to ThoughtGate.
#[derive(Debug, Clone)]
pub struct TestClient {
    base_url: String,
    client: Client,
}

/// Response from a JSON-RPC call.
#[derive(Debug, Clone)]
pub struct JsonRpcResponse {
    pub id: Value,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC error object.
#[derive(Debug, Clone)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

impl TestClient {
    /// Create a new test client pointing to the given base URL.
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
        }
    }

    /// Make a raw JSON-RPC request.
    pub async fn jsonrpc(&self, method: &str, params: Value, id: i64) -> JsonRpcResponse {
        let request = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id
        });

        let response = self
            .client
            .post(format!("{}/mcp/v1", self.base_url))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .expect("Failed to send request");

        let body: Value = response.json().await.expect("Failed to parse response");

        JsonRpcResponse {
            id: body["id"].clone(),
            result: body.get("result").cloned(),
            error: body.get("error").map(|e| JsonRpcError {
                code: e["code"].as_i64().unwrap_or(0) as i32,
                message: e["message"].as_str().unwrap_or("").to_string(),
                data: e.get("data").cloned(),
            }),
        }
    }

    /// Call a tool.
    pub async fn tools_call(&self, name: &str, arguments: Value) -> JsonRpcResponse {
        self.jsonrpc(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments
            }),
            1,
        )
        .await
    }

    /// Call a tool with task metadata (async mode).
    pub async fn tools_call_async(&self, name: &str, arguments: Value) -> JsonRpcResponse {
        self.jsonrpc(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
                "_meta": {
                    "task": true
                }
            }),
            1,
        )
        .await
    }

    /// List available tools.
    pub async fn tools_list(&self) -> JsonRpcResponse {
        self.jsonrpc("tools/list", json!({}), 1).await
    }

    /// Get task status.
    pub async fn tasks_get(&self, task_id: &str) -> JsonRpcResponse {
        self.jsonrpc(
            "tasks/get",
            json!({
                "task_id": task_id
            }),
            1,
        )
        .await
    }

    /// Get task result (blocks until terminal).
    pub async fn tasks_result(&self, task_id: &str) -> JsonRpcResponse {
        self.jsonrpc(
            "tasks/result",
            json!({
                "task_id": task_id
            }),
            1,
        )
        .await
    }

    /// List tasks for the current principal.
    pub async fn tasks_list(&self, cursor: Option<&str>) -> JsonRpcResponse {
        let mut params = json!({});
        if let Some(c) = cursor {
            params["cursor"] = json!(c);
        }
        self.jsonrpc("tasks/list", params, 1).await
    }

    /// Cancel a task.
    pub async fn tasks_cancel(&self, task_id: &str) -> JsonRpcResponse {
        self.jsonrpc(
            "tasks/cancel",
            json!({
                "task_id": task_id
            }),
            1,
        )
        .await
    }

    /// Initialize the MCP session.
    pub async fn initialize(&self) -> JsonRpcResponse {
        self.jsonrpc(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            }),
            1,
        )
        .await
    }
}

impl JsonRpcResponse {
    /// Check if the response is successful (has result, no error).
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.result.is_some() && self.error.is_none()
    }

    /// Check if the response is an error.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    /// Get the error code if present.
    #[must_use]
    pub fn error_code(&self) -> Option<i32> {
        self.error.as_ref().map(|e| e.code)
    }

    /// Get a task ID from the result if present.
    #[must_use]
    pub fn task_id(&self) -> Option<String> {
        self.result
            .as_ref()
            .and_then(|r| r.get("task_id"))
            .and_then(|t| t.as_str())
            .map(String::from)
    }

    /// Get the task status from a tasks/get response.
    #[must_use]
    pub fn task_status(&self) -> Option<String> {
        self.result
            .as_ref()
            .and_then(|r| r.get("status"))
            .and_then(|s| s.as_str())
            .map(String::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jsonrpc_response_is_success() {
        let response = JsonRpcResponse {
            id: json!(1),
            result: Some(json!({"ok": true})),
            error: None,
        };
        assert!(response.is_success());
        assert!(!response.is_error());
    }

    #[test]
    fn test_jsonrpc_response_is_error() {
        let response = JsonRpcResponse {
            id: json!(1),
            result: None,
            error: Some(JsonRpcError {
                code: -32600,
                message: "Invalid Request".to_string(),
                data: None,
            }),
        };
        assert!(!response.is_success());
        assert!(response.is_error());
        assert_eq!(response.error_code(), Some(-32600));
    }

    #[test]
    fn test_task_id_extraction() {
        let response = JsonRpcResponse {
            id: json!(1),
            result: Some(json!({
                "task_id": "tg_abc123",
                "status": "working"
            })),
            error: None,
        };
        assert_eq!(response.task_id(), Some("tg_abc123".to_string()));
        assert_eq!(response.task_status(), Some("working".to_string()));
    }
}
