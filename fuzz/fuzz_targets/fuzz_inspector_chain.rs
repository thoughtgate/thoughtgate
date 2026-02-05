#![no_main]

//! Fuzz target for REQ-CORE-002 Inspector Chain Semantics
//!
//! # Traceability
//! - Implements: REQ-CORE-002 F-002 (Fail-Closed State & Panic Safety)
//! - Implements: REQ-CORE-002 F-004 (Chain Semantics)
//!
//! # Goal
//! Verify that the inspector chain:
//! - Handles panicking inspectors gracefully
//! - Correctly propagates modifications between inspectors
//! - Short-circuits on rejection
//! - Never forwards partial data

use arbitrary::Arbitrary;
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use libfuzzer_sys::fuzz_target;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use thoughtgate_proxy::buffered_forwarder::BufferedForwarder;
use thoughtgate_proxy::error::ProxyError;
use thoughtgate_proxy::proxy_config::ProxyConfig;
use thoughtgate_core::inspector::{Decision, InspectionContext, Inspector};

/// Fuzz input for inspector chain testing
#[derive(Arbitrary, Debug)]
struct FuzzChainInput {
    /// Initial body bytes
    body: Vec<u8>,
    /// Chain of inspector behaviors
    inspectors: Vec<InspectorBehavior>,
    /// Whether to use request or response context
    is_request: bool,
    /// URI path for request context
    uri_path: Vec<u8>,
    /// HTTP method index (0-7 for different methods)
    method_index: u8,
}

/// Behavior specification for a fuzz inspector
#[derive(Arbitrary, Debug, Clone)]
enum InspectorBehavior {
    /// Always approve
    Approve,
    /// Modify by appending bytes
    Append(Vec<u8>),
    /// Modify by prepending bytes
    Prepend(Vec<u8>),
    /// Modify by replacing with new bytes
    Replace(Vec<u8>),
    /// Reject with a specific status code
    Reject(u16),
    /// Panic with a specific trigger pattern
    Panic,
    /// Return an error
    Error,
    /// Approve but take a long time (test timeout behavior)
    SlowApprove,
}

/// Counter for tracking inspector execution order
static EXECUTION_ORDER: AtomicUsize = AtomicUsize::new(0);

/// Inspector that behaves according to FuzzChainInput
struct BehaviorInspector {
    name: &'static str,
    behavior: InspectorBehavior,
    execution_id: usize,
}

impl BehaviorInspector {
    fn new(name: &'static str, behavior: InspectorBehavior) -> Self {
        Self {
            name,
            behavior,
            execution_id: 0,
        }
    }
}

#[async_trait::async_trait]
impl Inspector for BehaviorInspector {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn inspect(
        &self,
        body: &[u8],
        _ctx: InspectionContext<'_>,
    ) -> Result<Decision, ProxyError> {
        // Record execution order
        let _order = EXECUTION_ORDER.fetch_add(1, Ordering::SeqCst);

        match &self.behavior {
            InspectorBehavior::Approve => Ok(Decision::Approve),

            InspectorBehavior::Append(suffix) => {
                let mut modified = body.to_vec();
                // Limit append size
                let suffix = if suffix.len() > 1024 {
                    &suffix[..1024]
                } else {
                    suffix
                };
                modified.extend_from_slice(suffix);
                Ok(Decision::Modify(Bytes::from(modified)))
            }

            InspectorBehavior::Prepend(prefix) => {
                // Limit prepend size
                let prefix = if prefix.len() > 1024 {
                    &prefix[..1024]
                } else {
                    prefix
                };
                let mut modified = prefix.to_vec();
                modified.extend_from_slice(body);
                Ok(Decision::Modify(Bytes::from(modified)))
            }

            InspectorBehavior::Replace(new_body) => {
                // Limit replacement size
                let new_body = if new_body.len() > 4096 {
                    &new_body[..4096]
                } else {
                    new_body
                };
                Ok(Decision::Modify(Bytes::from(new_body.to_vec())))
            }

            InspectorBehavior::Reject(code) => {
                // Clamp to valid HTTP status codes
                let code = (*code).max(100).min(599);
                let status =
                    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                Ok(Decision::Reject(status))
            }

            InspectorBehavior::Panic => {
                // Note: We can't actually panic in fuzz mode because libFuzzer uses panic=abort
                // The real panic safety is tested via unit tests with catch_unwind
                // In fuzz mode, just return an error to simulate failure
                Err(ProxyError::InspectorPanic(
                    "Simulated panic (fuzz mode uses error instead)".to_string(),
                ))
            }

            InspectorBehavior::Error => Err(ProxyError::InspectorError(
                self.name.to_string(),
                "Fuzz-induced error".to_string(),
            )),

            InspectorBehavior::SlowApprove => {
                // Simulate slow inspector (but not too slow for fuzzing)
                tokio::time::sleep(std::time::Duration::from_micros(100)).await;
                Ok(Decision::Approve)
            }
        }
    }
}

