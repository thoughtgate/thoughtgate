//! ThoughtGate HTTP+SSE proxy sidecar.
//!
//! This crate contains the HTTP transport layer, admin server,
//! and proxy-specific infrastructure for ThoughtGate.
//!
//! # Traceability
//! - Implements: REQ-CORE-008 §1.6 (3-crate workspace: proxy binary)

pub mod admin;
pub mod error;
pub mod logging_layer;
pub mod mcp_handler;
pub mod ports;
pub mod proxy_body;
pub mod proxy_config;
pub mod proxy_service;
pub mod rate_limiter;
pub mod timeout;
pub mod traffic;
