//! Configuration schema type definitions.
//!
//! Implements: REQ-CFG-001 Section 7 (Data Structures)
//!
//! # Traceability
//! - Implements: REQ-CFG-001/7.1 through 7.6

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use super::defaults::ThoughtGateDefaults;
use super::duration_format;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 7.1 Top-Level Schema
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Root configuration structure.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 7.1 (Top-Level Schema)
///
/// # Example
/// ```yaml
/// schema: 1
///
/// sources:
///   - id: upstream
///     kind: mcp
///     url: http://mcp-server:8080
///
/// governance:
///   defaults:
///     action: forward
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Schema version (must be 1).
    pub schema: u32,

    /// Capability sources (v0.2: exactly one MCP source).
    pub sources: Vec<Source>,

    /// Governance rules.
    pub governance: Governance,

    /// Human approval workflows.
    #[serde(default)]
    pub approval: Option<HashMap<String, HumanWorkflow>>,

    /// Cedar policy configuration.
    #[serde(default)]
    pub cedar: Option<CedarConfig>,

    /// Telemetry configuration (REQ-OBS-002).
    #[serde(default)]
    pub telemetry: Option<TelemetryYamlConfig>,
}

impl Config {
    /// Get a source by ID.
    pub fn get_source(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.id() == id)
    }

    /// Get the first (and in v0.2, only) source.
    pub fn primary_source(&self) -> Option<&Source> {
        self.sources.first()
    }

    /// Get an approval workflow by name.
    pub fn get_workflow(&self, name: &str) -> Option<&HumanWorkflow> {
        self.approval.as_ref()?.get(name)
    }

    /// Pre-compile all glob patterns for rules and exposure configs.
    ///
    /// Called once after config loading to avoid re-parsing glob strings
    /// on every request. Returns an error if any pattern is invalid.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidGlobPattern` if any glob pattern fails to compile.
    pub fn compile_patterns(&mut self) -> Result<(), super::error::ConfigError> {
        // Compile governance rule patterns
        for rule in &mut self.governance.rules {
            match glob::Pattern::new(&rule.pattern) {
                Ok(compiled) => rule.compiled_pattern = Some(compiled),
                Err(e) => {
                    return Err(super::error::ConfigError::InvalidGlobPattern {
                        pattern: rule.pattern.clone(),
                        message: e.to_string(),
                    });
                }
            }
        }

        // Compile expose config patterns for each source
        for source in &mut self.sources {
            if let Some(expose) = source.expose_mut() {
                match expose {
                    ExposeConfig::Allowlist {
                        tools,
                        compiled_tools,
                    }
                    | ExposeConfig::Blocklist {
                        tools,
                        compiled_tools,
                    } => {
                        let mut compiled = Vec::with_capacity(tools.len());
                        for tool_pattern in tools.iter() {
                            match glob::Pattern::new(tool_pattern) {
                                Ok(p) => compiled.push(p),
                                Err(e) => {
                                    return Err(super::error::ConfigError::InvalidGlobPattern {
                                        pattern: tool_pattern.clone(),
                                        message: e.to_string(),
                                    });
                                }
                            }
                        }
                        *compiled_tools = compiled;
                    }
                    ExposeConfig::All => {}
                }
            }
        }
        Ok(())
    }

    /// Check if this configuration requires an approval engine.
    ///
    /// Returns true if any governance rules could lead to Gate 4 (approval):
    /// - `action: approve` - directly requires approval
    /// - `action: policy` - Cedar Permit routes to Gate 4 for approval
    ///
    /// This is used to decide whether to initialize the ApprovalEngine and
    /// require Slack credentials.
    pub fn requires_approval_engine(&self) -> bool {
        // Check if default action could lead to approval
        let default_needs_approval = matches!(
            self.governance.defaults.action,
            Action::Approve | Action::Policy
        );
        if default_needs_approval {
            return true;
        }

        // Check if any rule uses approve or policy action
        // (policy rules route to Gate 4 after Cedar permit)
        self.governance
            .rules
            .iter()
            .any(|rule| matches!(rule.action, Action::Approve | Action::Policy))
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 7.2 Source Configuration
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Source configuration for capability providers.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 7.2 (Source Configuration)
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind")]
pub enum Source {
    /// Static MCP server (v0.2).
    #[serde(rename = "mcp")]
    Mcp {
        /// Unique identifier for this source.
        id: String,

        /// URL of the MCP server.
        url: String,

        /// Optional prefix for tool names from this source.
        #[serde(default)]
        prefix: Option<String>,

        /// Tool exposure configuration.
        #[serde(default)]
        expose: Option<ExposeConfig>,

        /// Whether this source is enabled.
        #[serde(default = "default_true")]
        enabled: bool,
    },
    // v0.3+: A2a
    // v0.4+: McpDiscovery, OpenApi, A2aDiscovery
}

fn default_true() -> bool {
    true
}

impl Source {
    /// Get the source ID.
    pub fn id(&self) -> &str {
        match self {
            Source::Mcp { id, .. } => id,
        }
    }

    /// Get the source URL.
    pub fn url(&self) -> &str {
        match self {
            Source::Mcp { url, .. } => url,
        }
    }

    /// Get the optional prefix.
    pub fn prefix(&self) -> Option<&str> {
        match self {
            Source::Mcp { prefix, .. } => prefix.as_deref(),
        }
    }

    /// Check if this source is enabled.
    pub fn is_enabled(&self) -> bool {
        match self {
            Source::Mcp { enabled, .. } => *enabled,
        }
    }

    /// Get the exposure configuration.
    pub fn expose(&self) -> ExposeConfig {
        match self {
            Source::Mcp { expose, .. } => expose.clone().unwrap_or_default(),
        }
    }

    /// Get a mutable reference to the exposure configuration.
    fn expose_mut(&mut self) -> Option<&mut ExposeConfig> {
        match self {
            Source::Mcp { expose, .. } => expose.as_mut(),
        }
    }

    /// Get the source kind as a string.
    pub fn kind(&self) -> &'static str {
        match self {
            Source::Mcp { .. } => "mcp",
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 7.3 Exposure Configuration
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Tool exposure configuration for Gate 1 (Visibility).
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 7.3 (Exposure Configuration)
/// - Implements: REQ-CFG-001 Section 9.3 (Exposure Filtering)
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(tag = "mode")]
pub enum ExposeConfig {
    /// All tools visible (default).
    #[default]
    #[serde(rename = "all")]
    All,

    /// Only listed patterns visible.
    #[serde(rename = "allowlist")]
    Allowlist {
        tools: Vec<String>,
        /// Pre-compiled glob patterns (populated by `Config::compile_patterns()`).
        #[serde(skip)]
        compiled_tools: Vec<glob::Pattern>,
    },

    /// All except listed patterns visible.
    #[serde(rename = "blocklist")]
    Blocklist {
        tools: Vec<String>,
        /// Pre-compiled glob patterns (populated by `Config::compile_patterns()`).
        #[serde(skip)]
        compiled_tools: Vec<glob::Pattern>,
    },
}

impl ExposeConfig {
    /// Check if a tool name should be visible.
    ///
    /// # Traceability
    /// - Implements: REQ-CFG-001 Section 9.3 (Exposure Filtering)
    pub fn is_visible(&self, tool_name: &str) -> bool {
        match self {
            ExposeConfig::All => true,
            ExposeConfig::Allowlist {
                tools,
                compiled_tools,
            } => {
                if !compiled_tools.is_empty() {
                    compiled_tools.iter().any(|p| p.matches(tool_name))
                } else {
                    tools.iter().any(|pattern| {
                        glob::Pattern::new(pattern)
                            .map(|p| p.matches(tool_name))
                            .unwrap_or(false)
                    })
                }
            }
            ExposeConfig::Blocklist {
                tools,
                compiled_tools,
            } => {
                if !compiled_tools.is_empty() {
                    !compiled_tools.iter().any(|p| p.matches(tool_name))
                } else {
                    !tools.iter().any(|pattern| {
                        glob::Pattern::new(pattern)
                            .map(|p| p.matches(tool_name))
                            .unwrap_or(false)
                    })
                }
            }
        }
    }

    /// Get the patterns for validation.
    ///
    /// Returns None for All mode, Some with patterns for allowlist/blocklist.
    pub fn patterns(&self) -> Option<&[String]> {
        match self {
            ExposeConfig::All => None,
            ExposeConfig::Allowlist { tools, .. } => Some(tools),
            ExposeConfig::Blocklist { tools, .. } => Some(tools),
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 7.4 Governance Configuration
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Governance configuration for Gate 2 (Rules).
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 7.4 (Governance Configuration)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Governance {
    /// Default action when no rule matches.
    pub defaults: GovernanceDefaults,

    /// Ordered list of rules (first match wins).
    #[serde(default)]
    pub rules: Vec<Rule>,
}

/// Default governance settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GovernanceDefaults {
    /// Default action to take when no rule matches.
    pub action: Action,
}

/// Actions that can be taken for a tool call.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 5.2 (v0.2 Feature Restrictions)
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Forward to upstream immediately.
    Forward,
    /// Require human approval.
    Approve,
    /// Reject immediately.
    Deny,
    /// Evaluate Cedar policy.
    Policy,
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::Forward => write!(f, "forward"),
            Action::Approve => write!(f, "approve"),
            Action::Deny => write!(f, "deny"),
            Action::Policy => write!(f, "policy"),
        }
    }
}

/// A governance rule.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 7.4 (Rule)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Rule {
    /// Glob pattern for tool name matching.
    #[serde(rename = "match")]
    pub pattern: String,

    /// Action to take when matched.
    pub action: Action,

    /// Filter to specific source(s).
    #[serde(default)]
    pub source: Option<SourceFilter>,

    /// Cedar policy ID (required if action: policy).
    #[serde(default)]
    pub policy_id: Option<String>,

    /// Approval workflow name (for action: approve).
    #[serde(default)]
    pub approval: Option<String>,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Pre-compiled glob pattern (populated by `Config::compile_patterns()`).
    /// Avoids re-parsing the glob on every request evaluation.
    #[serde(skip)]
    pub compiled_pattern: Option<glob::Pattern>,
}

