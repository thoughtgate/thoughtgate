# REQ-GOV-002: Approval Execution Pipeline

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-GOV-002` |
| **Title** | Approval Execution Pipeline |
| **Type** | Governance Component |
| **Status** | Draft |
| **Priority** | **High** |
| **Tags** | `#governance` `#pipeline` `#execution` `#approval` `#blocking` |

## 1. Context & Decision Rationale

This requirement defines the **execution pipeline** for approval-required requests. When a tool call requires human approval, ThoughtGate coordinates the approval workflow and executes the tool upon approval.

### 1.1 Version Scope Overview

| Version | Pipeline Complexity | Features |
|---------|---------------------|----------|
| **v0.2** | **Simple** | Approve → Validate → Forward → Respond |
| v0.3+ | Full | Pre-Amber → Approve → Post-Amber → Forward |

### 1.2 v0.2: Simplified Pipeline

In v0.2, the execution pipeline is minimal because:
- REQ-CORE-002 (Buffered Inspection/Amber) is deferred
- No inspector chain to run
- No transform drift detection needed

```
┌─────────────────────────────────────────────────────────────────┐
│                    v0.2 SIMPLIFIED PIPELINE                     │
│                                                                 │
│   tools/call request                                            │
│         │                                                       │
│         ▼                                                       │
│   ┌─────────────────────────────────────────────────────────┐  │
│   │ 1. APPROVAL WAIT (blocking)                             │  │
│   │    • Post request to Slack                              │  │
│   │    • Wait for reaction (👍/👎)                          │  │
│   │    • Handle timeout                                     │  │
│   └─────────────────────────────────────────────────────────┘  │
│         │                                                       │
│         ├─── Rejected ──► Return -32007                         │
│         │                                                       │
│         ├─── Timeout ───► Execute on_timeout action             │
│         │                                                       │
│         ▼ Approved                                              │
│   ┌─────────────────────────────────────────────────────────┐  │
│   │ 2. VALIDATION                                           │  │
│   │    • Client still connected?                            │  │
│   │    • Approval not expired?                              │  │
│   └─────────────────────────────────────────────────────────┘  │
│         │                                                       │
│         ├─── Invalid ───► Return error                          │
│         │                                                       │
│         ▼ Valid                                                 │
│   ┌─────────────────────────────────────────────────────────┐  │
│   │ 3. FORWARD TO UPSTREAM                                  │  │
│   │    • Send original request to MCP server                │  │
│   │    • Apply execution timeout                            │  │
│   │    • Handle upstream errors                             │  │
│   └─────────────────────────────────────────────────────────┘  │
│         │                                                       │
│         ▼                                                       │
│   ┌─────────────────────────────────────────────────────────┐  │
│   │ 4. RETURN RESPONSE                                      │  │
│   │    • Pass through upstream result                       │  │
│   │    • Or return upstream error                           │  │
│   └─────────────────────────────────────────────────────────┘  │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### 1.3 v0.3+: Full Pipeline (Future)

The full pipeline adds inspection phases:

```
┌─────────────────────────────────────────────────────────────────┐
│                    v0.3+ FULL PIPELINE                          │
│                                                                 │
│   1. PRE-APPROVAL AMBER                                         │
│      • Run inspector chain                                      │
│      • Transform/validate request                               │
│      • Reject invalid requests early                            │
│                                                                 │
│   2. APPROVAL WAIT                                              │
│      • Human sees transformed request                           │
│      • Approves what will actually execute                      │
│                                                                 │
│   3. APPROVAL VALIDATION                                        │
│      • Check approval validity                                  │
│      • Check request hash matches                               │
│                                                                 │
│   4. POLICY RE-EVALUATION                                       │
│      • Re-evaluate with ApprovalGrant context                   │
│      • Detect policy drift                                      │
│                                                                 │
│   5. POST-APPROVAL AMBER                                        │
│      • Run inspector chain again                                │
│      • Detect transform drift                                   │
│                                                                 │
│   6. FORWARD TO UPSTREAM                                        │
│                                                                 │
│   7. RETURN RESPONSE                                            │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

