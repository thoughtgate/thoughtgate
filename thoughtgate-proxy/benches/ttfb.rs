//! TTFB (Time-To-First-Byte) Benchmark for REQ-OBS-001
//!
//! Tests actual ThoughtGate proxy latency with MCP traffic by:
//! 1. Starting a mock MCP server as upstream
//! 2. Starting ThoughtGate proxy pointing to the mock
//! 3. Measuring request latency direct vs through proxy
//!
//! # Traceability
//! - Implements: REQ-OBS-001 M-TTFB-001 (Time to first byte)
//! - Implements: REQ-OBS-001 M-OH-001 (Proxy overhead measurement)
//!
//! # Metrics
//! - `overhead/direct_baseline`: Direct to mock MCP (baseline for overhead calculation)
//! - `ttfb/proxied`: Through ThoughtGate proxy (measures TTFB, enables overhead calc)
//!
//! # Prerequisites
//!
//! Both binaries must be built before running:
//! ```bash
//! cargo build --release -p thoughtgate-proxy --features mock
//! cargo bench -p thoughtgate-proxy --bench ttfb
//! ```

use criterion::{Criterion, criterion_group, criterion_main};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::runtime::Runtime;

/// Test environment with mock MCP server and ThoughtGate proxy.
struct TestEnv {
    mock_mcp: Child,
    proxy: Child,
    proxy_port: u16,
    mock_port: u16,
}

impl TestEnv {
    /// Ensure mock_mcp and thoughtgate-proxy release binaries exist.
    ///
    /// If missing, triggers a cargo build. This makes the benchmark
    /// self-sufficient regardless of CI step ordering or profile mismatches.
    fn ensure_binaries() {
        let mock_path = std::path::Path::new("./target/release/mock_mcp");
        let proxy_path = std::path::Path::new("./target/release/thoughtgate-proxy");

        if mock_path.exists() && proxy_path.exists() {
            return;
        }

        eprintln!("Building release binaries (mock_mcp + thoughtgate-proxy)...");
        let status = Command::new("cargo")
            .args([
                "build",
                "--release",
                "-p",
                "thoughtgate-proxy",
                "--features",
                "mock",
            ])
            .status()
            .expect("Failed to run cargo build");
        assert!(
            status.success(),
            "Failed to build binaries. Run: cargo build --release -p thoughtgate-proxy --features mock"
        );
    }

    /// Start the test environment with mock MCP and proxy.
    fn start() -> Self {
        Self::ensure_binaries();

        let mock_port = 19999;
        let proxy_port = 19467;
        let admin_port = 19469;

        // Start mock MCP server with zero delay for benchmarking
        let mock_mcp = Command::new("./target/release/mock_mcp")
            .env("MOCK_MCP_PORT", mock_port.to_string())
            .env("MOCK_MCP_DELAY_MS", "0")
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("Failed to start mock MCP server. Run: cargo build --release -p thoughtgate-proxy --features mock");

        // Wait for mock to start
        std::thread::sleep(Duration::from_millis(500));

        // Start ThoughtGate proxy
        let proxy = Command::new("./target/release/thoughtgate-proxy")
            .env(
                "THOUGHTGATE_UPSTREAM_URL",
                format!("http://127.0.0.1:{}", mock_port),
            )
            .env("THOUGHTGATE_OUTBOUND_PORT", proxy_port.to_string())
            .env("THOUGHTGATE_ADMIN_PORT", admin_port.to_string())
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect(
                "Failed to start ThoughtGate proxy. Run: cargo build --release -p thoughtgate-proxy",
            );

        // Wait for proxy health check to pass
        for _ in 0..50 {
            if Command::new("curl")
                .args(["-sf", &format!("http://127.0.0.1:{}/health", admin_port)])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        Self {
            mock_mcp,
            proxy,
            proxy_port,
            mock_port,
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = self.proxy.kill();
        let _ = self.mock_mcp.kill();
        // Wait for processes to terminate
        let _ = self.proxy.wait();
        let _ = self.mock_mcp.wait();
    }
}

/// Benchmark TTFB for direct connection vs through proxy.
///
/// # Traceability
/// - Implements: REQ-OBS-001 M-TTFB-001 (P95 TTFB < 10ms)
/// - Implements: REQ-OBS-001 M-OH-001 (Overhead < 2ms)
fn bench_ttfb(c: &mut Criterion) {
    let rt = Runtime::new().expect("Failed to create Tokio runtime");
    let env = TestEnv::start();
    let client = reqwest::Client::new();

    // MCP JSON-RPC request payload
    let mcp_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "benchmark_tool",
            "arguments": {}
        }
    });

    let mock_url = format!("http://127.0.0.1:{}/mcp/v1", env.mock_port);
    let proxy_url = format!("http://127.0.0.1:{}/mcp/v1", env.proxy_port);

    // Warmup: establish connection pools before benchmarking
    // This prevents first-request connection establishment from skewing results
    rt.block_on(async {
        for _ in 0..10 {
            let _ = client.post(&mock_url).json(&mcp_payload).send().await;
            let _ = client.post(&proxy_url).json(&mcp_payload).send().await;
        }
    });

    // Note: No benchmark_group() to avoid metric path duplication
    // Metrics will be: overhead/direct_baseline, ttfb/proxied (not ttfb/overhead/...)
    c.bench_function("overhead/direct_baseline", |b| {
        let payload_for_direct = mcp_payload.clone();
        b.to_async(&rt).iter(|| {
            let client = client.clone();
            let url = mock_url.clone();
            let payload = payload_for_direct.clone();
            async move { client.post(&url).json(&payload).send().await }
        });
    });

    // REQ-OBS-001 M-TTFB-001: Time to first byte through proxy (target: p95 < 10ms)
    // REQ-OBS-001 M-OH-001: Proxy overhead = proxied - direct (target: < 2ms)
    c.bench_function("ttfb/proxied", |b| {
        b.to_async(&rt).iter(|| {
            let client = client.clone();
            let url = proxy_url.clone();
            let payload = mcp_payload.clone();
            async move { client.post(&url).json(&payload).send().await }
        });
    });
}

criterion_group!(benches, bench_ttfb);
criterion_main!(benches);
