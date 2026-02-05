//! Inspector trait and related types for the Amber Path.
//!
//! # v0.1 Status: DEFERRED
//!
//! This module is **deferred** to v0.2+. The inspector framework is implemented
//! but not active in v0.1 since Amber Path (buffered inspection) is not used.
//!
//! The module defines the core inspection interface for buffered payload analysis.
//! Inspectors can approve, modify (redact), or reject payloads based on policy.
//!
//! # When to Activate (v0.2+)
//!
//! - When PII detection/redaction is needed
//! - When schema validation is required
//! - When request/response transformation is needed
//!
//! # Traceability
//! - Deferred: REQ-CORE-002 F-003 (Async Inspector Interface)
//! - Deferred: REQ-CORE-002 F-004 (Chain Semantics)

use async_trait::async_trait;
use bytes::Bytes;
use http::StatusCode;
use thiserror::Error;

/// Errors that can occur during inspection.
///
/// This is a transport-agnostic error type for the inspector interface.
/// Transport-specific proxies (HTTP, stdio) convert to their own error types.
#[derive(Debug, Error)]
pub enum InspectionError {
    /// Inspector encountered an error during analysis.
    #[error("Inspector '{name}' failed: {reason}")]
    Failed {
        /// Inspector name
        name: String,
        /// Reason for the failure
        reason: String,
    },

    /// Inspector panicked during execution.
    #[error("Inspector '{name}' panicked")]
    Panicked {
        /// Inspector name
        name: String,
    },
}

/// The result of an inspection operation.
///
/// This enum represents the three possible outcomes of inspecting a payload:
/// - `Approve`: Forward the original bytes unchanged (zero-copy).
/// - `Modify`: Forward new/redacted bytes (requires allocation).
/// - `Reject`: Block the request/response with a specific HTTP status code.
///
/// # Traceability
/// - Implements: REQ-CORE-002 F-003 (Decision enum)
#[derive(Debug, Clone)]
pub enum Decision {
    /// Forward the original bytes unchanged.
    ///
    /// This is the zero-copy path - the original buffer is reused.
    Approve,

    /// Forward modified bytes (e.g., after redaction).
    ///
    /// This requires allocating new memory for the modified payload.
    /// The `Content-Length` header MUST be updated to reflect the new size.
    Modify(Bytes),

    /// Block the request/response with the given HTTP status code.
    ///
    /// Common rejection codes:
    /// - `403 Forbidden`: Policy violation
    /// - `400 Bad Request`: Malformed payload
    /// - `422 Unprocessable Entity`: Schema validation failure
    Reject(StatusCode),
}

impl Decision {
    /// Returns `true` if this decision approves the payload.
    pub fn is_approve(&self) -> bool {
        matches!(self, Decision::Approve)
    }

    /// Returns `true` if this decision modifies the payload.
    pub fn is_modify(&self) -> bool {
        matches!(self, Decision::Modify(_))
    }

    /// Returns `true` if this decision rejects the payload.
    pub fn is_reject(&self) -> bool {
        matches!(self, Decision::Reject(_))
    }
}

