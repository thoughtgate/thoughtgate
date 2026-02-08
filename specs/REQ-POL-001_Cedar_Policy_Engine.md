# REQ-POL-001: Cedar Policy Engine

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-POL-001` |
| **Title** | Cedar Policy Engine |
| **Type** | Policy Component |
| **Status** | Draft |
| **Priority** | **Critical** |
| **Tags** | `#policy` `#cedar` `#security` `#abac` `#gate3` |

## 1. Context & Decision Rationale

This requirement defines the **Cedar policy engine** for ThoughtGate—how complex policy decisions are evaluated when YAML governance rules delegate via `action: policy`.

### 1.1 Cedar's Role in the 4-Gate Model

Cedar is **Gate 3** in ThoughtGate's request decision flow. It is NOT the primary routing mechanism—YAML governance rules (Gate 2) handle most routing. Cedar is invoked only when a rule specifies `action: policy`.

```text
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│   GATE 1    │    │   GATE 2    │    │   GATE 3    │    │   GATE 4    │
│ Visibility  │ → │ Governance  │ → │   Cedar     │ → │  Approval   │
│  (expose)   │    │   (YAML)    │    │  (policy)   │    │  Workflow   │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
                         │                  │
                         │                  │
                   action: forward    Only when
                   action: deny       action: policy
                   action: approve    
                   action: policy ────────►│
```

### 1.2 Why Cedar?

Cedar provides value for **complex policy decisions** that YAML cannot express:

| Capability | YAML Rules | Cedar |
|------------|------------|-------|
| Tool name matching | ✅ Glob patterns | ✅ Resource matching |
| Simple conditions | ⚠️ Limited | ✅ Full boolean logic |
| Argument inspection | ❌ | ✅ `resource.arguments.*` |
| Principal-based ABAC | ❌ | ✅ Agent identity |
| Time-based rules | ❌ | ✅ `context.time.*` |
| Cross-attribute logic | ❌ | ✅ Complex expressions |

**When to use Cedar:**
- Spending limits: "Allow if amount < $10,000"
- Time windows: "Only during business hours"
- Principal restrictions: "Only production agents can deploy"
- Complex conditions: "(A && B) || (C && !D)"

**When YAML is sufficient:**
- Simple routing: "Forward all read operations"
- Blanket denials: "Block all admin tools"
- Basic approval: "Approve all deletions"

### 1.3 Cedar Decision Model

Cedar returns **Permit** or **Forbid** only. It does NOT return routing decisions like "Forward" or "Approve"—that is handled by YAML.

| Cedar Decision | ThoughtGate Behavior |
|----------------|---------------------|
| **Permit** | Continue to Gate 4 (use `approval:` from YAML rule) |
| **Forbid** | Deny immediately with `-32003 Policy Denied` |

> **Important:** Cedar is default-deny. If no policy matches, the request is **denied**.

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-CFG-001 | **Receives from** | Policy file paths (`cedar.policies`), policy_id binding |
| REQ-CORE-003 | **Receives from** | Parsed MCP requests via Governance Engine |
| REQ-CORE-004 | **Provides to** | Policy denial errors (-32003) |
| REQ-GOV-001 | **Coordinates with** | Approval workflow (via YAML, not Cedar) |

## 3. Intent

The system must:
1. Define a Cedar schema for MCP request evaluation
2. Evaluate policies and return **Permit/Forbid** decisions
3. Load policies from paths specified in YAML config (`cedar.policies`)
4. Hot-reload policies when files change
5. Bind `policy_id` from YAML rules to Cedar `context.policy_id`
6. Infer principal identity from Kubernetes environment

## 4. Scope

### 4.1 In Scope
- Cedar schema definition (entities, actions, context)
- Policy evaluation logic (Permit/Forbid)
- Policy loading from YAML-configured paths
- Policy hot-reload via file watching
- `policy_id` binding from YAML rules
- Principal identity inference (K8s)
- Local development mode
- Argument inspection via `resource.arguments`

