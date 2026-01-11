//! ThoughtGate - High-performance sidecar proxy library for governing AI traffic.
//!
//! This library provides the core proxy service, error handling, and logging
//! functionality for the ThoughtGate sidecar proxy.
//!
//! # v0.1 Traffic Model (Simplified)
//!
//! Policy evaluates to ONE action:
//!
//! | Action | Behavior |
//! |--------|----------|
//! | **Forward** | Send request to upstream immediately, pass response through |
//! | **Approve** | Block until Slack approval, then forward |
//! | **Reject** | Return JSON-RPC error immediately |
//!
//! All responses are passed through directly in v0.1. No inspection or streaming
//! distinction is required for MCP JSON-RPC responses.
//!
//! # MCP Transport
//!
//! The transport layer (REQ-CORE-003) handles:
//! - JSON-RPC 2.0 parsing and validation
//! - MCP method routing (tools/*, tasks/*, resources/*, prompts/*)
//! - Upstream forwarding with connection pooling
//!
//! # Deferred Capabilities (v0.2+)
//!
//! The following paths are implemented but **deferred** for v0.1:
//!
//! - **Green Path (REQ-CORE-001):** Zero-copy streaming for LLM token streams.
//! - **Amber Path (REQ-CORE-002):** Buffered inspection for PII detection.
//!
//! These will be activated when response inspection or LLM streaming is needed.
//!
//! # Traceability
//! - Implements: REQ-CORE-003 (MCP Transport & Routing)
//! - Implements: REQ-POL-001 (Cedar Policy Engine - 3-way: Forward/Approve/Reject)
//! - Deferred: REQ-CORE-001 (Zero-Copy Streaming)
//! - Deferred: REQ-CORE-002 (Buffered Inspection)

pub mod buffered_forwarder;
pub mod config;
pub mod error;
pub mod governance;
pub mod inspector;
pub mod lifecycle;
pub mod logging_layer;
pub mod metrics;
pub mod policy;
pub mod proxy_body;
pub mod proxy_service;
pub mod timeout;
pub mod transport;
