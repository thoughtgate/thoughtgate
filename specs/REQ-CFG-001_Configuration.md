# REQ-CFG-001: Configuration Schema

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-CFG-001` |
| **Title** | Configuration Schema |
| **Type** | Configuration Component |
| **Status** | Draft |
| **Priority** | **Critical** |
| **Tags** | `#configuration` `#yaml` `#validation` `#sources` `#governance` |

## 1. Context & Decision Rationale

This requirement defines the **unified configuration schema** for ThoughtGate. The configuration controls:

1. **Sources:** Where capabilities come from (MCP servers, OpenAPI, A2A agents)
2. **Governance:** Rules for routing requests (forward, approve, deny, policy)
3. **Approval:** How approval workflows are configured
4. **Audit:** Logging of approval decisions

**Why YAML over CLI flags?**
- Complex nested structures (sources, rules, chains)
- Self-documenting configuration
- Hot-reload capability
- GitOps-friendly

**Decision Flow (4 Gates):**
```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│   GATE 1    │    │   GATE 2    │    │   GATE 3    │    │   GATE 4    │
│ Visibility  │ → │ Governance  │ → │   Cedar     │ → │  Approval   │
│  (expose)   │    │   Rules     │    │  (policy)   │    │  Workflow   │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
```

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-POL-001 | **Provides to** | Cedar policy paths, policy_id binding |
| REQ-GOV-001 | **Provides to** | Approval timeout configuration |
| REQ-GOV-002 | **Provides to** | Pipeline configuration |
| REQ-GOV-003 | **Provides to** | Slack/approval destination config |
| REQ-CORE-003 | **Provides to** | Upstream URL configuration |
| REQ-CORE-004 | **Provides to** | Error handling configuration |
| REQ-OBS-002 | **Provides to** | Telemetry enable/disable, OTLP endpoints, sampling config |

## 3. Intent

The system must:
1. Parse and validate YAML configuration at startup
2. Support hot-reload of configuration without restart
3. Enforce validation rules to fail fast on invalid config
4. Provide clear error messages for configuration problems
5. Support environment variable substitution in sensitive fields

## 4. Scope

### 4.1 In Scope (v0.2)
- YAML schema parsing with `serde`
- Single MCP source (static)
- Governance rules (match, action, source filter)
- Human approval via Slack
- Approval timeout and on_timeout behavior
- Configuration validation at load time
- Environment variable substitution
- Hot-reload via file watching
- Telemetry configuration (OTel export, Prometheus, sampling)

### 4.2 In Scope (v0.3+)
- Multi-source MCP
- A2A sources and A2A approval
- Approval chains
- External approval service
- Audit configuration

### 4.3 Out of Scope
- MCP/A2A discovery (v0.4+)
- OpenAPI bridge (v0.4+)
- UI-based configuration
- CRD-based configuration (Kubernetes operator)

## 5. Constraints

### 5.1 Schema Version

```yaml
schema: 1  # Required, must be 1
```

Schema 1 is **additive** - new source kinds and features are added without version bump.

### 5.2 v0.2 Feature Restrictions

| Feature | v0.2 Support | Notes |
|---------|--------------|-------|
| Multiple sources | ❌ | Single source only |
| `kind: mcp` | ✅ | Static MCP server |
| `kind: mcp_discovery` | ❌ | v0.4+ |
| `kind: openapi` | ❌ | v0.4+ |
| `kind: a2a` | ❌ | v0.3+ |
| `kind: a2a_discovery` | ❌ | v0.4+ |
| `action: forward` | ✅ | Pass through |
| `action: deny` | ✅ | Reject immediately |
| `action: approve` | ✅ | Human approval |
| `action: policy` | ✅ | Cedar evaluation |
| Human approval (Slack) | ✅ | Async via Slack |
| Human approval (CLI) | ✅ | Interactive TTY |
| `type: approval_service` | ⚠️ | **Experimental** - See §8.8 |
| A2A approval | ❌ | v0.3+ |
| Approval chains | ❌ | v0.3+ |
| Audit logging | ❌ | v0.3+ |

### 5.3 Configuration File Location

| Priority | Source | Path / Variable | Use Case |
|----------|--------|-----------------|----------|
| 1 | CLI flag | `--config <path>` | Explicit override |
| 2 | Env var | `$THOUGHTGATE_CONFIG` | Container/CI |
| 3 | Default | `/etc/thoughtgate/config.yaml` | Production |
| 4 | Default | `./config.yaml` | Local development |

### 5.4 Duration Format

Duration fields support two formats:

| Format | Example | Use Case |
|--------|---------|----------|
| `humantime` | `10m`, `1h 30m`, `2d` | Human-authored config |
| ISO 8601 | `PT10M`, `PT1H30M`, `P2D` | Machine-generated config (Helm, Kustomize) |

**Parsing priority:** Try `humantime` first, then ISO 8601.

```rust
mod duration_format {
    use std::time::Duration;
    use serde::{Deserialize, Deserializer};
    
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = Deserialize::deserialize(deserializer)?;
        
        // Try humantime first (10m, 1h 30m, etc.)
        if let Ok(d) = humantime::parse_duration(&s) {
            return Ok(d);
        }
        
        // Fall back to ISO 8601 (PT10M, PT1H30M, etc.)
        if let Ok(d) = iso8601_duration::Duration::parse(&s) {
            return Ok(d.to_std());
        }
        
        Err(serde::de::Error::custom(format!(
            "Invalid duration '{}': expected humantime (10m) or ISO 8601 (PT10M)", s
        )))
    }
}
```

### 5.5 Environment Variable Substitution

Sensitive fields support `${VAR}` syntax:

```yaml
sources:
  - id: upstream
    kind: mcp
    url: ${UPSTREAM_URL}  # Substituted at load time (required env var)

approval:
  default:
    destination:
      type: slack
      token_env: SLACK_BOT_TOKEN  # Read at runtime, not substituted
```

**Substitution rules:**
- `${VAR}` - Required, fail if not set
- `${VAR:-default}` - Optional with default
- Only in string fields
- Validated after substitution

### 5.6 Centralized Default Values

The following defaults are used across ThoughtGate components. These values are centralized here to ensure consistency and provide a single source of truth.

