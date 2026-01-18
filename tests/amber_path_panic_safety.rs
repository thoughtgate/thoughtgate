#![cfg(feature = "amber_path")]
//! Panic Safety Tests for REQ-CORE-002 Amber Path
//!
//! # Traceability
//! - Implements: REQ-CORE-002 F-002 (Panic Safety)
//! - Implements: REQ-CORE-002 Section 5.1 EC-007
//!
//! # Purpose
//! Verify that inspector panics are caught and converted to errors,
//! preventing crashes in the proxy service.

use http::{Request, Response};
use std::sync::Arc;
use thoughtgate::buffered_forwarder::BufferedForwarder;
use thoughtgate::error::{ProxyError, ProxyResult};
use thoughtgate::inspector::{Decision, InspectionContext, Inspector};
use thoughtgate::proxy_config::ProxyConfig;

/// Test inspector that panics unconditionally
struct PanicInspector;

#[async_trait::async_trait]
impl Inspector for PanicInspector {
    fn name(&self) -> &'static str {
        "panic-inspector"
    }

    async fn inspect(
        &self,
        _body: &[u8],
        _ctx: InspectionContext<'_>,
    ) -> Result<Decision, ProxyError> {
        panic!("Intentional panic for testing");
    }
}

/// Test inspector that panics on specific patterns
struct ConditionalPanicInspector;

#[async_trait::async_trait]
impl Inspector for ConditionalPanicInspector {
    fn name(&self) -> &'static str {
        "conditional-panic-inspector"
    }

    async fn inspect(
        &self,
        body: &[u8],
        _ctx: InspectionContext<'_>,
    ) -> Result<Decision, ProxyError> {
        if body.len() > 3 && body[0] == 0xDE && body[1] == 0xAD {
            panic!("Panic on 0xDEAD pattern");
        }
        Ok(Decision::Approve)
    }
}

/// Test inspector that succeeds
struct SuccessInspector;

#[async_trait::async_trait]
impl Inspector for SuccessInspector {
    fn name(&self) -> &'static str {
        "success-inspector"
    }

    async fn inspect(
        &self,
        _body: &[u8],
        _ctx: InspectionContext<'_>,
    ) -> Result<Decision, ProxyError> {
        Ok(Decision::Approve)
    }
}

#[tokio::test]
async fn test_inspector_panic_is_caught() {
    let config = ProxyConfig::default();
    let mut forwarder = BufferedForwarder::new(config);

    // Add panic inspector
    forwarder.add_inspector(Arc::new(PanicInspector));

    // Create test context
    let req = Request::builder()
        .method("POST")
        .uri("/test")
        .body(())
        .unwrap();
    let (parts, _) = req.into_parts();
    let ctx = InspectionContext::Request(&parts);

    // Run inspector chain - should return error, not panic
    let body = b"test body";
    let result = forwarder.run_inspector_chain_public(body, ctx).await;

    // Should return InspectorPanic error
    assert!(result.is_err());
    match result {
        Err(ProxyError::InspectorPanic(name)) => {
            assert_eq!(name, "panic-inspector");
        }
        _ => panic!("Expected InspectorPanic error"),
    }
}

#[tokio::test]
async fn test_conditional_panic_is_caught() {
    let config = ProxyConfig::default();
    let mut forwarder = BufferedForwarder::new(config);

    // Add conditional panic inspector
    forwarder.add_inspector(Arc::new(ConditionalPanicInspector));

    // Create test context
    let req = Request::builder()
        .method("POST")
        .uri("/test")
        .body(())
        .unwrap();
    let (parts, _) = req.into_parts();
    let ctx = InspectionContext::Request(&parts);

    // Test 1: Body that triggers panic
    let panic_body = b"\xDE\xAD\xBE\xEF";
    let result = forwarder.run_inspector_chain_public(panic_body, ctx).await;

    assert!(result.is_err());
    match result {
        Err(ProxyError::InspectorPanic(name)) => {
            assert_eq!(name, "conditional-panic-inspector");
        }
        _ => panic!("Expected InspectorPanic error"),
    }

    // Test 2: Body that doesn't trigger panic (recreate context)
    let req2 = Request::builder()
        .method("POST")
        .uri("/test")
        .body(())
        .unwrap();
    let (parts2, _) = req2.into_parts();
    let ctx2 = InspectionContext::Request(&parts2);

    let safe_body = b"safe body";
    let result = forwarder.run_inspector_chain_public(safe_body, ctx2).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_panic_stops_chain_processing() {
    let config = ProxyConfig::default();
    let mut forwarder = BufferedForwarder::new(config);

    // Add inspectors: success -> panic -> success
    // The second success inspector should never run
    forwarder.add_inspector(Arc::new(SuccessInspector));
    forwarder.add_inspector(Arc::new(PanicInspector));

    // This inspector should never be reached due to panic
    struct UnreachableInspector;
    #[async_trait::async_trait]
    impl Inspector for UnreachableInspector {
        fn name(&self) -> &'static str {
            "unreachable-inspector"
        }

        async fn inspect(
            &self,
            _body: &[u8],
            _ctx: InspectionContext<'_>,
        ) -> ProxyResult<Decision> {
            panic!("This should never be reached");
        }
    }
    forwarder.add_inspector(Arc::new(UnreachableInspector));

    // Create test context
    let req = Request::builder()
        .method("POST")
        .uri("/test")
        .body(())
        .unwrap();
    let (parts, _) = req.into_parts();
    let ctx = InspectionContext::Request(&parts);

    // Run inspector chain
    let body = b"test";
    let result = forwarder.run_inspector_chain_public(body, ctx).await;

    // Should stop at panic inspector
    assert!(result.is_err());
    match result {
        Err(ProxyError::InspectorPanic(name)) => {
            assert_eq!(name, "panic-inspector");
        }
        _ => panic!("Expected InspectorPanic error"),
    }
}