**Why Two Amber Phases? (v0.3+)**

| Phase | Purpose |
|-------|---------|
| Pre-Approval | Don't waste human time on requests that would fail anyway |
| Post-Approval | Catch policy drift, re-validate with current rules |

## 2. Dependencies

| Requirement | Relationship | v0.2 | v0.3+ |
|-------------|--------------|------|-------|
| REQ-CFG-001 | **Receives from** | Workflow config, upstream URL | Same |
| REQ-CORE-002 | **Uses** | ❌ Not used | Amber Path infrastructure |
| REQ-CORE-003 | **Uses** | Upstream forwarding | Same |
| REQ-CORE-004 | **Uses** | Error responses | Same |
| REQ-POL-001 | **Uses** | ❌ Not re-evaluated | Policy re-evaluation |
| REQ-GOV-001 | **Uses** | Pending approval tracking | Task state transitions |
| REQ-GOV-003 | **Coordinates with** | Approval decisions | Same |

## 3. Intent

### 3.1 v0.2 Intent

The system must:
1. Coordinate blocking approval wait (REQ-GOV-001)
2. Validate approval before execution
3. Check client is still connected
4. Forward approved request to upstream
5. Return result or error to agent

### 3.2 v0.3+ Intent

The system must additionally:
1. Run Pre-Approval Amber inspection before approval request
2. Store both original and transformed request
3. Validate approval and re-evaluate policy
4. Run Post-Approval Amber inspection
5. Detect and handle transform drift
6. Forward final request to upstream

## 4. Scope

### 4.1 v0.2 Scope

| Component | Status | Notes |
|-----------|--------|-------|
| Blocking approval coordination | ✅ In Scope | Via REQ-GOV-001 |
| Approval validation | ✅ In Scope | Expiry, client connected |
| Upstream forwarding | ✅ In Scope | With timeout |
| Response handling | ✅ In Scope | Pass through or error |
| Metrics and logging | ✅ In Scope | Observability |
| Pre-Approval Amber | ❌ Out of Scope | v0.3+ |
| Post-Approval Amber | ❌ Out of Scope | v0.3+ |
| Policy re-evaluation | ❌ Out of Scope | v0.3+ |
| Transform drift detection | ❌ Out of Scope | v0.3+ |
| Request hashing | ❌ Out of Scope | v0.3+ |

### 4.2 v0.3+ Scope (Future)

| Component | Status | Notes |
|-----------|--------|-------|
| Pre-Approval Amber phase | In Scope | Transform/validate |
| Request hashing | In Scope | For integrity |
| Policy re-evaluation | In Scope | With ApprovalGrant |
| Post-Approval Amber phase | In Scope | Re-validate |
| Transform drift detection | In Scope | Strict/permissive modes |
| All v0.2 components | In Scope | Enhanced |

## 5. Constraints

### 5.1 v0.2 Configuration

| Setting | Default | Source | Description |
|---------|---------|--------|-------------|
| Execution timeout | 30s | Env var | Max upstream wait |
| Approval validity | Workflow timeout | YAML | From workflow config |

**Environment Variables:**

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTGATE_EXECUTION_TIMEOUT_SECS` | `30` | Upstream execution timeout |

### 5.2 v0.3+ Configuration (Future)

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| Approval validity window | 300s (5 min) | `THOUGHTGATE_APPROVAL_VALIDITY_SECS` |
| Transform drift mode | strict | `THOUGHTGATE_TRANSFORM_DRIFT_MODE` |
| Execution timeout | 30s | `THOUGHTGATE_EXECUTION_TIMEOUT_SECS` |

**Transform Drift Modes (v0.3+):**
| Mode | Behavior |
|------|----------|
| `strict` | Fail if Post-Approval transform differs from Pre-Approval |
| `permissive` | Log warning, continue with new transform |

## 6. Interfaces

### 6.1 v0.2: Pipeline Input/Output

```rust
/// Input to execution pipeline (v0.2)
pub struct PipelineInput {
    /// Original request from agent
    pub request: ToolCallRequest,
    /// Principal making the request
    pub principal: Principal,
    /// Workflow configuration
    pub workflow: HumanWorkflow,
    /// Upstream URL
    pub upstream_url: String,
}

