//! Governance Smoke Tests - Full HTTP stack with YAML configuration.
//!
//! These tests spawn the actual proxy binary as a subprocess with real YAML config,
//! validating the complete flow: HTTP → Proxy → Governance → Upstream → Response.
//!
//! Unlike handler-level tests that call `McpHandler::handle()` directly, smoke tests
//! verify real-world behavior including:
//! - Binary startup and config loading
//! - HTTP server binding
//! - Admin endpoint availability
//! - Full request/response cycle through TCP
//!
//! # Traceability
//! - Implements: REQ-CORE-003 (4-Gate Decision Flow)
//! - Implements: REQ-GOV-002 (Governance Rules)
//! - Implements: REQ-CORE-004 (Error Codes: -32014 for governance deny)

mod helpers;

use helpers::MockUpstream;
use serde_json::{Value, json};
use std::io::Write;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::sleep;

/// Error code for governance rule denied (Gate 2).
const GOVERNANCE_RULE_DENIED: i32 = -32014;

/// Test harness for smoke tests - manages proxy subprocess and mock upstream.
struct TestHarness {
    _mock_handle: helpers::MockServerHandle,
    proxy: Child,
    proxy_port: u16,
    admin_port: u16,
    _config_file: NamedTempFile,
    client: reqwest::Client,
}

impl TestHarness {
    /// Create a new test harness with the given YAML config.
    ///
    /// 1. Starts MockUpstream on a random port
    /// 2. Writes config to temp file (substituting mock port)
    /// 3. Spawns proxy binary as subprocess
    /// 4. Waits for /health endpoint to be ready
    async fn new(config_yaml: &str, mock: MockUpstream) -> Self {
        // Start mock upstream
        let (mock_addr, mock_handle) = mock.start().await;
        let mock_url = format!("http://{}", mock_addr);

        // Find available ports for proxy
        let proxy_port = find_available_port();
        let admin_port = find_available_port();

        // Write config to temp file with mock URL substituted
        let config_content = config_yaml.replace("${MOCK_URL}", &mock_url);
        let mut config_file = NamedTempFile::new().expect("Failed to create temp config file");
        config_file
            .write_all(config_content.as_bytes())
            .expect("Failed to write config");
        config_file.flush().expect("Failed to flush config");

        // Find the proxy binary
        let binary_path = find_proxy_binary();

        // Spawn proxy subprocess
        // Note: THOUGHTGATE_UPSTREAM is the env var for UpstreamConfig::from_env()
        let mut proxy = Command::new(&binary_path)
            .env("THOUGHTGATE_CONFIG", config_file.path())
            .env("THOUGHTGATE_UPSTREAM", &mock_url)
            .env("THOUGHTGATE_OUTBOUND_PORT", proxy_port.to_string())
            .env("THOUGHTGATE_ADMIN_PORT", admin_port.to_string())
            .env("RUST_LOG", "warn") // Reduce noise in test output
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|e| panic!("Failed to spawn proxy binary at {:?}: {}", binary_path, e));

        // Capture stderr for debugging if startup fails
        let stderr = proxy.stderr.take().expect("Failed to capture stderr");
        let mut stderr_reader = BufReader::new(stderr).lines();

        // Spawn task to drain stderr (prevents blocking)
        tokio::spawn(async move {
            while let Ok(Some(line)) = stderr_reader.next_line().await {
                eprintln!("[proxy stderr] {}", line);
            }
        });

        // Wait for proxy to be ready (ready endpoint, not just health)
        // The /ready endpoint returns 200 only after lifecycle.mark_ready() is called
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("Failed to build HTTP client");

        let ready_url = format!("http://127.0.0.1:{}/ready", admin_port);
        let ready = wait_for_endpoint(&client, &ready_url, Duration::from_secs(15)).await;
        if !ready {
            panic!(
                "Proxy failed to become ready within timeout. Ready URL: {}",
                ready_url
            );
        }

        Self {
            _mock_handle: mock_handle,
            proxy,
            proxy_port,
            admin_port,
            _config_file: config_file,
            client,
        }
    }

    /// Send a JSON-RPC request to the proxy MCP endpoint.
    async fn post_mcp(&self, body: &Value) -> reqwest::Response {
        let url = format!("http://127.0.0.1:{}/mcp/v1", self.proxy_port);
        self.client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .expect("Failed to send MCP request")
    }

    /// Get the /health endpoint response.
    async fn get_health(&self) -> reqwest::Response {
        let url = format!("http://127.0.0.1:{}/health", self.admin_port);
        self.client
            .get(&url)
            .send()
            .await
            .expect("Failed to get /health")
    }

    /// Get the /ready endpoint response.
    async fn get_ready(&self) -> reqwest::Response {
        let url = format!("http://127.0.0.1:{}/ready", self.admin_port);
        self.client
            .get(&url)
            .send()
            .await
            .expect("Failed to get /ready")
    }

    /// Get the /metrics endpoint response.
    async fn get_metrics(&self) -> reqwest::Response {
        let url = format!("http://127.0.0.1:{}/metrics", self.admin_port);
        self.client
            .get(&url)
            .send()
            .await
            .expect("Failed to get /metrics")
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        // Kill proxy process on cleanup
        // kill_on_drop is set, but we explicitly start kill to ensure cleanup
        let _ = self.proxy.start_kill();
    }
}

