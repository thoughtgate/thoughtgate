//! MCP Transport & Routing layer (transport-agnostic).
//!
//! This module handles JSON-RPC 2.0 message parsing, method routing, and
//! upstream forwarding for MCP (Model Context Protocol) traffic.
//!
//! The HTTP-specific handler (`McpHandler`, `McpServer`) lives in
//! `thoughtgate-proxy`. This module provides the building blocks.
//!
//! # Traceability
//! - Implements: REQ-CORE-003 (MCP Transport & Routing)

pub mod jsonrpc;
pub mod router;
pub mod upstream;

// Re-export core types
pub use jsonrpc::{
    BatchItem, JsonRpcId, JsonRpcRequest, JsonRpcResponse, McpRequest, ParsedRequests, TaskMetadata,
};
pub use router::{McpRouter, RouteTarget, TaskMethod};
pub use upstream::{NoopUpstreamForwarder, UpstreamClient, UpstreamConfig, UpstreamForwarder};
