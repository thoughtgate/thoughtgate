//! Test fixtures and data builders for integration testing.
//!
//! Provides common test data and configuration builders.
//!
//! Note: Some functions are provided for future test expansion and may not
//! be used yet. They are marked with `#[allow(dead_code)]`.

#![allow(dead_code)]

use serde_json::{Value, json};
use std::time::Duration;

/// Build a minimal YAML configuration for testing.
#[must_use]
pub fn minimal_config_yaml() -> String {
    r#"
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${THOUGHTGATE_UPSTREAM}

governance:
  defaults:
    action: forward
"#
    .to_string()
}

/// Build a configuration with approval workflow.
#[must_use]
pub fn config_with_approval_yaml(workflow_name: &str, pattern: &str) -> String {
    format!(
        r#"
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${{THOUGHTGATE_UPSTREAM}}

governance:
  defaults:
    action: forward

  rules:
    - match: "{pattern}"
      action: approve
      approval: {workflow_name}

approval:
  {workflow_name}:
    destination:
      type: slack
      channel: '#test-approvals'
    timeout: 5m
    on_timeout: deny
"#
    )
}

/// Build a configuration with Cedar policy gate.
#[must_use]
pub fn config_with_policy_yaml(pattern: &str, policy_id: &str) -> String {
    format!(
        r#"
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${{THOUGHTGATE_UPSTREAM}}

governance:
  defaults:
    action: forward

  rules:
    - match: "{pattern}"
      action: policy
      policy_id: {policy_id}
      approval: default

approval:
  default:
    destination:
      type: mock
    timeout: 5m
    on_timeout: deny
"#
    )
}

/// Build a configuration with deny rule.
#[must_use]
pub fn config_with_deny_yaml(pattern: &str) -> String {
    format!(
        r#"
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${{THOUGHTGATE_UPSTREAM}}

governance:
  defaults:
    action: forward

  rules:
    - match: "{pattern}"
      action: deny
"#
    )
}

/// Build a configuration with visibility filter (allowlist).
#[must_use]
pub fn config_with_allowlist_yaml(patterns: &[&str]) -> String {
    let tools_list = patterns
        .iter()
        .map(|p| format!("      - \"{}\"", p))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${{THOUGHTGATE_UPSTREAM}}
    expose:
      mode: allowlist
      tools:
{tools_list}

governance:
  defaults:
    action: forward
"#
    )
}

/// Cedar policy that permits all requests.
#[must_use]
pub fn cedar_permit_all() -> String {
    r#"
permit (
    principal,
    action == ThoughtGate::Action::"tools/call",
    resource
);
"#
    .to_string()
}

/// Cedar policy that denies all requests.
#[must_use]
pub fn cedar_deny_all() -> String {
    r#"
forbid (
    principal,
    action == ThoughtGate::Action::"tools/call",
    resource
);
"#
    .to_string()
}

/// Cedar policy that permits only amounts under a threshold.
#[must_use]
pub fn cedar_deny_large_amounts(threshold: u64) -> String {
    format!(
        r#"
// Permit small transfers
permit (
    principal,
    action == ThoughtGate::Action::"tools/call",
    resource
) when {{
    resource.arguments.amount < {threshold}
}};

// Deny large transfers
forbid (
    principal,
    action == ThoughtGate::Action::"tools/call",
    resource
) when {{
    resource.arguments.amount >= {threshold}
}};
"#
    )
}

/// Cedar policy that permits only during business hours.
#[must_use]
pub fn cedar_business_hours_only() -> String {
    r#"
permit (
    principal,
    action == ThoughtGate::Action::"tools/call",
    resource
) when {
    context.time.hour >= 9 && context.time.hour < 17
};
"#
    .to_string()
}

/// Build a JSON-RPC tools/call request.
#[must_use]
pub fn tools_call_request(name: &str, arguments: Value, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments
        },
        "id": id
    })
}

/// Build a JSON-RPC tools/call request with task metadata.
#[must_use]
pub fn tools_call_async_request(name: &str, arguments: Value, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments,
            "_meta": {
                "task": true
            }
        },
        "id": id
    })
}

/// Build a JSON-RPC tasks/get request.
#[must_use]
pub fn tasks_get_request(task_id: &str, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "tasks/get",
        "params": {
            "task_id": task_id
        },
        "id": id
    })
}

/// Build a JSON-RPC tasks/result request.
#[must_use]
pub fn tasks_result_request(task_id: &str, id: i64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "tasks/result",
        "params": {
            "task_id": task_id
        },
        "id": id
    })
}

/// Well-known error codes from REQ-CORE-004.
pub mod error_codes {
    /// Parse error - invalid JSON
    pub const PARSE_ERROR: i32 = -32700;
    /// Invalid request structure
    pub const INVALID_REQUEST: i32 = -32600;
    /// Method not found
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid method parameters
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal error
    pub const INTERNAL_ERROR: i32 = -32603;

    /// Upstream connection failed
    pub const UPSTREAM_CONN_FAILED: i32 = -32000;
    /// Upstream timeout
    pub const UPSTREAM_TIMEOUT: i32 = -32001;
    /// Upstream error
    pub const UPSTREAM_ERROR: i32 = -32002;

    /// Policy denied (Gate 3)
    pub const POLICY_DENIED: i32 = -32003;

    /// Method not allowed (Gate 2 deny)
    pub const METHOD_NOT_ALLOWED: i32 = -32006;

    /// Approval rejected (Gate 4)
    pub const APPROVAL_REJECTED: i32 = -32007;
    /// Approval timeout (Gate 4)
    pub const APPROVAL_TIMEOUT: i32 = -32008;

    /// Task not found
    pub const TASK_NOT_FOUND: i32 = -32010;
    /// Task result not ready
    pub const TASK_NOT_READY: i32 = -32011;

    /// Rate limited
    pub const RATE_LIMITED: i32 = -32013;

    /// Tool not exposed (Gate 1)
    pub const TOOL_NOT_EXPOSED: i32 = -32015;
}

/// Poll interval for approval tests.
pub const TEST_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Short timeout for tests.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_config_parses() {
        let yaml = minimal_config_yaml();
        assert!(yaml.contains("schema: 1"));
        assert!(yaml.contains("action: forward"));
    }

    #[test]
    fn test_config_with_approval() {
        let yaml = config_with_approval_yaml("default", "delete_*");
        assert!(yaml.contains("match: \"delete_*\""));
        assert!(yaml.contains("action: approve"));
        assert!(yaml.contains("approval: default"));
    }

    #[test]
    fn test_tools_call_request() {
        let req = tools_call_request("test_tool", json!({"key": "value"}), 42);
        assert_eq!(req["method"], "tools/call");
        assert_eq!(req["params"]["name"], "test_tool");
        assert_eq!(req["id"], 42);
    }
}
