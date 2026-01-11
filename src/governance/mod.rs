//! Governance module for ThoughtGate.
//!
//! Implements: REQ-GOV-001 (Task Lifecycle), REQ-GOV-002 (Execution Pipeline),
//!             REQ-GOV-003 (Approval Integration)
//!
//! This module provides task lifecycle management, execution pipeline, and approval
//! integration for human-in-the-loop workflows. In v0.1, ThoughtGate uses **blocking
//! mode** where HTTP connections are held until approval decisions are received.
//!
//! ## Module Organization
//!
//! - `task` - Task lifecycle state machine (REQ-GOV-001)
//! - `pipeline` - Approval execution pipeline (REQ-GOV-002)
//! - `approval` - External approval system integration (REQ-GOV-003)
//!
//! ## v0.1 Constraints
//!
//! - **Blocking mode only** - No SEP-1686 task API methods exposed
//! - **In-memory storage** - Tasks are lost on restart
//! - **Polling model** - Sidecars poll for decisions (no callbacks)
//!
//! ## Future Versions
//!
//! v0.2+ will implement full SEP-1686 with:
//! - `tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel` methods
//! - Task capability advertisement during initialize
//! - Tool annotation rewriting during tools/list

pub mod approval;
pub mod pipeline;
pub mod task;

pub use task::{
    ApprovalDecision, ApprovalRecord, FailureInfo, FailureStage, JsonRpcId, Principal, Task,
    TaskError, TaskId, TaskStatus, TaskStore, TaskStoreConfig, TaskTransition, ToolCallRequest,
    ToolCallResult, hash_request,
};

// Re-export approval types
pub use approval::{
    AdapterError, ApprovalAdapter, ApprovalReference, ApprovalRequest, DecisionMethod,
    PollDecision, PollResult, PollingConfig, PollingScheduler, RateLimiter, SlackAdapter,
    SlackConfig,
};

// Re-export pipeline types
pub use pipeline::{
    ApprovalPipeline, ExecutionPipeline, PipelineConfig, PipelineError, PipelineResult,
    PreHitlResult, TransformDriftMode,
};
