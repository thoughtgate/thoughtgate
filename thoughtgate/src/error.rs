//! Framing errors for stdio NDJSON transport.
//!
//! Implements: REQ-CORE-008 §6.5.3 (FramingError)
//!
//! These errors cover all failure modes during NDJSON line parsing: size limits,
//! malformed JSON, JSON-RPC version validation, batch rejection, broken pipes,
//! and underlying IO errors.

/// Errors that can occur when parsing an NDJSON-framed JSON-RPC message.
///
/// Each variant maps to a specific failure mode in the stdio transport pipeline.
/// The shim uses these to generate appropriate JSON-RPC error responses or to
/// trigger shutdown on unrecoverable IO failures.
///
/// Implements: REQ-CORE-008 §6.5.3
#[derive(Debug, thiserror::Error)]
pub enum FramingError {
    /// A single NDJSON line exceeds the configured maximum size.
    ///
    /// Checked before JSON parsing to prevent allocation of oversized values.
    /// Default limit: 10 MB (REQ-CORE-008 §5.2).
    #[error("Message exceeds maximum size of {max_bytes} bytes")]
    MessageTooLarge {
        /// The configured maximum message size in bytes.
        max_bytes: usize,
    },

    /// The line is not valid JSON, or its structure is invalid for JSON-RPC.
    #[error("Malformed JSON: {reason}")]
    MalformedJson {
        /// Human-readable description of the parse failure.
        reason: String,
    },

    /// The `jsonrpc` field is absent from the JSON object.
    #[error("Missing required jsonrpc field")]
    MissingVersion,

    /// The `jsonrpc` field is present but not `"2.0"`.
    #[error("Unsupported JSON-RPC version: {version}")]
    UnsupportedVersion {
        /// The version string found in the message.
        version: String,
    },

    /// The stdio pipe closed unexpectedly (EOF or broken pipe).
    #[error("Pipe closed unexpectedly")]
    BrokenPipe,

    /// The message is a JSON array, indicating a JSON-RPC batch request.
    ///
    /// MCP does not support batch requests (F-007b).
    #[error("JSON-RPC batch requests (arrays) are not supported")]
    UnsupportedBatch,

    /// An underlying IO error occurred while reading from stdin/stdout.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
