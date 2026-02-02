//! SEP-1686 Protocol Types for MCP Task Support.
//!
//! Implements: REQ-CORE-007 (SEP-1686 Protocol Compliance)
//!
//! This module provides:
//! - TaskId with `tg_` prefix format for ThoughtGate-owned tasks
//! - SEP-1686 compliant TaskStatus enum
//! - TaskMetadata for task state representation
//! - Request/response types for `tasks/*` methods
//! - Capability advertisement types
//!
//! ## Task ID Format
//!
//! ThoughtGate-owned tasks use the format: `tg_<nanoid>`
//! - Prefix: "tg_" (3 characters)
//! - Body: 21-character nanoid (alphanumeric)
//! - Example: "tg_V1StGXR8_Z5jdHi6B-myT"
//!
//! ## SEP-1686 Compliance
//!
//! All types serialize to match the SEP-1686 specification exactly:
//! - Status values are lowercase strings
//! - Timestamps are ISO 8601 UTC
//! - Task responses include pollInterval in milliseconds

mod capability;
mod methods;
mod task;

pub use capability::*;
pub use methods::*;
pub use task::*;
