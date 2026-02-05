//! Policy Evaluation Benchmark for REQ-OBS-001
//!
//! # Traceability
//! - Implements: REQ-OBS-001 M-POL-001, M-POL-002 (Policy Evaluation Performance)
//!
//! # Metrics
//! - `policy/eval_v2_with_args`: v0.2 API with tool arguments (target: < 100µs)
//! - `policy/reload`: Policy hot-reload performance (target: < 100ms)
//!
//! # Usage
//! ```bash
//! cargo bench -p thoughtgate-core --bench policy_eval
//! ```

use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::json;
use thoughtgate_core::policy::{
    Principal,
    engine::CedarEngine,
    types::{CedarContext, CedarRequest, CedarResource, TimeContext},
};

/// Create a test principal for benchmarking.
fn create_test_principal() -> Principal {
    Principal {
        app_name: "benchmark-app".to_string(),
        namespace: "default".to_string(),
        service_account: "default".to_string(),
        roles: vec!["user".to_string()],
    }
}

/// Benchmark v0.2 API with tool arguments.
///
/// Tests the CedarRequest/CedarResource API that supports
/// argument inspection in Cedar policies.
///
/// # Traceability
/// - Implements: REQ-OBS-001 M-POL-001 (Policy eval performance)
/// - Implements: REQ-POL-001/F-003 (Argument inspection)
fn bench_policy_eval_v2(c: &mut Criterion) {
    let engine = CedarEngine::new().expect("Failed to create Cedar engine");

    // Create a request with complex arguments for argument inspection
    let request = CedarRequest {
        principal: create_test_principal(),
        resource: CedarResource::ToolCall {
            name: "transfer_funds".to_string(),
            server: "mcp-server".to_string(),
            arguments: json!({
                "amount": 5000,
                "currency": "USD",
                "recipient": "user@example.com",
                "metadata": {
                    "reference": "INV-12345",
                    "category": "payment"
                }
            }),
        },
        context: CedarContext {
            policy_id: "financial_transfer".to_string(),
            source_id: "test-source".to_string(),
            time: TimeContext::now(),
        },
    };

    c.bench_function("policy/eval_v2", |b| {
        b.iter(|| engine.evaluate_v2(&request))
    });
}

/// Benchmark policy reload performance.
///
/// # Traceability
/// - Implements: REQ-OBS-001 M-POL-003 (Policy reload < 100ms)
fn bench_policy_reload(c: &mut Criterion) {
    let engine = CedarEngine::new().expect("Failed to create Cedar engine");

    c.bench_function("policy/reload", |b| b.iter(|| engine.reload().ok()));
}

criterion_group!(benches, bench_policy_eval_v2, bench_policy_reload,);
criterion_main!(benches);
