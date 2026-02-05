//! Per-server stdio shim modules.
//!
//! Implements: REQ-CORE-008 §6.5 (Stdio Transport Layer), §6.6 (Process
//!             Lifecycle Types), §7.4 (Shim: stdio Proxy)
//!
//! This module contains:
//! - `ndjson` — NDJSON framing, message parsing, and smuggling detection
//! - `lifecycle` — Process lifecycle types (state machine, shutdown config)
//! - `proxy` — Bidirectional stdio proxy with governance evaluation

pub mod lifecycle;
pub mod ndjson;
pub mod proxy;