/// Result from execution pipeline (v0.2)
pub enum PipelineResult {
    /// Tool executed successfully
    Success {
        result: serde_json::Value,
    },
    /// Approval rejected
    Rejected {
        reason: Option<String>,
        decided_by: String,
    },
    /// Approval timed out
    Timeout,
    /// Client disconnected during wait
    ClientDisconnected,
    /// Upstream error
    UpstreamError {
        code: i32,
        message: String,
    },
    /// Internal error
    InternalError {
        message: String,
    },
}
```

### 6.2 v0.2: Pipeline Interface

```rust
#[async_trait]
pub trait ExecutionPipeline: Send + Sync {
    /// Execute the full approval pipeline (blocking mode)
    async fn execute(&self, input: PipelineInput) -> PipelineResult;
}
```

### 6.3 v0.2: Pipeline Implementation

```rust
pub struct BlockingPipeline {
    approval_waiter: Arc<dyn ApprovalWaiter>,
    approval_poster: Arc<dyn ApprovalPoster>,
    upstream_client: Arc<UpstreamClient>,
    config: PipelineConfig,
}

pub struct PipelineConfig {
    pub execution_timeout: Duration,
}

#[async_trait]
impl ExecutionPipeline for BlockingPipeline {
    async fn execute(&self, input: PipelineInput) -> PipelineResult {
        // 1. Create pending approval
        let pending = self.create_pending_approval(&input);
        
        // 2. Post to Slack
        if let Err(e) = self.approval_poster.post(&input, &pending.id).await {
            return PipelineResult::InternalError {
                message: format!("Failed to post approval request: {}", e),
            };
        }
        
        // 3. Wait for approval (blocking)
        let outcome = self.approval_waiter.wait_for_approval(&pending).await;
        
        // 4. Handle outcome
        match outcome {
            ApprovalOutcome::Approved => {
                // 5. Validate (client still connected?)
                if !pending.client_connected.load(Ordering::Relaxed) {
                    return PipelineResult::ClientDisconnected;
                }
                
                // 6. Forward to upstream
                self.forward_to_upstream(&input).await
            }
            ApprovalOutcome::Rejected { reason } => {
                PipelineResult::Rejected {
                    reason,
                    decided_by: "approver".to_string(), // TODO: get from decision
                }
            }
            ApprovalOutcome::Timeout => {
                PipelineResult::Timeout
            }
            ApprovalOutcome::ClientDisconnected => {
                PipelineResult::ClientDisconnected
            }
        }
    }
}
```

### 6.4 v0.3+: Full Pipeline Interface (Future Reference)

```rust
#[async_trait]
pub trait ExecutionPipeline: Send + Sync {
    /// Run Pre-Approval Amber phase before approval request
    async fn pre_approval_amber(
        &self,
        request: &ToolCallRequest,
        principal: &Principal,
    ) -> Result<PreAmberResult, PipelineError>;
    
    /// Execute approved task through full pipeline
    async fn execute_approved(
        &self,
        task: &Task,
        approval: &ApprovalRecord,
    ) -> PipelineResult;
}