```rust
/// Centralized default values for ThoughtGate configuration.
///
/// These defaults are used when explicit configuration is not provided.
/// All timing-related values should reference this struct to ensure consistency.
pub struct ThoughtGateDefaults {
    /// Maximum time to wait for upstream tool execution (REQ-GOV-002)
    pub execution_timeout: Duration,

    /// Maximum time for graceful shutdown (REQ-CORE-005)
    pub shutdown_timeout: Duration,

    /// Time to wait for in-flight requests to complete during shutdown (REQ-CORE-005)
    /// Must be less than shutdown_timeout to allow cleanup
    pub drain_timeout: Duration,

    /// Interval between Slack API polls for approval decisions (REQ-GOV-003)
    pub approval_poll_interval: Duration,

    /// Maximum interval between Slack polls after backoff (REQ-GOV-003)
    pub approval_poll_max_interval: Duration,

    /// Interval for health check probes (REQ-CORE-005)
    pub health_check_interval: Duration,

    /// Default approval workflow timeout if not specified in config
    pub default_approval_timeout: Duration,

    /// Default task TTL for SEP-1686 tasks (REQ-GOV-001)
    pub default_task_ttl: Duration,

    /// Maximum task TTL allowed (REQ-GOV-001)
    pub max_task_ttl: Duration,

    /// Interval for expired task cleanup (REQ-GOV-001)
    pub task_cleanup_interval: Duration,

    // Telemetry defaults (REQ-OBS-002)

    /// Default OTLP batch export delay
    pub telemetry_batch_delay: Duration,

    /// Default OTLP export timeout
    pub telemetry_export_timeout: Duration,

    /// Default sampling rate (1.0 = 100%)
    pub telemetry_sample_rate: f64,

    /// Default max span queue size
    pub telemetry_max_queue_size: usize,
}

impl Default for ThoughtGateDefaults {
    fn default() -> Self {
        Self {
            execution_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(30),
            drain_timeout: Duration::from_secs(25),  // Must be < shutdown_timeout
            approval_poll_interval: Duration::from_secs(5),
            approval_poll_max_interval: Duration::from_secs(30),
            health_check_interval: Duration::from_secs(10),
            default_approval_timeout: Duration::from_secs(600),  // 10 minutes
            default_task_ttl: Duration::from_secs(600),          // 10 minutes
            max_task_ttl: Duration::from_secs(86400),            // 24 hours
            task_cleanup_interval: Duration::from_secs(60),
            // Telemetry defaults
            telemetry_batch_delay: Duration::from_secs(5),
            telemetry_export_timeout: Duration::from_secs(30),
            telemetry_sample_rate: 1.0,
            telemetry_max_queue_size: 2048,
        }
    }
}
```

**Default Values Reference Table:**

| Setting | Default | Component | Notes |
|---------|---------|-----------|-------|
| `execution_timeout` | 30s | REQ-GOV-002 | Max upstream wait |
| `shutdown_timeout` | 30s | REQ-CORE-005 | Graceful shutdown limit |
| `drain_timeout` | 25s | REQ-CORE-005 | Must be < shutdown_timeout |
| `approval_poll_interval` | 5s | REQ-GOV-003 | Base Slack poll interval |
| `approval_poll_max_interval` | 30s | REQ-GOV-003 | Max after backoff |
| `health_check_interval` | 10s | REQ-CORE-005 | Health probe frequency |
| `default_approval_timeout` | 10m | REQ-GOV-003 | If workflow timeout unset |
| `default_task_ttl` | 10m | REQ-GOV-001 | Task expiration |
| `max_task_ttl` | 24h | REQ-GOV-001 | Maximum task lifetime |
| `task_cleanup_interval` | 60s | REQ-GOV-001 | Expired task pruning |
| `telemetry_batch_delay` | 5s | REQ-OBS-002 | OTLP batch export interval |
| `telemetry_export_timeout` | 30s | REQ-OBS-002 | OTLP export timeout |
| `telemetry_sample_rate` | 1.0 | REQ-OBS-002 | Trace sampling rate (100%) |
| `telemetry_max_queue_size` | 2048 | REQ-OBS-002 | Max queued spans |

**Invariants:**

1. `drain_timeout` < `shutdown_timeout` (allows time for cleanup after drain)
2. `approval_poll_interval` < `approval_poll_max_interval` (backoff range)
3. `default_task_ttl` <= `max_task_ttl`

**Environment Variable Overrides:**

All defaults can be overridden via environment variables:

| Default | Environment Variable |
|---------|---------------------|
| `execution_timeout` | `THOUGHTGATE_EXECUTION_TIMEOUT_SECS` |
| `shutdown_timeout` | `THOUGHTGATE_SHUTDOWN_TIMEOUT_SECS` |
| `drain_timeout` | `THOUGHTGATE_DRAIN_TIMEOUT_SECS` |
| `approval_poll_interval` | `THOUGHTGATE_APPROVAL_POLL_INTERVAL_SECS` |
| `approval_poll_max_interval` | `THOUGHTGATE_APPROVAL_POLL_MAX_INTERVAL_SECS` |
| `health_check_interval` | `THOUGHTGATE_HEALTH_CHECK_INTERVAL_SECS` |
| `default_approval_timeout` | `THOUGHTGATE_DEFAULT_APPROVAL_TIMEOUT_SECS` |
| `default_task_ttl` | `THOUGHTGATE_DEFAULT_TASK_TTL_SECS` |
| `max_task_ttl` | `THOUGHTGATE_MAX_TASK_TTL_SECS` |
| `task_cleanup_interval` | `THOUGHTGATE_TASK_CLEANUP_INTERVAL_SECS` |

## 6. Interfaces

### 6.1 Input: Configuration File

**v0.2 Minimal Configuration:**

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${UPSTREAM_URL}  # Required: set via UPSTREAM_URL env var

governance:
  defaults:
    action: forward

  rules:
    - match: "delete_*"
      action: approve

approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
    timeout: 10m
    on_timeout: deny
```

**Required Environment Variable:**
```bash
export UPSTREAM_URL=http://mcp-server:3000
```

### 6.2 Output: Parsed Configuration

```rust
#[derive(Debug, Deserialize)]
pub struct Config {
    pub schema: u32,
    pub sources: Vec<Source>,
    pub governance: Governance,
    #[serde(default)]
    pub approval: Option<ApprovalWorkflows>,
    #[serde(default)]
    pub cedar: Option<CedarConfig>,
}
```

### 6.3 Configuration Loading Interface

```rust
pub trait ConfigLoader: Send + Sync {
    /// Load configuration from file path
    fn load(&self, path: &Path) -> Result<Config, ConfigError>;
    
    /// Validate configuration
    fn validate(&self, config: &Config) -> Result<Vec<ValidationWarning>, ConfigError>;
    
    /// Watch for changes and notify
    fn watch(&self, path: &Path, callback: impl Fn(Config) + Send + 'static) -> Result<(), ConfigError>;
}
```

## 7. Data Structures

### 7.1 Top-Level Schema

```rust
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Schema version (must be 1)
    pub schema: u32,
    
    /// Capability sources (v0.2: exactly one MCP source)
    pub sources: Vec<Source>,
    
    /// Governance rules
    pub governance: Governance,
    
    /// Human approval workflows
    #[serde(default)]
    pub approval: Option<HashMap<String, HumanWorkflow>>,
    
