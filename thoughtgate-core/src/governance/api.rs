//! Governance API types for shim↔governance service communication.
//!
//! Implements: REQ-CORE-008 §7.4 F-016 (Governance Integration)
//!
//! These types define the HTTP wire format between stdio shim instances and
//! the governance service running on localhost. Both the shim (client) and
//! governance service (server) import these same types, so wire format changes
//! are caught at compile time.

use serde::{Deserialize, Serialize};

use crate::StreamDirection;
use crate::governance::TaskStatus;
use crate::governance::evaluator::DenySource;
use crate::jsonrpc::JsonRpcId;
use crate::profile::Profile;

/// Request from shim to governance service.
///
/// Sent as `POST /governance/evaluate` body. Contains the classified
/// JSON-RPC message metadata needed for 4-gate governance evaluation.
///
/// Implements: REQ-CORE-008/F-016
#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceEvaluateRequest {
    /// MCP server identifier (from config rewrite).
    pub server_id: String,
    /// Direction of the message (agent→server or server→agent).
    pub direction: StreamDirection,
    /// JSON-RPC method name (e.g., `"tools/call"`).
    pub method: String,
    /// JSON-RPC request/response ID, if present. Used for audit trail correlation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<JsonRpcId>,
    /// Parsed `params` (for requests/notifications) — used for policy evaluation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Classification of the JSON-RPC message.
    pub message_type: MessageType,
    /// Active configuration profile.
    pub profile: Profile,
}

/// Classification of a JSON-RPC message for governance purposes.
///
/// Implements: REQ-CORE-008/F-016
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    /// JSON-RPC request (has `id` and `method`).
    Request,
    /// JSON-RPC response (has `id`, no `method`).
    Response,
    /// JSON-RPC notification (has `method`, no `id`).
    Notification,
}

/// Response from governance service to shim.
///
/// Returned from `POST /governance/evaluate`. The shim uses `decision` to
/// determine whether to forward, deny, or await approval for the message.
///
/// Implements: REQ-CORE-008/F-016
#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceEvaluateResponse {
    /// The governance decision for this message.
    pub decision: GovernanceDecision,
    /// Task ID if an approval workflow was created (`PendingApproval`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Cedar policy ID that triggered the decision (for audit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<String>,
    /// Human-readable reason for the decision (for audit/logging).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Poll interval hint in ms for PendingApproval decisions.
    ///
    /// When `decision` is `PendingApproval`, the shim should poll
    /// `GET /governance/task/{task_id}` at this interval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_interval_ms: Option<u64>,
    /// When true, the shim must initiate graceful shutdown (F-018).
    /// Set by the governance service when `wrap` triggers shutdown (F-019).
    pub shutdown: bool,
    /// Which gate produced a Deny decision (for error code mapping).
    /// Not serialized over the wire — only used in-process by the proxy.
    #[serde(skip)]
    pub deny_source: Option<DenySource>,
}

/// Governance decision for a single message.
///
/// Implements: REQ-CORE-008/F-016
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceDecision {
    /// Message is permitted — forward to destination.
    Forward,
    /// Message is denied — return JSON-RPC error to sender.
    Deny,
    /// Message requires approval — shim must poll `GET /governance/task/{task_id}`.
    PendingApproval,
}

