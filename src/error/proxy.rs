//! Error types for the ThoughtGate proxy.
//!
//! # Traceability
//! - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy)
//! - Implements: REQ-CORE-001 F-002 (Fail-Fast Error Propagation)
//! - Implements: REQ-CORE-002 (Buffered Termination Strategy)

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use thiserror::Error;

/// Errors that can occur during proxy operations.
///
/// This enum uses `thiserror` to provide structured error types that preserve
/// type information and enable explicit error handling throughout the proxy.
///
/// # Traceability
/// - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy - error handling)
/// - Implements: REQ-CORE-001 F-002 (Fail-Fast Error Propagation)
/// - Implements: REQ-CORE-002 F-001 (Safe Buffering errors)
/// - Implements: REQ-CORE-002 F-002 (Fail-Closed State)
#[derive(Error, Debug)]
pub enum ProxyError {
    // ─────────────────────────────────────────────────────────────────────────
    // Common Errors
    // ─────────────────────────────────────────────────────────────────────────
    /// HTTP protocol error
    #[error("HTTP error: {0}")]
    Http(#[from] hyper::Error),

    /// Invalid URI or target
    #[error("Invalid URI: {0}")]
    InvalidUri(String),

    /// I/O error during connection or streaming
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    // ─────────────────────────────────────────────────────────────────────────
    // Green Path Errors (REQ-CORE-001)
    // ─────────────────────────────────────────────────────────────────────────
    /// Connection error to upstream (maps to 502 Bad Gateway)
    #[error("Connection error: {0}")]
    Connection(String),

    /// Connection refused by upstream (maps to 502 Bad Gateway)
    #[error("Connection refused: {0}")]
    ConnectionRefused(String),

    /// Timeout error (maps to 504 Gateway Timeout)
    #[error("Timeout: {0}")]
    Timeout(String),

    /// Client disconnected (should close upstream immediately)
    #[error("Client disconnected")]
    ClientDisconnect,

    /// Request timeout (maps to 408 Request Timeout)
    #[error("Request timeout: {0}")]
    RequestTimeout(String),

    /// Client error from hyper_util
    #[error("Client error: {0}")]
    Client(String),

    // ─────────────────────────────────────────────────────────────────────────
    // Amber Path Errors (REQ-CORE-002)
    // ─────────────────────────────────────────────────────────────────────────
    /// Payload exceeds buffer limit (maps to 413 Payload Too Large)
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 5.1 EC-001
    #[error("Payload too large: {0} bytes exceeds limit of {1} bytes")]
    PayloadTooLarge(usize, usize),

    /// Buffer timeout expired (maps to 408 Request Timeout)
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 5.1 EC-002 (Slowloris)
    #[error("Buffer timeout: {0}")]
    BufferTimeout(String),

    /// Semaphore exhausted - too many concurrent buffered requests (maps to 503)
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 5.1 EC-003
    #[error("Service unavailable: too many concurrent buffered requests")]
    BufferSemaphoreExhausted,

    /// Compressed response detected in Amber Path (maps to 502 Bad Gateway)
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 3.3 (Compression Handling)
    /// - Implements: REQ-CORE-002 Section 5.1 EC-004
    #[error("Compressed response not allowed in Amber Path: {0}")]
    CompressedResponse(String),

    /// Inspector rejected the payload (maps to the status code in the decision)
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 F-003 (Decision::Reject)
    #[error("Inspector '{0}' rejected payload")]
    Rejected(String, StatusCode),

    /// Inspector panicked (maps to 500 Internal Server Error)
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 F-002 (Panic Safety)
    /// - Implements: REQ-CORE-002 Section 5.1 EC-007
    #[error("Inspector '{0}' panicked")]
    InspectorPanic(String),

    /// Inspector returned an error (maps to 500 Internal Server Error)
    #[error("Inspector '{0}' error: {1}")]
    InspectorError(String, String),
}

impl ProxyError {
    /// Convert error to HTTP response with appropriate status code.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-001 F-002 (Fail-Fast Error Propagation)
    /// - Implements: REQ-CORE-002 F-002 (Fail-Closed State)
    ///
    /// # Error Mapping (Green Path - REQ-CORE-001)
    /// - `ConnectionRefused` / `Connection` -> 502 Bad Gateway
    /// - `Timeout` -> 504 Gateway Timeout
    /// - `RequestTimeout` -> 408 Request Timeout
    /// - `InvalidUri` -> 400 Bad Request
    ///
    /// # Error Mapping (Amber Path - REQ-CORE-002)
    /// - `PayloadTooLarge` -> 413 Payload Too Large
    /// - `BufferTimeout` -> 408 Request Timeout
    /// - `BufferSemaphoreExhausted` -> 503 Service Unavailable
    /// - `CompressedResponse` -> 502 Bad Gateway
    /// - `Rejected` -> Status code from Decision
    /// - `InspectorPanic` / `InspectorError` -> 500 Internal Server Error
    ///
    /// # Others
    /// - Fallback -> 500 Internal Server Error
    pub fn to_response(&self) -> Response<Full<Bytes>> {
        let (status, message) = match self {
            // Green Path errors
            ProxyError::ConnectionRefused(_) | ProxyError::Connection(_) => (
                StatusCode::BAD_GATEWAY,
                "502 Bad Gateway\n\nFailed to connect to upstream server.",
            ),
            ProxyError::Timeout(_) => (
                StatusCode::GATEWAY_TIMEOUT,
                "504 Gateway Timeout\n\nUpstream server did not respond in time.",
            ),
            ProxyError::RequestTimeout(_) => (
                StatusCode::REQUEST_TIMEOUT,
                "408 Request Timeout\n\nRequest took too long to complete.",
            ),
            ProxyError::InvalidUri(_) => (
                StatusCode::BAD_REQUEST,
                "400 Bad Request\n\nInvalid request URI.",
            ),
            ProxyError::ClientDisconnect => {
                // Client has disconnected - return 400 for consistency, though
                // in practice this response won't be sent since the client is gone
                (
                    StatusCode::BAD_REQUEST,
                    "400 Bad Request\n\nClient disconnected.",
                )
            }

            // Amber Path errors (REQ-CORE-002)
            ProxyError::PayloadTooLarge(_, _) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "413 Payload Too Large\n\nRequest body exceeds maximum allowed size.",
            ),
            ProxyError::BufferTimeout(_) => (
                StatusCode::REQUEST_TIMEOUT,
                "408 Request Timeout\n\nBuffering operation timed out.",
            ),
            ProxyError::BufferSemaphoreExhausted => (
                StatusCode::SERVICE_UNAVAILABLE,
                "503 Service Unavailable\n\nToo many concurrent requests. Please retry later.",
            ),
            ProxyError::CompressedResponse(_) => (
                StatusCode::BAD_GATEWAY,
                "502 Bad Gateway\n\nUpstream returned compressed response which cannot be inspected.",
            ),
            ProxyError::Rejected(_, status) => {
                // Use the status code from the rejection decision
                return Response::builder()
                    .status(*status)
                    .header("Content-Type", "text/plain")
                    .body(Full::new(Bytes::from(format!(
                        "{} {}\n\nRequest rejected by policy.",
                        status.as_u16(),
                        status.canonical_reason().unwrap_or("Unknown")
                    ))))
                    .unwrap_or_else(|_| {
                        Response::builder()
                            .status(*status)
                            .body(Full::new(Bytes::from("Request rejected")))
                            .unwrap()
                    });
            }
            ProxyError::InspectorPanic(_) | ProxyError::InspectorError(_, _) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 Internal Server Error\n\nInspection failed.",
            ),

