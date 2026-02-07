//! Error types for the stdio transport and CLI wrapper.
//!
//! Implements: REQ-CORE-008 §6.5.3 (FramingError), §6.8 (StdioError)
//!
//! `FramingError` covers NDJSON line parsing failures: size limits, malformed
//! JSON, JSON-RPC version validation, batch rejection, broken pipes, and IO.
//!
//! `StdioError` covers higher-level shim and wrapper failures: config discovery,
//! server spawn, governance unavailability, and process lifecycle errors.

use std::path::PathBuf;

use thoughtgate_core::StreamDirection;

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

    /// The message is a JSON array, indicating a JSON-RPC batch request.
    ///
    /// MCP does not support batch requests (F-007b).
    #[error("JSON-RPC batch requests (arrays) are not supported")]
    UnsupportedBatch,

    /// An underlying IO error occurred while reading from stdin/stdout.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// ─────────────────────────────────────────────────────────────────────────────
// Stdio Transport Errors
// ─────────────────────────────────────────────────────────────────────────────

/// Errors specific to the stdio transport and CLI wrapper.
///
/// Covers the full range of failures in the shim proxy and `wrap` command:
/// config discovery/rewrite, server process lifecycle, governance connectivity,
/// and framing errors encountered during proxying.
///
/// Implements: REQ-CORE-008 §6.8
#[derive(Debug, thiserror::Error)]
pub enum StdioError {
    /// Agent type could not be detected from the command name/path.
    #[error("Agent type could not be detected from command: {command}")]
    UnknownAgentType {
        /// The command that was not recognized.
        command: String,
    },

    /// Config file not found at the expected location.
    #[error("Config file not found at {}", path.display())]
    ConfigNotFound {
        /// The path that was checked.
        path: PathBuf,
    },

    /// Config file is locked by another ThoughtGate instance.
    #[error("Config file is locked by another ThoughtGate instance: {}", path.display())]
    ConfigLocked {
        /// The path to the locked config file.
        path: PathBuf,
    },

    /// Failed to parse the agent's config file.
    #[error("Failed to parse config: {reason}")]
    ConfigParseError {
        /// The path to the config file.
        path: PathBuf,
        /// Human-readable description of the parse failure.
        reason: String,
    },

    /// Failed to write the rewritten config file.
    #[error("Failed to write config: {reason}")]
    ConfigWriteError {
        /// The path to the config file.
        path: PathBuf,
        /// Human-readable description of the write failure.
        reason: String,
    },

    /// Server process failed to start.
    #[error("Server process failed to start: {reason}")]
    ServerSpawnError {
        /// The MCP server identifier.
        server_id: String,
        /// Human-readable description of the spawn failure.
        reason: String,
    },

    /// Framing error encountered during proxying.
    #[error("Framing error on {direction:?} stream: {source}")]
    Framing {
        /// The MCP server identifier.
        server_id: String,
        /// Which direction the error occurred on.
        direction: StreamDirection,
        /// The underlying framing error.
        source: FramingError,
    },

    /// Governance service unavailable after readiness polling.
    #[error("Governance service unavailable after readiness polling")]
    GovernanceUnavailable,

    /// An underlying IO error occurred.
    #[error("IO error: {0}")]
    StdioIo(std::io::Error),
}
