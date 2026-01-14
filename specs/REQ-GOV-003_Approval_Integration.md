# REQ-GOV-003: Approval Integration

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-GOV-003` |
| **Title** | Approval Integration |
| **Type** | Governance Component |
| **Status** | Draft |
| **Priority** | **High** |
| **Tags** | `#governance` `#approval` `#polling` `#slack` `#integration` `#gate4` |

## 1. Context & Decision Rationale

This requirement defines how ThoughtGate integrates with external approval systems. It implements **Gate 4** of the request decision flow.

### 1.1 Gate 4 in the Decision Flow

```
  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
  │   GATE 1    │    │   GATE 2    │    │   GATE 3    │    │   GATE 4    │
  │ Visibility  │ → │ Governance  │ → │   Cedar     │ → │  Approval   │
  │  (expose)   │    │   (YAML)    │    │  (policy)   │    │  (this REQ) │
  └─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
                           │                  │                  │
                           │                  │                  │
                     action: approve ─────────┼──────────────────►│
                                              │                  │
                     action: policy ──────────│                  │
                           │          Cedar Permit ──────────────►│
```

Gate 4 is invoked when:
- YAML governance rule has `action: approve`, OR
- YAML rule has `action: policy` AND Cedar returns Permit

### 1.2 The Sidecar Networking Challenge

