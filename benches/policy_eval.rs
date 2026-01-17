//! Policy Evaluation Benchmark for REQ-OBS-001
//!
//! # Traceability
//! - Implements: REQ-OBS-001 M-POL-001, M-POL-002 (Policy Evaluation Performance)
//!
//! # Metrics
//! - `policy/eval_p50`: Median policy evaluation time (target: < 100µs)
//! - `policy/eval_p99`: 99th percentile evaluation time (target: < 500µs)
//!
//! # Usage
//! ```bash
//! cargo bench --bench policy_eval
//! ```

#![allow(deprecated)] // Using v0.1 API for benchmarking backward compatibility

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::time::Duration;
use thoughtgate::policy::{PolicyRequest, Principal, Resource, engine::CedarEngine};

/// Create a test principal for benchmarking.
fn create_test_principal() -> Principal {
    Principal {
        app_name: "benchmark-app".to_string(),
        namespace: "default".to_string(),
        service_account: "default".to_string(),
        roles: vec!["user".to_string()],
    }
}

/// Create a test resource (tool call) for benchmarking.
fn create_test_tool_call(name: &str) -> Resource {
    Resource::ToolCall {
        name: name.to_string(),
        server: "mcp-server".to_string(),
    }
}

/// Create a test policy request.
fn create_test_request(tool_name: &str) -> PolicyRequest {
    PolicyRequest {
        principal: create_test_principal(),
        resource: create_test_tool_call(tool_name),
        context: None,
    }
}

/// Benchmark simple policy evaluation (single tool, no context).
///
/// # Traceability
/// - Implements: REQ-OBS-001 M-POL-001 (Policy eval p50 < 100µs)
fn bench_policy_eval_simple(c: &mut Criterion) {
    // Create engine with default policies
    let engine = CedarEngine::new().expect("Failed to create Cedar engine");
    let request = create_test_request("read_file");

    c.bench_function("policy/eval_simple", |b| {
        b.iter(|| engine.evaluate(&request))
    });
}

/// Benchmark policy evaluation with various tool names.
///
/// Tests evaluation performance across different tool names to ensure
/// consistent performance regardless of the resource being accessed.
fn bench_policy_eval_various_tools(c: &mut Criterion) {
    let engine = CedarEngine::new().expect("Failed to create Cedar engine");

    let tool_names = [
        "read_file",
        "write_file",
        "delete_file",
        "execute_command",
        "create_user",
        "delete_user",
        "send_email",
        "database_query",
    ];

    let mut group = c.benchmark_group("policy/eval_tools");
    group.measurement_time(Duration::from_secs(5));

    for tool_name in &tool_names {
        let request = create_test_request(tool_name);
        group.bench_with_input(BenchmarkId::from_parameter(tool_name), tool_name, |b, _| {
            b.iter(|| engine.evaluate(&request))
        });
    }

    group.finish();
}

/// Benchmark policy evaluation with MCP method resources.
///
/// Tests evaluation performance for MCP methods (vs tool calls).
fn bench_policy_eval_mcp_methods(c: &mut Criterion) {
    let engine = CedarEngine::new().expect("Failed to create Cedar engine");

    let methods = [
        "resources/read",
        "resources/list",
        "tools/call",
        "prompts/get",
    ];

    let mut group = c.benchmark_group("policy/eval_methods");
    group.measurement_time(Duration::from_secs(5));

    for method in &methods {
        let request = PolicyRequest {
            principal: create_test_principal(),
            resource: Resource::McpMethod {
                method: method.to_string(),
                server: "mcp-server".to_string(),
            },
            context: None,
        };

        group.bench_with_input(BenchmarkId::from_parameter(method), method, |b, _| {
            b.iter(|| engine.evaluate(&request))
        });
    }

    group.finish();
}

/// Benchmark policy reload performance.
///
/// # Traceability
/// - Implements: REQ-OBS-001 M-POL-003 (Policy reload < 100ms)
fn bench_policy_reload(c: &mut Criterion) {
    let engine = CedarEngine::new().expect("Failed to create Cedar engine");

    c.bench_function("policy/reload", |b| {
        b.iter(|| {
            // Reload returns Result, which we need to handle
            engine.reload().ok()
        })
    });
}

/// Benchmark concurrent policy evaluation.
///
/// Tests that policy evaluation is thread-safe and performant
/// under concurrent access (simulating multiple requests).
fn bench_policy_eval_concurrent(c: &mut Criterion) {
    use std::sync::Arc;
    use std::thread;

    let engine = Arc::new(CedarEngine::new().expect("Failed to create Cedar engine"));

    let mut group = c.benchmark_group("policy/eval_concurrent");
    group.measurement_time(Duration::from_secs(10));

    for num_threads in [1, 2, 4] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_threads", num_threads)),
            &num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    let handles: Vec<_> = (0..num_threads)
                        .map(|i| {
                            let engine = Arc::clone(&engine);
                            let tool_name = format!("tool_{}", i);
                            thread::spawn(move || {
                                let request = create_test_request(&tool_name);
                                for _ in 0..100 {
                                    engine.evaluate(&request);
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().expect("Thread panicked");
                    }
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_policy_eval_simple,
    bench_policy_eval_various_tools,
    bench_policy_eval_mcp_methods,
    bench_policy_reload,
    bench_policy_eval_concurrent,
);
criterion_main!(benches);
