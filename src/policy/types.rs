//! v0.2 Cedar Policy Engine types.
//!
//! Implements: REQ-POL-001 Section 6 (Interfaces)
//!
//! In v0.2, Cedar is Gate 3 in the 4-Gate model and returns only Permit/Forbid
//! decisions. Routing decisions (Forward/Approve/Deny) are handled by YAML
//! governance rules in Gate 2.
//!
//! # Key Changes from v0.1
//!
//! | v0.1 | v0.2 |
//! |------|------|
//! | Cedar returns Forward/Approve/Reject | Cedar returns Permit/Forbid |
//! | Cedar is primary router | Cedar is Gate 3 (evaluation only) |
//! | No argument inspection | `resource.arguments.*` available |
//! | No time context | `context.time.*` for time-based rules |
//! | No policy_id binding | `context.policy_id` from YAML rules |

use std::collections::HashMap;

use super::Principal;

// ═══════════════════════════════════════════════════════════════════════════
// v0.2 Request Types (REQ-POL-001 §6.1)
// ═══════════════════════════════════════════════════════════════════════════

/// Request from Governance Engine to Cedar (Gate 3).
///
/// Implements: REQ-POL-001/§6.1 (Input: Policy Evaluation Request)
///
/// Created by the Governance Engine when a rule specifies `action: policy`.
/// The `context.policy_id` binds to the YAML rule's `policy_id` field.
#[derive(Debug, Clone)]
pub struct CedarRequest {
    /// Principal making the request (from K8s identity).
    pub principal: Principal,

    /// Resource being accessed (tool call with arguments).
    pub resource: CedarResource,

    /// Context from YAML governance rules.
    pub context: CedarContext,
}

/// v0.2 Resource with arguments for inspection.
///
/// Implements: REQ-POL-001/§6.1 (Resource)
///
/// Unlike v0.1 `Resource`, this includes `arguments` for Cedar policies
/// to inspect tool call parameters (e.g., `resource.arguments.amount < 10000`).
#[derive(Debug, Clone)]
pub enum CedarResource {
    /// MCP tool call with arguments.
    ToolCall {
        /// Tool name (e.g., "transfer_funds").
        name: String,
        /// Source ID from config.
        server: String,
        /// Tool arguments for inspection.
        /// Accessible in Cedar as `resource.arguments.*`.
        arguments: serde_json::Value,
    },

    /// Generic MCP method (non-tool requests).
    McpMethod {
        /// Method name (e.g., "resources/read").
        method: String,
        /// Source ID from config.
        server: String,
    },
}

impl CedarResource {
    /// Get the tool/method name.
    pub fn name(&self) -> &str {
        match self {
            CedarResource::ToolCall { name, .. } => name,
            CedarResource::McpMethod { method, .. } => method,
        }
    }

    /// Get the server/source ID.
    pub fn server(&self) -> &str {
        match self {
            CedarResource::ToolCall { server, .. } => server,
            CedarResource::McpMethod { server, .. } => server,
        }
    }
}

/// Context passed from YAML governance rules to Cedar.
///
/// Implements: REQ-POL-001/§6.1 (CedarContext)
///
/// The `policy_id` is bound from the YAML rule that matched:
/// ```yaml
/// governance:
///   rules:
///     - match: "transfer_*"
///       action: policy
///       policy_id: "financial_transfer"  # ← Bound to context.policy_id
/// ```
#[derive(Debug, Clone)]
pub struct CedarContext {
    /// Policy ID from YAML rule (required for `action: policy`).
    ///
    /// Cedar policies filter by this value:
    /// ```cedar
    /// permit(...) when { context.policy_id == "financial_transfer" && ... };
    /// ```
    pub policy_id: String,

    /// Source ID that matched in Gate 2.
    pub source_id: String,

    /// Current time for time-based rules.
    pub time: TimeContext,
}

