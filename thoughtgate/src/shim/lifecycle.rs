//! Process lifecycle types for managed MCP server processes.
//!
//! Implements: REQ-CORE-008 §6.6 (Process Lifecycle Types)
//!
//! These types model the state of a child MCP server process spawned by the
//! shim, and the configurable grace periods for the shutdown sequence (F-018).

use std::time::Duration;

/// State of a managed MCP server process.
///
/// Tracks the lifecycle from spawn attempt through running to termination.
/// Used for metrics reporting (`server_state` gauge) and shutdown coordination.
///
/// Implements: REQ-CORE-008 §6.6
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerProcessState {
    /// Process is starting up (spawn called, not yet confirmed running).
    Starting,
    /// Process is running and the stdio proxy is active.
    Running,
    /// Process exited normally with an exit code.
    Exited {
        /// The process exit code.
        code: i32,
    },
    /// Process was killed by a signal (Unix only).
    Signalled {
        /// The signal number that terminated the process.
        signal: i32,
    },
    /// Process failed to start (e.g., command not found).
    FailedToStart {
        /// Human-readable description of the failure.
        reason: String,
    },
}

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

    #[test]
    fn test_server_process_state_variants() {
        let starting = ServerProcessState::Starting;
        let running = ServerProcessState::Running;
        let exited = ServerProcessState::Exited { code: 0 };
        let signalled = ServerProcessState::Signalled { signal: 15 };
        let failed = ServerProcessState::FailedToStart {
            reason: "command not found".to_string(),
        };

        assert_eq!(starting, ServerProcessState::Starting);
        assert_eq!(running, ServerProcessState::Running);
        assert_eq!(exited, ServerProcessState::Exited { code: 0 });
        assert_ne!(exited, ServerProcessState::Exited { code: 1 });
        assert_eq!(signalled, ServerProcessState::Signalled { signal: 15 });
        assert_eq!(
            failed,
            ServerProcessState::FailedToStart {
                reason: "command not found".to_string()
            }
        );
    }
}
