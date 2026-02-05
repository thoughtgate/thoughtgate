//! Test helpers for ThoughtGate integration tests.
//!
//! This module provides reusable utilities for testing the governance pipeline:
//! - Mock MCP upstream server
//! - JSON-RPC test client
//! - Test fixtures and data builders
//! - Custom assertions

#![allow(unused_imports)] // Re-exports may not be used by all test files

pub mod fixtures;
pub mod mock_upstream;
pub mod test_client;

pub use fixtures::*;
pub use mock_upstream::*;
pub use test_client::*;
