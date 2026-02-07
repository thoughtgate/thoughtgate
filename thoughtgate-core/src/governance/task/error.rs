//! Task operation errors.
//!
//! Implements: REQ-GOV-001/§6.5

use std::time::Duration;
use thiserror::Error;

use super::status::TaskStatus;
use super::types::TaskId;

// ============================================================================
// Task Errors
// ============================================================================

/// Errors that can occur during task operations.
///
/// Implements: REQ-GOV-001/§6.5
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TaskError {
    /// Task with the given ID was not found.
    #[error("Task '{task_id}' not found")]
    NotFound {
        /// The task ID that was not found
        task_id: TaskId,
    },

    /// Task has expired.
    #[error("Task '{task_id}' has expired")]
    Expired {
        /// The task ID that expired
        task_id: TaskId,
    },

    /// Task is already in a terminal state.
    #[error("Task '{task_id}' is already in terminal state '{status}'")]
    AlreadyTerminal {
        /// The task ID
        task_id: TaskId,
        /// The current terminal status
        status: TaskStatus,
    },

    /// Invalid state transition.
    #[error("Invalid transition for task '{task_id}': {from} -> {to}")]
    InvalidTransition {
        /// The task ID
        task_id: TaskId,
        /// Current status
        from: TaskStatus,
        /// Attempted new status
        to: TaskStatus,
    },

    /// Concurrent modification detected (optimistic locking failure).
    #[error("Concurrent modification of task '{task_id}': expected {expected}, found {actual}")]
    ConcurrentModification {
        /// The task ID
        task_id: TaskId,
        /// Expected status
        expected: TaskStatus,
        /// Actual status found
        actual: TaskStatus,
    },

    /// Rate limit exceeded for the principal.
    #[error("Rate limit exceeded for principal '{principal}', retry after {retry_after:?}")]
    RateLimited {
        /// The principal that exceeded the limit
        principal: String,
        /// How long to wait before retrying
        retry_after: Duration,
    },

    /// Global capacity exceeded.
    #[error("Global task capacity exceeded")]
    CapacityExceeded,

    /// Result is not yet available.
    #[error("Result not ready for task '{task_id}'")]
    ResultNotReady {
        /// The task ID
        task_id: TaskId,
    },

    /// Internal error.
    #[error("Internal error: {details}")]
    Internal {
        /// Error details
        details: String,
    },
}