/// Context provided to inspectors for making decisions.
///
/// This allows inspectors to examine HTTP headers, method, path, etc.
/// without needing to parse the body themselves.
///
/// # Traceability
/// - Implements: REQ-CORE-002 F-003 (InspectionContext)
#[derive(Debug, Clone, Copy)]
pub enum InspectionContext<'a> {
    /// Context for inspecting a request body.
    Request(&'a http::request::Parts),

    /// Context for inspecting a response body.
    Response(&'a http::response::Parts),
}

impl<'a> InspectionContext<'a> {
    /// Returns `true` if this is a request context.
    pub fn is_request(&self) -> bool {
        matches!(self, InspectionContext::Request(_))
    }

    /// Returns `true` if this is a response context.
    pub fn is_response(&self) -> bool {
        matches!(self, InspectionContext::Response(_))
    }

    /// Returns the HTTP headers from the context.
    pub fn headers(&self) -> &http::HeaderMap {
        match self {
            InspectionContext::Request(parts) => &parts.headers,
            InspectionContext::Response(parts) => &parts.headers,
        }
    }

    /// Returns the URI (only valid for requests).
    pub fn uri(&self) -> Option<&http::Uri> {
        match self {
            InspectionContext::Request(parts) => Some(&parts.uri),
            InspectionContext::Response(_) => None,
        }
    }

    /// Returns the HTTP method (only valid for requests).
    pub fn method(&self) -> Option<&http::Method> {
        match self {
            InspectionContext::Request(parts) => Some(&parts.method),
            InspectionContext::Response(_) => None,
        }
    }

    /// Returns the HTTP status code (only valid for responses).
    pub fn status(&self) -> Option<StatusCode> {
        match self {
            InspectionContext::Request(_) => None,
            InspectionContext::Response(parts) => Some(parts.status),
        }
    }
}

/// The core inspector trait for Amber Path payload analysis.
///
/// Implementers receive the payload as `&[u8]` for maximum compatibility.
/// The inspector can analyze the content and return a decision.
///
/// # Chain Semantics (F-004)
///
/// - Inspectors are executed in registration order.
/// - If any inspector returns `Reject`, the chain halts immediately.
/// - If Inspector A returns `Modify(Bytes)`, Inspector B receives the *new* bytes.
/// - If all inspectors return `Approve`, the original buffer is reused (zero-copy).
///
/// # Panic Safety
///
/// Inspector implementations should NOT panic. If a panic occurs, the proxy
/// catches it and returns a `500 Internal Server Error` while logging the
/// inspector name (but NOT the payload content) at `ERROR` level.
///
/// # Example
///
/// ```ignore
/// struct PiiRedactor;
///
/// #[async_trait]
/// impl Inspector for PiiRedactor {
///     fn name(&self) -> &'static str {
///         "pii-redactor"
///     }
///
///     async fn inspect(
///         &self,
///         body: &[u8],
///         ctx: InspectionContext<'_>,
///     ) -> Result<Decision, InspectionError> {
///         // Check for PII patterns and redact if found
///         if contains_pii(body) {
///             let redacted = redact_pii(body);
///             Ok(Decision::Modify(Bytes::from(redacted)))
///         } else {
///             Ok(Decision::Approve)
///         }
///     }
/// }
/// ```
///
/// # Traceability
/// - Implements: REQ-CORE-002 F-003 (Inspector trait)
#[async_trait]
pub trait Inspector: Send + Sync {
    /// Returns the unique name of this inspector.
    ///
    /// Used for logging, metrics, and debugging.
    fn name(&self) -> &'static str;

    /// Inspect the payload and return a decision.
    ///
    /// # Arguments
    ///
    /// * `body` - The payload bytes to inspect. May be empty (`&[]`) for
    ///   requests/responses with `Content-Length: 0`.
    /// * `ctx` - Context containing HTTP headers, method, URI, etc.
    ///
    /// # Returns
    ///
    /// * `Ok(Decision::Approve)` - Forward original bytes (zero-copy)
    /// * `Ok(Decision::Modify(bytes))` - Forward modified bytes
    /// * `Ok(Decision::Reject(status))` - Block with HTTP status
    /// * `Err(InspectionError)` - Internal error (becomes 500)
    async fn inspect(
        &self,
        body: &[u8],
        ctx: InspectionContext<'_>,
    ) -> Result<Decision, InspectionError>;
}

/// A no-op inspector that always approves.
///
/// Useful for testing and as a default when no inspectors are configured.
#[derive(Debug, Clone, Copy)]
pub struct NoOpInspector;

#[async_trait]
impl Inspector for NoOpInspector {
    fn name(&self) -> &'static str {
        "noop"
    }

    async fn inspect(
        &self,
        _body: &[u8],
        _ctx: InspectionContext<'_>,
    ) -> Result<Decision, InspectionError> {
        Ok(Decision::Approve)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{Request, Response};

    #[test]
    fn test_decision_helpers() {
        assert!(Decision::Approve.is_approve());
        assert!(!Decision::Approve.is_modify());
        assert!(!Decision::Approve.is_reject());

        let modify = Decision::Modify(Bytes::from("test"));
        assert!(!modify.is_approve());
        assert!(modify.is_modify());
        assert!(!modify.is_reject());

        let reject = Decision::Reject(StatusCode::FORBIDDEN);
        assert!(!reject.is_approve());
        assert!(!reject.is_modify());
        assert!(reject.is_reject());
    }

    #[test]
    fn test_inspection_context_request() {
        let req = Request::builder()
            .method("POST")
            .uri("/api/test")
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        assert!(ctx.is_request());
        assert!(!ctx.is_response());
        assert_eq!(ctx.method(), Some(&http::Method::POST));
        assert_eq!(ctx.uri().map(|u| u.path()), Some("/api/test"));
        assert!(ctx.status().is_none());
        assert!(ctx.headers().contains_key("content-type"));
    }

    #[test]
    fn test_inspection_context_response() {
        let resp = Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        let (parts, _) = resp.into_parts();
        let ctx = InspectionContext::Response(&parts);

        assert!(!ctx.is_request());
        assert!(ctx.is_response());
        assert!(ctx.method().is_none());
        assert!(ctx.uri().is_none());
        assert_eq!(ctx.status(), Some(StatusCode::OK));
        assert!(ctx.headers().contains_key("content-type"));
    }

    #[tokio::test]
    async fn test_noop_inspector() {
        let inspector = NoOpInspector;

        assert_eq!(inspector.name(), "noop");

        let req = Request::builder().body(()).unwrap();
        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        let result = inspector.inspect(b"test payload", ctx).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_approve());
    }

    /// Test inspector that always rejects for testing purposes.
    struct RejectingInspector(StatusCode);

    #[async_trait]
    impl Inspector for RejectingInspector {
        fn name(&self) -> &'static str {
            "rejecting"
        }

        async fn inspect(
            &self,
            _body: &[u8],
            _ctx: InspectionContext<'_>,
        ) -> Result<Decision, InspectionError> {
            Ok(Decision::Reject(self.0))
        }
    }

    #[tokio::test]
    async fn test_rejecting_inspector() {
        let inspector = RejectingInspector(StatusCode::FORBIDDEN);

        let req = Request::builder().body(()).unwrap();
        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        let result = inspector.inspect(b"", ctx).await.unwrap();
        assert!(matches!(result, Decision::Reject(StatusCode::FORBIDDEN)));
    }

    /// Test inspector that modifies the payload.
    struct ModifyingInspector;

    #[async_trait]
    impl Inspector for ModifyingInspector {
        fn name(&self) -> &'static str {
            "modifying"
        }

        async fn inspect(
            &self,
            body: &[u8],
            _ctx: InspectionContext<'_>,
        ) -> Result<Decision, InspectionError> {
            // Double the payload for testing
            let mut modified = body.to_vec();
            modified.extend_from_slice(body);
            Ok(Decision::Modify(Bytes::from(modified)))
        }
    }

    #[tokio::test]
    async fn test_modifying_inspector() {
        let inspector = ModifyingInspector;

        let req = Request::builder().body(()).unwrap();
        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        let result = inspector.inspect(b"test", ctx).await.unwrap();
        match result {
            Decision::Modify(bytes) => {
                assert_eq!(&bytes[..], b"testtest");
            }
            _ => panic!("Expected Modify decision"),
        }
    }
}
