//! Task lifecycle status and state machine.
//!
//! Implements: REQ-GOV-001/§5.1, F-001

use serde::{Deserialize, Serialize};

// ============================================================================
// Task Status
// ============================================================================

/// Task lifecycle status.
///
/// Implements: REQ-GOV-001/§5.1, F-001
///
/// State machine transitions:
/// - Working → InputRequired (pre-approval complete)
/// - Working → Failed (pre-approval inspection failed)
/// - Working → Expired (TTL exceeded during pre-approval)
/// - InputRequired → Executing (approval received)
/// - InputRequired → Rejected (approver rejected)
/// - InputRequired → Cancelled (agent cancelled)
/// - InputRequired → Expired (TTL exceeded)
/// - Executing → Completed (execution succeeded)
/// - Executing → Failed (execution failed)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Request is being processed (pre-approval inspection)
    Working,
    /// Awaiting external input (approval decision)
    InputRequired,
    /// Executing the approved tool call (internal, not visible to agent)
    Executing,
    /// Task completed successfully
    Completed,
    /// Task failed due to error
    Failed,
    /// Approver rejected the request
    Rejected,
    /// Agent cancelled the task
    Cancelled,
    /// TTL exceeded without completion
    Expired,
}

impl TaskStatus {
    /// Returns true if this is a terminal state.
    ///
    /// Implements: REQ-GOV-001/§5.1
    ///
    /// Terminal states are immutable and indicate the task lifecycle has ended.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Rejected | Self::Cancelled | Self::Expired
        )
    }

    /// Returns true if this state should be visible to agents.
    ///
    /// Implements: REQ-GOV-001/§6.2
    ///
    /// The `Executing` state is internal and should be mapped to `Working`
    /// when exposed to agents via SEP-1686.
    #[must_use]
    pub fn is_agent_visible(&self) -> bool {
        !matches!(self, Self::Executing)
    }

    /// Converts to SEP-1686 status string.
    ///
    /// Implements: REQ-GOV-001/§6.2
    #[must_use]
    pub fn to_sep1686(&self) -> &'static str {
        match self {
            Self::Working | Self::Executing => "working",
            Self::InputRequired => "input_required",
            Self::Completed => "completed",
            Self::Failed | Self::Expired | Self::Rejected => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Checks if a transition from this status to another is valid.
    ///
    /// Implements: REQ-GOV-001/F-001.1, F-001.2
    #[must_use]
    pub fn can_transition_to(&self, to: TaskStatus) -> bool {
        matches!(
            (self, to),
            // From Working
            (TaskStatus::Working, TaskStatus::InputRequired)
                | (TaskStatus::Working, TaskStatus::Failed)
                | (TaskStatus::Working, TaskStatus::Expired)
                // From InputRequired
                | (TaskStatus::InputRequired, TaskStatus::Executing)
                | (TaskStatus::InputRequired, TaskStatus::Rejected)
                | (TaskStatus::InputRequired, TaskStatus::Cancelled)
                | (TaskStatus::InputRequired, TaskStatus::Expired)
                // From Executing
                | (TaskStatus::Executing, TaskStatus::Completed)
                | (TaskStatus::Executing, TaskStatus::Failed)
        )
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Working => write!(f, "working"),
            Self::InputRequired => write!(f, "input_required"),
            Self::Executing => write!(f, "executing"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Rejected => write!(f, "rejected"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Expired => write!(f, "expired"),
        }
    }
}

/// Convert internal TaskStatus to SEP-1686 wire format status.
///
/// Implements: REQ-GOV-001/§6.2, REQ-CORE-007/§6.6
///
/// Mappings:
/// - Working, Executing → Working (Executing is internal-only)
/// - InputRequired → InputRequired
/// - Completed → Completed
/// - Failed, Rejected, Expired → Failed (all error terminal states)
/// - Cancelled → Cancelled
impl From<TaskStatus> for crate::protocol::Sep1686Status {
    fn from(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Working | TaskStatus::Executing => Self::Working,
            TaskStatus::InputRequired => Self::InputRequired,
            TaskStatus::Completed => Self::Completed,
            TaskStatus::Failed | TaskStatus::Rejected | TaskStatus::Expired => Self::Failed,
            TaskStatus::Cancelled => Self::Cancelled,
        }
    }
}