/// Find an available TCP port by binding to port 0.
fn find_available_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("Failed to bind to port 0");
    listener
        .local_addr()
        .expect("Failed to get local addr")
        .port()
}

/// Find the proxy binary in target directory.
fn find_proxy_binary() -> PathBuf {
    // Try release first, then debug
    let release_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/release/thoughtgate-proxy");

    if release_path.exists() {
        return release_path;
    }

    let debug_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/debug/thoughtgate-proxy");

    if debug_path.exists() {
        return debug_path;
    }

    panic!(
        "Could not find thoughtgate-proxy binary. Run 'cargo build' or 'cargo build --release' first.\n\
         Searched:\n  - {:?}\n  - {:?}",
        release_path, debug_path
    );
}

/// Wait for an HTTP endpoint to return 200.
async fn wait_for_endpoint(client: &reqwest::Client, url: &str, max_wait: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < max_wait {
        if let Ok(resp) = client.get(url).send().await {
            if resp.status().is_success() {
                return true;
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Config template for governance smoke tests.
/// Uses deny rules for delete_* and *_unsafe patterns.
fn governance_config_yaml() -> &'static str {
    r#"
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${MOCK_URL}

governance:
  defaults:
    action: forward

  rules:
    - match: "delete_*"
      action: deny
    - match: "*_unsafe"
      action: deny
"#
}

// ============================================================================
// Smoke Tests
// ============================================================================

/// Test: Forward action allows tools through to upstream.
///
/// Verifies: REQ-CORE-003 (Gate 2 forward action)
#[tokio::test]
async fn test_governance_forward_allowed_tools() {
    let mock = MockUpstream::new()
        .with_tool("get_weather", "Get weather info", json!({"temp": 72}))
        .with_tool(
            "list_files",
            "List files",
            json!({"files": ["a.txt", "b.txt"]}),
        );

    let harness = TestHarness::new(governance_config_yaml(), mock).await;

    // Test get_weather - should forward to upstream
    let response = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "get_weather",
                "arguments": {"city": "Seattle"}
            },
            "id": 1
        }))
        .await;

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.expect("Failed to parse response");
    assert!(
        body.get("result").is_some(),
        "Should have result: {:?}",
        body
    );
    assert!(
        body.get("error").is_none(),
        "Should not have error: {:?}",
        body
    );

    // Test list_files - should also forward
    let response = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "list_files",
                "arguments": {}
            },
            "id": 2
        }))
        .await;

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.expect("Failed to parse response");
    assert!(body.get("result").is_some(), "Should have result");
}

/// Test: Deny action blocks tools matching delete_* pattern.
///
/// Verifies: REQ-CORE-004 (-32014 Governance Rule Denied)
#[tokio::test]
async fn test_governance_deny_delete_pattern() {
    let mock = MockUpstream::new()
        .with_tool("delete_user", "Delete a user", json!({"deleted": true}))
        .with_tool("delete_file", "Delete a file", json!({"deleted": true}));

    let harness = TestHarness::new(governance_config_yaml(), mock).await;

    // Test delete_user - should be denied by governance
    let response = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "delete_user",
                "arguments": {"user_id": "123"}
            },
            "id": 1
        }))
        .await;

    assert_eq!(response.status(), 200); // JSON-RPC errors still return 200
    let body: Value = response.json().await.expect("Failed to parse response");
    assert!(body.get("error").is_some(), "Should have error: {:?}", body);

    let error = body.get("error").unwrap();
    assert_eq!(
        error["code"].as_i64(),
        Some(GOVERNANCE_RULE_DENIED as i64),
        "Should be governance denied error: {:?}",
        error
    );

    // Test delete_file - should also be denied
    let response = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "delete_file",
                "arguments": {"path": "/tmp/test.txt"}
            },
            "id": 2
        }))
        .await;

    let body: Value = response.json().await.expect("Failed to parse response");
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(GOVERNANCE_RULE_DENIED as i64)
    );
}

