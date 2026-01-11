//! Governance module for ThoughtGate.
//!
//! Implements: REQ-GOV-001 (Task Lifecycle)
//!
//! This module provides task lifecycle management for approval workflows.
//! In v0.1, ThoughtGate uses **blocking mode** where HTTP connections are held
//! until approval decisions are received. Tasks are used for internal state
//! tracking only.
//!
//! ## v0.1 Constraints
//!
//! - **Blocking mode only** - No SEP-1686 task API methods exposed
//! - **In-memory storage** - Tasks are lost on restart
//! - **Internal tracking** - Tasks track approval state, not exposed to agents
//!
//! ## Future Versions
//!
//! v0.2+ will implement full SEP-1686 with:
//! - `tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel` methods
//! - Task capability advertisement during initialize
//! - Tool annotation rewriting during tools/list

pub mod task;

pub use task::{
    ApprovalDecision, ApprovalRecord, FailureInfo, FailureStage, JsonRpcId, Principal, Task,
    TaskError, TaskId, TaskStatus, TaskStore, TaskStoreConfig, TaskTransition, ToolCallRequest,
    ToolCallResult,
};