### 4.2 Out of Scope
- Routing decisions (handled by YAML governance rules)
- Approval workflow selection (handled by YAML `approval:` field)
- Policy authoring UI
- Policy testing framework (deferred)
- Policy versioning/history (deferred)
- "Advice" or metadata in Cedar responses (Cedar doesn't support this)

## 5. Constraints

### 5.1 Runtime & Dependencies

| Crate | Purpose | Version |
|-------|---------|---------|
| `cedar-policy` | Policy engine | Latest stable |
| `arc-swap` | Atomic policy swap | 1.x |
| `smallvec` | Inline policy ID vectors | 1.x |
| `notify` or polling | File watching | - (not yet implemented; see F-006.1) |

### 5.2 Cedar Schema

```cedar
namespace ThoughtGate;

// ═══════════════════════════════════════════════════════════
// PRINCIPALS
// ═══════════════════════════════════════════════════════════

/// The application pod making requests through ThoughtGate
entity App in [Role] = {
    name: String,               // From HOSTNAME
    namespace: String,          // From K8s ServiceAccount
    service_account: String,    // From K8s ServiceAccount token
};

/// Role for RBAC grouping
entity Role = {
    name: String,
};

// ═══════════════════════════════════════════════════════════
// RESOURCES
// ═══════════════════════════════════════════════════════════

/// An MCP tool call request
entity ToolCall = {
    name: String,               // Tool name, e.g., "transfer_funds"
    server: String,             // Source ID from config
    arguments: Record,          // Tool arguments for inspection
};

/// Generic MCP method for non-tool requests
entity McpMethod = {
    method: String,             // e.g., "resources/read"
    server: String,
};

// ═══════════════════════════════════════════════════════════
// CONTEXT
// ═══════════════════════════════════════════════════════════

/// Context passed from YAML governance rules
type RequestContext = {
    policy_id: String,          // From YAML rule's policy_id field
    source_id: String,          // Source that matched
    time: TimeContext,          // Current time for time-based rules
};

type TimeContext = {
    hour: Long,                 // 0-23 UTC
    day_of_week: Long,          // 0=Sunday, 6=Saturday
    timestamp: Long,            // Unix timestamp
};

// ═══════════════════════════════════════════════════════════
// ACTIONS
// ═══════════════════════════════════════════════════════════

/// The only action: tools/call evaluation
/// Cedar decides Permit or Forbid
action "tools/call" appliesTo {
    principal: [App, Role],
    resource: [ToolCall],
    context: RequestContext,
};

action "mcp/method" appliesTo {
    principal: [App, Role],
    resource: [McpMethod],
    context: RequestContext,
};
```

### 5.3 Policy Loading

Policy paths are configured in YAML config (`cedar.policies`):

```yaml
cedar:
  policies:
    - /etc/thoughtgate/policies/financial.cedar
    - /etc/thoughtgate/policies/time_restrictions.cedar
  schema: /etc/thoughtgate/schema.cedarschema  # Optional
```

**Loading Priority (4-tier fallback):**

Each tier is tried in order. If a configured source exists but is
unreadable, loading fails hard (fail-closed) instead of falling
through to a lower tier.

| Priority | Source | Use Case |
|----------|--------|----------|
| 1 | `$THOUGHTGATE_POLICY_FILE` (default: `/etc/thoughtgate/policies.cedar`) | K8s ConfigMap mount |
| 2 | `$THOUGHTGATE_POLICIES` env var (inline policy text) | Simple / CI (< 10KB) |
| 3 | YAML config `cedar.policies[]` paths (concatenated) | Production multi-file |
| 4 | Embedded defaults (compile-time) | Local dev fallback |

> **Note:** Tier 3 is only reached via `CedarEngine::new_with_config()`,
> which receives paths from the parsed YAML config. The env-var-only
> constructor `CedarEngine::new()` skips tier 3.

### 5.4 Identity Inference

**Kubernetes Sources:**
| Attribute | Source | Path |
|-----------|--------|------|
| `name` | Hostname | `$HOSTNAME` |
| `namespace` | SA mount | `/var/run/secrets/kubernetes.io/serviceaccount/namespace` |
| `service_account` | SA token | `/var/run/secrets/kubernetes.io/serviceaccount/token` (parse) |

**JWT Parsing for K8s ServiceAccount Tokens:**

The ServiceAccount token is a JWT. ThoughtGate uses `base64` decoding (without
verification, since the token is already trusted by the kubelet mount) to extract
the payload claims:
- `kubernetes.io/serviceaccount/namespace` → `namespace`
- `kubernetes.io/serviceaccount/service-account.name` → `service_account`
- `sub` claim → used as fallback for `service_account`

No cryptographic verification is performed; the token is trusted as a local
identity source provided by the Kubernetes control plane.

**Local Development Override:**
| Variable | Purpose |
|----------|---------|
| `THOUGHTGATE_DEV_MODE=true` | Enable dev mode |
| `THOUGHTGATE_DEV_PRINCIPAL` | Override principal (default: `dev-app`) |
| `THOUGHTGATE_DEV_NAMESPACE` | Override namespace (default: `development`) |

### 5.5 Configuration

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| Schema file path | (from YAML) | `THOUGHTGATE_SCHEMA_FILE` |
| Hot-reload interval | 10s | `THOUGHTGATE_POLICY_RELOAD_INTERVAL_SECS` |
| Dev mode | false | `THOUGHTGATE_DEV_MODE` |

## 6. Interfaces

### 6.1 Input: Policy Evaluation Request

```rust
/// Request from Governance Engine to Cedar
pub struct CedarRequest {
    pub principal: Principal,
    pub resource: CedarResource,
    pub context: CedarContext,
}

pub struct Principal {
    pub app_name: String,
    pub namespace: String,
    pub service_account: String,
    pub roles: Vec<String>,
}

pub enum CedarResource {
    ToolCall {
        name: String,
        server: String,
        arguments: serde_json::Value,
    },
    McpMethod {
        method: String,
        server: String,
    },
}

/// Context passed from YAML rule
pub struct CedarContext {
    /// policy_id from YAML rule (required for action: policy)
    pub policy_id: String,
    /// Source ID that matched
    pub source_id: String,
    /// Current time for time-based rules
    pub time: TimeContext,
}

pub struct TimeContext {
    pub hour: u8,           // 0-23 UTC
    pub day_of_week: u8,    // 0=Sunday
    pub timestamp: i64,     // Unix timestamp
}
```

### 6.2 Output: Cedar Decision

```rust
/// Cedar returns Permit or Forbid only
/// Routing decisions come from YAML, not Cedar
pub enum CedarDecision {
    /// Policy permits the request
    /// ThoughtGate continues to Gate 4 (approval workflow from annotation or YAML)
    Permit {
        /// Policy IDs that contributed to the permit decision.
        /// Used to look up @thoughtgate_approval annotations.
        /// SmallVec avoids heap allocation for the common 0-1 policy case.
        determining_policies: SmallVec<[String; 1]>,
    },

    /// Policy forbids the request
    /// ThoughtGate denies immediately
    Forbid {
        /// Reason for denial (safe for logging)
        reason: String,
        /// Policy IDs that caused the denial.
        /// SmallVec avoids heap allocation for the common 1-2 policy case.
        policy_ids: SmallVec<[String; 2]>,
    },
}
// Note: `SmallVec` from the `smallvec` crate is used instead of `Vec<String>`
// to avoid heap allocation in the common case of 0-2 policy IDs per decision.
```

> **Note:** There is no "NoDecision" variant. Cedar is default-deny—if no policy explicitly permits, the result is Forbid.

### 6.3 Cedar Engine Interface

`CedarEngine` is a concrete struct (not a trait). The v0.2 evaluation
entry point is `evaluate_v2()`; the legacy `evaluate()` method is
retained for backward compatibility but deprecated.

`reload()` is **synchronous** -- it performs blocking file I/O and
atomically swaps the policy set via `ArcSwap`. Callers on the Tokio
runtime should wrap it in `tokio::task::spawn_blocking` if needed.

```rust
pub struct CedarEngine {
    authorizer: Authorizer,
    /// Current policy snapshot (atomic for hot-reload via ArcSwap)
    snapshot: ArcSwap<PolicySnapshot>,
    schema: Schema,
    stats_v2: Arc<StatsV2>,
    tg_metrics: Option<Arc<ThoughtGateMetrics>>,
}

impl CedarEngine {
    /// Create a new Cedar engine using env vars only.
    pub fn new() -> Result<Self, PolicyError>;

    /// Create a new Cedar engine with optional YAML config paths.
    pub fn new_with_config(
        cedar_config: Option<&CedarConfig>,
    ) -> Result<Self, PolicyError>;

    /// Evaluate a request and return Permit/Forbid (v0.2 API).
    pub fn evaluate_v2(&self, request: &CedarRequest) -> CedarDecision;

    /// Reload policies from source (synchronous).
    /// On success, atomically swaps in new policies.
    /// On failure, keeps old policies and returns error.
    pub fn reload(&self) -> Result<(), PolicyError>;

    /// Get policy source information.
    pub fn policy_source(&self) -> PolicySource;

    /// Set ThoughtGate Prometheus metrics for gauge reporting.
    pub fn set_metrics(&mut self, metrics: Arc<ThoughtGateMetrics>);

    /// Builder-style method to set metrics.
    pub fn with_metrics(self, metrics: Arc<ThoughtGateMetrics>) -> Self;
}
```

### 6.4 Errors

The policy error type is `PolicyError` (defined in `policy/mod.rs`),
not `CedarError`. It uses `thiserror` for `Display`/`Error` derives.

```rust
#[derive(Debug, Error, Clone)]
pub enum PolicyError {
    /// Policy syntax error
    #[error("Policy parse error at line {line:?}: {details}")]
    ParseError { details: String, line: Option<usize> },

    /// Schema validation failed
    #[error("Schema validation failed: {details}")]
    SchemaValidation { details: String },

    /// Identity inference failed
    #[error("Identity error: {details}")]
    IdentityError { details: String },

    /// Cedar engine error (request building, entity creation, etc.)
    #[error("Cedar engine error: {details}")]
    CedarError { details: String },

    /// Policy file I/O error (file exists but cannot be read)
    #[error("failed to load policy from {path}: {reason}")]
    PolicyLoadError { path: String, reason: String },
}
```

## 7. Functional Requirements

### F-001: Policy Evaluation

```text
┌──────────────────────────────────────────────────────────────┐
│              CEDAR EVALUATION (Gate 3)                        │
│                                                              │
│   Input:                                                     │
│   - Principal (App identity)                                 │
│   - Resource (ToolCall with arguments)                       │
│   - Context (policy_id, source_id, time)                     │
│                                                              │
│   Evaluation:                                                │
│   1. Find policies matching context.policy_id                │
│   2. Evaluate permit/forbid conditions                       │
│   3. If ANY forbid matches → Forbid                          │
│   4. If ANY permit matches (no forbid) → Permit              │
│   5. If NO policy matches → Forbid (default-deny)            │
│                                                              │
│   Output: Permit or Forbid                                   │
└──────────────────────────────────────────────────────────────┘
```

- **F-001.1:** Evaluate policies filtered by `context.policy_id`
- **F-001.2:** Forbid takes precedence over Permit (Cedar semantics)
- **F-001.3:** No matching policy results in Forbid (default-deny)
- **F-001.4:** Evaluation must complete in < 1ms (P99)
- **F-001.5:** Arguments must be accessible via `resource.arguments.*`

### F-002: Policy ID Binding

The `policy_id` from YAML rules binds to Cedar's `context.policy_id`:

```yaml
# YAML governance rule
governance:
  rules:
    - match: "transfer_*"
      action: policy
      policy_id: "financial_transfer"  # ← Bound to Cedar context
      approval: finance                 # ← Used if Cedar permits
```

```cedar
// Cedar policy
permit(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "financial_transfer" &&
    resource.arguments.amount < 10000
};

forbid(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "financial_transfer" &&
    resource.arguments.amount >= 100000
};
```

- **F-002.1:** `policy_id` MUST be passed to Cedar as `context.policy_id`
- **F-002.2:** Policies SHOULD filter by `context.policy_id` for clarity
- **F-002.3:** Multiple rules can share the same `policy_id`

### F-003: Argument Inspection

Cedar can inspect tool arguments for fine-grained decisions:

```cedar
// Allow small transfers
permit(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "transfer_check" &&
    resource.arguments.amount < 1000 &&
    resource.arguments.currency == "USD"
};

// Forbid transfers to blocked countries
forbid(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "transfer_check" &&
    resource.arguments.destination_country in ["XX", "YY", "ZZ"]
};
```

- **F-003.1:** Tool arguments MUST be available as `resource.arguments`
- **F-003.2:** Arguments are JSON values (string, number, boolean, object, array)
- **F-003.3:** Missing arguments result in condition failure (not error)

### F-004: Time-Based Rules

Cedar can enforce time-based restrictions:

```cedar
// Only allow deployments during business hours
permit(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "deploy_check" &&
    context.time.hour >= 9 &&
    context.time.hour < 17 &&
    context.time.day_of_week >= 1 &&  // Monday
    context.time.day_of_week <= 5     // Friday
};
```

- **F-004.1:** Time context MUST be populated with current UTC time
- **F-004.2:** Time fields: `hour` (0-23), `day_of_week` (0-6), `timestamp`

### F-005: Principal-Based Rules

Cedar can make decisions based on agent identity:

```cedar
// Only production agents can access production tools
permit(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "prod_access" &&
    principal.namespace == "production"
};

// Deny staging agents from production
forbid(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "prod_access" &&
    principal.namespace == "staging"
};
```

- **F-005.1:** Principal identity MUST be inferred from K8s environment
- **F-005.2:** Principal attributes: `app_name`, `namespace`, `service_account`

### F-006: Policy Hot-Reload

- **F-006.1:** ~~Watch policy files for changes~~ **DEFERRED** -- `reload()` is implemented and works (called via admin API or programmatically), but there is no automatic file-watching trigger yet. A `notify`/polling-based watcher may be added in a future version.
- **F-006.2:** Reload policies atomically (no partial updates)
- **F-006.3:** Log reload success/failure
- **F-006.4:** Continue with old policies if reload fails
- **F-006.5:** Emit metric on reload (`thoughtgate_cedar_reload_total{status}`)

### F-007: Integration with Governance Engine

Cedar is invoked by the Governance Engine when `action: policy`:

```rust
// In Governance Engine (REQ-CFG-001)
let rule_match = governance.evaluate(tool_name, source_id);

match rule_match.action {
    Action::Forward => forward_to_upstream(request).await,
    Action::Deny => deny_request("governance rule"),
    Action::Approve => {
        // Go directly to Gate 4
        let workflow = rule_match.approval_workflow.unwrap_or("default");
        approval_engine.request_approval(request, workflow).await
    }
    Action::Policy => {
        // Invoke Cedar (Gate 3)
        let cedar_request = CedarRequest {
            principal: infer_principal()?,
            resource: CedarResource::ToolCall {
                name: tool_name,
                server: source_id,
                arguments: request.params.clone(),
            },
            context: CedarContext {
                policy_id: rule_match.policy_id.expect("required for action: policy"),
                source_id: source_id.to_string(),
                time: current_time_context(),
            },
        };
        
        match cedar_engine.evaluate_v2(&cedar_request) {
            CedarDecision::Permit { determining_policies } => {
                // Lookup workflow: annotation → YAML rule → "default"
                let workflow = policy_annotations
                    .get_workflow(&determining_policies)
                    .or(rule_match.approval_workflow.as_deref())
                    .unwrap_or("default");
                
                // Continue to Gate 4
                approval_engine.request_approval(request, workflow).await
            }
            CedarDecision::Forbid { reason, .. } => {
                deny_request(&reason)
            }
        }
    }
}
```

- **F-007.1:** Cedar MUST only be invoked for `action: policy` rules
- **F-007.2:** `policy_id` from YAML rule MUST be passed in context
- **F-007.3:** Approval workflow MUST be resolved from annotation → YAML → "default"
- **F-007.4:** Cedar `Forbid` MUST return -32003 error immediately
```

## 8. Example Policies

### 8.0 Using Policy Annotations for Workflow Routing

Cedar policies return only `permit` or `forbid`—they cannot return arbitrary data. To specify which approval workflow to use for a permitted request, use **static annotations** on the policy:

```cedar
// Standard annotation format
@id("policy_unique_id")
@thoughtgate_approval("workflow_name")  // ← References YAML approval config
permit(
    principal,
    action == Action::"tools/call",
    resource
)
when { ... };
```

**How ThoughtGate processes annotations:**

```rust
// At policy load time: Parse and cache annotations
pub struct PolicyAnnotations {
    /// Map from Policy ID to approval workflow name
    pub workflow_mapping: HashMap<PolicyId, String>,
}

impl PolicyAnnotations {
    pub fn load_from(policy_set: &PolicySet) -> Self {
        let mut workflow_mapping = HashMap::new();
        
        for policy in policy_set.policies() {
            // Cedar's annotation() method returns Option<&Annotation>
            if let Some(workflow) = policy.annotation("thoughtgate_approval") {
                // workflow.as_str() gives us the annotation value
                workflow_mapping.insert(
                    policy.id().clone(), 
                    workflow.as_str().to_string()
                );
            }
        }
        
        PolicyAnnotations { workflow_mapping }
    }
}

// At evaluation time: Look up workflow for the determining policy
fn get_workflow_for_permit(
    annotations: &PolicyAnnotations,
    determining_policies: &[PolicyId],
    yaml_fallback: Option<&str>,
) -> String {
    // Check annotation on first determining policy
    if let Some(policy_id) = determining_policies.first() {
        if let Some(workflow) = annotations.workflow_mapping.get(policy_id) {
            return workflow.clone();
        }
    }
    
    // Fall back to YAML rule's approval field, then "default"
    yaml_fallback.unwrap_or("default").to_string()
}
```

**Workflow resolution priority:**
1. Cedar policy `@thoughtgate_approval` annotation
2. YAML rule `approval:` field
3. Literal `"default"`

**⚠️ Important:** The `@thoughtgate_approval` annotation is a ThoughtGate-specific convention, not a built-in Cedar feature. ThoughtGate parses these at load time using Cedar's annotation API.

### 8.1 Financial Transfer Limits

```cedar
// Low-value: permit (approval handled by YAML)
permit(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "financial" &&
    resource.arguments.amount < 10000
};

// High-value: forbid entirely
forbid(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "financial" &&
    resource.arguments.amount >= 1000000
};
```

### 8.2 Time-Based Deployment Window

```cedar
// Allow deployments only during business hours (UTC)
permit(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "deploy_window" &&
    context.time.hour >= 9 &&
    context.time.hour < 17 &&
    context.time.day_of_week >= 1 &&
    context.time.day_of_week <= 5
};
```

### 8.3 Namespace Isolation

```cedar
// Production namespace can access production tools
permit(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "namespace_isolation" &&
    principal.namespace == "production" &&
    resource.server.contains("prod")
};

// Block staging from production
forbid(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "namespace_isolation" &&
    principal.namespace == "staging" &&
    resource.server.contains("prod")
};
```

## 9. Testing Requirements

### 9.1 Unit Tests

| Test | Description |
|------|-------------|
| `test_permit_simple` | Basic permit policy |
| `test_forbid_simple` | Basic forbid policy |
| `test_forbid_precedence` | Forbid wins over permit |
| `test_default_deny` | No matching policy = forbid |
| `test_argument_inspection` | Access resource.arguments |
| `test_time_context` | Time-based conditions |
| `test_principal_attributes` | Principal-based conditions |
| `test_policy_id_filtering` | Policy scoped by policy_id |
| `test_hot_reload` | Policy reload works |
| `test_reload_failure` | Bad policy keeps old |

### 9.2 Integration Tests

| Test | Description |
|------|-------------|
| `test_governance_to_cedar` | Governance engine invokes Cedar |
| `test_cedar_permit_to_approval` | Permit flows to approval |
| `test_cedar_forbid_denies` | Forbid returns error |
| `test_yaml_config_paths` | Load from config paths |

### 9.3 Edge Case Matrix

| Scenario | Expected Behavior | Test ID |
|----------|-------------------|---------|
| Empty policy file | Valid (no policies = default deny) | EC-POL-001 |
| Policy file not found | Fail fast at startup | EC-POL-002 |
| Policy syntax error | Fail fast, log specific error | EC-POL-003 |
| Policy evaluation timeout (slow query) | Return forbid, log timeout | EC-POL-004 |
| No matching policy for request | Default deny (forbid) | EC-POL-005 |
| Multiple permit policies match | All must permit (AND) | EC-POL-006 |
| One forbid overrides permits | Forbid wins | EC-POL-007 |
| Hot reload during evaluation | Complete with old policy | EC-POL-008 |
| Policy references unknown entity | Fail at load time | EC-POL-009 |
| Very large policy set (1000+) | Performance test target | EC-POL-010 |

## 10. Observability

### 10.1 Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `thoughtgate_cedar_evaluations_total` | Counter | `decision`, `policy_id` | Evaluation count |
| `thoughtgate_cedar_evaluation_duration_ms` | Histogram | `decision` | Evaluation latency (milliseconds) |
| `thoughtgate_cedar_reload_total` | Counter | `{status}` | Reload attempts (`status`: `"success"` or `"error"`) |
| `thoughtgate_cedar_policies_loaded` | Gauge | - | Current policy count |
| `thoughtgate_policy_evaluation_cache_hits` | Counter | - | Cache effectiveness (v0.3+) |

### 10.2 Logging

| Event | Level | Fields |
|-------|-------|--------|
| Policy evaluation | DEBUG | `policy_id`, `decision`, `duration_us` |
| Policy permit | INFO | `policy_id`, `tool`, `principal` |
| Policy forbid | WARN | `policy_id`, `tool`, `reason` |
| Policy reload success | INFO | `path`, `policy_count` |
| Policy reload failure | ERROR | `path`, `error` |

## 11. Migration from v0.1

### 11.1 Breaking Changes

| v0.1 | v0.2 | Migration |
|------|------|-----------|
| Cedar returns Forward/Approve/Reject | Cedar returns Permit/Forbid | Move routing logic to YAML |
| Cedar is primary router | Cedar is Gate 3 only | Add YAML governance rules |
| `THOUGHTGATE_POLICY_FILE` env var | `cedar.policies` in YAML | Update config |

### 11.2 Policy Migration

**v0.1 Policy (routing):**
```cedar
// OLD: Cedar decided to forward
permit(
    principal,
    action == Action::"Forward",
    resource
)
when { resource.name like "get_*" };
```

**v0.2 Policy (evaluation):**
```yaml
# NEW: YAML handles routing
governance:
  rules:
    - match: "get_*"
      action: forward  # Routing in YAML
    
    - match: "transfer_*"
      action: policy
      policy_id: "financial"  # Delegate to Cedar
      approval: finance
```

```cedar
// NEW: Cedar only evaluates when action: policy
permit(
    principal,
    action == Action::"tools/call",
    resource
)
when {
    context.policy_id == "financial" &&
    resource.arguments.amount < 10000
};
```
