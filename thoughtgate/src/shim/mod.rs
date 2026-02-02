//! Per-server stdio shim modules.
//!
//! Implements: REQ-CORE-008 §6.5 (Stdio Transport Layer)
//!
//! This module contains the NDJSON framing, message parsing, and smuggling
//! detection logic used by each shim subprocess to proxy between the agent
//! and a single MCP server over stdin/stdout.

pub mod ndjson;
