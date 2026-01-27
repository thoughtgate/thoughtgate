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
//! cargo build --release --bin thoughtgate --bin mock_mcp --features mock
//! cargo bench --bench ttfb
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
    /// Start the test environment with mock MCP and proxy.
    fn start() -> Self {
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
            .expect("Failed to start mock MCP server. Run: cargo build --release --bin mock_mcp --features mock");

        // Wait for mock to start
        std::thread::sleep(Duration::from_millis(500));

        // Start ThoughtGate proxy
        let proxy = Command::new("./target/release/thoughtgate")
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
                "Failed to start ThoughtGate proxy. Run: cargo build --release --bin thoughtgate",
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

    let mut group = c.benchmark_group("ttfb");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    // Benchmark direct connection to mock MCP (baseline for overhead calculation)
    // REQ-OBS-001 M-OH-001: Measures baseline latency for overhead calculation
    let mock_url = format!("http://127.0.0.1:{}/mcp/v1", env.mock_port);
    let payload_for_direct = mcp_payload.clone();
    group.bench_function("overhead/direct_baseline", |b| {
        b.to_async(&rt).iter(|| {
            let client = client.clone();
            let url = mock_url.clone();
            let payload = payload_for_direct.clone();
            async move { client.post(&url).json(&payload).send().await }
        });
    });

    // Benchmark through ThoughtGate proxy
    // REQ-OBS-001 M-TTFB-001: Time to first byte through proxy (target: p95 < 10ms)
    // REQ-OBS-001 M-OH-001: Proxy overhead = proxied - direct (target: < 2ms)
    let proxy_url = format!("http://127.0.0.1:{}/mcp/v1", env.proxy_port);
    group.bench_function("ttfb/proxied", |b| {
        b.to_async(&rt).iter(|| {
            let client = client.clone();
            let url = proxy_url.clone();
            let payload = mcp_payload.clone();
            async move { client.post(&url).json(&payload).send().await }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_ttfb);
criterion_main!(benches);