/// Source filter for rules.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SourceFilter {
    /// Match a single source.
    Single(String),
    /// Match multiple sources.
    Multiple(Vec<String>),
}

impl SourceFilter {
    /// Check if the filter matches a source ID.
    pub fn matches(&self, source_id: &str) -> bool {
        match self {
            SourceFilter::Single(id) => id == source_id,
            SourceFilter::Multiple(ids) => ids.iter().any(|id| id == source_id),
        }
    }
}

/// Result of evaluating governance rules.
#[derive(Debug, Clone)]
pub struct MatchResult {
    /// The action to take.
    pub action: Action,
    /// Cedar policy ID (if action is Policy).
    pub policy_id: Option<String>,
    /// Approval workflow name.
    pub approval_workflow: Option<String>,
    /// The pattern that matched (None if default).
    pub matched_rule: Option<String>,
}

impl Governance {
    /// Find first matching rule, or return default action.
    ///
    /// # Traceability
    /// - Implements: REQ-CFG-001 Section 9.2 (Rule Matching)
    pub fn evaluate(&self, tool_name: &str, source_id: &str) -> MatchResult {
        for rule in &self.rules {
            // Check source filter
            if let Some(ref filter) = rule.source {
                if !filter.matches(source_id) {
                    continue;
                }
            }

            // Check pattern match (use pre-compiled if available)
            let matches = if let Some(ref compiled) = rule.compiled_pattern {
                compiled.matches(tool_name)
            } else if let Ok(pattern) = glob::Pattern::new(&rule.pattern) {
                pattern.matches(tool_name)
            } else {
                false
            };
            if matches {
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

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 7.5 Approval Configuration
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Human approval workflow configuration.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 7.5 (Approval Configuration)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HumanWorkflow {
    /// Destination for approval requests.
    pub destination: ApprovalDestination,

    /// Timeout for approval (defaults to 10 minutes).
    #[serde(
        default,
        deserialize_with = "duration_format::deserialize_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub timeout: Option<Duration>,

    /// Action to take on timeout.
    #[serde(default)]
    pub on_timeout: Option<TimeoutAction>,
}

impl HumanWorkflow {
    /// Get the timeout, using default if not specified.
    pub fn timeout_or_default(&self) -> Duration {
        self.timeout
            .unwrap_or(ThoughtGateDefaults::default().default_approval_timeout)
    }

    /// Get the timeout action, using default if not specified.
    pub fn on_timeout_or_default(&self) -> TimeoutAction {
        self.on_timeout.clone().unwrap_or_default()
    }
}

/// Destination for approval requests.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 7.5 (ApprovalDestination)
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ApprovalDestination {
    /// Send to Slack channel.
    #[serde(rename = "slack")]
    Slack {
        /// Slack channel (e.g., "#approvals").
        channel: String,
        /// Environment variable containing the bot token.
        #[serde(default)]
        token_env: Option<String>,
        /// Users to mention.
        #[serde(default)]
        mention: Option<Vec<String>>,
    },
}

/// Action to take when approval times out.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutAction {
    /// Deny the request on timeout.
    #[default]
    Deny,
    // v0.3+: Escalate, AutoApprove
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 7.6 Cedar Configuration
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Cedar policy engine configuration.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 7.6 (Cedar Configuration)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CedarConfig {
    /// List of Cedar policy file paths.
    pub policies: Vec<PathBuf>,

    /// Optional Cedar schema file.
    #[serde(default)]
    pub schema: Option<PathBuf>,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 7.7 Telemetry Configuration (REQ-OBS-002)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// YAML configuration for telemetry (tracing and metrics export).
///
/// # Traceability
/// - Implements: REQ-OBS-002 §8.2 (OTLP Export Configuration)
/// - Implements: REQ-OBS-002 §8.5 (Sampling Strategies)
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TelemetryYamlConfig {
    /// Master enable/disable switch for OTLP trace export.
    /// Default: false (B-OBS2-001: disabled by default)
    #[serde(default)]
    pub enabled: bool,

    /// OTLP export configuration.
    #[serde(default)]
    pub otlp: Option<OtlpConfig>,

    /// Sampling configuration.
    #[serde(default)]
    pub sampling: Option<SamplingConfig>,

    /// Batch processor configuration.
    #[serde(default)]
    pub batch: Option<BatchConfig>,

    /// Resource attributes for OTLP export.
    #[serde(default)]
    pub resource: Option<HashMap<String, String>>,

    /// Stdio transport configuration (REQ-OBS-002 §7.3).
    #[serde(default)]
    pub stdio: Option<StdioTelemetryConfig>,
}

/// Stdio transport telemetry configuration.
///
/// # Traceability
/// - Implements: REQ-OBS-002 §7.3.1 (Stdio Transport Propagation)
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct StdioTelemetryConfig {
    /// If true, preserve `_meta.traceparent` and `_meta.tracestate` when
    /// forwarding to upstream MCP servers.
    ///
    /// Default: false (strip trace fields to avoid breaking strict schema servers).
    ///
    /// Set to true when the upstream MCP server is trace-aware and can accept
    /// W3C trace context in the `_meta` field.
    #[serde(default)]
    pub propagate_upstream: bool,
}

/// OTLP exporter configuration.
///
/// Implements: REQ-CFG-001 §8.2
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OtlpConfig {
    /// OTLP HTTP endpoint (e.g., "http://otel-collector:4318").
    pub endpoint: String,

    /// Protocol: only "http/protobuf" supported in v0.2.
    #[serde(default = "default_otlp_protocol")]
    pub protocol: String,

    /// Custom HTTP headers for OTLP export (e.g., Authorization tokens).
    ///
    /// Values may contain `${ENV_VAR:-default}` references resolved at config
    /// load time.
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
}

fn default_otlp_protocol() -> String {
    "http/protobuf".to_string()
}

/// Sampling configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SamplingConfig {
    /// Sampling strategy: "head" only in v0.2.
    #[serde(default = "default_sampling_strategy")]
    pub strategy: String,

    /// Sample rate for successful requests (0.0 to 1.0).
    #[serde(default = "default_sample_rate")]
    pub success_sample_rate: f64,
}

fn default_sampling_strategy() -> String {
    "head".to_string()
}

fn default_sample_rate() -> f64 {
    1.0
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            strategy: "head".to_string(),
            success_sample_rate: 1.0,
        }
    }
}

/// Batch span processor configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchConfig {
    /// Maximum spans in the export queue.
    #[serde(default = "default_max_queue_size")]
    pub max_queue_size: usize,

    /// Maximum spans per export batch.
    #[serde(default = "default_max_export_batch_size")]
    pub max_export_batch_size: usize,

    /// Delay between scheduled exports in milliseconds.
    #[serde(default = "default_scheduled_delay_ms")]
    pub scheduled_delay_ms: u64,

    /// Export timeout in milliseconds.
    #[serde(default = "default_export_timeout_ms")]
    pub export_timeout_ms: u64,
}

fn default_max_queue_size() -> usize {
    2048
}
fn default_max_export_batch_size() -> usize {
    512
}
fn default_scheduled_delay_ms() -> u64 {
    5000
}
fn default_export_timeout_ms() -> u64 {
    30000
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_accessors() {
        let source = Source::Mcp {
            id: "test".to_string(),
            url: "http://localhost:8080".to_string(),
            prefix: Some("test_".to_string()),
            expose: None,
            enabled: true,
        };

        assert_eq!(source.id(), "test");
        assert_eq!(source.url(), "http://localhost:8080");
        assert_eq!(source.prefix(), Some("test_"));
        assert!(source.is_enabled());
        assert_eq!(source.kind(), "mcp");
    }

    #[test]
    fn test_expose_config_all() {
        let expose = ExposeConfig::All;
        assert!(expose.is_visible("any_tool"));
        assert!(expose.is_visible("delete_user"));
    }

    #[test]
    fn test_expose_config_allowlist() {
        let expose = ExposeConfig::Allowlist {
            tools: vec!["read_*".to_string(), "list_*".to_string()],
            compiled_tools: vec![],
        };
        assert!(expose.is_visible("read_file"));
        assert!(expose.is_visible("list_users"));
        assert!(!expose.is_visible("delete_user"));
    }

    #[test]
    fn test_expose_config_blocklist() {
        let expose = ExposeConfig::Blocklist {
            tools: vec!["delete_*".to_string(), "*_unsafe".to_string()],
            compiled_tools: vec![],
        };
        assert!(expose.is_visible("read_file"));
        assert!(!expose.is_visible("delete_user"));
        assert!(!expose.is_visible("operation_unsafe"));
    }

    #[test]
    fn test_source_filter_single() {
        let filter = SourceFilter::Single("upstream".to_string());
        assert!(filter.matches("upstream"));
        assert!(!filter.matches("other"));
    }

    #[test]
    fn test_source_filter_multiple() {
        let filter = SourceFilter::Multiple(vec!["source1".to_string(), "source2".to_string()]);
        assert!(filter.matches("source1"));
        assert!(filter.matches("source2"));
        assert!(!filter.matches("source3"));
    }

    #[test]
    fn test_governance_evaluate_first_match() {
        let governance = Governance {
            defaults: GovernanceDefaults {
                action: Action::Forward,
            },
            rules: vec![
                Rule {
                    pattern: "delete_*".to_string(),
                    action: Action::Approve,
                    source: None,
                    policy_id: None,
                    approval: Some("default".to_string()),
                    description: None,
                    compiled_pattern: None,
                },
                Rule {
                    pattern: "*".to_string(),
                    action: Action::Deny,
                    source: None,
                    policy_id: None,
                    approval: None,
                    description: None,
                    compiled_pattern: None,
                },
            ],
        };

        let result = governance.evaluate("delete_user", "upstream");
        assert_eq!(result.action, Action::Approve);
        assert_eq!(result.matched_rule, Some("delete_*".to_string()));
    }

    #[test]
    fn test_governance_evaluate_default() {
        let governance = Governance {
            defaults: GovernanceDefaults {
                action: Action::Forward,
            },
            rules: vec![Rule {
                pattern: "admin_*".to_string(),
                action: Action::Deny,
                source: None,
                policy_id: None,
                approval: None,
                description: None,
                compiled_pattern: None,
            }],
        };

        let result = governance.evaluate("read_file", "upstream");
        assert_eq!(result.action, Action::Forward);
        assert_eq!(result.matched_rule, None);
    }

    #[test]
    fn test_governance_evaluate_source_filter() {
        let governance = Governance {
            defaults: GovernanceDefaults {
                action: Action::Forward,
            },
            rules: vec![Rule {
                pattern: "*".to_string(),
                action: Action::Approve,
                source: Some(SourceFilter::Single("restricted".to_string())),
                policy_id: None,
                approval: None,
                description: None,
                compiled_pattern: None,
            }],
        };

        // Rule should match for "restricted" source
        let result = governance.evaluate("any_tool", "restricted");
        assert_eq!(result.action, Action::Approve);

        // Rule should NOT match for "other" source
        let result = governance.evaluate("any_tool", "other");
        assert_eq!(result.action, Action::Forward);
    }

    #[test]
    fn test_action_display() {
        assert_eq!(Action::Forward.to_string(), "forward");
        assert_eq!(Action::Approve.to_string(), "approve");
        assert_eq!(Action::Deny.to_string(), "deny");
        assert_eq!(Action::Policy.to_string(), "policy");
    }

    #[test]
    fn test_telemetry_config_parsing() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://localhost:8080
governance:
  defaults:
    action: forward
telemetry:
  enabled: true
  otlp:
    endpoint: "http://otel-collector:4318"
    protocol: http/protobuf
  sampling:
    strategy: head
    success_sample_rate: 0.10
  batch:
    max_queue_size: 4096
    scheduled_delay_ms: 10000
  resource:
    service.name: my-thoughtgate
    deployment.environment: staging
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();

        let telemetry = config.telemetry.unwrap();
        assert!(telemetry.enabled);
        assert_eq!(
            telemetry.otlp.as_ref().unwrap().endpoint,
            "http://otel-collector:4318"
        );
        assert_eq!(
            telemetry.sampling.as_ref().unwrap().success_sample_rate,
            0.10
        );
        assert_eq!(telemetry.batch.as_ref().unwrap().max_queue_size, 4096);
        assert_eq!(
            telemetry
                .resource
                .as_ref()
                .unwrap()
                .get("service.name")
                .unwrap(),
            "my-thoughtgate"
        );
    }

    #[test]
    fn test_telemetry_config_defaults() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://localhost:8080
governance:
  defaults:
    action: forward
telemetry:
  enabled: false
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();

        let telemetry = config.telemetry.unwrap();
        assert!(!telemetry.enabled);
        assert!(telemetry.otlp.is_none());
        assert!(telemetry.sampling.is_none());
        assert!(telemetry.batch.is_none());
    }

    #[test]
    fn test_telemetry_config_absent() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://localhost:8080
governance:
  defaults:
    action: forward
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        assert!(config.telemetry.is_none());
    }
}
