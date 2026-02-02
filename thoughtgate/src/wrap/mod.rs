//! Config discovery, rewriting, and lifecycle management for `thoughtgate wrap`.
//!
//! Implements: REQ-CORE-008 §6.2 (ConfigAdapter), §6.3 (McpServerEntry),
//!             §6.4 (ShimOptions), §10.2 (ConfigGuard)
//!
//! This module provides agent-specific config file handling (discovery, parsing,
//! rewriting, restoration) and RAII-based config backup/restore guard with
//! advisory file locking.

pub mod config_adapter;
pub mod config_guard;
