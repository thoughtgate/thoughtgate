//! Error types for the ThoughtGate HTTP proxy layer.
//!
//! # Traceability
//! - Implements: REQ-CORE-003 (MCP Transport)

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use thiserror::Error;

/// Errors that can occur during HTTP proxy operations.
///
/// # Traceability
/// - Implements: REQ-CORE-003 (MCP Transport errors)
#[derive(Error, Debug)]
pub enum ProxyError {
    /// Invalid URI or target
    #[error("Invalid URI: {0}")]
    InvalidUri(String),

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
}

impl ProxyError {
    /// Convert error to HTTP response with appropriate status code.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-003 (Error Propagation)
    pub fn to_response(&self) -> Response<Full<Bytes>> {
        let (status, message) = match self {
            ProxyError::ConnectionRefused(_) | ProxyError::Connection(_) => (
                StatusCode::BAD_GATEWAY,
                "502 Bad Gateway\n\nFailed to connect to upstream server.",
            ),
            ProxyError::Timeout(_) => (
                StatusCode::GATEWAY_TIMEOUT,
                "504 Gateway Timeout\n\nUpstream server did not respond in time.",
            ),
            ProxyError::InvalidUri(_) => (
                StatusCode::BAD_REQUEST,
                "400 Bad Request\n\nInvalid request URI.",
            ),
            ProxyError::ClientDisconnect => (
                StatusCode::BAD_REQUEST,
                "400 Bad Request\n\nClient disconnected.",
            ),
        };

        Response::builder()
            .status(status)
            .header("Content-Type", "text/plain")
            .body(Full::new(Bytes::from(message)))
            .unwrap_or_else(|_| {
                let mut resp = Response::new(Full::new(Bytes::from("500 Internal Server Error")));
                *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                resp
            })
    }
}

/// Result type alias for proxy operations.
pub type ProxyResult<T> = Result<T, ProxyError>;