pub struct PreAmberResult {
    pub transformed_request: ToolCallRequest,
    pub request_hash: String,
}
```

## 7. Behavior Specification

### 7.1 v0.2: Simplified Execution Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                  v0.2 EXECUTION FLOW                            │
└─────────────────────────────────────────────────────────────────┘

  Input: PipelineInput {request, principal, workflow, upstream_url}
         │
         ▼
  ┌───────────────────────────────────────────────────────────────┐
  │ 1. CREATE PENDING APPROVAL                                    │
  │                                                               │
  │    • Generate correlation ID                                  │
  │    • Track client connection state                            │
  │    • Register with PendingApprovalStore                       │
  │                                                               │
  └───────────────────────────────────────────────────────────────┘
         │
         ▼
  ┌───────────────────────────────────────────────────────────────┐
  │ 2. POST APPROVAL REQUEST                                      │
  │                                                               │
  │    • Format message for Slack                                 │
  │    • Include tool name, arguments summary, principal          │
  │    • Send via REQ-GOV-003                                     │
  │                                                               │
  │    If post fails → Return InternalError                       │
  │                                                               │
  └───────────────────────────────────────────────────────────────┘
         │
         ▼
  ┌───────────────────────────────────────────────────────────────┐
  │ 3. WAIT FOR APPROVAL (blocking)                               │
  │                                                               │
  │    Poll for:                                                  │
  │    • Approval decision from Slack polling                     │
  │    • Timeout expiration                                       │
  │    • Client disconnection                                     │
  │                                                               │
  └───────────────────────────────────────────────────────────────┘
         │
         ├─── Rejected ─────► Return PipelineResult::Rejected
         │
         ├─── Timeout ──────► Return PipelineResult::Timeout
         │
         ├─── Disconnected ─► Return PipelineResult::ClientDisconnected
         │
         ▼ Approved
  ┌───────────────────────────────────────────────────────────────┐
  │ 4. VALIDATE APPROVAL                                          │
  │                                                               │
  │    • Check client still connected                             │
  │      (one final check before execution)                       │
  │                                                               │
  │    If disconnected → Return ClientDisconnected                │
  │    (prevents zombie execution)                                │
  │                                                               │
  └───────────────────────────────────────────────────────────────┘
         │
         ▼
  ┌───────────────────────────────────────────────────────────────┐
  │ 5. FORWARD TO UPSTREAM                                        │
  │                                                               │
  │    • Build HTTP request to upstream_url                       │
  │    • Send original request (no transformation in v0.2)        │
  │    • Apply execution timeout                                  │
  │                                                               │
  │    Timeout → Return UpstreamError(-32001)                     │
  │    Error   → Return UpstreamError(code, message)              │
  │                                                               │
  └───────────────────────────────────────────────────────────────┘
         │
         ▼
  ┌───────────────────────────────────────────────────────────────┐
  │ 6. RETURN RESPONSE                                            │
  │                                                               │
  │    Return PipelineResult::Success { result }                  │
  │                                                               │
  └───────────────────────────────────────────────────────────────┘
```

### F-001: Pending Approval Creation (v0.2)

- **F-001.1:** Generate UUID for correlation
- **F-001.2:** Create `Arc<AtomicBool>` for client connection tracking
- **F-001.3:** Register with `PendingApprovalStore` (REQ-GOV-001)
- **F-001.4:** Log creation with correlation ID, tool name, principal

### F-002: Approval Request Posting (v0.2)

- **F-002.1:** Delegate to REQ-GOV-003 for Slack posting
- **F-002.2:** Include correlation ID for later decision matching
- **F-002.3:** Handle posting errors gracefully
- **F-002.4:** Log post success/failure

### F-003: Blocking Wait (v0.2)

- **F-003.1:** Delegate to REQ-GOV-001 `ApprovalWaiter`
- **F-003.2:** Return immediately when any condition triggers
- **F-003.3:** Log outcome with correlation ID and duration

### F-004: Approval Validation (v0.2)

- **F-004.1:** Final check that client is still connected
- **F-004.2:** Prevent zombie execution (tool running with no client)
- **F-004.3:** Log validation result

### F-005: Upstream Forwarding (v0.2)

