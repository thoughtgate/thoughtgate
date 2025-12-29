//! Error types for the ThoughtGate proxy.

use thiserror::Error;

/// Errors that can occur during proxy operations.
#[derive(Error, Debug)]
pub enum ProxyError {
    /// HTTP protocol error
    #[error("HTTP error: {0}")]
    Http(#[from] hyper::Error),

    /// Invalid URI or target
    #[error("Invalid URI: {0}")]
    InvalidUri(String),

    /// I/O error during connection or streaming
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Connection error to upstream
    #[error("Connection error: {0}")]
    Connection(String),

    /// Timeout error
    #[error("Timeout: {0}")]
    #[allow(dead_code)]
    Timeout(String),
    
    /// Client error from hyper_util
    #[error("Client error: {0}")]
    #[allow(dead_code)]
    Client(String),
}

/// Result type alias for proxy operations.
pub type ProxyResult<T> = Result<T, ProxyError>;