/// Time context for time-based Cedar policies.
///
/// Implements: REQ-POL-001/F-004 (Time-Based Rules)
///
/// All times are in UTC. Example policy:
/// ```cedar
/// permit(...)
/// when {
///     context.time.hour >= 9 &&
///     context.time.hour < 17 &&
///     context.time.day_of_week >= 1 &&
///     context.time.day_of_week <= 5
/// };
/// ```
#[derive(Debug, Clone, Copy)]
pub struct TimeContext {
    /// Hour of day (0-23 UTC).
    pub hour: u8,

    /// Day of week (0=Sunday, 6=Saturday).
    pub day_of_week: u8,

    /// Unix timestamp (seconds since epoch).
    pub timestamp: i64,
}

impl TimeContext {
    /// Create a TimeContext from the current UTC time.
    #[must_use]
    pub fn now() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Self::from_timestamp(timestamp)
    }

    /// Create a TimeContext from a Unix timestamp.
    ///
    /// Uses Euclidean division to handle negative timestamps correctly
    /// (timestamps before Unix epoch).
    #[must_use]
    pub fn from_timestamp(timestamp: i64) -> Self {
        // Calculate hour and day_of_week from timestamp
        // Unix epoch (Jan 1, 1970) was a Thursday (day 4)
        let secs_per_day: i64 = 86400;
        let secs_per_hour: i64 = 3600;

        // Use Euclidean division for correct handling of negative timestamps
        let days_since_epoch = timestamp.div_euclid(secs_per_day);
        let secs_today = timestamp.rem_euclid(secs_per_day);

        // Thursday = 4, so add 4 and mod 7 (Euclidean for negative days)
        let day_of_week = (days_since_epoch + 4).rem_euclid(7) as u8;
        let hour = (secs_today / secs_per_hour) as u8;

        Self {
            hour,
            day_of_week,
            timestamp,
        }
    }
}

impl Default for TimeContext {
    fn default() -> Self {
        Self::now()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// v0.2 Decision Types (REQ-POL-001 §6.2)
// ═══════════════════════════════════════════════════════════════════════════

/// Cedar returns Permit or Forbid only.
///
/// Implements: REQ-POL-001/§6.2 (Output: Cedar Decision)
///
/// This is NOT a routing decision. Cedar evaluates conditions and returns:
/// - `Permit` → Continue to Gate 4 (approval workflow from YAML/annotation)
/// - `Forbid` → Deny immediately with -32003 PolicyDenied
///
/// **Default-deny**: If no policy explicitly permits, the result is Forbid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CedarDecision {
    /// Policy permits the request.
    ///
    /// ThoughtGate continues to Gate 4 (approval workflow).
    /// The workflow is determined by:
    /// 1. `@thoughtgate_approval` annotation on determining policy
    /// 2. YAML rule's `approval:` field
    /// 3. Default: "default"
    Permit {
        /// Policy IDs that contributed to the permit decision.
        /// Used to look up `@thoughtgate_approval` annotations.
        determining_policies: Vec<String>,
    },

    /// Policy forbids the request.
    ///
    /// ThoughtGate denies immediately with -32003 PolicyDenied.
    Forbid {
        /// Reason for denial (safe for logging).
        reason: String,
        /// Policy IDs that caused the denial.
        policy_ids: Vec<String>,
    },
}

impl CedarDecision {
    /// Returns `true` if this decision permits the request.
    pub fn is_permit(&self) -> bool {
        matches!(self, CedarDecision::Permit { .. })
    }

    /// Returns `true` if this decision forbids the request.
    pub fn is_forbid(&self) -> bool {
        matches!(self, CedarDecision::Forbid { .. })
    }

    /// Get the determining policy IDs (for Permit decisions).
    pub fn determining_policies(&self) -> &[String] {
        match self {
            CedarDecision::Permit {
                determining_policies,
            } => determining_policies,
            CedarDecision::Forbid { .. } => &[],
        }
    }