    /// Cedar policy configuration
    #[serde(default)]
    pub cedar: Option<CedarConfig>,
    
    /// Telemetry configuration (tracing, metrics, sampling)
    #[serde(default)]
    pub telemetry: TelemetryConfig,
}
```

### 7.2 Source Configuration

```rust
#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum Source {
    /// Static MCP server (v0.2)
    #[serde(rename = "mcp")]
    Mcp {
        id: String,
        url: String,
        #[serde(default)]
        prefix: Option<String>,
        #[serde(default)]
        expose: Option<ExposeConfig>,
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default)]
        description: Option<String>,
    },
    
    // v0.3+: A2a, v0.4+: McpDiscovery, OpenApi, A2aDiscovery
}

impl Source {
    pub fn id(&self) -> &str {
        match self {
            Source::Mcp { id, .. } => id,
        }
    }
    
    pub fn prefix(&self) -> Option<&str> {
        match self {
            Source::Mcp { prefix, .. } => prefix.as_deref(),
        }
    }
    
    pub fn is_enabled(&self) -> bool {
        match self {
            Source::Mcp { enabled, .. } => *enabled,
        }
    }
}
```

### 7.3 Exposure Configuration

```rust
#[derive(Debug, Deserialize)]
#[serde(tag = "mode")]
pub enum ExposeConfig {
    /// All tools visible (default)
    #[serde(rename = "all")]
    All,
    
    /// Only listed patterns visible
    #[serde(rename = "allowlist")]
    Allowlist {
        tools: Vec<String>,
    },
    
    /// All except listed patterns visible
    #[serde(rename = "blocklist")]
    Blocklist {
        tools: Vec<String>,
    },
}

impl Default for ExposeConfig {
    fn default() -> Self {
        ExposeConfig::All
    }
}
```

### 7.4 Governance Configuration

```rust
#[derive(Debug, Deserialize)]
pub struct Governance {
    pub defaults: GovernanceDefaults,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
pub struct GovernanceDefaults {
    pub action: Action,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Forward,
    Approve,
    Deny,
    Policy,
}

#[derive(Debug, Deserialize)]
pub struct Rule {
    /// Glob pattern for tool name matching
    #[serde(rename = "match")]
    pub pattern: String,
    
    /// Action to take when matched
    pub action: Action,
    
    /// Filter to specific source(s)
    #[serde(default)]
    pub source: Option<SourceFilter>,
    
    /// Cedar policy ID (required if action: policy)
    #[serde(default)]
    pub policy_id: Option<String>,
    
    /// Approval workflow name (for action: approve)
    #[serde(default)]
    pub approval: Option<String>,
    
    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,
    
    // ──────────────────────────────────────────────────────────────
    // Future slots (v0.3+) - Parsed but ignored in v0.2
    // This allows v0.3 configs to be parsed by v0.2 binaries
    // ──────────────────────────────────────────────────────────────
    
    /// Rate limiting configuration (v0.3+)
    #[serde(default)]
    pub limits: Option<serde_json::Value>,
    
    /// Inspector chain for this rule (v0.3+)
    #[serde(default)]
    pub inspectors: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SourceFilter {
    Single(String),
    Multiple(Vec<String>),
}
```

### 7.5 Approval Configuration (v0.2: Human Only)

```rust
/// Map of workflow name to configuration
pub type ApprovalWorkflows = HashMap<String, HumanWorkflow>;

#[derive(Debug, Deserialize)]
pub struct HumanWorkflow {
    pub destination: ApprovalDestination,
    
    #[serde(default, deserialize_with = "duration_format::deserialize_option")]
    pub timeout: Option<Duration>,
    