#[tokio::test]
async fn test_panic_in_response_inspection() {
    let config = ProxyConfig::default();
    let mut forwarder = BufferedForwarder::new(config);

    forwarder.add_inspector(Arc::new(PanicInspector));

    // Create response context
    let resp = Response::builder().status(200).body(()).unwrap();
    let (parts, _) = resp.into_parts();
    let ctx = InspectionContext::Response(&parts);

    // Run inspector chain - should catch panic in response context too
    let body = b"response body";
    let result = forwarder.run_inspector_chain_public(body, ctx).await;

    assert!(result.is_err());
    match result {
        Err(ProxyError::InspectorPanic(name)) => {
            assert_eq!(name, "panic-inspector");
        }
        _ => panic!("Expected InspectorPanic error"),
    }
}

#[tokio::test]
async fn test_panic_with_string_message() {
    struct StringPanicInspector;

    #[async_trait::async_trait]
    impl Inspector for StringPanicInspector {
        fn name(&self) -> &'static str {
            "string-panic-inspector"
        }

        async fn inspect(
            &self,
            _body: &[u8],
            _ctx: InspectionContext<'_>,
        ) -> ProxyResult<Decision> {
            panic!("{}", "Formatted panic message".to_string());
        }
    }

    let config = ProxyConfig::default();
    let mut forwarder = BufferedForwarder::new(config);
    forwarder.add_inspector(Arc::new(StringPanicInspector));

    let req = Request::builder()
        .method("POST")
        .uri("/test")
        .body(())
        .unwrap();
    let (parts, _) = req.into_parts();
    let ctx = InspectionContext::Request(&parts);

    let result = forwarder.run_inspector_chain_public(b"test", ctx).await;

    assert!(result.is_err());
    match result {
        Err(ProxyError::InspectorPanic(name)) => {
            assert_eq!(name, "string-panic-inspector");
        }
        _ => panic!("Expected InspectorPanic error"),
    }
}

#[tokio::test]
async fn test_multiple_panics_in_sequence() {
    // Test that panic handling works correctly across multiple requests
    let config = ProxyConfig::default();
    let mut forwarder = BufferedForwarder::new(config);
    forwarder.add_inspector(Arc::new(PanicInspector));

    let req = Request::builder()
        .method("POST")
        .uri("/test")
        .body(())
        .unwrap();
    let (parts, _) = req.into_parts();
    let ctx = InspectionContext::Request(&parts);

    // First panic
    let result1 = forwarder.run_inspector_chain_public(b"test1", ctx).await;
    assert!(matches!(result1, Err(ProxyError::InspectorPanic(_))));

    // Second panic - should still work (recreate context)
    let req2 = Request::builder()
        .method("POST")
        .uri("/test")
        .body(())
        .unwrap();
    let (parts2, _) = req2.into_parts();
    let ctx2 = InspectionContext::Request(&parts2);
    let result2 = forwarder.run_inspector_chain_public(b"test2", ctx2).await;
    assert!(matches!(result2, Err(ProxyError::InspectorPanic(_))));

    // Third panic - should still work (recreate context)
    let req3 = Request::builder()
        .method("POST")
        .uri("/test")
        .body(())
        .unwrap();
    let (parts3, _) = req3.into_parts();
    let ctx3 = InspectionContext::Request(&parts3);
    let result3 = forwarder.run_inspector_chain_public(b"test3", ctx3).await;
    assert!(matches!(result3, Err(ProxyError::InspectorPanic(_))));
}
