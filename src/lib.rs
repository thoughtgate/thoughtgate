//! ThoughtGate - High-performance sidecar proxy library for governing AI traffic.
//!
//! This library provides the core proxy service, error handling, and logging
//! functionality for the ThoughtGate sidecar proxy.
//!
//! # Traffic Paths
//!
//! ThoughtGate implements four traffic paths based on policy decisions:
//!
//! - **Green Path (REQ-CORE-001):** Zero-copy streaming for trusted traffic.
//! - **Amber Path (REQ-CORE-002):** Buffered inspection for validation.
//! - **Approval Path (REQ-GOV-001/002/003):** Human-in-the-loop approval.
//! - **Red Path (REQ-CORE-004):** Policy-denied requests.
//!
//! # MCP Transport
//!
//! The transport layer (REQ-CORE-003) handles:
//! - JSON-RPC 2.0 parsing and validation
//! - MCP method routing (tools/*, tasks/*, resources/*, prompts/*)
//! - Upstream forwarding with connection pooling
//!
//! # Traceability
//! - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy)
//! - Implements: REQ-CORE-002 (Buffered Termination Strategy)
//! - Implements: REQ-CORE-003 (MCP Transport & Routing)
//! - Implements: REQ-POL-001 (Cedar Policy Engine)

pub mod buffered_forwarder;
pub mod config;
pub mod error;
pub mod inspector;
pub mod logging_layer;
pub mod metrics;
pub mod policy;
pub mod proxy_body;
pub mod proxy_service;
pub mod timeout;
pub mod transport;