    /// Create a default Forbid decision (no matching policy).
    pub fn default_forbid() -> Self {
        CedarDecision::Forbid {
            reason: "No policy permits this action (default-deny)".to_string(),
            policy_ids: vec![],
        }
    }
}

impl Default for CedarDecision {
    /// Default is Forbid (default-deny).
    fn default() -> Self {
        Self::default_forbid()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Policy Annotations (REQ-POL-001 §8.0)
// ═══════════════════════════════════════════════════════════════════════════

/// Cached policy annotations parsed at load time.
///
/// Implements: REQ-POL-001/§8.0 (Using Policy Annotations for Workflow Routing)
///
/// Cedar policies can specify which approval workflow to use:
/// ```cedar
/// @id("high_value_transfer")
/// @thoughtgate_approval("finance")
/// permit(...) when { ... };
/// ```
///
/// At evaluation time, ThoughtGate looks up the workflow for determining policies.
#[derive(Debug, Clone, Default)]
pub struct PolicyAnnotations {
    /// Map from policy ID to approval workflow name.
    ///
    /// Populated at policy load time by parsing `@thoughtgate_approval` annotations.
    pub workflow_mapping: HashMap<String, String>,
}

impl PolicyAnnotations {
    /// Create empty annotations.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the workflow for a set of determining policies.
    ///
    /// Implements: REQ-POL-001/§8.0 (Workflow Resolution Priority)
    ///
    /// Returns the workflow from the first policy that has an annotation,
    /// or `None` if no annotation is found.
    pub fn get_workflow(&self, determining_policies: &[String]) -> Option<&str> {
        for policy_id in determining_policies {
            if let Some(workflow) = self.workflow_mapping.get(policy_id) {
                return Some(workflow.as_str());
            }
        }
        None
    }

    /// Add a workflow mapping for a policy ID.
    pub fn add_workflow(&mut self, policy_id: String, workflow: String) {
        self.workflow_mapping.insert(policy_id, workflow);
    }

    /// Check if any annotations are present.
    pub fn is_empty(&self) -> bool {
        self.workflow_mapping.is_empty()
    }

    /// Number of annotated policies.
    pub fn len(&self) -> usize {
        self.workflow_mapping.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Engine Statistics (REQ-POL-001 §6.3)
// ═══════════════════════════════════════════════════════════════════════════

/// v0.2 Cedar engine statistics.
///
/// Implements: REQ-POL-001/§6.3 (CedarStats)
#[derive(Debug, Clone, Default)]
pub struct CedarStats {
    /// Total number of evaluations.
    pub evaluation_count: u64,

    /// Number of Permit decisions.
    pub permit_count: u64,

    /// Number of Forbid decisions.
    pub forbid_count: u64,

    /// Average evaluation time in microseconds.
    pub avg_eval_time_us: u64,
}

/// Policy information for debugging/observability.
///
/// Implements: REQ-POL-001/§6.3 (PolicyInfo)
#[derive(Debug, Clone, Default)]
pub struct PolicyInfo {
    /// Paths from which policies were loaded.
    pub paths: Vec<std::path::PathBuf>,

    /// Number of policies loaded.
    pub policy_count: usize,

    /// When policies were last reloaded.
    pub last_reload: Option<std::time::SystemTime>,

    /// Number of policies with `@thoughtgate_approval` annotations.
    pub annotated_policy_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─────────────────────────────────────────────────────────────────────────
    // CedarResource tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_cedar_resource_tool_call() {
        let resource = CedarResource::ToolCall {
            name: "transfer_funds".to_string(),
            server: "banking".to_string(),
            arguments: serde_json::json!({
                "amount": 5000,
                "currency": "USD",
                "destination": "account-123"
            }),
        };

        assert_eq!(resource.name(), "transfer_funds");
        assert_eq!(resource.server(), "banking");
    }

    #[test]
    fn test_cedar_resource_mcp_method() {
        let resource = CedarResource::McpMethod {
            method: "resources/read".to_string(),
            server: "upstream".to_string(),
        };

        assert_eq!(resource.name(), "resources/read");
        assert_eq!(resource.server(), "upstream");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TimeContext tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_time_context_from_timestamp() {
        // 2024-01-15 14:30:00 UTC (Monday)
        // This is 1705329000 seconds since epoch
        let time = TimeContext::from_timestamp(1705329000);

        assert_eq!(time.hour, 14);
        assert_eq!(time.day_of_week, 1); // Monday
        assert_eq!(time.timestamp, 1705329000);
    }

    #[test]
    fn test_time_context_epoch() {
        // Unix epoch: Jan 1, 1970 00:00:00 UTC (Thursday)
        let time = TimeContext::from_timestamp(0);

        assert_eq!(time.hour, 0);
        assert_eq!(time.day_of_week, 4); // Thursday
        assert_eq!(time.timestamp, 0);
    }

    #[test]
    fn test_time_context_sunday() {
        // Jan 4, 1970 00:00:00 UTC (Sunday)
        let time = TimeContext::from_timestamp(3 * 86400);

        assert_eq!(time.day_of_week, 0); // Sunday
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CedarDecision tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_cedar_decision_permit() {
        let decision = CedarDecision::Permit {
            determining_policies: vec!["policy1".to_string(), "policy2".to_string()],
        };

        assert!(decision.is_permit());
        assert!(!decision.is_forbid());
        assert_eq!(decision.determining_policies().len(), 2);
    }

    #[test]
    fn test_cedar_decision_forbid() {
        let decision = CedarDecision::Forbid {
            reason: "Amount exceeds limit".to_string(),
            policy_ids: vec!["high_value_block".to_string()],
        };

        assert!(!decision.is_permit());
        assert!(decision.is_forbid());
        assert!(decision.determining_policies().is_empty());
    }

    #[test]
    fn test_cedar_decision_default_is_forbid() {
        let decision = CedarDecision::default();
        assert!(decision.is_forbid());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // PolicyAnnotations tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_policy_annotations_empty() {
        let annotations = PolicyAnnotations::new();

        assert!(annotations.is_empty());
        assert_eq!(annotations.len(), 0);
        assert_eq!(annotations.get_workflow(&["policy1".to_string()]), None);
    }

    #[test]
    fn test_policy_annotations_workflow_lookup() {
        let mut annotations = PolicyAnnotations::new();
        annotations.add_workflow("high_value".to_string(), "finance".to_string());
        annotations.add_workflow("deploy".to_string(), "ops".to_string());

        assert!(!annotations.is_empty());
        assert_eq!(annotations.len(), 2);

        // First matching policy wins
        let policies = vec!["high_value".to_string(), "deploy".to_string()];
        assert_eq!(annotations.get_workflow(&policies), Some("finance"));

        // Single policy lookup
        assert_eq!(
            annotations.get_workflow(&["deploy".to_string()]),
            Some("ops")
        );

        // Unknown policy
        assert_eq!(annotations.get_workflow(&["unknown".to_string()]), None);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CedarRequest tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_cedar_request_construction() {
        let request = CedarRequest {
            principal: Principal {
                app_name: "trading-bot".to_string(),
                namespace: "production".to_string(),
                service_account: "trading-sa".to_string(),
                roles: vec!["trader".to_string()],
            },
            resource: CedarResource::ToolCall {
                name: "execute_trade".to_string(),
                server: "trading-api".to_string(),
                arguments: serde_json::json!({
                    "symbol": "AAPL",
                    "quantity": 100,
                    "action": "buy"
                }),
            },
            context: CedarContext {
                policy_id: "trading_policy".to_string(),
                source_id: "trading-api".to_string(),
                time: TimeContext::from_timestamp(1705329000),
            },
        };

        assert_eq!(request.principal.app_name, "trading-bot");
        assert_eq!(request.resource.name(), "execute_trade");
        assert_eq!(request.context.policy_id, "trading_policy");
    }
}
