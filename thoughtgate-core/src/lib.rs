//! ThoughtGate Core — transport-agnostic governance library.
//!
//! This library provides the shared governance engine, Cedar policy evaluation,
//! task lifecycle management, JSON-RPC classification, configuration, telemetry,
//! and error types used by both the HTTP sidecar (`thoughtgate-proxy`) and the
//! CLI wrapper (`thoughtgate`).
//!
//! # Traceability
//! - Implements: REQ-CORE-003 (MCP Transport & Routing — JSON-RPC parsing/routing)
//! - Implements: REQ-CORE-004 (Error Handling)
//! - Implements: REQ-CORE-008 (stdio Transport — shared types)
//! - Implements: REQ-POL-001 (Cedar Policy Engine)
//! - Implements: REQ-GOV-001 (Task Lifecycle)
//! - Implements: REQ-GOV-002 (Approval Execution Pipeline)
//! - Implements: REQ-GOV-003 (Approval Integration)
//! - Implements: REQ-CFG-001 (Configuration)
//! - Implements: REQ-CORE-005 (Operational Lifecycle — state machine)

use serde::{Deserialize, Serialize};

pub mod config;
pub mod error;
pub mod governance;
pub mod inspector;
pub mod jsonrpc;
pub mod lifecycle;
pub mod policy;
pub mod profile;
pub mod protocol;
pub mod telemetry;
pub mod transport;

// ─────────────────────────────────────────────────────────────────────────────
// Shared Transport Types
// ─────────────────────────────────────────────────────────────────────────────

/// Stream direction — shared across transports.
///
/// Both HTTP and stdio transports distinguish inbound (server→agent) from
/// outbound (agent→server) message flow for governance evaluation and
/// audit logging.
///
/// Implements: REQ-CORE-008 §6.8
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamDirection {
    /// Message flowing from agent to MCP server (outbound).
    AgentToServer,
    /// Message flowing from MCP server to agent (inbound).
    ServerToAgent,
}
