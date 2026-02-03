//! Integration tests for the ThoughtGate CLI wrapper.
//!
//! Covers: round-trip message forwarding, malformed JSON skip, config
//! backup/restore lifecycle, and concurrent lock rejection.
//!
//! Implements verification plan: REQ-CORE-008 §9

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use thoughtgate::wrap::config_adapter::{
    ClaudeDesktopAdapter, ConfigAdapter, ConfigError, ShimOptions,
};
use thoughtgate::wrap::config_guard::ConfigGuard;
use thoughtgate_core::governance::service::{GovernanceServiceState, start_governance_service};
use thoughtgate_core::profile::Profile;

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Start a stub governance service (always returns Forward) on an ephemeral port.
async fn start_test_governance() -> (u16, Arc<GovernanceServiceState>) {
    let state = Arc::new(GovernanceServiceState::new());
    let (port, _handle) = start_governance_service(0, state.clone())
        .await
        .expect("failed to start governance service");
    (port, state)
}

/// Create a unique temp directory for test isolation.
fn temp_dir_unique(prefix: &str) -> PathBuf {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let dir =
        std::env::temp_dir().join(format!("tg-{prefix}-{}-{}", d.as_secs(), d.subsec_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write an echo MCP server script and return its path.
#[cfg(unix)]
fn write_echo_script(dir: &Path) -> PathBuf {
    let script_path = dir.join("echo_server.sh");
    std::fs::write(
        &script_path,
        "#!/bin/bash\nwhile IFS= read -r line; do echo \"$line\"; done\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    script_path
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 1: Full round-trip message forwarding
// ─────────────────────────────────────────────────────────────────────────────

/// Verify messages flow from test stdin → shim → echo server → shim → test stdout.
///
/// The echo script echoes every line back, so we expect exact round-trip.
/// Uses the actual `thoughtgate shim` binary as a child process.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn test_full_round_trip_forward() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (port, _state) = start_test_governance().await;
    let dir = temp_dir_unique("roundtrip");
    let script = write_echo_script(&dir);

    let bin = env!("CARGO_BIN_EXE_thoughtgate");

    let mut child = tokio::process::Command::new(bin)
        .args([
            "shim",
            "--server-id",
            "echo-test",
            "--governance-endpoint",
            &format!("http://127.0.0.1:{port}"),
            "--profile",
            "production",
            "--",
            "bash",
        ])
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn thoughtgate shim");

    let mut child_stdin = child.stdin.take().unwrap();
    let child_stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(child_stdout);

    // 1. Send an `initialize` request (passthrough path — bypasses governance).
    let init_req = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n";
    child_stdin.write_all(init_req.as_bytes()).await.unwrap();
    child_stdin.flush().await.unwrap();

    // Read echoed response.
    let mut line = String::new();
    let read_result = tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .expect("timeout waiting for initialize echo");
    assert!(read_result.unwrap() > 0, "expected echoed initialize");
    let echoed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(echoed["method"], "initialize");
    assert_eq!(echoed["id"], 1);

    // 2. Send a `tools/call` request (governance-evaluated path — stub returns Forward).
    line.clear();
    let tools_req = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"read_file\"}}\n";
    child_stdin.write_all(tools_req.as_bytes()).await.unwrap();
    child_stdin.flush().await.unwrap();

    let read_result = tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .expect("timeout waiting for tools/call echo");
    assert!(read_result.unwrap() > 0, "expected echoed tools/call");
    let echoed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(echoed["method"], "tools/call");
    assert_eq!(echoed["id"], 2);

    // 3. Close stdin → shim should exit.
    drop(child_stdin);
    let exit = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("timeout waiting for shim exit");
    assert!(exit.is_ok(), "shim should have exited");

    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: Malformed JSON is skipped, proxy continues
// ─────────────────────────────────────────────────────────────────────────────

/// EC-STDIO-015: Malformed JSON is skipped, valid messages still flow through.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn test_malformed_json_skipped_no_crash() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (port, _state) = start_test_governance().await;
    let dir = temp_dir_unique("malformed");
    let script = write_echo_script(&dir);

    let bin = env!("CARGO_BIN_EXE_thoughtgate");

    let mut child = tokio::process::Command::new(bin)
        .args([
            "shim",
            "--server-id",
            "malformed-test",
            "--governance-endpoint",
            &format!("http://127.0.0.1:{port}"),
            "--profile",
            "production",
            "--",
            "bash",
        ])
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn thoughtgate shim");

    let mut child_stdin = child.stdin.take().unwrap();
    let child_stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(child_stdout);

    // 1. Send malformed line (not JSON at all).
    child_stdin.write_all(b"not json at all\n").await.unwrap();
    child_stdin.flush().await.unwrap();

    // 2. Send valid JSON-RPC request (passthrough path for simplicity).
    let valid_req = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n";
    child_stdin.write_all(valid_req.as_bytes()).await.unwrap();
    child_stdin.flush().await.unwrap();

    // 3. Read response — the malformed line was skipped, so we should get
    //    the valid message echoed back.
    let mut line = String::new();
    let read_result = tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .expect("timeout waiting for valid echo after malformed skip");
    assert!(read_result.unwrap() > 0, "expected echoed valid message");
    let echoed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(echoed["method"], "initialize");

    // Cleanup.
    drop(child_stdin);
    let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: Config backup → rewrite → ConfigGuard → drop → restore cycle
// ─────────────────────────────────────────────────────────────────────────────

/// Verify the full config rewrite → ConfigGuard → drop → restore cycle.
#[test]
fn test_config_backup_restore_full_cycle() {
    let dir = temp_dir_unique("restore");
    let config_path = dir.join("claude_desktop_config.json");

    // Write a known original config.
    let original_content = serde_json::json!({
        "mcpServers": {
            "filesystem": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
            }
        }
    });
    std::fs::write(
        &config_path,
        serde_json::to_string_pretty(&original_content).unwrap(),
    )
    .unwrap();

    let original_bytes = std::fs::read_to_string(&config_path).unwrap();

    // Parse servers via the adapter.
    let adapter = ClaudeDesktopAdapter;
    let servers = adapter.parse_servers(&config_path).unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].command, "npx");

    // Rewrite config with shim binary path.
    let shim_binary = PathBuf::from("/usr/local/bin/thoughtgate");
    let options = ShimOptions {
        server_id: String::new(),
        governance_endpoint: "http://127.0.0.1:19090".to_string(),
        profile: Profile::Production,
        config_path: PathBuf::from("thoughtgate.yaml"),
    };

    let backup_path = adapter
        .rewrite_config(&config_path, &servers, &shim_binary, &options)
        .unwrap();

    // Verify backup exists and matches original.
    assert!(backup_path.exists(), "backup file should exist");
    let backup_bytes = std::fs::read_to_string(&backup_path).unwrap();
    assert_eq!(backup_bytes, original_bytes, "backup should match original");

    // Verify config was rewritten (command field changed).
    let rewritten: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(
        rewritten["mcpServers"]["filesystem"]["command"]
            .as_str()
            .unwrap(),
        "/usr/local/bin/thoughtgate"
    );

    // Create ConfigGuard.
    let guard = ConfigGuard::new(&config_path, &backup_path).unwrap();

    // Verify lock file exists.
    let mut lock_path_os = config_path.as_os_str().to_os_string();
    lock_path_os.push(".thoughtgate-lock");
    let lock_path = PathBuf::from(&lock_path_os);
    assert!(lock_path.exists(), "lock file should exist");

    // Drop guard (triggers restore).
    drop(guard);

    // Verify config restored to original content.
    let restored_bytes = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(
        restored_bytes, original_bytes,
        "config should be restored to original"
    );

    // Verify lock file removed.
    assert!(
        !lock_path.exists(),
        "lock file should be removed after drop"
    );

    // Verify backup file removed.
    assert!(
        !backup_path.exists(),
        "backup file should be removed after restore"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 4: Concurrent lock rejection (EC-STDIO-005)
// ─────────────────────────────────────────────────────────────────────────────

/// EC-STDIO-005: Second lock attempt fails with `ConfigError::Locked`.
#[test]
fn test_concurrent_lock_rejected() {
    let dir = temp_dir_unique("lock");
    let config_path = dir.join("config.json");
    let backup_path = dir.join("config.json.thoughtgate-backup");

    std::fs::write(&config_path, r#"{"mcpServers":{}}"#).unwrap();
    std::fs::write(&backup_path, r#"{"mcpServers":{}}"#).unwrap();

    // First lock succeeds.
    let guard1 = ConfigGuard::new(&config_path, &backup_path).unwrap();

    // Second lock fails with Locked.
    let result = ConfigGuard::new(&config_path, &backup_path);
    assert!(
        matches!(result, Err(ConfigError::Locked)),
        "second lock should fail with Locked"
    );

    // Drop first guard — releases lock.
    drop(guard1);

    // Third lock succeeds (lock was released).
    let guard3 = ConfigGuard::new(&config_path, &backup_path).unwrap();
    drop(guard3);

    let _ = std::fs::remove_dir_all(&dir);
}
