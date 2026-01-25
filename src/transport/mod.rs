//! MCP Transport & Routing layer.
//!
//! This module handles JSON-RPC 2.0 message parsing, method routing, and
//! upstream forwarding for MCP (Model Context Protocol) traffic.
//!
//! # Architecture
//!
//! All inbound MCP requests flow through this module:
//! 1. HTTP request received at `POST /mcp/v1`
//! 2. JSON-RPC 2.0 parsing (single, batch, notification)
//! 3. Method routing (Policy, Task Handler, Passthrough)
//! 4. Response correlation and formatting
//!
//! # Traffic Flow
//!
//! ```text
//! ┌─────────────┐     ┌─────────────────────────────────────┐     ┌─────────────┐
//! │  MCP Host   │────▶│           ThoughtGate               │────▶│  MCP Server │
//! │  (Agent)    │◀────│      [This Transport Layer]         │◀────│  (Tools)    │
//! └─────────────┘     └─────────────────────────────────────┘     └─────────────┘
//! ```
//!
//! # Traceability
//! - Implements: REQ-CORE-003 (MCP Transport & Routing)

pub mod jsonrpc;
pub mod router;
pub mod server;
pub mod upstream;

// Re-export core types
pub use jsonrpc::{
    BatchItem, JsonRpcId, JsonRpcRequest, JsonRpcResponse, McpRequest, ParsedRequests, TaskMetadata,
};
pub use router::{McpRouter, RouteTarget, TaskMethod};
pub use server::{McpHandler, McpHandlerConfig, McpServer, McpServerConfig, McpState};
pub use upstream::{UpstreamClient, UpstreamConfig, UpstreamForwarder};