    #[serde(default)]
    pub on_timeout: Option<TimeoutAction>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ApprovalDestination {
    #[serde(rename = "slack")]
    Slack {
        channel: String,
        #[serde(default)]
        token_env: Option<String>,
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
    Cli,
    
    /// External approval service (EXPERIMENTAL - v0.3+)
    /// Allows integration with enterprise approval systems (ServiceNow, Jira, etc.)
    /// See Section 8.8 for webhook contract specification.
    #[serde(rename = "approval_service")]
    ApprovalService {
        /// URL of the approval service endpoint
        url: String,
        /// Optional authentication configuration
        #[serde(default)]
        auth: Option<WebhookAuth>,
        /// Custom headers to include in requests
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
    },
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutAction {
    #[default]
    Deny,
    // v0.3+: Escalate, AutoApprove
}

#[derive(Debug, Deserialize)]
pub struct WebhookAuth {
    #[serde(rename = "type")]
    pub auth_type: String,  // "bearer" | "basic"
    #[serde(default)]
    pub token_env: Option<String>,
}
```

### 7.6 Cedar Configuration

```rust
#[derive(Debug, Deserialize)]
pub struct CedarConfig {
    /// List of Cedar policy file paths
    pub policies: Vec<PathBuf>,
    
    /// Optional Cedar schema file
    #[serde(default)]
    pub schema: Option<PathBuf>,
}
```

### 7.7 Telemetry Configuration

The `telemetry:` section controls distributed tracing, metrics export, and sampling behavior per REQ-OBS-002.

#### 7.7.1 YAML Schema

```yaml
telemetry:
  # Master enable/disable. When false, no spans are created, no OTLP traffic,
  # but Prometheus /metrics endpoint still works if prometheus.enabled is true.
  enabled: true  # Default: false
  
  otlp:
    # OTLP exporter endpoint. Overridden by OTEL_EXPORTER_OTLP_ENDPOINT env var.
    endpoint: "http://otel-collector:4318"  # Default: none (export disabled)
    
    # Export protocol. v0.2 supports http/protobuf only.
    # "grpc" is accepted but falls back to http/protobuf with a warning.
    protocol: http/protobuf  # Default: http/protobuf
    
    # Optional headers for OTLP export (e.g., authentication)
    headers:
      Authorization: "Bearer ${OTEL_AUTH_TOKEN:-}"
  
  prometheus:
    # Enable Prometheus /metrics endpoint on admin port
    enabled: true  # Default: true
    # Note: Port is admin port (default 7469), path is always /metrics
  
  sampling:
    # Sampling strategy. v0.2 supports "head" only.
    # "tail" is accepted but falls back to "head" with a warning.
    strategy: head  # Default: head
    
    # Sample rate for successful requests (0.0 to 1.0)
    # Errors are always sampled at 100% regardless of this setting.
    success_sample_rate: 0.10  # Default: 1.0 (sample everything)
  
  batch:
    # Maximum spans queued for export before dropping
    max_queue_size: 2048  # Default: 2048
    
    # Maximum spans per export batch
    max_export_batch_size: 512  # Default: 512
    
    # Delay between batch exports (milliseconds)
    scheduled_delay_ms: 5000  # Default: 5000
    
    # Export timeout (milliseconds)
    export_timeout_ms: 30000  # Default: 30000
  
  resource:
    # OTel resource attributes. These appear on every span.
    # Standard attributes use dotted keys.
    service.name: thoughtgate  # Default: "thoughtgate"
    service.version: ${VERSION:-unknown}
    deployment.environment: ${DEPLOY_ENV:-development}
    # Kubernetes attributes auto-populated from downward API if available
    # k8s.namespace.name, k8s.pod.name, k8s.node.name
```

#### 7.7.2 Rust Types

```rust
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TelemetryConfig {
    /// Master enable/disable for tracing
    pub enabled: bool,
    
    /// OTLP export configuration
    pub otlp: OtlpConfig,
    
    /// Prometheus endpoint configuration
    pub prometheus: PrometheusConfig,
    
    /// Sampling configuration
    pub sampling: SamplingConfig,
    
    /// Batch export configuration
    pub batch: BatchConfig,
    
    /// OTel resource attributes
    #[serde(default)]
    pub resource: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct OtlpConfig {
    /// OTLP endpoint URL (e.g., "http://collector:4318")
    pub endpoint: Option<String>,
    
    /// Export protocol: "http/protobuf" or "grpc"
    /// v0.2: grpc falls back to http/protobuf
    #[serde(default = "default_otlp_protocol")]
    pub protocol: String,
    
    /// Optional headers for authentication
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

fn default_otlp_protocol() -> String {
    "http/protobuf".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PrometheusConfig {
    /// Enable /metrics endpoint
    pub enabled: bool,
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SamplingConfig {
    /// Sampling strategy: "head" or "tail"
    /// v0.2: tail falls back to head
    pub strategy: String,
    
    /// Sample rate for successful requests (0.0 to 1.0)
    pub success_sample_rate: f64,
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            strategy: "head".to_string(),
            success_sample_rate: 1.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BatchConfig {
    /// Maximum spans queued before dropping
    pub max_queue_size: usize,
    
    /// Maximum spans per export batch
    pub max_export_batch_size: usize,
    
    /// Delay between exports (ms)
    pub scheduled_delay_ms: u64,
    
    /// Export timeout (ms)
    pub export_timeout_ms: u64,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_queue_size: 2048,
            max_export_batch_size: 512,
            scheduled_delay_ms: 5000,
            export_timeout_ms: 30000,
        }
    }
}
```

#### 7.7.3 Environment Variable Overrides

Standard OTel environment variables take precedence over YAML config:

| Env Var | Overrides | Notes |
|---------|-----------|-------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `telemetry.otlp.endpoint` | Standard OTel env var |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `telemetry.otlp.protocol` | Standard OTel env var |
| `OTEL_TRACES_SAMPLER` | `telemetry.sampling.strategy` | Must be `parentbased_traceidratio` |
| `OTEL_TRACES_SAMPLER_ARG` | `telemetry.sampling.success_sample_rate` | Float 0.0-1.0 |
| `OTEL_SERVICE_NAME` | `telemetry.resource.service.name` | Standard OTel env var |

**Precedence:** Environment variable > YAML config > Default

#### 7.7.4 Hot-Reload Behavior

| Field | Hot-Reloadable | Notes |
|-------|----------------|-------|
| `enabled` | ⚠️ Partial | Disable takes effect immediately; enable requires restart |
| `otlp.endpoint` | ❌ No | Requires restart (exporter is initialized once) |
| `prometheus.enabled` | ❌ No | Requires restart |
| `sampling.success_sample_rate` | ✅ Yes | New rate applies to new spans |
| `batch.*` | ❌ No | Requires restart |
| `resource.*` | ❌ No | Requires restart |

#### 7.7.5 Behavioral Specifications

| ID | Behavior |
|----|----------|
| B-CFG-TEL-001 | When `telemetry.enabled: false`, zero OTLP network traffic is generated |
| B-CFG-TEL-002 | When `telemetry.enabled: false` AND `prometheus.enabled: true`, the `/metrics` endpoint still works |
| B-CFG-TEL-003 | When `otlp.protocol: grpc`, log warning "gRPC not supported in v0.2, using http/protobuf" and use http/protobuf |
| B-CFG-TEL-004 | When `sampling.strategy: tail`, log warning "Tail sampling not supported in v0.2, using head" and use head |
| B-CFG-TEL-005 | Resource attributes from config are merged with auto-detected K8s attributes (K8s takes precedence on conflict) |

## 8. Validation Rules

### 8.1 Validation Rules Table

| ID | Rule | Error | Severity |
|----|------|-------|----------|
| V-001 | Source ID must be unique | `DuplicateSourceId` | Error |
| V-002 | Source ID cannot start with `_` | `ReservedPrefix` | Error |
| V-003 | Prefix must be unique across sources | `DuplicatePrefix` | Error |
| V-004 | `policy_id` requires `action: policy` | `PolicyIdWithoutPolicyAction` | Warning |
| V-005 | `action: policy` requires `policy_id` | `MissingPolicyId` | Error |
| V-006 | `approval` workflow must be defined | `UndefinedWorkflow` | Error |
| V-007 | Duration fields must be valid humantime | `InvalidDuration` | Error |
| V-008 | `url` fields must be valid URLs | `InvalidUrl` | Error |
| V-009 | `match` patterns must be valid globs | `InvalidGlobPattern` | Error |
| V-010 | At least one source must be defined | `NoSourcesDefined` | Error |
| V-011 | v0.2: Exactly one source allowed | `V02SingleSourceOnly` | Error |
| V-012 | v0.2: Source must be `kind: mcp` | `V02McpOnly` | Error |
| V-013 | Cedar policy files must exist | `PolicyFileNotFound` | Error |
| V-014 | Env var references must resolve | `MissingEnvVar` | Warning |
| V-TEL-001 | `sampling.success_sample_rate` must be 0.0 ≤ rate ≤ 1.0 | `InvalidSampleRate` | Error |
| V-TEL-002 | `otlp.endpoint` must be valid URL if set | `InvalidOtlpEndpoint` | Error |
| V-TEL-003 | `otlp.protocol` must be "http/protobuf" or "grpc" | `UnknownOtlpProtocol` | Error |
| V-TEL-004 | `sampling.strategy` must be "head" or "tail" | `UnknownSamplingStrategy` | Error |
| V-TEL-005 | `batch.max_queue_size` must be > 0 | `InvalidQueueSize` | Error |
| V-TEL-006 | `batch.scheduled_delay_ms` must be ≥ 100 | `InvalidBatchDelay` | Error |

### 8.2 Validation Implementation

```rust
impl Config {
    pub fn validate(&self, version: Version) -> Result<Vec<ValidationWarning>, ConfigError> {
        let mut warnings = Vec::new();
        
        // V-010: At least one source
        if self.sources.is_empty() {
            return Err(ConfigError::NoSourcesDefined);
        }
        
        // V-011, V-012: v0.2 restrictions
        if version.major == 0 && version.minor == 2 {
            if self.sources.len() != 1 {
                return Err(ConfigError::V02SingleSourceOnly);
            }
            if !matches!(self.sources[0], Source::Mcp { .. }) {
                return Err(ConfigError::V02McpOnly);
            }
        }
        
        // V-001: Unique source IDs
        let mut seen_ids = HashSet::new();
        for source in &self.sources {
            if !seen_ids.insert(source.id()) {
                return Err(ConfigError::DuplicateSourceId(source.id().to_string()));
            }
        }
        
        // V-002: Reserved prefix
        for source in &self.sources {
            if source.id().starts_with('_') {
                return Err(ConfigError::ReservedPrefix {
                    id: source.id().to_string(),
                });
            }
        }
        
        // V-005, V-006: Rule validation
        let workflow_names: HashSet<&str> = self.approval
            .as_ref()
            .map(|a| a.keys().map(|s| s.as_str()).collect())
            .unwrap_or_default();
        
        for rule in &self.governance.rules {
            // V-005: action: policy requires policy_id
            if rule.action == Action::Policy && rule.policy_id.is_none() {
                return Err(ConfigError::MissingPolicyId {
                    pattern: rule.pattern.clone(),
                });
            }
            
            // V-004: policy_id without action: policy
            if rule.policy_id.is_some() && rule.action != Action::Policy {
                warnings.push(ValidationWarning::PolicyIdWithoutPolicyAction {
                    pattern: rule.pattern.clone(),
                });
            }
            
            // V-006: approval workflow must exist
            if rule.action == Action::Approve {
                if let Some(ref workflow) = rule.approval {
                    if !workflow_names.contains(workflow.as_str()) && workflow != "default" {
                        return Err(ConfigError::UndefinedWorkflow {
                            workflow: workflow.clone(),
                            pattern: rule.pattern.clone(),
                        });
                    }
                }
            }
            
            // V-009: Valid glob pattern
            if let Err(e) = glob::Pattern::new(&rule.pattern) {
                return Err(ConfigError::InvalidGlobPattern {
                    pattern: rule.pattern.clone(),
                    message: e.to_string(),
                });
            }
        }
        
        // V-013: Cedar policy files exist
        if let Some(ref cedar) = self.cedar {
            for path in &cedar.policies {
                if !path.exists() {
                    return Err(ConfigError::PolicyFileNotFound {
                        path: path.clone(),
                    });
                }
            }
        }
        
        Ok(warnings)
    }
}
```

### 8.3 Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("No sources defined")]
    NoSourcesDefined,
    
    #[error("v0.2 supports only single source")]
    V02SingleSourceOnly,
    
    #[error("v0.2 supports only kind: mcp")]
    V02McpOnly,
    
    #[error("Duplicate source ID: {0}")]
    DuplicateSourceId(String),
    
    #[error("Reserved prefix in source ID: {id}")]
    ReservedPrefix { id: String },
    
    #[error("Duplicate prefix: {0}")]
    DuplicatePrefix(String),
    
    #[error("Missing policy_id for action: policy in rule '{pattern}'")]
    MissingPolicyId { pattern: String },
    
    #[error("Undefined workflow '{workflow}' in rule '{pattern}'")]
    UndefinedWorkflow { workflow: String, pattern: String },
    
    #[error("Invalid duration '{value}': {message}")]
    InvalidDuration { value: String, message: String },
    
    #[error("Invalid URL '{url}': {message}")]
    InvalidUrl { url: String, message: String },
    
    #[error("Invalid glob pattern '{pattern}': {message}")]
    InvalidGlobPattern { pattern: String, message: String },
    
    #[error("Policy file not found: {path}")]
    PolicyFileNotFound { path: PathBuf },
    
    #[error("YAML parse error: {0}")]
    ParseError(#[from] serde_yaml::Error),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[derive(Debug)]
pub enum ValidationWarning {
    PolicyIdWithoutPolicyAction { pattern: String },
    MissingEnvVar { var: String, field: String },
}
```

### 8.8 External Approval Service (EXPERIMENTAL)

⚠️ **EXPERIMENTAL**: This interface is available in v0.2 but may change. Mark configurations using `approval_service` as experimental.

This destination type allows ThoughtGate to integrate with enterprise approval systems (ServiceNow, Jira, internal tools) without waiting for native adapters.

**Configuration Example:**

```yaml
approval:
  enterprise:
    destination:
      type: approval_service
      url: https://approvals.internal/api/v1/requests
      auth:
        type: bearer
        token_env: APPROVAL_SERVICE_TOKEN
      headers:
        X-Source: thoughtgate
        X-Environment: production
    timeout: 30m
    on_timeout: deny
```

**Request Payload (POST to `url`):**

```json
{
  "version": "1.0",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2025-01-13T10:30:00.123Z",
  "tool": {
    "name": "delete_user",
    "arguments": {
      "user_id": "12345"
    }
  },
  "principal": {
    "namespace": "production",
    "app": "payment-agent"
  },
  "context": {
    "matched_rule": "delete_*",
    "workflow": "enterprise",
    "timeout_seconds": 1800
  },
  "callback": {
    "poll_url": "https://thoughtgate.internal/api/v1/approval/{request_id}/status",
    "decision_url": "https://thoughtgate.internal/api/v1/approval/{request_id}/decide"
  }
}
```

**Response (Expected from approval service):**

```json
{
  "status": "pending",
  "approval_id": "sn-req-98765",
  "external_url": "https://servicenow.company.com/approval/98765"
}
```

**Decision Callback (POST to `decision_url`):**

```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "decision": "approved",  // or "rejected"
  "decided_by": "jane.smith@company.com",
  "decided_at": "2025-01-13T10:35:00.000Z",
  "reason": "Verified user termination request"
}
```

**Error Response:**

```json
{
  "error": "service_unavailable",
  "message": "Approval service temporarily unavailable",
  "retry_after_seconds": 60
}
```

## 9. Behavior Specification

### 9.1 Configuration Loading Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                    CONFIGURATION LOADING                         │
└─────────────────────────────────────────────────────────────────┘

  1. Locate config file
     │
     ├─ CLI flag: --config <path>
     ├─ Env var: $THOUGHTGATE_CONFIG
     ├─ Default: /etc/thoughtgate/config.yaml
     └─ Fallback: ./config.yaml
     │
     ▼
  2. Read file contents
     │
     └─ If file not found → ConfigError::IoError
     │
     ▼
  3. Environment variable substitution
     │
     ├─ Replace ${VAR} with env value
     ├─ Replace ${VAR:-default} with env or default
     └─ If required var missing → ConfigError (or Warning)
     │
     ▼
  4. Parse YAML
     │
     └─ If invalid YAML → ConfigError::ParseError
     │
     ▼
  5. Validate configuration
     │
     ├─ Run all V-xxx rules
     ├─ Collect warnings
     └─ If error → ConfigError
     │
     ▼
  6. Return Config + Warnings
```

### 9.2 Rule Matching (Governance Gate)

```rust
impl Governance {
    /// Find first matching rule, or return default action
    pub fn evaluate(&self, tool_name: &str, source_id: &str) -> MatchResult {
        for rule in &self.rules {
            // Check source filter
            if let Some(ref filter) = rule.source {
                if !filter.matches(source_id) {
                    continue;
                }
            }
            
            // Check pattern match
            let pattern = glob::Pattern::new(&rule.pattern).unwrap();
            if pattern.matches(tool_name) {
                return MatchResult {
                    action: rule.action,
                    policy_id: rule.policy_id.clone(),
                    approval_workflow: rule.approval.clone(),
                    matched_rule: Some(rule.pattern.clone()),
                };
            }
        }
        
        // No rule matched, use default
        MatchResult {
            action: self.defaults.action,
            policy_id: None,
            approval_workflow: None,
            matched_rule: None,
        }
    }
}

pub struct MatchResult {
    pub action: Action,
    pub policy_id: Option<String>,
    pub approval_workflow: Option<String>,
    pub matched_rule: Option<String>,
}
```

### 9.3 Exposure Filtering (Visibility Gate)

```rust
impl ExposeConfig {
    /// Check if a tool name should be visible
    pub fn is_visible(&self, tool_name: &str) -> bool {
        match self {
            ExposeConfig::All => true,
            
            ExposeConfig::Allowlist { tools } => {
                tools.iter().any(|pattern| {
                    glob::Pattern::new(pattern)
                        .map(|p| p.matches(tool_name))
                        .unwrap_or(false)
                })
            }
            
            ExposeConfig::Blocklist { tools } => {
                !tools.iter().any(|pattern| {
                    glob::Pattern::new(pattern)
                        .map(|p| p.matches(tool_name))
                        .unwrap_or(false)
                })
            }
        }
    }
}
```

### 9.4 Hot-Reload

```rust
pub struct ConfigWatcher {
    config: ArcSwap<Config>,
    path: PathBuf,
}

impl ConfigWatcher {
    pub fn new(path: PathBuf) -> Result<Self, ConfigError> {
        let config = load_config(&path)?;
        Ok(Self {
            config: ArcSwap::new(Arc::new(config)),
            path,
        })
    }
    
    /// Start watching for changes
    pub fn watch(&self) -> Result<(), ConfigError> {
        let path = self.path.clone();
        let config = self.config.clone();
        
        std::thread::spawn(move || {
            let mut last_modified = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok();
            
            loop {
                std::thread::sleep(Duration::from_secs(10));
                
                let current_modified = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok();
                
                if current_modified != last_modified {
                    match load_config(&path) {
                        Ok(new_config) => {
                            info!("Configuration reloaded");
                            config.store(Arc::new(new_config));
                            last_modified = current_modified;
                        }
                        Err(e) => {
                            error!("Failed to reload config: {}", e);
                            // Keep using old config
                        }
                    }
                }
            }
        });
        
        Ok(())
    }
    
    /// Get current configuration
    pub fn get(&self) -> Arc<Config> {
        self.config.load_full()
    }
}
```

## 10. Integration Points

### 10.1 With REQ-POL-001 (Cedar Policy Engine)

**Important:** Cedar policies return only `permit` or `forbid` decisions. They cannot return arbitrary data like approval workflow names. To map a Cedar policy to an approval workflow, use **static annotations** on the policy.

**Cedar Policy with Annotations:**

```cedar
// ✅ CORRECT: Use annotations to specify workflow
@id("payment_high_value")
@thoughtgate_approval("treasury_workflow")  // Maps to YAML approval config
permit(
    principal,
    action == Action::"execute",
    resource
)
when {
    resource.tool == "transfer_funds" &&
    resource.arguments.amount > 10000
};
```

**ThoughtGate reads the annotation at policy load time:**

```rust
// At startup: Parse policy annotations and build lookup table
fn load_policy_annotations(policy_set: &PolicySet) -> HashMap<PolicyId, String> {
    let mut annotations = HashMap::new();
    for policy in policy_set.policies() {
        if let Some(workflow) = policy.annotation("thoughtgate_approval") {
            annotations.insert(policy.id().clone(), workflow.to_string());
        }
    }
    annotations
}

// At evaluation time: Get workflow from annotation lookup
let match_result = config.governance.evaluate(tool_name, source_id);
if match_result.action == Action::Policy {
    let policy_id = match_result.policy_id.expect("validated");
    let decision = cedar_engine.evaluate(principal, resource, policy_id);
    
    if decision == CedarDecision::Permit {
        // Get workflow from annotation (not from Cedar response)
        let workflow_name = policy_annotations
            .get(&determining_policy_id)
            .cloned()
            .or(match_result.approval_workflow)  // Fallback to YAML rule
            .unwrap_or_else(|| "default".to_string());
        
        // Continue to Gate 4 with this workflow
    }
}
```

**Workflow Resolution Priority:**
1. Cedar policy `@thoughtgate_approval` annotation (if policy permits)
2. YAML rule `approval:` field (from governance.rules)
3. Fallback: `"default"` workflow

### 10.2 With REQ-GOV-003 (Approval Integration)

```rust
// Config provides approval workflow configuration
let workflow_name = match_result.approval_workflow.unwrap_or("default".to_string());
let workflow = config.approval
    .as_ref()
    .and_then(|a| a.get(&workflow_name))
    .ok_or(ConfigError::UndefinedWorkflow { .. })?;

// Use workflow destination and timeout
let approval_request = ApprovalRequest {
    destination: workflow.destination.clone(),
    timeout: workflow.timeout.unwrap_or(Duration::from_secs(600)),
    on_timeout: workflow.on_timeout.clone().unwrap_or_default(),
};
```

### 10.3 With REQ-CORE-003 (MCP Transport)

```rust
// Config provides upstream URL
let source = config.sources.first().expect("validated");
let upstream_url = match source {
    Source::Mcp { url, .. } => url,
};

// Config provides tool visibility
let expose = match source {
    Source::Mcp { expose, .. } => expose.clone().unwrap_or_default(),
};
if !expose.is_visible(tool_name) {
    return Err(ThoughtGateError::MethodNotFound { method: tool_name.to_string() });
}
```

## 11. Testing Requirements

### 11.1 Unit Tests

| Test | Description |
|------|-------------|
| `test_parse_minimal_config` | Parse minimal valid v0.2 config |
| `test_parse_full_config` | Parse config with all v0.2 fields |
| `test_validate_duplicate_source_id` | V-001 validation |
| `test_validate_reserved_prefix` | V-002 validation |
| `test_validate_missing_policy_id` | V-005 validation |
| `test_validate_undefined_workflow` | V-006 validation |
| `test_validate_v02_single_source` | V-011 validation |
| `test_validate_v02_mcp_only` | V-012 validation |
| `test_env_var_substitution` | ${VAR} replacement |
| `test_env_var_default` | ${VAR:-default} replacement |
| `test_rule_matching_exact` | Exact tool name match |
| `test_rule_matching_glob` | Glob pattern match |
| `test_rule_matching_first_wins` | First match wins behavior |
| `test_expose_allowlist` | Allowlist filtering |
| `test_expose_blocklist` | Blocklist filtering |
| `test_telemetry_config_defaults` | Telemetry defaults applied correctly |
| `test_telemetry_invalid_sample_rate` | V-TEL-001 validation (rate out of bounds) |
| `test_telemetry_invalid_otlp_endpoint` | V-TEL-002 validation (malformed URL) |
| `test_telemetry_unknown_protocol` | V-TEL-003 validation (invalid protocol) |
| `test_telemetry_env_override` | OTEL_* env vars override YAML |

### 11.2 Integration Tests

| Test | Description |
|------|-------------|
| `test_hot_reload` | Config changes detected and applied |
| `test_hot_reload_invalid` | Invalid config keeps old config |
| `test_file_not_found` | Graceful error on missing file |
| `test_cedar_integration` | Policy paths passed to Cedar engine |
| `test_approval_integration` | Workflow config used by approval |

### 11.3 Edge Case Matrix

| Scenario | Expected Behavior | Test ID |
|----------|-------------------|---------|
| Empty config file (0 bytes) | Fail with clear error | EC-CFG-001 |
| Valid YAML, unsupported schema version | Fail with version error | EC-CFG-002 |
| Env var undefined, no default (`${MISSING}`) | Fail with env var error | EC-CFG-003 |
| Env var undefined, has default (`${MISSING:-default}`) | Use default value | EC-CFG-004 |
| Hot reload during active request | Complete request with old config | EC-CFG-005 |
| Hot reload with syntax error | Keep old config, log error | EC-CFG-006 |
| Config file deleted while running | Keep old config, log warning | EC-CFG-007 |
| Glob pattern matches zero tools | Warning, no error | EC-CFG-008 |
| Rule matches tool that doesn't exist | Warning only, rule still valid | EC-CFG-009 |
| Duplicate rule patterns | First match wins (no error) | EC-CFG-010 |

## 12. Observability

### 12.1 Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `thoughtgate_config_load_total` | Counter | `status` | Config load attempts |
| `thoughtgate_config_load_duration_seconds` | Histogram | - | Config load time |
| `thoughtgate_config_reload_total` | Counter | `status` | Hot-reload attempts |
| `thoughtgate_rule_matches_total` | Counter | `action`, `rule` | Rule match counts |
| `thoughtgate_validation_warnings_total` | Counter | `type` | Validation warnings |
| `thoughtgate_config_age_seconds` | Gauge | - | Time since last config reload |

### 12.2 Logging

| Event | Level | Fields |
|-------|-------|--------|
| Config loaded | INFO | `path`, `sources_count`, `rules_count` |
| Config reloaded | INFO | `path` |
| Config reload failed | ERROR | `path`, `error` |
| Validation warning | WARN | `warning_type`, `details` |
| Rule matched | DEBUG | `tool`, `rule`, `action` |
| No rule matched | DEBUG | `tool`, `default_action` |

## 13. Example Configurations

### 13.1 Minimal v0.2

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${UPSTREAM_URL}  # Set via UPSTREAM_URL env var

governance:
  defaults:
    action: forward
```

**Required environment variable:**
```bash
export UPSTREAM_URL=http://mcp-server:3000
```

### 13.2 With Approval

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${UPSTREAM_URL}
    description: "Primary MCP server"

governance:
  defaults:
    action: forward

  rules:
    - match: "delete_*"
      action: approve
      approval: default
      description: "All deletions require approval"

    - match: "*_unsafe"
      action: deny
      description: "Block unsafe operations"

approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
      mention:
        - "@oncall"
    timeout: 10m
    on_timeout: deny
```

### 13.3 With Cedar Policies

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${UPSTREAM_URL}

governance:
  defaults:
    action: forward

  rules:
    - match: "transfer_*"
      action: policy
      policy_id: "financial"
      approval: finance          # Fallback if Cedar policy has no annotation
      description: "Financial ops evaluated by Cedar"

    - match: "admin_*"
      action: deny
      description: "Block admin operations"

approval:
  finance:
    destination:
      type: slack
      channel: "#finance-approvals"
    timeout: 30m
    on_timeout: deny
  
  treasury:                      # Referenced by Cedar policy annotation
    destination:
      type: slack
      channel: "#treasury-approvals"
      mention:
        - "@cfo"
    timeout: 1h
    on_timeout: deny

cedar:
  policies:
    - /etc/thoughtgate/policies/financial.cedar
  schema: /etc/thoughtgate/schema.cedarschema
```

**Corresponding Cedar Policy (`financial.cedar`):**

```cedar
// Standard transfers → use YAML workflow (finance)
@id("financial_standard")
permit(
    principal,
    action == Action::"execute",
    resource
)
when {
    resource.tool == "transfer_funds" &&
    resource.arguments.amount <= 10000
};

// High-value transfers → override to treasury workflow via annotation
@id("financial_high_value")
@thoughtgate_approval("treasury")
permit(
    principal,
    action == Action::"execute",
    resource
)
when {
    resource.tool == "transfer_funds" &&
    resource.arguments.amount > 10000
};
```

### 13.4 With Full Observability

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: ${UPSTREAM_URL}

governance:
  defaults:
    action: forward

  rules:
    - match: "delete_*"
      action: approve
      approval: default

approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
      token_env: SLACK_BOT_TOKEN
    timeout: 10m
    on_timeout: deny

telemetry:
  enabled: true
  otlp:
    endpoint: "http://otel-collector:4318"
    protocol: http/protobuf
  prometheus:
    enabled: true
  sampling:
    strategy: head
    success_sample_rate: 0.10  # Sample 10% of successful requests
  batch:
    max_queue_size: 2048
    scheduled_delay_ms: 5000
  resource:
    service.name: thoughtgate
    deployment.environment: ${DEPLOY_ENV:-production}
```

**Required environment variables:**
```bash
export UPSTREAM_URL=http://mcp-server:3000
export SLACK_BOT_TOKEN=xoxb-your-token
export DEPLOY_ENV=production  # Optional, defaults to "production"
```

**Optional OTel overrides:**
```bash
# These take precedence over YAML config
export OTEL_EXPORTER_OTLP_ENDPOINT=http://different-collector:4318
export OTEL_SERVICE_NAME=my-thoughtgate-instance
```

## 14. Migration & Compatibility

### 14.1 From v0.1 (No Config File)

v0.1 used environment variables and CLI flags. Migration:

| v0.1 | v0.2 Config |
|------|-------------|
| `$MCP_SERVER_URL` | `$UPSTREAM_URL` (substituted into `sources[0].url`) |
| `$SLACK_CHANNEL` | `approval.default.destination.channel` |
| `$APPROVAL_TIMEOUT_SECS` | `approval.default.timeout` |
| Cedar policy path (CLI) | `cedar.policies` |

### 14.3 Port Configuration

ThoughtGate v0.2 uses a 3-port Envoy-style architecture:

| Port | Env Variable | Default | Purpose |
|------|--------------|---------|---------|
| Outbound | `THOUGHTGATE_OUTBOUND_PORT` | 7467 | MCP traffic (agent → upstream) |
| Inbound | (reserved) | 7468 | Future callbacks (not wired) |
| Admin | `THOUGHTGATE_ADMIN_PORT` | 7469 | Health, ready, metrics |

**Port environment variables:**

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTGATE_OUTBOUND_PORT` | `7467` | Main proxy port for MCP traffic |
| `THOUGHTGATE_ADMIN_PORT` | `7469` | Admin port for health/ready/metrics |
| `UPSTREAM_URL` | (required) | Upstream server URL (all traffic) |

**Note:** Port 7468 is reserved for future inbound callback functionality.

### 14.2 Experimental Mode

Set `THOUGHTGATE_EXPERIMENTAL=1` to test v0.3+ features:

```bash
THOUGHTGATE_EXPERIMENTAL=1 thoughtgate --config config.yaml
```

This allows:
- Multiple sources (with warning)
- A2A sources (with warning)

⚠️ Experimental features may change without notice.

## 15. Open Questions

| # | Question | Options | Decision |
|---|----------|---------|----------|
| 1 | Should glob patterns support `**`? | Yes / No | No (single-level only) |
| 2 | Should config support TOML alternative? | Yes / No | No (YAML only) |
| 3 | Should we validate URL connectivity? | Yes / No | No (warning only) |
| 4 | ~~Support ISO 8601 durations?~~ | Yes / No | **Yes** - Both humantime and ISO 8601 (§5.4) |
| 5 | ~~Define approval service API in v0.2?~~ | Yes / No | **Yes** - Experimental (§8.8) |
| 6 | ~~Cedar `advice` block for workflow routing?~~ | Yes / No | **No** - Use `@thoughtgate_approval` annotation (§10.1) |

---

## Appendix A: Request Decision Flow

This diagram shows how configuration controls the 4-gate request flow:

```
  Agent Request (tool + arguments)
         │
         ▼
  ┌──────────────────────────────────────────────────────────────┐
  │ GATE 1: Visibility (expose config)                           │
  │                                                              │
  │   expose:                                                    │
  │     mode: allowlist | blocklist | all                        │
  │     tools: ["pattern_*"]                                     │
  │                                                              │
  │   If NOT visible → -32015 Tool Not Exposed                   │
  └──────────────────────────────────────────────────────────────┘
         │
         │ Tool is visible
         ▼
  ┌──────────────────────────────────────────────────────────────┐
  │ GATE 2: Governance Rules (governance.rules)                  │
  │                                                              │
  │   rules:                                                     │
  │     - match: "delete_*"    ──→ action: approve               │
  │     - match: "transfer_*"  ──→ action: policy                │
  │     - match: "*_unsafe"    ──→ action: deny                  │
  │                                                              │
  │   First match wins. No match → governance.defaults.action    │
  └──────────────────────────────────────────────────────────────┘
         │
         ├─── action: forward ──────────────────────┐
         │                                          │
         ├─── action: deny ─────────→ ❌ REJECT     │
         │                                          │
         ├─── action: policy ───┐                   │
         │                      ▼                   │
         │    ┌─────────────────────────────────┐   │
         │    │ GATE 3: Cedar Policy            │   │
         │    │                                 │   │
         │    │   cedar.policies evaluate:      │   │
         │    │   - policy_id from rule         │   │
         │    │   - resource.arguments          │   │
         │    │   - principal (agent identity)  │   │
         │    │                                 │   │
         │    │   Forbid → ❌ REJECT            │   │
         │    │   Permit → continue             │   │
         │    └─────────────────────────────────┘   │
         │                      │                   │
         │                      ▼                   │
         ├─── action: approve ──┼───────────────────┤
         │                      │                   │
         ▼                      ▼                   │
  ┌──────────────────────────────────────────────────────────────┐
  │ GATE 4: Approval Workflow (approval config)                  │
  │                                                              │
  │   v0.2: Human approval only                                  │
  │                                                              │
  │   approval:                                                  │
  │     default:                                                 │
  │       destination:                                           │
  │         type: slack                                          │
  │         channel: "#approvals"                                │
  │       timeout: 10m                                           │
  │       on_timeout: deny                                       │
  │                                                              │
  │   Wait for approval decision...                              │
  │                                                              │
  │   Approved → continue                                        │
  │   Rejected → ❌ REJECT                                       │
  │   Timeout  → on_timeout action                               │
  └──────────────────────────────────────────────────────────────┘
         │
         │ Approved (or forward)
         ▼
  ┌──────────────────────────────────────────────────────────────┐
  │ FORWARD TO UPSTREAM                                          │
  │                                                              │
  │   sources[0].url: ${UPSTREAM_URL}                            │
  │                                                              │
  │   → Execute tool call                                        │
  │   → Return result to agent                                   │
  └──────────────────────────────────────────────────────────────┘
```

**Gate Summary:**

| Gate | Config Section | Purpose | v0.2 Support |
|------|----------------|---------|--------------|
| 1. Visibility | `sources[].expose` | Hide tools from agent | ✅ |
| 2. Governance | `governance.rules` | Route by action | ✅ |
| 3. Cedar | `cedar.policies` | Complex policy decisions | ✅ |
| 4. Approval | `approval.*` | Human/A2A approval | ✅ Human only |

**Action Flow:**

| Action | Gate 3 (Cedar) | Gate 4 (Approval) | Result |
|--------|----------------|-------------------|--------|
| `forward` | Skip | Skip | Forward immediately |
| `deny` | Skip | Skip | Reject immediately |
| `approve` | Skip | Execute | Forward if approved |
| `policy` | Evaluate | If permit: Execute | Depends on Cedar + Approval |