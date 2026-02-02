//! JSON-RPC Parsing Micro-Benchmark
//!
//! Tracks the T-001 optimization (eliminate double deserialization) by measuring
//! `parse_jsonrpc()` with payloads of varying sizes and batch counts.
//!
//! # Traceability
//! - Implements: REQ-OBS-001 (Performance Tracking)
//! - Tracks: T-001 (Single-request fast path)
//!
//! # Usage
//! ```bash
//! cargo bench --bench jsonrpc_parse
//! ```

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use serde_json::json;
use thoughtgate::transport::jsonrpc::parse_jsonrpc;

/// Minimal tools/call request (~120 bytes).
fn small_request() -> Vec<u8> {
    json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {"name": "echo", "arguments": {"msg": "hi"}},
        "id": 1
    })
    .to_string()
    .into_bytes()
}

/// Medium request with nested arguments (~600 bytes).
fn medium_request() -> Vec<u8> {
    json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "transfer_funds",
            "arguments": {
                "source_account": "ACC-001234567890",
                "destination_account": "ACC-009876543210",
                "amount": 1500.50,
                "currency": "USD",
                "memo": "Monthly payment for cloud infrastructure services",
                "metadata": {
                    "request_id": "req-abc-123",
                    "ip_address": "192.168.1.100",
                    "user_agent": "MCP-Agent/1.0"
                }
            }
        },
        "id": "req-42"
    })
    .to_string()
    .into_bytes()
}

/// Large request with complex nested params (~2KB).
fn large_request() -> Vec<u8> {
    let items: Vec<serde_json::Value> = (0..20)
        .map(|i| {
            json!({
                "id": i,
                "name": format!("item_{}", i),
                "value": i as f64 * 1.5,
                "tags": ["alpha", "beta", "gamma"],
                "nested": {"depth": 1, "data": "x".repeat(50)}
            })
        })
        .collect();

    json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "bulk_process",
            "arguments": {
                "items": items,
                "options": {
                    "parallel": true,
                    "timeout_ms": 30000,
                    "retry_count": 3
                }
            }
        },
        "id": 999
    })
    .to_string()
    .into_bytes()
}

/// Build a JSON-RPC batch with `n` copies of the small request.
fn batch_request(n: usize) -> Vec<u8> {
    let items: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            json!({
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": {"name": "echo", "arguments": {"msg": format!("batch-{}", i)}},
                "id": i
            })
        })
        .collect();

    serde_json::to_vec(&items).expect("failed to serialize batch")
}

/// Benchmark single-request parsing at varying payload sizes.
///
/// This directly tracks the T-001 optimization (single-request fast path
/// via byte-peeking and direct `from_slice::<RawJsonRpcRequest>`).
fn bench_single_request(c: &mut Criterion) {
    let mut group = c.benchmark_group("jsonrpc/single");

    let small = small_request();
    let medium = medium_request();
    let large = large_request();

    group.bench_with_input(BenchmarkId::new("parse", "small"), &small, |b, data| {
        b.iter(|| parse_jsonrpc(data))
    });

    group.bench_with_input(BenchmarkId::new("parse", "medium"), &medium, |b, data| {
        b.iter(|| parse_jsonrpc(data))
    });

    group.bench_with_input(BenchmarkId::new("parse", "large"), &large, |b, data| {
        b.iter(|| parse_jsonrpc(data))
    });

    group.finish();
}

/// Benchmark batch-request parsing at varying batch sizes.
fn bench_batch_request(c: &mut Criterion) {
    let mut group = c.benchmark_group("jsonrpc/batch");

    for &n in &[5, 20] {
        let data = batch_request(n);
        group.bench_with_input(BenchmarkId::new("parse", n), &data, |b, data| {
            b.iter(|| parse_jsonrpc(data))
        });
    }

    group.finish();
}

criterion_group!(benches, bench_single_request, bench_batch_request);
criterion_main!(benches);