- **F-005.1:** Build JSON-RPC request for upstream MCP server
- **F-005.2:** Apply configurable execution timeout
- **F-005.3:** Handle upstream connection errors
- **F-005.4:** Handle upstream JSON-RPC errors
- **F-005.5:** Log request/response with correlation ID

```rust
async fn forward_to_upstream(&self, input: &PipelineInput) -> PipelineResult {
    let client = reqwest::Client::new();
    
    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": input.request.name,
            "arguments": input.request.arguments,
        }
    });
    
    let response = match tokio::time::timeout(
        self.config.execution_timeout,
        client.post(&input.upstream_url)
            .json(&request_body)
            .send()
    ).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            return PipelineResult::UpstreamError {
                code: -32000,
                message: format!("Connection failed: {}", e),
            };
        }
        Err(_) => {
            return PipelineResult::UpstreamError {
                code: -32001,
                message: "Execution timeout".to_string(),
            };
        }
    };
    
    // Parse JSON-RPC response
    let json_response: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(e) => {
            return PipelineResult::UpstreamError {
                code: -32002,
                message: format!("Invalid response: {}", e),
            };
        }
    };
    
    // Check for JSON-RPC error
    if let Some(error) = json_response.get("error") {
        return PipelineResult::UpstreamError {
            code: error.get("code").and_then(|c| c.as_i64()).unwrap_or(-32603) as i32,
            message: error.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error").to_string(),
        };
    }
    
    // Return result
    PipelineResult::Success {
        result: json_response.get("result").cloned().unwrap_or(serde_json::Value::Null),
    }
}
```

### F-006: Response Handling (v0.2)

- **F-006.1:** Map `PipelineResult` to JSON-RPC response
- **F-006.2:** Success → return tool result
- **F-006.3:** Rejected → return -32007 error
- **F-006.4:** Timeout → execute `on_timeout` action
- **F-006.5:** UpstreamError → return appropriate error code

```rust
fn pipeline_result_to_response(result: PipelineResult, on_timeout: TimeoutAction) -> JsonRpcResponse {
    match result {
        PipelineResult::Success { result } => {
            JsonRpcResponse::success(result)
        }
        PipelineResult::Rejected { reason, .. } => {
            JsonRpcResponse::error(-32007, "Approval rejected", reason)
        }
        PipelineResult::Timeout => {
            match on_timeout {
                TimeoutAction::Deny => {
                    JsonRpcResponse::error(-32008, "Approval timeout", None)
                }
                // Future: TimeoutAction::Escalate, TimeoutAction::AutoApprove
            }
        }
        PipelineResult::ClientDisconnected => {
            JsonRpcResponse::error(-32603, "Client disconnected", None)
        }
        PipelineResult::UpstreamError { code, message } => {
            JsonRpcResponse::error(code, &message, None)
        }
        PipelineResult::InternalError { message } => {
            JsonRpcResponse::error(-32603, "Internal error", Some(message))
        }
    }
}
```

### 7.2 v0.3+: Full Pipeline Flow (Future Reference)

```
┌─────────────────────────────────────────────────────────────────┐
│                  v0.3+ FULL PIPELINE FLOW                       │
└─────────────────────────────────────────────────────────────────┘

  1. PRE-APPROVAL AMBER
     │
     ├─ Run inspector chain
     ├─ Apply transformations
     ├─ Compute request hash
     └─ If rejected → Return error (no approval needed)
     │
     ▼
  2. CREATE TASK
     │
     ├─ Store original request
     ├─ Store transformed request
     └─ Store request hash
     │
     ▼
  3. POST APPROVAL REQUEST
     │
     └─ Human sees transformed request
     │
     ▼
  4. WAIT FOR APPROVAL
     │
     ├─── Rejected → Task::Rejected
     ├─── Timeout → Task::Expired
     └─── Approved → Continue
     │
     ▼
  5. APPROVAL VALIDATION
     │
     ├─ Check approval not expired
     ├─ Check request hash matches
     └─ Check task in correct state
     │
     ▼
  6. POLICY RE-EVALUATION
     │
     ├─ Evaluate with ApprovalGrant context
     ├─ If still permitted → Continue
     └─ If denied → Fail (policy drift)
     │
     ▼
  7. POST-APPROVAL AMBER
     │
     ├─ Run inspector chain again
     ├─ Compute new hash
     ├─ Compare to stored hash
     └─ If different → Handle transform drift
     │
     ▼
  8. FORWARD TO UPSTREAM
     │
     └─ Send final (possibly re-transformed) request
     │
     ▼
  9. STORE RESULT AND RESPOND
```

