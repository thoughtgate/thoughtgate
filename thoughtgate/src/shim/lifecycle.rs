//! Process lifecycle types for managed MCP server processes.
//!
//! Implements: REQ-CORE-008 §6.6 (Process Lifecycle Types)
//!
//! These types model the state of a child MCP server process spawned by the
//! shim, and the configurable grace periods for the shutdown sequence (F-018).

use std::time::Duration;

/// Shutdown request with configurable grace periods.
///
/// Controls the escalation sequence in F-018:
/// 1. Close server's stdin pipe
/// 2. Wait `stdin_close_grace` for clean exit
/// 3. Send SIGTERM to process group
/// 4. Wait `sigterm_grace`
/// 5. Send SIGKILL
///
/// Implements: REQ-CORE-008 §6.6
#[derive(Debug, Clone)]
pub struct ShutdownRequest {
    /// Time to wait after closing stdin before sending SIGTERM.
    pub stdin_close_grace: Duration,
    /// Time to wait after SIGTERM before sending SIGKILL.
    pub sigterm_grace: Duration,
}

impl Default for ShutdownRequest {
    fn default() -> Self {
        Self {
            stdin_close_grace: Duration::from_secs(5),
            sigterm_grace: Duration::from_secs(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shutdown_request_defaults() {
        let req = ShutdownRequest::default();
        assert_eq!(req.stdin_close_grace, Duration::from_secs(5));
        assert_eq!(req.sigterm_grace, Duration::from_secs(2));
    }
}
