//! Config discovery, rewriting, and lifecycle management for `thoughtgate wrap`.
//!
//! Implements: REQ-CORE-008 §6.2 (ConfigAdapter), §6.3 (McpServerEntry),
//!             §6.4 (ShimOptions), §7.3 (Agent Launch), §10.2 (ConfigGuard)
//!
//! This module provides agent-specific config file handling (discovery, parsing,
//! rewriting, restoration), RAII-based config backup/restore guard with
//! advisory file locking, and the main `wrap` orchestration.

pub mod agent_launch;
pub mod config_adapter;
pub mod config_guard;

pub use agent_launch::run_wrap;