## 8. Non-Functional Requirements

### NFR-001: Observability (v0.2)

**Metrics:**
```
thoughtgate_pipeline_executions_total{outcome="success|rejected|timeout|disconnected|upstream_error"}
thoughtgate_pipeline_duration_seconds{stage="total|approval_wait|upstream"}
thoughtgate_upstream_requests_total{status="success|error|timeout"}
thoughtgate_upstream_duration_seconds
```

**Logging:**
```json
{"level":"info","event":"pipeline_start","correlation_id":"abc-123","tool":"delete_user","principal":"app-xyz"}
{"level":"info","event":"approval_posted","correlation_id":"abc-123","channel":"#approvals"}
{"level":"info","event":"approval_received","correlation_id":"abc-123","outcome":"approved","wait_ms":45000}
{"level":"info","event":"upstream_request","correlation_id":"abc-123","url":"http://mcp:8080"}
{"level":"info","event":"upstream_response","correlation_id":"abc-123","status":"success","duration_ms":150}
{"level":"info","event":"pipeline_complete","correlation_id":"abc-123","outcome":"success","total_ms":45200}
```

### NFR-002: Performance (v0.2)

| Metric | Target |
|--------|--------|
| Pipeline overhead (excluding wait) | < 10ms |
| Upstream forwarding overhead | < 5ms |
| Memory per execution | < 1KB |

### NFR-003: Reliability (v0.2)

- No zombie executions (tool never runs if client disconnected)
- Proper cleanup on all exit paths
- Clear error attribution (approval vs upstream vs internal)

## 9. Verification Plan

### 9.1 v0.2 Edge Case Matrix

| Scenario | Expected Behavior | Test ID |
|----------|-------------------|---------|
| Approval approved, upstream succeeds | Return tool result | EC-PIP-001 |
| Approval rejected | Return -32007 | EC-PIP-002 |
| Approval timeout (on_timeout: deny) | Return -32008 | EC-PIP-003 |
| Client disconnects during wait | No execution, cleanup | EC-PIP-004 |
| Client disconnects after approval | No execution | EC-PIP-005 |
| Slack post fails | Return -32603 | EC-PIP-006 |
| Upstream connection fails | Return -32000 | EC-PIP-007 |
| Upstream returns error | Return upstream error | EC-PIP-008 |
| Upstream timeout | Return -32001 | EC-PIP-009 |
| Upstream returns invalid JSON | Return -32002 | EC-PIP-010 |

### 9.2 v0.2 Assertions

**Unit Tests:**
- `test_pipeline_success` — Full success path
- `test_pipeline_rejected` — Rejection handling
- `test_pipeline_timeout` — Timeout handling
- `test_pipeline_client_disconnect_during_wait` — Disconnect during wait
- `test_pipeline_client_disconnect_after_approval` — Disconnect after approval
- `test_upstream_connection_error` — Connection failure
- `test_upstream_timeout` — Execution timeout
- `test_upstream_json_error` — JSON-RPC error from upstream

**Integration Tests:**
- `test_full_pipeline_with_slack` — Real Slack integration
- `test_full_pipeline_with_upstream` — Real upstream MCP server

## 10. v0.3+ Reference: Full Pipeline Specification

This section documents the full pipeline implementation for future reference. **Not implemented in v0.2.**