/// Test: Deny action blocks tools matching *_unsafe pattern.
///
/// Verifies: REQ-GOV-002 (Pattern Matching)
#[tokio::test]
async fn test_governance_deny_unsafe_pattern() {
    let mock = MockUpstream::new()
        .with_tool(
            "run_unsafe",
            "Run unsafe command",
            json!({"output": "done"}),
        )
        .with_tool("exec_unsafe", "Execute unsafe", json!({"output": "done"}));

    let harness = TestHarness::new(governance_config_yaml(), mock).await;

    // Test run_unsafe - should be denied
    let response = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "run_unsafe",
                "arguments": {}
            },
            "id": 1
        }))
        .await;

    let body: Value = response.json().await.expect("Failed to parse response");
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(GOVERNANCE_RULE_DENIED as i64),
        "run_unsafe should be denied: {:?}",
        body
    );

    // Test exec_unsafe - should also be denied
    let response = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "exec_unsafe",
                "arguments": {}
            },
            "id": 2
        }))
        .await;

    let body: Value = response.json().await.expect("Failed to parse response");
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(GOVERNANCE_RULE_DENIED as i64),
        "exec_unsafe should be denied"
    );
}

/// Test: Non tools/call methods bypass governance rules.
///
/// Methods like tools/list and initialize go directly to upstream
/// without passing through Gate 2 governance rules.
///
/// Verifies: REQ-CORE-003 (Only tools/call goes through Gate 2)
#[tokio::test]
async fn test_governance_non_tool_methods_bypass() {
    let mock = MockUpstream::new()
        .with_tool("safe_tool", "A safe tool", json!({"ok": true}))
        .with_tool("delete_all", "Delete everything", json!({"deleted": true}));

    let harness = TestHarness::new(governance_config_yaml(), mock).await;

    // tools/list should bypass governance and return all tools
    let response = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1
        }))
        .await;

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.expect("Failed to parse response");
    assert!(body.get("result").is_some(), "tools/list should succeed");

    let tools = &body["result"]["tools"];
    assert!(tools.is_array(), "Should have tools array");
    // Both tools should be listed (delete_all is listed, just can't be called)
    assert_eq!(tools.as_array().unwrap().len(), 2);

    // initialize should also bypass governance
    let response = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "test",
                    "version": "1.0"
                }
            },
            "id": 2
        }))
        .await;

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.expect("Failed to parse response");
    assert!(
        body.get("result").is_some(),
        "initialize should succeed: {:?}",
        body
    );
}

/// Test: Admin endpoints work correctly with governance config.
///
/// Verifies: REQ-CORE-005 (Health/Readiness probes)
#[tokio::test]
async fn test_governance_admin_endpoints() {
    let mock = MockUpstream::new().with_tool("test_tool", "Test", json!({}));

    let harness = TestHarness::new(governance_config_yaml(), mock).await;

    // Health endpoint
    let response = harness.get_health().await;
    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("Failed to read body");
    assert!(
        body.contains("OK") || body.contains("ok") || body.contains("healthy"),
        "Health should indicate OK: {}",
        body
    );

    // Ready endpoint
    let response = harness.get_ready().await;
    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("Failed to read body");
    assert!(
        body.contains("Ready") || body.contains("ready") || body.contains("ok"),
        "Ready should indicate ready: {}",
        body
    );

    // Metrics endpoint should also be available
    let response = harness.get_metrics().await;
    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("Failed to read body");
    // Metrics should contain at least some Prometheus format
    assert!(
        body.contains("# HELP") || body.contains("# TYPE") || body.contains("thoughtgate_"),
        "Metrics should contain Prometheus format: {}",
        body
    );
}

/// Test: Mixed allowed and denied tools in same session.
///
/// Verifies end-to-end governance with realistic workflow.
#[tokio::test]
async fn test_governance_mixed_allow_deny() {
    let mock = MockUpstream::new()
        .with_tool("read_file", "Read a file", json!({"content": "hello"}))
        .with_tool("write_file", "Write a file", json!({"written": true}))
        .with_tool("delete_file", "Delete a file", json!({"deleted": true}))
        .with_tool("run_unsafe", "Run unsafe op", json!({"result": "done"}));

    let harness = TestHarness::new(governance_config_yaml(), mock).await;

    // read_file - should succeed (no deny rule matches)
    let resp = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {"name": "read_file", "arguments": {"path": "/tmp/test"}},
            "id": 1
        }))
        .await;
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("result").is_some(), "read_file should succeed");

    // write_file - should succeed
    let resp = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {"name": "write_file", "arguments": {"path": "/tmp/test", "content": "hi"}},
            "id": 2
        }))
        .await;
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("result").is_some(), "write_file should succeed");

    // delete_file - should be denied (matches delete_*)
    let resp = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {"name": "delete_file", "arguments": {"path": "/tmp/test"}},
            "id": 3
        }))
        .await;
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(GOVERNANCE_RULE_DENIED as i64),
        "delete_file should be denied"
    );

    // run_unsafe - should be denied (matches *_unsafe)
    let resp = harness
        .post_mcp(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {"name": "run_unsafe", "arguments": {}},
            "id": 4
        }))
        .await;
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(GOVERNANCE_RULE_DENIED as i64),
        "run_unsafe should be denied"
    );
}