            // Fallback
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 Internal Server Error\n\nAn internal error occurred.",
            ),
        };

        Response::builder()
            .status(status)
            .header("Content-Type", "text/plain")
            .body(Full::new(Bytes::from(message)))
            .unwrap_or_else(|_| {
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Full::new(Bytes::from("500 Internal Server Error")))
                    .unwrap()
            })
    }

    /// Check if this error indicates an upstream connection failure.
    pub fn is_upstream_error(&self) -> bool {
        matches!(
            self,
            ProxyError::ConnectionRefused(_)
                | ProxyError::Connection(_)
                | ProxyError::Timeout(_)
                | ProxyError::CompressedResponse(_)
        )
    }

    /// Check if this error indicates a client-side issue.
    pub fn is_client_error(&self) -> bool {
        matches!(
            self,
            ProxyError::InvalidUri(_)
                | ProxyError::ClientDisconnect
                | ProxyError::RequestTimeout(_)
                | ProxyError::PayloadTooLarge(_, _)
                | ProxyError::BufferTimeout(_)
        )
    }

    /// Check if this error is an Amber Path error (REQ-CORE-002).
    pub fn is_amber_path_error(&self) -> bool {
        matches!(
            self,
            ProxyError::PayloadTooLarge(_, _)
                | ProxyError::BufferTimeout(_)
                | ProxyError::BufferSemaphoreExhausted
                | ProxyError::CompressedResponse(_)
                | ProxyError::Rejected(_, _)
                | ProxyError::InspectorPanic(_)
                | ProxyError::InspectorError(_, _)
        )
    }

    /// Check if this error is an inspection failure (panic or error).
    pub fn is_inspection_failure(&self) -> bool {
        matches!(
            self,
            ProxyError::InspectorPanic(_) | ProxyError::InspectorError(_, _)
        )
    }
}

/// Result type alias for proxy operations.
pub type ProxyResult<T> = Result<T, ProxyError>;
