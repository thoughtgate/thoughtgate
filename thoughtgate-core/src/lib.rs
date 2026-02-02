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
//! - Implements: REQ-POL-001 (Cedar Policy Engine)
//! - Implements: REQ-GOV-001 (Task Lifecycle)
//! - Implements: REQ-GOV-002 (Approval Execution Pipeline)
//! - Implements: REQ-GOV-003 (Approval Integration)
//! - Implements: REQ-CFG-001 (Configuration)
//! - Implements: REQ-CORE-005 (Operational Lifecycle — state machine)

pub mod config;
pub mod error;
pub mod governance;
pub mod inspector;
pub mod lifecycle;
pub mod metrics;
pub mod policy;
pub mod protocol;
pub mod transport;