### 10.1 Pre-Approval Amber Phase (v0.3+)

```rust
async fn pre_approval_amber(
    &self,
    request: &ToolCallRequest,
    principal: &Principal,
) -> Result<PreAmberResult, PipelineError> {
    let context = InspectionContext {
        principal: principal.clone(),
        direction: Direction::Request,
        phase: Phase::PreApproval,
    };
    
    let mut current_body = serde_json::to_vec(request)?;
    
    for inspector in &self.inspectors {
        match inspector.inspect(&current_body, &context).await? {
            InspectorDecision::Pass => continue,
            InspectorDecision::Reject { reason } => {
                return Err(PipelineError::InspectionRejected {
                    inspector: inspector.name().to_string(),
                    reason,
                });
            }
            InspectorDecision::Transform { new_body } => {
                current_body = new_body;
            }
        }
    }
    
    let transformed: ToolCallRequest = serde_json::from_slice(&current_body)?;
    let hash = hash_request(&transformed);
    
    Ok(PreAmberResult {
        transformed_request: transformed,
        request_hash: hash,
    })
}
```

### 10.2 Policy Re-evaluation (v0.3+)

```rust
async fn reevaluate_policy(
    &self,
    task: &Task,
    approval: &ApprovalRecord,
) -> Result<(), PipelineError> {
    let request = CedarRequest {
        principal: task.principal.clone(),
        resource: Resource::ToolCall {
            name: task.original_request.name.clone(),
            arguments: task.original_request.arguments.clone(),
        },
        context: CedarContext {
            approval_grant: Some(ApprovalGrant {
                approved_at: approval.decided_at,
                approved_by: approval.decided_by.clone(),
                valid_until: approval.approval_valid_until,
            }),
            ..Default::default()
        },
    };
    
    match self.policy_engine.evaluate(&request).await {
        CedarDecision::Permit { .. } => Ok(()),
        CedarDecision::Forbid { reason, .. } => {
            Err(PipelineError::PolicyDrift { reason })
        }
    }
}
```

### 10.3 Transform Drift Detection (v0.3+)

```rust
async fn check_transform_drift(
    &self,
    task: &Task,
    new_transformed: &ToolCallRequest,
) -> Result<(), PipelineError> {
    let new_hash = hash_request(new_transformed);
    
    if new_hash != task.request_hash {
        match self.config.transform_drift_mode {
            TransformDriftMode::Strict => {
                return Err(PipelineError::TransformDrift {
                    original_hash: task.request_hash.clone(),
                    new_hash,
                });
            }
            TransformDriftMode::Permissive => {
                warn!(
                    task_id = %task.id,
                    original_hash = %task.request_hash,
                    new_hash = %new_hash,
                    "Transform drift detected (permissive mode)"
                );
            }
        }
    }
    
    Ok(())
}
```

## 11. Definition of Done

### 11.1 v0.2 Definition of Done

- [ ] `PipelineInput` and `PipelineResult` types defined
- [ ] `BlockingPipeline` implementation complete
- [ ] Pending approval creation working
- [ ] Approval request posting via REQ-GOV-003
- [ ] Blocking wait via REQ-GOV-001
- [ ] Client disconnection check before execution
- [ ] Upstream forwarding with timeout
- [ ] Response mapping (success, rejected, timeout, errors)
- [ ] Metrics for all pipeline stages
- [ ] All edge cases (EC-PIP-001 to EC-PIP-010) covered
- [ ] Integration with REQ-GOV-001 and REQ-GOV-003

### 11.2 v0.3+ Definition of Done (Future)

- [ ] Pre-Approval Amber phase implemented
- [ ] Request hashing working
- [ ] Task creation with both requests stored
- [ ] Policy re-evaluation with ApprovalGrant
- [ ] Post-Approval Amber phase implemented
- [ ] Transform drift detection (strict and permissive)
- [ ] Full audit trail in task transitions
