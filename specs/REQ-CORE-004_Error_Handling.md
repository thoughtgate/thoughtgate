# REQ-CORE-004: Error Handling

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-CORE-004` |
| **Title** | Error Handling |
| **Type** | Core Mechanic |
| **Status** | Draft |
| **Priority** | **High** |
| **Tags** | `#errors` `#json-rpc` `#red-path` `#reliability` `#4-gate` |

## 1. Context & Decision Rationale

This requirement defines how ThoughtGate handles and communicates errors. Proper error handling is critical for:

1. **Debuggability:** Agents and operators need clear error messages
2. **Reliability:** Graceful degradation rather than crashes
3. **Security:** Don't leak internal details in error messages
4. **Protocol compliance:** JSON-RPC 2.0 error format

**4-Gate Error Sources:**
The 4-gate decision flow (REQ-CORE-003) can produce errors at each gate:

| Gate | Error Source | Error Code |
|------|--------------|------------|
| Gate 1 (Visibility) | Tool not exposed | -32015 |
| Gate 2 (Governance) | `action: deny` matched | -32014 |
| Gate 3 (Cedar) | Policy forbid | -32003 |
| Gate 4 (Approval) | Rejected or timeout | -32007, -32008 |

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-CFG-001 | **Receives from** | Configuration errors |
| REQ-CORE-003 | **Receives from** | Parse errors, upstream errors, gate errors |
| REQ-POL-001 | **Receives from** | Cedar policy denial decisions (Gate 3) |
| REQ-GOV-001 | **Receives from** | Task errors (not found, expired, etc.) |
| REQ-GOV-002 | **Receives from** | Execution pipeline failures |
| REQ-GOV-003 | **Receives from** | Approval errors (Gate 4) |
| All | **Provides to** | Standardized error formatting |

## 3. Intent

The system must:
1. Classify all error conditions into well-defined categories
2. Map errors to appropriate JSON-RPC error codes
3. Provide actionable error messages (without leaking sensitive data)
4. Log errors with full context for debugging
5. Track error metrics for observability
6. Support error recovery where possible

## 4. Scope

### 4.1 In Scope
- JSON-RPC 2.0 error response formatting
- Error classification and categorization
- Error code assignment (including 4-gate errors)
- Error logging with context
- Error metrics
- Gate-specific error handling (visibility, governance, policy, approval)
- Upstream error handling
- Configuration error handling
- Internal error handling
- Panic recovery

### 4.2 Out of Scope
- Business logic for when to error (defined by other REQs)
- Retry logic (handled by callers or specific REQs)
- Circuit breakers (deferred to future version)

## 5. Constraints

### 5.1 JSON-RPC 2.0 Error Codes

**Standard Codes (MUST use):**
| Code | Message | Meaning |
|------|---------|---------|
| -32700 | Parse error | Invalid JSON |
| -32600 | Invalid Request | Not valid JSON-RPC |
| -32601 | Method not found | Method doesn't exist |
| -32602 | Invalid params | Invalid method parameters |
| -32603 | Internal error | Internal server error |

**Server Error Range (MUST use for custom errors):**
| Range | Usage |
|-------|-------|
| -32000 to -32099 | Server errors (ThoughtGate-defined) |

### 5.2 ThoughtGate Custom Error Codes

| Code | Name | Gate | Meaning |
|------|------|------|---------|
| -32000 | Upstream Connection Failed | - | Cannot connect to MCP server |
| -32001 | Upstream Timeout | - | MCP server didn't respond in time |
| -32002 | Upstream Error | - | MCP server returned error |
| -32003 | Policy Denied | 3 | Cedar policy forbid |
| -32004 | Task Not Found | - | Invalid task ID |
| -32005 | Task Expired | - | Task TTL exceeded |
| -32006 | Task Cancelled | - | Task was cancelled |
| -32007 | Approval Rejected | 4 | Human rejected the request |
| -32008 | Approval Timeout | 4 | Approval window expired |
| -32009 | Rate Limited | - | Too many requests |
| -32010 | Inspection Failed | - | Amber path inspector rejected |
| -32011 | Policy Drift | - | Policy changed, denying approved request |
| -32012 | Transform Drift | - | Request changed during approval |
| -32013 | Service Unavailable | - | ThoughtGate overloaded |
| -32014 | Governance Rule Denied | 2 | `action: deny` matched in YAML |
| -32015 | Tool Not Exposed | 1 | Tool hidden by `expose` config |
| -32016 | Configuration Error | - | Invalid configuration |
| -32017 | Workflow Not Found | 4 | Approval workflow not defined |

### 5.3 Error Message Guidelines

**DO:**
- Be specific about what failed
- Indicate which gate rejected the request
- Suggest remediation when possible
- Include request correlation ID
- Use consistent terminology

