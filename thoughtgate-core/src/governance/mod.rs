//! Governance module for ThoughtGate.
//!
//! Implements: REQ-GOV-001 (Task Lifecycle), REQ-GOV-002 (Execution Pipeline),
//!             REQ-GOV-003 (Approval Integration)
//!
//! This module provides task lifecycle management, execution pipeline, and approval
//! integration for human-in-the-loop workflows.
//!
//! ## Module Organization
//!
//! - `task` - Task lifecycle state machine (REQ-GOV-001)
//! - `handlers` - SEP-1686 task method handlers (REQ-GOV-001/F-003 through F-006)
//! - `pipeline` - Approval execution pipeline (REQ-GOV-002)
//! - `engine` - Approval engine coordinator (REQ-GOV-002)
//! - `approval` - External approval system integration (REQ-GOV-003)
//! - `api` - Governance API wire types for shim↔service communication (REQ-CORE-008)
//!
//! ## v0.2 Features
//!
//! - **SEP-1686 Task API** - `tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel`
//! - **Async tools/call** - Optional `task` metadata enables async mode
//! - **In-memory storage** - Tasks are lost on restart
//! - **Polling model** - Sidecars poll for decisions (no callbacks)
//! - **Approval Engine** - Coordinates approval workflow (create, poll, execute)

pub mod api;
pub mod approval;
pub mod engine;
pub mod handlers;
pub mod pipeline;
pub mod service;
pub mod task;

pub use task::{
    ApprovalDecision, ApprovalRecord, FailureInfo, FailureStage, JsonRpcId, Principal, Task,
    TaskError, TaskId, TaskStatus, TaskStore, TaskStoreConfig, TaskTransition, ToolCallRequest,
    ToolCallResult, hash_request,
};

// Re-export handler types
pub use handlers::TaskHandler;

// Re-export approval types
pub use approval::{
    AdapterError, ApprovalAdapter, ApprovalReference, ApprovalRequest, DecisionMethod,
    PollDecision, PollResult, PollingConfig, PollingScheduler, SlackAdapter, SlackConfig,
};

// Re-export pipeline types
pub use pipeline::{
    ApprovalPipeline, ExecutionPipeline, PipelineConfig, PipelineError, PipelineResult,
    PreHitlResult, TransformDriftMode,
};

// Re-export engine types
pub use engine::{
    ApprovalEngine, ApprovalEngineConfig, ApprovalEngineError, ApprovalStartResult, TimeoutAction,
};

// Re-export governance API wire types (REQ-CORE-008)
pub use api::{
    GovernanceDecision, GovernanceEvaluateRequest, GovernanceEvaluateResponse, MessageType,
};

// Re-export governance HTTP service types (REQ-CORE-008/F-008, F-016)
pub use service::{
    GovernanceServiceState, HeartbeatRequest, HeartbeatResponse, governance_router,
    start_governance_service,
};