ThoughtGate runs as a **sidecar** inside a Kubernetes pod. This creates a fundamental networking constraint:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    THE CALLBACK PROBLEM                                         │
│                                                                                 │
│   ❌ CALLBACK MODEL (Doesn't work for sidecars)                                │
│                                                                                 │
│   Pod 1: 10.0.0.1 ──webhook──► Slack ──callback──► ??? (which pod?)            │
│   Pod 2: 10.0.0.2 ──webhook──► Slack ──callback──► ???                         │
│   Pod 3: 10.0.0.3 ──webhook──► Slack ──callback──► ???                         │
│                                                                                 │
│   Problem: Slack has no way to route callback to correct sidecar               │
│   Sidecars are not individually addressable from external systems              │
│                                                                                 │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│   ✅ POLLING MODEL (Sidecar-compatible)                                        │
│                                                                                 │
│   Pod 1: 10.0.0.1 ──post message──► Slack ◄───poll for reactions── Pod 1       │
│   Pod 2: 10.0.0.2 ──post message──► Slack ◄───poll for reactions── Pod 2       │
│   Pod 3: 10.0.0.3 ──post message──► Slack ◄───poll for reactions── Pod 3       │
│                                                                                 │
│   Solution: Each sidecar polls for its own task's approval decision           │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 1.3 Design Philosophy

- ThoughtGate posts approval requests to Slack (outbound only)
- ThoughtGate polls Slack API for decisions (no inbound callbacks)
- Each sidecar is self-sufficient (no central coordinator needed)
- Adapters encapsulate polling logic for different systems
- **Workflow configuration loaded from YAML config** (REQ-CFG-001)

**Future Extensibility:**
This architecture supports A2A (Agent-to-Agent) approval in v0.3+—an approval agent monitors a channel and responds via reactions or replies.

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-CFG-001 | **Receives from** | Workflow configuration (`approval.*`) |
| REQ-GOV-001 | **Updates** | Task state on approval decision |
| REQ-GOV-002 | **Triggers** | Execution pipeline on approval |
| REQ-CORE-003 | **Invoked by** | Gate 4 of decision flow |
| REQ-CORE-004 | **Uses** | Error responses for failures |
| REQ-CORE-005 | **Coordinates with** | Shutdown stops polling tasks |

## 3. Intent

The system must:
1. Load approval workflow configuration from YAML config
2. Post approval requests to external systems (Slack)
3. Poll external systems for approval decisions
4. Detect approval via reactions, button clicks, or replies
5. Update task state when decision is detected
6. Provide a Slack adapter as reference implementation
7. Support polling multiple tasks concurrently

## 4. Scope

### 4.1 In Scope (v0.2)
- Load workflow config from YAML (`approval.*`)
- Outbound message posting to Slack
- Polling Slack API for decisions
- Reaction-based approval detection (👍 = approve, 👎 = reject)
- Reply-based approval detection ("approved", "rejected")
- Polling backoff and rate limiting
- Adapter interface for extensibility
- Approval metrics and logging
- `on_timeout` behavior from config

### 4.2 In Scope (v0.3+)
- A2A approval adapter
- Approval chains (sequential approvers)
- Webhook destination with callback
- External approval service integration

### 4.3 Out of Scope
- Inbound webhook/callback endpoints (not needed for polling model)
- Building custom approval UIs
- Teams adapter (future version)
- PagerDuty adapter (future version)

## 5. Constraints

### 5.1 Configuration (from YAML)

Approval workflows are defined in YAML config (REQ-CFG-001):

```yaml
approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
      mention:
        - "@oncall"
    timeout: 10m
    on_timeout: deny

  finance:
    destination:
      type: slack
      channel: "#finance-approvals"
      token_env: SLACK_FINANCE_BOT_TOKEN  # Different bot for finance
    timeout: 30m
    on_timeout: deny
```

**Workflow Selection:**
The workflow name comes from the YAML governance rule's `approval:` field:

```yaml
governance:
  rules:
    - match: "delete_*"
      action: approve
      approval: default     # Uses approval.default config

    - match: "transfer_*"
      action: policy
      policy_id: "financial"
      approval: finance     # Uses approval.finance config if Cedar permits
```

### 5.2 Workflow Configuration Schema

```rust
/// Loaded from YAML approval.* config
#[derive(Debug, Deserialize)]
pub struct HumanWorkflow {
    pub destination: ApprovalDestination,
    pub timeout: Option<Duration>,      // Default: 10m
    pub on_timeout: Option<TimeoutAction>,  // Default: deny
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ApprovalDestination {
    #[serde(rename = "slack")]
    Slack {
        channel: String,
        #[serde(default)]
        token_env: Option<String>,  // Env var name for bot token
        #[serde(default)]
        mention: Option<Vec<String>>,
    },
    
    #[serde(rename = "webhook")]
    Webhook {
        url: String,
        #[serde(default)]
        auth: Option<WebhookAuth>,
    },
    
    #[serde(rename = "cli")]
    Cli,  // For local development
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutAction {
    #[default]
    Deny,
    // v0.3+: Escalate, AutoApprove
}
```

### 5.3 Slack Configuration

Slack bot token is loaded from environment variable specified in config:

| Config Field | Default Env Var | Notes |
|--------------|-----------------|-------|
| `token_env` | `SLACK_BOT_TOKEN` | Env var containing bot token |
| `channel` | (required) | Channel name or ID |
| `mention` | (optional) | Users/groups to @mention |

**Example:**
```yaml
approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
      token_env: SLACK_BOT_TOKEN  # Reads from $SLACK_BOT_TOKEN
```

### 5.4 Polling Configuration

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| Poll interval (base) | 5s | `THOUGHTGATE_APPROVAL_POLL_INTERVAL_SECS` |
| Poll interval (max) | 30s | `THOUGHTGATE_APPROVAL_POLL_MAX_INTERVAL_SECS` |
| Slack API rate limit | 1/sec | `THOUGHTGATE_SLACK_RATE_LIMIT_PER_SEC` |
| Max concurrent polls | 100 | `THOUGHTGATE_MAX_CONCURRENT_POLLS` |

### 5.5 Reaction Configuration

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| Approve reaction | `+1` (👍) | `SLACK_APPROVE_REACTION` |
| Reject reaction | `-1` (👎) | `SLACK_REJECT_REACTION` |

### 5.6 Rate Limiting (CRITICAL)

**⚠️ Slack API Protection**

Slack API has strict rate limits (~1 request/second for most endpoints). With many pending approvals, polling can quickly exhaust limits.

**Rate Limiting Strategy:**
| Component | Limit | Behavior When Exceeded |
|-----------|-------|------------------------|
| API calls | 1 req/sec (tier 3) | Queue and batch |
| Concurrent polls | 100 tasks | Oldest tasks polled first |
| Backoff | Exponential | 5s → 10s → 20s → 30s max |

**Batch Polling Efficiency:**

```rust
impl SlackAdapter {
    /// Efficient batch poll using conversations.history
    /// Returns decisions for ALL pending tasks in the channel with ONE API call
    async fn batch_poll_channel(
        &self,
        channel: &str,
        pending_tasks: &[ApprovalReference],
    ) -> Result<HashMap<TaskId, Option<ApprovalDecision>>, AdapterError> {
        // 1. Fetch recent messages from channel (single API call)
        let history = self.client
            .conversations_history(channel)
            .limit(100)  // Covers most pending approvals
            .await?;
        
        // 2. Build lookup by message timestamp
        let messages: HashMap<&str, &Message> = history.messages
            .iter()
            .map(|m| (m.ts.as_str(), m))
            .collect();
        
        // 3. Check each pending task against fetched messages
        let mut results = HashMap::new();
        for task_ref in pending_tasks {
            if let Some(msg) = messages.get(task_ref.external_id.as_str()) {
                let decision = self.check_reactions(&msg.reactions);
                results.insert(task_ref.task_id.clone(), decision);
            } else {
                results.insert(task_ref.task_id.clone(), None);
            }
        }
        
        Ok(results)
    }
}
```

### 5.7 Security Requirements

- Bot token MUST NOT be logged
- Slack API calls MUST use HTTPS
- User identity from Slack is trusted (Slack authenticates users)
- Token loaded from environment variable, never from config file directly

## 6. Interfaces

### 6.1 Workflow Loading

```rust
/// Load workflow configuration from YAML config
pub fn load_workflow(
    config: &Config,
    workflow_name: &str,
) -> Result<&HumanWorkflow, ApprovalError> {
    config.approval
        .as_ref()
        .ok_or(ApprovalError::NoApprovalConfig)?
        .get(workflow_name)
        .ok_or(ApprovalError::WorkflowNotFound { 
            name: workflow_name.to_string() 
        })
}

/// Create adapter from workflow configuration
pub fn create_adapter(
    workflow: &HumanWorkflow,
) -> Result<Box<dyn ApprovalAdapter>, ApprovalError> {
    match &workflow.destination {
        ApprovalDestination::Slack { channel, token_env, mention } => {
            let token_var = token_env.as_deref().unwrap_or("SLACK_BOT_TOKEN");
            let token = std::env::var(token_var)
                .map_err(|_| ApprovalError::MissingToken { var: token_var.to_string() })?;
            
            Ok(Box::new(SlackAdapter::new(
                token,
                channel.clone(),
                mention.clone(),
            )))
        }
        ApprovalDestination::Webhook { url, auth } => {
            Ok(Box::new(WebhookAdapter::new(url.clone(), auth.clone())))
        }
        ApprovalDestination::Cli => {
            Ok(Box::new(CliAdapter::new()))
        }
    }
}
```

### 6.2 Adapter Interface

```rust
#[async_trait]
pub trait ApprovalAdapter: Send + Sync {
    /// Post approval request to external system
    /// Returns external reference for polling
    async fn post_approval_request(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalReference, AdapterError>;
    
    /// Poll for decision on a pending approval
    /// Returns None if still pending
    async fn poll_for_decision(
        &self,
        reference: &ApprovalReference,
    ) -> Result<Option<ApprovalDecision>, AdapterError>;
    
    /// Cancel a pending approval (best-effort)
    async fn cancel_approval(
        &self,
        reference: &ApprovalReference,
    ) -> Result<(), AdapterError>;
    
    /// Adapter name for logging/metrics
    fn name(&self) -> &'static str;
}

pub struct ApprovalRequest {
    pub task_id: TaskId,
    pub tool_name: String,
    pub tool_arguments: serde_json::Value,
    pub principal: Principal,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub correlation_id: String,
    pub workflow_name: String,  // For logging/audit
}

pub struct ApprovalReference {
    pub task_id: TaskId,
    pub external_id: String,      // e.g., Slack message_ts
    pub channel: String,          // e.g., Slack channel ID
    pub posted_at: DateTime<Utc>,
    pub next_poll_at: Instant,
    pub poll_count: u32,
}

pub struct ApprovalDecision {
    pub decision: Decision,
    pub decided_by: String,       // User who reacted/replied
    pub decided_at: DateTime<Utc>,
    pub method: DecisionMethod,
}

pub enum Decision {
    Approved,
    Rejected,
}

pub enum DecisionMethod {
    Reaction { emoji: String },
    Reply { text: String },
}
```

### 6.3 Approval Engine Interface

```rust
/// Main approval engine used by Gate 4
pub struct ApprovalEngine {
    config: Arc<ArcSwap<Config>>,
    adapters: HashMap<String, Box<dyn ApprovalAdapter>>,
    polling_scheduler: PollingScheduler,
}

impl ApprovalEngine {
    /// Start approval workflow and return Task ID immediately (v0.2 SEP-1686)
    /// 
    /// This function does NOT block. It:
    /// 1. Creates an ApprovalRequest
    /// 2. Posts to the approval adapter (e.g., Slack)
    /// 3. Spawns a background polling task
    /// 4. Returns the Task ID immediately
    /// 
    /// The background task polls for the decision and updates the task state.
    /// Client retrieves result via `tasks/result` after polling `tasks/get`.
    pub async fn start_approval(
        &self,
        request: &McpRequest,
        workflow_name: &str,
        task_manager: Arc<TaskManager>,
    ) -> Result<TaskId, ApprovalError> {
        let config = self.config.load();
        let workflow = load_workflow(&config, workflow_name)?;
        let adapter = self.get_or_create_adapter(workflow_name, workflow)?;
        
        let timeout = workflow.timeout.unwrap_or(Duration::from_secs(600));
        let on_timeout = workflow.on_timeout.clone().unwrap_or_default();
        
        // Create task ID and approval request
        let task_id = TaskId::new();
        let approval_request = ApprovalRequest {
            task_id: task_id.clone(),
            tool_name: request.tool_name().to_string(),
            tool_arguments: request.arguments().clone(),
            principal: request.principal.clone(),
            expires_at: Utc::now() + chrono::Duration::from_std(timeout).unwrap(),
            created_at: Utc::now(),
            correlation_id: request.correlation_id.to_string(),
            workflow_name: workflow_name.to_string(),
        };
        
        // Post approval request to adapter (e.g., Slack)
        let reference = adapter.post_approval_request(&approval_request).await?;
        
        // Create task in InputRequired state
        task_manager.create_task(
            task_id.clone(),
            request.clone(),
            TaskState::InputRequired,
            timeout,
        ).await?;
        
        // Spawn background polling task - does NOT block the response
        let task_id_clone = task_id.clone();
        let adapter_clone = adapter.clone();
        let task_manager_clone = task_manager.clone();
        let request_clone = request.clone();
        
        tokio::spawn(async move {
            poll_for_approval_decision(
                task_id_clone,
                adapter_clone,
                reference,
                timeout,
                on_timeout,
                task_manager_clone,
                request_clone,
            ).await
        });
        
        // Return Task ID immediately - client will poll via tasks/get
        Ok(task_id)
    }
}

/// Background polling task for approval decision
/// 
/// This runs independently after start_approval returns.
/// Updates task state when decision is received.
async fn poll_for_approval_decision(
    task_id: TaskId,
    adapter: Arc<dyn ApprovalAdapter>,
    reference: ApprovalReference,
    timeout: Duration,
    on_timeout: TimeoutAction,
    task_manager: Arc<TaskManager>,
    original_request: McpRequest,
) {
    let deadline = Instant::now() + timeout;
    let mut poll_interval = Duration::from_secs(5);
    
    loop {
        if Instant::now() >= deadline {
            // Timeout reached
            match on_timeout {
                TimeoutAction::Deny => {
                    task_manager.fail_task(
                        &task_id,
                        "Approval timeout",
                        Some(-32008),
                    ).await;
                }
                // v0.3+: Handle escalate, auto-approve
            }
            return;
        }
        
        tokio::time::sleep(poll_interval).await;
        
        match adapter.poll_for_decision(&reference).await {
            Ok(Some(decision)) => {
                match decision.decision {
                    Decision::Approved { by } => {
                        // Transition to Executing state
                        // Actual upstream execution happens on tasks/result call
                        task_manager.approve_task(&task_id, &by).await;
                    }
                    Decision::Rejected { by, reason } => {
                        task_manager.reject_task(&task_id, &by, reason.as_deref()).await;
                    }
                }
                return;
            }
            Ok(None) => {
                // No decision yet, continue polling
            }
            Err(e) => {
                tracing::warn!(task_id = %task_id, error = %e, "Polling error");
                // Continue polling on transient errors
            }
        }
        
        // Exponential backoff
        poll_interval = (poll_interval * 2).min(Duration::from_secs(30));
    }
}
```

### 6.4 Errors

```rust
pub enum ApprovalError {
    NoApprovalConfig,
    WorkflowNotFound { name: String },
    MissingToken { var: String },
    Timeout,
    Rejected { by: String, reason: Option<String> },
    AdapterError(AdapterError),
}

pub enum AdapterError {
    PostFailed { reason: String, retriable: bool },
    PollFailed { reason: String, retriable: bool },
    RateLimited { retry_after: Duration },
    InvalidToken,
    ChannelNotFound { channel: String },
    MessageNotFound { ts: String },
}
```

## 7. Functional Requirements

### F-001: Workflow Loading

- **F-001.1:** Load workflow config from YAML `approval.*` section
- **F-001.2:** Fall back to `default` workflow if not specified
- **F-001.3:** Error if workflow not found in config
- **F-001.4:** Hot-reload workflow config without restart
- **F-001.5:** Validate workflow config at load time

### F-002: Adapter Selection

- **F-002.1:** Create adapter based on `destination.type`
- **F-002.2:** Load bot token from env var specified in `token_env`
- **F-002.3:** Cache adapters per workflow (reuse connections)
- **F-002.4:** Support multiple adapters for different workflows

### F-003: Approval Request Posting

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     APPROVAL REQUEST FLOW                                       │
│                                                                                 │
│   Gate 4 Invoked (from REQ-CORE-003)                                           │
│         │                                                                       │
│         ▼                                                                       │
│   ┌─────────────────────────────────────────────────────────────────────────┐  │
│   │  1. Load workflow config from YAML                                      │  │
│   │  2. Create/get adapter for destination                                  │  │
│   │  3. Build Slack Block Kit message                                       │  │
│   │  4. Rate limit check (wait if needed)                                   │  │
│   │  5. POST to chat.postMessage                                            │  │
│   │  6. Store channel + ts in ApprovalReference                             │  │
│   │  7. Start polling loop                                                  │  │
│   └─────────────────────────────────────────────────────────────────────────┘  │
│         │                                                                       │
│         ▼                                                                       │
│   Poll until decision or timeout                                               │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

- **F-003.1:** Build Block Kit message from request data
- **F-003.2:** Include @mentions from workflow config
- **F-003.3:** Include task ID in message metadata
- **F-003.4:** Apply rate limiting before API call
- **F-003.5:** Store message reference for polling
- **F-003.6:** Handle posting failures with retry

### F-004: Polling Loop

- **F-004.1:** Poll at configurable interval (default 5s)
- **F-004.2:** Exponential backoff on repeated polls (5s → 10s → 20s → 30s)
- **F-004.3:** Respect rate limits
- **F-004.4:** Exit on timeout (from workflow config)
- **F-004.5:** Return decision when detected

### F-005: Decision Detection

- **F-005.1:** Check for approval reaction (👍 by default)
- **F-005.2:** Check for rejection reaction (👎 by default)
- **F-005.3:** If both reactions present, first one wins
- **F-005.4:** Extract user ID of reactor for audit
- **F-005.5:** Support reply-based decisions as fallback

**Decision Priority:**
| Check Order | Signal | Decision |
|-------------|--------|----------|
| 1 | 👍 reaction | Approved |
| 2 | 👎 reaction | Rejected |
| 3 | Reply containing "approved" | Approved |
| 4 | Reply containing "rejected" | Rejected |

### F-006: Timeout Handling

- **F-006.1:** Read timeout from workflow config
- **F-006.2:** Read `on_timeout` action from config
- **F-006.3:** Execute `on_timeout` action when deadline reached
- **F-006.4:** v0.2: Only `deny` supported
- **F-006.5:** v0.3+: Support `escalate`, `auto_approve`

### F-007: Slack Message Format

```
┌───────────────────────────────────────────────────────────────────────────┐
│ 🔑 Approval Required: delete_user                                           │
├───────────────────────────────────────────────────────────────────────────┤
│ *Tool:* `delete_user`                                                       │
│ *Principal:* production/agent-service                                       │
│ *Workflow:* default                                                         │
│                                                                             │
│ *Arguments:*                                                                │
│ ```                                                                         │
│ {                                                                           │
│   "user_id": "12345",                                                       │
│   "reason": "Account inactive"                                              │
│ }                                                                           │
│ ```                                                                         │
│                                                                             │
│ @oncall ← React with 👍 to *approve* or 👎 to *reject*                      │
│                                                                             │
│ Task ID: `abc-123` • Expires: 2025-01-08 11:30 UTC                         │
└───────────────────────────────────────────────────────────────────────────┘
```

## 8. Integration with Decision Flow

```rust
// In REQ-CORE-003 handle_tools_call()
async fn execute_gate4(
    request: &McpRequest,
    workflow_name: &str,
    config: &Config,
) -> Result<McpResponse, ThoughtGateError> {
    let approval_engine = get_approval_engine();
    
    // v0.2: SEP-1686 task mode
    // Start approval (non-blocking) - returns Task ID immediately
    let task_id = approval_engine
        .start_approval(request, workflow_name, task_manager.clone())
        .await?;
    
    // Return Task ID to client - background poller handles the wait
    return Ok(TaskResponse::new(task_id, TaskStatus::InputRequired));
    
    match decision.decision {
        Decision::Approved => {
            info!(
                task_id = %request.correlation_id,
                approved_by = %decision.decided_by,
                workflow = %workflow_name,
                "Approval granted"
            );
            // Forward to upstream
            upstream.forward(request).await
        }
        Decision::Rejected => {
            Err(ThoughtGateError::ApprovalRejected {
                tool: request.tool_name().to_string(),
                rejected_by: Some(decision.decided_by),
            })
        }
    }
}
```

## 9. Non-Functional Requirements

### NFR-001: Observability

**Metrics:**
```
thoughtgate_approval_posts_total{adapter, workflow, status}
thoughtgate_approval_polls_total{adapter, workflow, result}
thoughtgate_approval_decisions_total{adapter, workflow, decision, method}
thoughtgate_approval_poll_latency_seconds{adapter}
thoughtgate_approval_decision_latency_seconds{adapter, workflow}
thoughtgate_approval_timeout_total{workflow, on_timeout}
```

**Logging:**
```json
{"level":"info","event":"approval_requested","workflow":"default","task_id":"abc-123","tool":"delete_user"}
{"level":"info","event":"approval_posted","workflow":"default","task_id":"abc-123","adapter":"slack","channel":"C123"}
{"level":"debug","event":"approval_poll","task_id":"abc-123","poll_count":3,"result":"pending"}
{"level":"info","event":"approval_decided","workflow":"default","task_id":"abc-123","decision":"approved","decided_by":"alice"}
{"level":"warn","event":"approval_timeout","workflow":"finance","task_id":"xyz-789","on_timeout":"deny"}
```

### NFR-002: Performance

| Metric | Target |
|--------|--------|
| Workflow config lookup | < 1ms |
| Message post latency | < 500ms (P99) |
| Poll latency | < 200ms (P99) |
| Decision detection | Within 2 poll cycles of reaction |

### NFR-003: Reliability

- Polling continues across transient Slack API failures
- Exponential backoff prevents thundering herd
- Rate limiting prevents API exhaustion
- Config hot-reload doesn't drop pending approvals

### NFR-004: Security

- Bot token loaded from env var, never logged
- Bot token never stored in config file
- HTTPS for all Slack API calls
- User identity from Slack trusted

## 10. Testing Requirements

### 10.1 Unit Tests

| Test | Description |
|------|-------------|
| `test_workflow_loading` | Load workflow from config |
| `test_workflow_not_found` | Error on missing workflow |
| `test_default_workflow_fallback` | Fall back to default |
| `test_adapter_creation_slack` | Create Slack adapter from config |
| `test_adapter_creation_webhook` | Create webhook adapter from config |
| `test_token_from_env` | Load token from configured env var |
| `test_timeout_from_config` | Use timeout from workflow config |
| `test_on_timeout_deny` | Deny on timeout |

### 10.2 Integration Tests

| Test | Description |
|------|-------------|
| `test_slack_post_and_poll` | Post message, poll for reaction |
| `test_approval_flow_approve` | Full approve flow |
| `test_approval_flow_reject` | Full reject flow |
| `test_approval_flow_timeout` | Timeout triggers on_timeout |
| `test_config_hot_reload` | New workflow available after reload |

## 11. Example Configurations

### 11.1 Simple Single-Channel

```yaml
approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
    timeout: 10m
    on_timeout: deny
```

### 11.2 Multiple Workflows

```yaml
approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
    timeout: 10m
    on_timeout: deny

  finance:
    destination:
      type: slack
      channel: "#finance-approvals"
      token_env: SLACK_FINANCE_BOT_TOKEN
      mention:
        - "@finance-oncall"
    timeout: 30m
    on_timeout: deny

  security:
    destination:
      type: slack
      channel: "#security-approvals"
      mention:
        - "@security-team"
    timeout: 5m
    on_timeout: deny
```

### 11.3 Local Development

```yaml
approval:
  default:
    destination:
      type: cli  # Prompt in terminal
    timeout: 5m
    on_timeout: deny
```