**DON'T:**
- Expose internal implementation details
- Include stack traces in responses
- Reveal policy rules or Cedar internals
- Include sensitive data (arguments, credentials)
- Leak configuration details

## 6. Interfaces

### 6.1 Input: Error Conditions

```rust
/// All error types that can occur in ThoughtGate
pub enum ThoughtGateError {
    // ═══════════════════════════════════════════════════════════
    // Protocol errors (from REQ-CORE-003)
    // ═══════════════════════════════════════════════════════════
    ParseError { details: String },
    InvalidRequest { details: String },
    MethodNotFound { method: String },
    InvalidParams { details: String },
    
    // ═══════════════════════════════════════════════════════════
    // Gate 1: Visibility errors (from REQ-CFG-001)
    // ═══════════════════════════════════════════════════════════
    ToolNotExposed { 
        tool: String,
        source: String,
    },
    
    // ═══════════════════════════════════════════════════════════
    // Gate 2: Governance rule errors (from REQ-CFG-001)
    // ═══════════════════════════════════════════════════════════
    GovernanceRuleDenied { 
        tool: String,
        rule: Option<String>,  // Pattern that matched
    },
    
    // ═══════════════════════════════════════════════════════════
    // Gate 3: Cedar policy errors (from REQ-POL-001)
    // ═══════════════════════════════════════════════════════════
    PolicyDenied { 
        tool: String, 
        policy_id: Option<String>,
        reason: Option<String>,
    },
    
    // ═══════════════════════════════════════════════════════════
    // Gate 4: Approval errors (from REQ-GOV-003)
    // ═══════════════════════════════════════════════════════════
    ApprovalRejected { 
        tool: String, 
        rejected_by: Option<String>,
        workflow: Option<String>,
    },
    ApprovalTimeout { 
        tool: String, 
        timeout_secs: u64,
        workflow: Option<String>,
    },
    WorkflowNotFound {
        workflow: String,
    },
    
    // ═══════════════════════════════════════════════════════════
    // Upstream errors (from REQ-CORE-003)
    // ═══════════════════════════════════════════════════════════
    UpstreamConnectionFailed { url: String, reason: String },
    UpstreamTimeout { url: String, timeout_secs: u64 },
    UpstreamError { code: i32, message: String },
    
    // ═══════════════════════════════════════════════════════════
    // Task errors (from REQ-GOV-001)
    // ═══════════════════════════════════════════════════════════
    TaskNotFound { task_id: String },
    TaskExpired { task_id: String },
    TaskCancelled { task_id: String },
    
    // ═══════════════════════════════════════════════════════════
    // Pipeline errors (from REQ-GOV-002)
    // ═══════════════════════════════════════════════════════════
    InspectionFailed { inspector: String, reason: String },
    PolicyDrift { task_id: String },
    TransformDrift { task_id: String },
    
    // ═══════════════════════════════════════════════════════════
    // Configuration errors (from REQ-CFG-001)
    // ═══════════════════════════════════════════════════════════
    ConfigurationError { details: String },
    
    // ═══════════════════════════════════════════════════════════
    // Operational errors
    // ═══════════════════════════════════════════════════════════
    RateLimited { retry_after_secs: Option<u64> },
    ServiceUnavailable { reason: String },
    InternalError { correlation_id: String },
}
```

### 6.2 Output: JSON-RPC Error Response

```rust
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<ErrorData>,
}

pub struct ErrorData {
    pub correlation_id: String,
    pub gate: Option<String>,      // Which gate rejected: "visibility", "governance", "policy", "approval"
    pub tool: Option<String>,      // Tool that was being called
    pub details: Option<String>,   // Safe details for debugging
}
```

### 6.3 Error Data Population Rules

The following table specifies when each `ErrorData` field should be populated:

| Error Type | `gate` | `tool` | `details` |
|------------|--------|--------|-----------|
| `ToolNotExposed` | `"visibility"` | Yes | No |
| `GovernanceRuleDenied` | `"governance"` | Yes | Rule pattern (if safe to expose) |
| `PolicyDenied` | `"policy"` | Yes | No (security: don't expose policy internals) |
| `ApprovalRejected` | `"approval"` | Yes | Rejector identity (if approved by user) |
| `ApprovalTimeout` | `"approval"` | Yes | Timeout duration |
| `WorkflowNotFound` | `"approval"` | No | Workflow name |
| `UpstreamConnectionFailed` | `null` | No | URL (without auth params) |
| `UpstreamTimeout` | `null` | No | Timeout value |
| `UpstreamError` | `null` | No | Upstream error message (sanitized) |
| `TaskNotFound` | `null` | No | No |
| `TaskExpired` | `null` | No | TTL duration |
| `TaskCancelled` | `null` | No | No |
| `RateLimited` | `null` | No | `retry_after` value |
| `InspectionFailed` | `null` | No | Inspector name (not reason) |
| `PolicyDrift` | `null` | No | No (security) |
| `TransformDrift` | `null` | No | No (security) |
| `ServiceUnavailable` | `null` | No | Generic reason |
| `ConfigurationError` | `null` | No | Sanitized config details |
| `InternalError` | `null` | No | No (log correlation_id only) |
| `ParseError` | `null` | No | Parse location hint |
| `InvalidRequest` | `null` | No | Validation details |
| `MethodNotFound` | `null` | No | Requested method name |
| `InvalidParams` | `null` | No | Parameter validation details |

**Population Guidelines:**

1. **`gate`:** Only set for gate-specific errors (visibility, governance, policy, approval). Helps agents understand _where_ in the decision flow the error occurred.

2. **`tool`:** Set when the error is associated with a specific tool call. Omit for system-level errors (rate limiting, service unavailable).

3. **`details`:** Provide actionable debugging information while avoiding:
   - Policy rule internals (security)
   - Full stack traces (security)
   - Credentials or tokens (security)
   - Internal implementation details

### 6.4 Error Mapping Implementation

```rust
impl From<ThoughtGateError> for JsonRpcError {
    fn from(err: ThoughtGateError) -> Self {
        match err {
            // Gate 1: Visibility
            ThoughtGateError::ToolNotExposed { tool, .. } => JsonRpcError {
                code: -32015,
                message: format!("Tool '{}' is not available", tool),
                data: Some(ErrorData {
                    gate: Some("visibility".into()),
                    tool: Some(tool),
                    ..Default::default()
                }),
            },
            
            // Gate 2: Governance
            ThoughtGateError::GovernanceRuleDenied { tool, rule } => JsonRpcError {
                code: -32014,
                message: format!("Tool '{}' is denied by governance rules", tool),
                data: Some(ErrorData {
                    gate: Some("governance".into()),
                    tool: Some(tool),
                    details: rule.map(|r| format!("Matched rule: {}", r)),
                    ..Default::default()
                }),
            },
            
            // Gate 3: Cedar Policy
            ThoughtGateError::PolicyDenied { tool, policy_id, .. } => JsonRpcError {
                code: -32003,
                message: format!("Policy denied access to tool '{}'", tool),
                data: Some(ErrorData {
                    gate: Some("policy".into()),
                    tool: Some(tool),
                    // Don't include policy_id or reason in response (security)
                    ..Default::default()
                }),
            },
            
            // Gate 4: Approval
            ThoughtGateError::ApprovalRejected { tool, rejected_by, .. } => JsonRpcError {
                code: -32007,
                message: format!("Approval rejected for tool '{}'", tool),
                data: Some(ErrorData {
                    gate: Some("approval".into()),
                    tool: Some(tool),
                    details: rejected_by.map(|by| format!("Rejected by: {}", by)),
                    ..Default::default()
                }),
            },
            
            ThoughtGateError::ApprovalTimeout { tool, timeout_secs, .. } => JsonRpcError {
                code: -32008,
                message: format!("Approval timeout for tool '{}' after {}s", tool, timeout_secs),
                data: Some(ErrorData {
                    gate: Some("approval".into()),
                    tool: Some(tool),
                    ..Default::default()
                }),
            },
            
            ThoughtGateError::WorkflowNotFound { workflow } => JsonRpcError {
                code: -32017,
                message: format!("Approval workflow '{}' not found", workflow),
                data: Some(ErrorData {
                    gate: Some("approval".into()),
                    details: Some(format!("Check approval.{} in config", workflow)),
                    ..Default::default()
                }),
            },
            
            // Configuration errors
            ThoughtGateError::ConfigurationError { details } => JsonRpcError {
                code: -32016,
                message: "Configuration error".into(),
                data: Some(ErrorData {
                    details: Some(details),
                    ..Default::default()
                }),
            },
            
            // ... other error mappings unchanged
            _ => todo!("Map remaining errors"),
        }
    }
}
```

## 7. Functional Requirements

### F-001: Gate Error Classification

- **F-001.1:** Classify errors by originating gate
- **F-001.2:** Include gate identifier in error data
- **F-001.3:** Log gate-specific context for debugging
- **F-001.4:** Track metrics per gate

### F-002: Error Response Formatting

- **F-002.1:** Format all errors as JSON-RPC 2.0 error responses
- **F-002.2:** Include correlation ID in all error responses
- **F-002.3:** Include gate identifier for gate-related errors
- **F-002.4:** Sanitize error messages (no internal details)

### F-003: Error Logging

```rust
fn log_error(err: &ThoughtGateError, correlation_id: &str) {
    match err {
        ThoughtGateError::ToolNotExposed { tool, source } => {
            warn!(
                correlation_id = %correlation_id,
                gate = "visibility",
                tool = %tool,
                source = %source,
                "Tool not exposed"
            );
        }
        ThoughtGateError::GovernanceRuleDenied { tool, rule } => {
            warn!(
                correlation_id = %correlation_id,
                gate = "governance",
                tool = %tool,
                rule = ?rule,
                "Governance rule denied"
            );
        }
        ThoughtGateError::PolicyDenied { tool, policy_id, reason } => {
            warn!(
                correlation_id = %correlation_id,
                gate = "policy",
                tool = %tool,
                policy_id = ?policy_id,
                reason = ?reason,
                "Cedar policy denied"
            );
        }
        ThoughtGateError::ApprovalRejected { tool, rejected_by, workflow } => {
            info!(
                correlation_id = %correlation_id,
                gate = "approval",
                tool = %tool,
                rejected_by = ?rejected_by,
                workflow = ?workflow,
                "Approval rejected"
            );
        }
        // ... other error logging
        _ => {}
    }
}
```

- **F-003.1:** Log all errors with correlation ID
- **F-003.2:** Log full internal context (not exposed to client)
- **F-003.3:** Use appropriate log levels (error, warn, info)
- **F-003.4:** Include gate information for gate errors

### F-004: Error Metrics

```
thoughtgate_errors_total{code, gate, category}
thoughtgate_gate_denials_total{gate, reason}
```

- **F-004.1:** Count errors by code
- **F-004.2:** Count errors by gate
- **F-004.3:** Track denial reasons per gate

## 8. Non-Functional Requirements

### NFR-001: Observability

**Metrics:**
```
thoughtgate_errors_total{code="-32015", gate="visibility", category="client"}
thoughtgate_errors_total{code="-32014", gate="governance", category="client"}
thoughtgate_errors_total{code="-32003", gate="policy", category="client"}
thoughtgate_errors_total{code="-32007", gate="approval", category="client"}
thoughtgate_errors_total{code="-32008", gate="approval", category="client"}
thoughtgate_errors_total{code="-32000", gate="", category="upstream"}
thoughtgate_gate_denials_total{gate="visibility"}
thoughtgate_gate_denials_total{gate="governance"}
thoughtgate_gate_denials_total{gate="policy"}
thoughtgate_gate_denials_total{gate="approval"}
```

**Logging:**
```json
{"level":"warn","correlation_id":"abc-123","gate":"visibility","tool":"admin_delete","source":"upstream","message":"Tool not exposed"}
{"level":"warn","correlation_id":"def-456","gate":"governance","tool":"delete_all","rule":"*_all","message":"Governance rule denied"}
{"level":"warn","correlation_id":"ghi-789","gate":"policy","tool":"transfer_funds","policy_id":"financial","message":"Cedar policy denied"}
{"level":"info","correlation_id":"jkl-012","gate":"approval","tool":"deploy_prod","rejected_by":"alice","message":"Approval rejected"}
```

### NFR-002: Security

- Never include sensitive data in error responses
- Never include policy rules or Cedar details
- Never include configuration internals
- Log full context internally for debugging

## 9. Testing Requirements

### 9.1 Unit Tests

| Test | Description |
|------|-------------|
| `test_gate1_error_format` | ToolNotExposed maps to -32015 |
| `test_gate2_error_format` | GovernanceRuleDenied maps to -32014 |
| `test_gate3_error_format` | PolicyDenied maps to -32003 |
| `test_gate4_rejected_format` | ApprovalRejected maps to -32007 |
| `test_gate4_timeout_format` | ApprovalTimeout maps to -32008 |
| `test_workflow_not_found_format` | WorkflowNotFound maps to -32017 |
| `test_error_data_includes_gate` | Gate field populated |
| `test_error_data_includes_correlation` | Correlation ID included |
| `test_no_sensitive_data_in_response` | Policy rules not leaked |

### 9.2 Integration Tests

| Test | Description |
|------|-------------|
| `test_gate1_returns_32015` | Blocked tool returns -32015 |
| `test_gate2_returns_32014` | action: deny returns -32014 |
| `test_gate3_returns_32003` | Cedar forbid returns -32003 |
| `test_gate4_reject_returns_32007` | Rejection returns -32007 |
| `test_gate4_timeout_returns_32008` | Timeout returns -32008 |

### 9.3 Edge Case Matrix

| Scenario | Expected Behavior | Test ID |
|----------|-------------------|---------|
| Error during error handling | Return generic -32603 | EC-ERR-001 |
| Very long error message (>1KB) | Truncate to 1KB | EC-ERR-002 |
| Non-UTF8 in upstream error | Replace invalid chars | EC-ERR-003 |
| Missing correlation ID header | Generate new ID | EC-ERR-004 |
| Multiple gates fail (hypothetical) | First gate error wins | EC-ERR-005 |
| Error data contains sensitive info | Scrub before returning | EC-ERR-006 |