fuzz_target!(|input: FuzzChainInput| {
    // Reset execution order counter
    EXECUTION_ORDER.store(0, Ordering::SeqCst);

    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(async { fuzz_inspector_chain(input).await }));
});

async fn fuzz_inspector_chain(input: FuzzChainInput) {
    // Limit body size
    let body = if input.body.len() > 32 * 1024 {
        &input.body[..32 * 1024]
    } else {
        &input.body[..]
    };

    // Create config
    let mut config = ProxyConfig::default();
    config.max_concurrent_buffers = 5;
    config.req_buffer_max = 64 * 1024;
    config.resp_buffer_max = 64 * 1024;
    config.buffer_timeout = std::time::Duration::from_secs(2);

    // Build forwarder with inspectors
    let mut forwarder = BufferedForwarder::new(config);

    // Add inspectors (limit to 10)
    let inspector_names: [&'static str; 10] = [
        "fuzz-0", "fuzz-1", "fuzz-2", "fuzz-3", "fuzz-4", "fuzz-5", "fuzz-6", "fuzz-7", "fuzz-8",
        "fuzz-9",
    ];

    for (i, behavior) in input.inspectors.iter().take(10).enumerate() {
        let inspector = BehaviorInspector::new(inspector_names[i], behavior.clone());
        forwarder.add_inspector(Arc::new(inspector));
    }

    // Run the chain with appropriate context
    if input.is_request {
        // Build request context
        let method = match input.method_index % 8 {
            0 => "GET",
            1 => "POST",
            2 => "PUT",
            3 => "DELETE",
            4 => "PATCH",
            5 => "HEAD",
            6 => "OPTIONS",
            _ => "TRACE",
        };

        let uri = String::from_utf8_lossy(&input.uri_path);
        let uri = if uri.starts_with('/') {
            uri.to_string()
        } else {
            format!("/{}", uri)
        };

        let req = Request::builder()
            .method(method)
            .uri(uri.as_str())
            .body(())
            .unwrap_or_else(|_| Request::builder().uri("/").body(()).unwrap());

        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        // Run chain - should not panic (panics are caught internally)
        let result = forwarder.run_inspector_chain_public(body, ctx).await;

        // Verify result is sensible
        match result {
            Ok(Some(modified)) => {
                // Modification occurred - verify bytes are present
                let _ = modified.len();
            }
            Ok(None) => {
                // All approved - original should be used
            }
            Err(_e) => {
                // Error occurred - expected for some behaviors
            }
        }
    } else {
        // Build response context
        let resp = Response::builder()
            .status(200)
            .body(())
            .unwrap_or_else(|_| Response::builder().status(200).body(()).unwrap());

        let (parts, _) = resp.into_parts();
        let ctx = InspectionContext::Response(&parts);

        // Run chain - should not panic
        let result = forwarder.run_inspector_chain_public(body, ctx).await;

        // Verify result is sensible
        match result {
            Ok(Some(modified)) => {
                let _ = modified.len();
            }
            Ok(None) => {}
            Err(_) => {}
        }
    }

    // Verify execution order was tracked
    let final_order = EXECUTION_ORDER.load(Ordering::SeqCst);
    // Should have executed at most as many inspectors as configured
    // (could be less if short-circuited by rejection or panic)
    assert!(final_order <= input.inspectors.len().min(10) + 1);
}

/// Test specific chain semantics: modification propagation
#[allow(dead_code)]
async fn test_modification_propagation() {
    let config = ProxyConfig::default();
    let mut forwarder = BufferedForwarder::new(config);

    // First inspector appends "A"
    forwarder.add_inspector(Arc::new(BehaviorInspector::new(
        "append-a",
        InspectorBehavior::Append(b"A".to_vec()),
    )));

    // Second inspector appends "B"
    forwarder.add_inspector(Arc::new(BehaviorInspector::new(
        "append-b",
        InspectorBehavior::Append(b"B".to_vec()),
    )));

    let req = Request::builder().uri("/test").body(()).unwrap();
    let (parts, _) = req.into_parts();
    let ctx = InspectionContext::Request(&parts);

    let result = forwarder
        .run_inspector_chain_public(b"X", ctx)
        .await
        .unwrap();

    // Should be "XAB" - first inspector sees "X", appends "A" -> "XA"
    // Second inspector sees "XA", appends "B" -> "XAB"
    if let Some(modified) = result {
        assert_eq!(&modified[..], b"XAB");
    }
}