/// Response from `GET /governance/task/{task_id}`.
///
/// Returns the current task status and approval outcome so the shim can
/// determine whether to forward, deny, or continue polling.
///
/// Implements: REQ-CORE-008/F-016
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskStatusResponse {
    /// The task ID being queried.
    pub task_id: String,
    /// Current task lifecycle status.
    pub status: TaskStatus,
    /// Approval outcome, if a terminal decision has been reached.
    /// `None` means the task is still pending approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<ApprovalOutcome>,
    /// Human-readable reason (e.g., rejection reason from approver).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Outcome of an approval workflow as seen by the shim.
///
/// Maps from the internal `ApprovalDecision` + terminal task states into
/// a simplified enum the shim can act on directly.
///
/// Implements: REQ-CORE-008/F-016
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    /// Approval granted — shim should forward the original message.
    Approved,
    /// Approval explicitly rejected by a human reviewer.
    Rejected,
    /// Task expired before a decision was made (TTL elapsed).
    Expired,
    /// Task was cancelled (e.g., by another governance action).
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_governance_request_serde_roundtrip() {
        let req = GovernanceEvaluateRequest {
            server_id: "filesystem".to_string(),
            direction: StreamDirection::AgentToServer,
            method: "tools/call".to_string(),
            id: Some(JsonRpcId::Number(42)),
            params: Some(serde_json::json!({"name": "read_file"})),
            message_type: MessageType::Request,
            profile: Profile::Production,
        };

        let json = serde_json::to_string(&req).unwrap();
        let parsed: GovernanceEvaluateRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.server_id, "filesystem");
        assert_eq!(parsed.direction, StreamDirection::AgentToServer);
        assert_eq!(parsed.method, "tools/call");
        assert_eq!(parsed.message_type, MessageType::Request);
    }

    #[test]
    fn test_governance_response_serde_roundtrip() {
        let resp = GovernanceEvaluateResponse {
            decision: GovernanceDecision::Forward,
            task_id: None,
            policy_id: Some("policy-1".to_string()),
            reason: None,
            poll_interval_ms: None,
            shutdown: false,
            deny_source: None,
        };

        let json = serde_json::to_string(&resp).unwrap();
        let parsed: GovernanceEvaluateResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.decision, GovernanceDecision::Forward);
        assert!(!parsed.shutdown);
        assert_eq!(parsed.policy_id.as_deref(), Some("policy-1"));
    }

    #[test]
    fn test_governance_response_with_shutdown() {
        let resp = GovernanceEvaluateResponse {
            decision: GovernanceDecision::Deny,
            task_id: None,
            policy_id: None,
            reason: Some("service shutting down".to_string()),
            poll_interval_ms: None,
            shutdown: true,
            deny_source: None,
        };

        let json = serde_json::to_string(&resp).unwrap();
        let parsed: GovernanceEvaluateResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.decision, GovernanceDecision::Deny);
        assert!(parsed.shutdown);
    }

    #[test]
    fn test_governance_response_pending_approval() {
        let resp = GovernanceEvaluateResponse {
            decision: GovernanceDecision::PendingApproval,
            task_id: Some("tg_abc123".to_string()),
            policy_id: Some("require-approval".to_string()),
            reason: None,
            poll_interval_ms: Some(5000),
            shutdown: false,
            deny_source: None,
        };

        let json = serde_json::to_string(&resp).unwrap();
        let parsed: GovernanceEvaluateResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.decision, GovernanceDecision::PendingApproval);
        assert_eq!(parsed.task_id.as_deref(), Some("tg_abc123"));
    }

    #[test]
    fn test_decision_serde_values() {
        assert_eq!(
            serde_json::to_string(&GovernanceDecision::Forward).unwrap(),
            r#""forward""#
        );
        assert_eq!(
            serde_json::to_string(&GovernanceDecision::Deny).unwrap(),
            r#""deny""#
        );
        assert_eq!(
            serde_json::to_string(&GovernanceDecision::PendingApproval).unwrap(),
            r#""pending_approval""#
        );
    }

    #[test]
    fn test_message_type_serde_values() {
        assert_eq!(
            serde_json::to_string(&MessageType::Request).unwrap(),
            r#""request""#
        );
        assert_eq!(
            serde_json::to_string(&MessageType::Response).unwrap(),
            r#""response""#
        );
        assert_eq!(
            serde_json::to_string(&MessageType::Notification).unwrap(),
            r#""notification""#
        );
    }

    #[test]
    fn test_optional_fields_omitted() {
        let resp = GovernanceEvaluateResponse {
            decision: GovernanceDecision::Forward,
            task_id: None,
            policy_id: None,
            reason: None,
            poll_interval_ms: None,
            shutdown: false,
            deny_source: None,
        };

        let json = serde_json::to_string(&resp).unwrap();
        // Optional None fields should be skipped
        assert!(!json.contains("task_id"));
        assert!(!json.contains("policy_id"));
        assert!(!json.contains("reason"));
        assert!(!json.contains("poll_interval_ms"));
        // Required fields always present
        assert!(json.contains("decision"));
        assert!(json.contains("shutdown"));
    }
}
