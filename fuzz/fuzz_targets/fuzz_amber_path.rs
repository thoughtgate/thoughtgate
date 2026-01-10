#![no_main]

//! Fuzz target for REQ-CORE-002 Amber Path (Buffered Termination Strategy)
//!
//! # Traceability
//! - Implements: REQ-CORE-002 Section 5.2 (Fuzzing - cargo fuzz run amber_path)
//!
//! # Goal
//! Verify that malformed chunks or payloads do not cause:
//! - Panics in the BufferedForwarder
//! - Memory leaks or unbounded allocation
//! - Inspector chain failures
//! - Deadlocks or infinite loops

use arbitrary::Arbitrary;
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::Full;
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

use thoughtgate::buffered_forwarder::BufferedForwarder;
use thoughtgate::config::ProxyConfig;
use thoughtgate::error::ProxyError;
use thoughtgate::inspector::{Decision, InspectionContext, Inspector};

/// Fuzz input for Amber Path testing
#[derive(Arbitrary, Debug)]
struct FuzzAmberInput {
    /// Raw body bytes to buffer
    body_bytes: Vec<u8>,
    /// Headers to include
    headers: Vec<(Vec<u8>, Vec<u8>)>,
    /// Whether this is a request (true) or response (false)
    is_request: bool,
    /// Inspector decisions to simulate
    inspector_decisions: Vec<FuzzDecision>,
}

/// Fuzz decision for inspector chain
#[derive(Arbitrary, Debug, Clone)]
enum FuzzDecision {
    Approve,
    Modify(Vec<u8>),
    Reject(u16),
}

impl From<FuzzDecision> for Decision {
    fn from(fd: FuzzDecision) -> Self {
        match fd {
            FuzzDecision::Approve => Decision::Approve,
            FuzzDecision::Modify(bytes) => Decision::Modify(Bytes::from(bytes)),
            FuzzDecision::Reject(code) => {
                // Clamp to valid HTTP status codes
                let code = code.max(100).min(599);
                Decision::Reject(StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST))
            }
        }
    }
}

/// Test inspector that returns predetermined decisions
struct FuzzInspector {
    name: &'static str,
    decision: Decision,
}

#[async_trait::async_trait]
impl Inspector for FuzzInspector {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn inspect(
        &self,
        _body: &[u8],
        _ctx: InspectionContext<'_>,
    ) -> Result<Decision, ProxyError> {
        Ok(self.decision.clone())
    }
}

/// Error-returning inspector for error path testing
/// 
/// Note: Actual panic safety is tested in unit tests (tests/amber_path_panic_safety.rs)
/// Fuzz tests should find unexpected panics, not test intentional ones.
struct ErrorInspector;

#[async_trait::async_trait]
impl Inspector for ErrorInspector {
    fn name(&self) -> &'static str {
        "error-inspector"
    }

    async fn inspect(
        &self,
        body: &[u8],
        _ctx: InspectionContext<'_>,
    ) -> Result<Decision, ProxyError> {
        // Return error on specific byte patterns to test error handling
        if body.len() > 3 && body[0] == 0xDE && body[1] == 0xAD {
            return Err(ProxyError::InspectorError(
                "error-inspector".to_string(),
                "Intentional error for fuzz testing".to_string(),
            ));
        }
        Ok(Decision::Approve)
    }
}

fuzz_target!(|input: FuzzAmberInput| {
    // Use tokio runtime for async code
    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(async { fuzz_amber_path(input).await }));
});

async fn fuzz_amber_path(input: FuzzAmberInput) {
    // Create config with small limits for fuzzing
    let mut config = ProxyConfig::default();
    config.max_concurrent_buffers = 10;
    config.req_buffer_max = 1024 * 1024; // 1MB for fuzzing
    config.resp_buffer_max = 1024 * 1024; // 1MB for fuzzing
    config.buffer_timeout = std::time::Duration::from_secs(5);

    // Create forwarder
    let mut forwarder = BufferedForwarder::new(config);

    // Add fuzz inspectors based on input
    for (i, decision) in input.inspector_decisions.iter().take(5).enumerate() {
        let inspector = FuzzInspector {
            name: match i {
                0 => "fuzz-inspector-0",
                1 => "fuzz-inspector-1",
                2 => "fuzz-inspector-2",
                3 => "fuzz-inspector-3",
                _ => "fuzz-inspector-4",
            },
            decision: decision.clone().into(),
        };
        forwarder.add_inspector(Arc::new(inspector));
    }

    // Add error inspector occasionally to test error handling
    if input.body_bytes.len() % 7 == 0 {
        forwarder.add_inspector(Arc::new(ErrorInspector));
    }

    // Limit body size to prevent OOM in fuzzer
    let body_bytes = if input.body_bytes.len() > 64 * 1024 {
        &input.body_bytes[..64 * 1024]
    } else {
        &input.body_bytes[..]
    };

    if input.is_request {
        // Test request processing
        let mut builder = Request::builder().method("POST").uri("/fuzz");

        // Add fuzzy headers (limit count)
        for (name, value) in input.headers.iter().take(20) {
            if let (Ok(n), Ok(v)) = (
                http::header::HeaderName::from_bytes(name),
                http::header::HeaderValue::from_bytes(value),
            ) {
                builder = builder.header(n, v);
            }
        }

        // Build request with body
        if let Ok(req) = builder.body(Full::new(Bytes::from(body_bytes.to_vec()))) {
            // Convert to Incoming-compatible body for testing
            // Note: We can't directly create Incoming, so we test the inspector chain logic
            let _ = test_inspector_chain(&forwarder, body_bytes, true).await;
        }
    } else {
        // Test response processing
        let mut builder = Response::builder().status(200);

        // Add fuzzy headers (limit count)
        for (name, value) in input.headers.iter().take(20) {
            if let (Ok(n), Ok(v)) = (
                http::header::HeaderName::from_bytes(name),
                http::header::HeaderValue::from_bytes(value),
            ) {
                builder = builder.header(n, v);
            }
        }

        if let Ok(_resp) = builder.body(Full::new(Bytes::from(body_bytes.to_vec()))) {
            let _ = test_inspector_chain(&forwarder, body_bytes, false).await;
        }
    }
}

/// Test the inspector chain directly with raw bytes
async fn test_inspector_chain(forwarder: &BufferedForwarder, body: &[u8], is_request: bool) {
    // Create minimal request/response parts for context
    if is_request {
        let req = Request::builder()
            .method("POST")
            .uri("/fuzz")
            .body(())
            .unwrap();
        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        // Run inspector chain - should not panic
        let _ = forwarder.run_inspector_chain_public(body, ctx).await;
    } else {
        let resp = Response::builder().status(200).body(()).unwrap();
        let (parts, _) = resp.into_parts();
        let ctx = InspectionContext::Response(&parts);

        // Run inspector chain - should not panic
        let _ = forwarder.run_inspector_chain_public(body, ctx).await;
    }
}

