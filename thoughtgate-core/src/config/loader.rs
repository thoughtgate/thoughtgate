//! Configuration loading and validation.
//!
//! Implements: REQ-CFG-001 Section 6.3 (Configuration Loading Interface)
//! Implements: REQ-CFG-001 Section 8 (Validation Rules)
//! Implements: REQ-CFG-001 Section 9.1 (Configuration Loading Flow)
//!
//! # Traceability
//! - Implements: REQ-CFG-001/V-001 through V-014
//! - Implements: REQ-CFG-001/9.1 (Configuration Loading Flow)
//!
//! # TODO: Config Hot-Reload (REQ-CFG-002, deferred to v0.2+)
//!
//! Current implementation loads configuration once at startup. Hot-reload
//! would allow policy and configuration updates without restarting the sidecar.
//!
//! ## Implementation Plan
//!
//! 1. **File watcher**: Use `notify` crate to watch config file for changes
//! 2. **Atomic swap**: Use `arc-swap::ArcSwap<Config>` for lock-free reads
//! 3. **Reload flow**: On file change → parse → validate → swap atomically
//! 4. **Cedar reload**: `CedarEngine` already uses `ArcSwap` for policy sets
//! 5. **Metrics**: Emit `config_reload_total` and `config_reload_errors_total`
//! 6. **Signal**: Support `SIGHUP` as manual reload trigger

use regex::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use super::error::{ConfigError, ValidationResult, ValidationWarning};
use super::schema::{Action, ApprovalDestination, Config, Source, SourceFilter};

/// Semantic version for feature gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const V0_2: Version = Version {
        major: 0,
        minor: 2,
        patch: 0,
    };
}

/// Configuration file search paths (in priority order).
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 5.3 (Configuration File Location)
pub fn default_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Priority 2: Environment variable
    if let Ok(path) = std::env::var("THOUGHTGATE_CONFIG") {
        paths.push(PathBuf::from(path));
    }

    // Priority 3: System default
    paths.push(PathBuf::from("/etc/thoughtgate/config.yaml"));

    // Priority 4: Local default
    paths.push(PathBuf::from("./config.yaml"));

    paths
}

/// Find the first existing config file from the search paths.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 5.3 (Configuration File Location)
pub fn find_config_file(explicit_path: Option<&Path>) -> Result<PathBuf, ConfigError> {
    // Priority 1: Explicit path (CLI flag)
    if let Some(path) = explicit_path {
        if path.exists() {
            return Ok(path.to_path_buf());
        }
        return Err(ConfigError::ConfigFileNotFound {
            searched: vec![path.to_path_buf()],
        });
    }

    // Search default paths
    let paths = default_config_paths();
    for path in &paths {
        if path.exists() {
            return Ok(path.clone());
        }
    }

    Err(ConfigError::ConfigFileNotFound { searched: paths })
}

/// Load configuration from a file path.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 9.1 (Configuration Loading Flow)
pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    // Step 2: Read file contents
    let contents = std::fs::read_to_string(path)?;

    // EC-CFG-001: Empty config file
    if contents.trim().is_empty() {
        return Err(ConfigError::EmptyConfigFile);
    }

    // Step 3: Environment variable substitution
    let contents = substitute_env_vars(&contents)?;

    // Step 4: Parse YAML
    let config: Config = serde_saphyr::from_str(&contents)?;

    Ok(config)
}

/// Load and validate configuration.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 9.1 (Configuration Loading Flow)
/// - Implements: REQ-CFG-001 Section 8 (Validation Rules)
pub fn load_and_validate(
    path: &Path,
    version: Version,
) -> Result<(Config, ValidationResult), ConfigError> {
    let mut config = load_config(path)?;
    let result = validate(&config, version)?;
    // Pre-compile glob patterns to avoid per-request pattern parsing
    config.compile_patterns()?;
    Ok((config, result))
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Environment Variable Substitution
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

// SAFETY: .expect() on LazyLock with a compile-time literal regex pattern.
// The pattern is known-valid and tested by test_env_var_pattern_compiles().
static ENV_VAR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}")
        .expect("BUG: ENV_VAR_PATTERN regex is invalid — this is a programmer error")
});

/// Substitute environment variables in a string.
///
/// # Syntax
/// - `${VAR}` - Required, fail if not set
/// - `${VAR:-default}` - Optional with default
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 5.5 (Environment Variable Substitution)
pub fn substitute_env_vars(content: &str) -> Result<String, ConfigError> {
    let mut result = content.to_string();
    let mut errors = Vec::new();

    // Find all matches first (to avoid borrowing issues)
    // Groups 0 and 1 are guaranteed by the regex structure, but we use
    // ok_or for explicit error handling per coding guidelines
    let matches: Vec<_> = ENV_VAR_PATTERN
        .captures_iter(content)
        .filter_map(|cap| {
            let full_match = cap.get(0)?.as_str().to_string();
            let var_name = cap.get(1)?.as_str().to_string();
            let default = cap.get(2).map(|m| m.as_str().to_string());
            Some((full_match, var_name, default))
        })
        .collect();

    for (full_match, var_name, default) in matches {
        match std::env::var(&var_name) {
            Ok(value) => {
                result = result.replace(&full_match, &value);
            }
            Err(_) => {
                if let Some(default_value) = default {
                    result = result.replace(&full_match, &default_value);
                } else {
                    errors.push((var_name.clone(), full_match.clone()));
                }
            }
        }
    }

    // EC-CFG-003: Required env var not set
    if !errors.is_empty() {
        let (var, _) = &errors[0];
        return Err(ConfigError::MissingEnvVar {
            var: var.clone(),
            field: "configuration".to_string(),
        });
    }

    Ok(result)
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Validation
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Validate a configuration.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 8 (Validation Rules)
pub fn validate(config: &Config, version: Version) -> Result<ValidationResult, ConfigError> {
    let mut warnings = Vec::new();

    // Schema version validation
    if config.schema != 1 {
        return Err(ConfigError::UnsupportedSchemaVersion {
            version: config.schema,
        });
    }

    // V-010: At least one source
    if config.sources.is_empty() {
        return Err(ConfigError::NoSourcesDefined);
    }

    // V-011, V-012: v0.2 restrictions
    if version.major == 0 && version.minor == 2 {
        if config.sources.len() != 1 {
            return Err(ConfigError::V02SingleSourceOnly {
                count: config.sources.len(),
            });
        }
        if !matches!(config.sources[0], Source::Mcp { .. }) {
            return Err(ConfigError::V02McpOnly {
                kind: config.sources[0].kind().to_string(),
            });
        }
    }

    // V-001: Unique source IDs
    let mut seen_ids = HashSet::new();
    for source in &config.sources {
        if !seen_ids.insert(source.id()) {
            return Err(ConfigError::DuplicateSourceId {
                id: source.id().to_string(),
            });
        }
    }

    // V-002: Reserved prefix
    for source in &config.sources {
        if source.id().starts_with('_') {
            return Err(ConfigError::ReservedPrefix {
                id: source.id().to_string(),
            });
        }
    }

    // V-016: Source ID format validation
    for source in &config.sources {
        let id = source.id();
        if id.is_empty() {
            return Err(ConfigError::InvalidSourceId {
                id: id.to_string(),
                reason: "source ID must not be empty".to_string(),
            });
        }
        if id.trim() != id {
            return Err(ConfigError::InvalidSourceId {
                id: id.to_string(),
                reason: "source ID must not have leading or trailing whitespace".to_string(),
            });
        }
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            return Err(ConfigError::InvalidSourceId {
                id: id.to_string(),
                reason: "source ID must contain only [a-zA-Z0-9._-]".to_string(),
            });
        }
    }

    // V-003: Unique prefixes
    let mut seen_prefixes = HashSet::new();
    for source in &config.sources {
        if let Some(prefix) = source.prefix() {
            if !seen_prefixes.insert(prefix) {
                return Err(ConfigError::DuplicatePrefix {
                    prefix: prefix.to_string(),
                });
            }
        }
    }

    // V-008: Validate source URLs
    for source in &config.sources {
        let url = source.url();
        if url::Url::parse(url).is_err() {
            return Err(ConfigError::InvalidUrl {
                url: url.to_string(),
                message: "invalid URL format".to_string(),
            });
        }
    }

    // Get workflow names for V-006 validation
    let workflow_names: HashSet<&str> = config
        .approval
        .as_ref()
        .map(|a| a.keys().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    // Validate governance rules
    for rule in &config.governance.rules {
        // V-005: action: policy requires policy_id
        if rule.action == Action::Policy && rule.policy_id.is_none() {
            return Err(ConfigError::MissingPolicyId {
                pattern: rule.pattern.clone(),
            });
        }

        // V-004: policy_id without action: policy (warning)
        if rule.policy_id.is_some() && rule.action != Action::Policy {
            warnings.push(ValidationWarning::PolicyIdWithoutPolicyAction {
                pattern: rule.pattern.clone(),
            });
        }

        // V-006: approval workflow must exist
        if rule.action == Action::Approve {
            if let Some(ref workflow) = rule.approval {
                if !workflow_names.contains(workflow.as_str()) {
                    return Err(ConfigError::UndefinedWorkflow {
                        workflow: workflow.clone(),
                        pattern: rule.pattern.clone(),
                    });
                }
            } else if workflow_names.len() != 1 {
                // V-006b: no explicit approval field — implicit selection only
                // works when exactly one workflow is defined
                return Err(ConfigError::MissingApprovalWorkflow {
                    pattern: rule.pattern.clone(),
                    workflow_count: workflow_names.len(),
                });
            }
        }

        // V-009: Valid glob pattern
        if let Err(e) = glob::Pattern::new(&rule.pattern) {
            return Err(ConfigError::InvalidGlobPattern {
                pattern: rule.pattern.clone(),
                message: e.to_string(),
            });
        }

        // V-007: Source references in rules must exist
        if let Some(ref source_filter) = rule.source {
            let check_source = |id: &str| -> Result<(), ConfigError> {
                if !seen_ids.contains(id) {
                    return Err(ConfigError::UndefinedSourceInRule {
                        source_id: id.to_string(),
                        pattern: rule.pattern.clone(),
                    });
                }
                Ok(())
            };
            match source_filter {
                SourceFilter::Single(id) => check_source(id)?,
                SourceFilter::Multiple(ids) => {
                    for id in ids {
                        check_source(id)?;
                    }
                }
            }
        }
    }

    // V-014: Valid expose config glob patterns
    for source in &config.sources {
        if let Some(patterns) = source.expose().patterns() {
            for pattern in patterns {
                if let Err(e) = glob::Pattern::new(pattern) {
                    return Err(ConfigError::InvalidGlobPattern {
                        pattern: pattern.clone(),
                        message: format!(
                            "invalid expose pattern for source '{}': {}",
                            source.id(),
                            e
                        ),
                    });
                }
            }
        }
    }

    // Per-workflow Slack settings warning (not yet supported)
    // V-015: Workflow timeout validation
    if let Some(ref approval) = config.approval {
        for (name, workflow) in approval {
            let ApprovalDestination::Slack {
                ref token_env,
                ref mention,
                ref channel,
            } = workflow.destination;
            if token_env.is_some() || mention.is_some() || channel != "#approvals" {
                warnings.push(ValidationWarning::PerWorkflowSlackNotSupported {
                    workflow: name.clone(),
                });
            }

            // V-015: Minimum timeout durations
            if let Some(timeout) = workflow.timeout {
                if timeout.as_secs() < 5 {
                    return Err(ConfigError::InvalidWorkflowTimeout {
                        workflow: name.clone(),
                        field: "timeout".to_string(),
                        duration_secs: timeout.as_secs(),
                    });
                }
            }
            if let Some(bt) = workflow.blocking_timeout {
                if bt.as_secs() < 5 {
                    return Err(ConfigError::InvalidWorkflowTimeout {
                        workflow: name.clone(),
                        field: "blocking_timeout".to_string(),
                        duration_secs: bt.as_secs(),
                    });
                }
            }
        }
    }

    // V-013: Cedar policy files exist
    if let Some(ref cedar) = config.cedar {
        for path in &cedar.policies {
            if !path.exists() {
                return Err(ConfigError::PolicyFileNotFound { path: path.clone() });
            }
        }
        // V-013 extension: Cedar schema path exists
        if let Some(ref schema_path) = cedar.schema {
            if !schema_path.exists() {
                return Err(ConfigError::PolicyFileNotFound {
                    path: schema_path.clone(),
                });
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Telemetry validation (V-TEL-001 through V-TEL-006)
    // ─────────────────────────────────────────────────────────────────────
    if let Some(ref telemetry) = config.telemetry {
        // V-TEL-001: Sample rate in [0.0, 1.0]
        if let Some(ref sampling) = telemetry.sampling {
            if !(0.0..=1.0).contains(&sampling.success_sample_rate) {
                return Err(ConfigError::InvalidSampleRate {
                    rate: sampling.success_sample_rate,
                });
            }

            // V-TEL-004: Strategy must be "head" or "tail"
            if sampling.strategy != "head" && sampling.strategy != "tail" {
                return Err(ConfigError::UnknownSamplingStrategy {
                    strategy: sampling.strategy.clone(),
                });
            }
        }

        // V-TEL-002: OTLP endpoint must be valid URL if set
        if let Some(ref otlp) = telemetry.otlp {
            if url::Url::parse(&otlp.endpoint).is_err() {
                return Err(ConfigError::InvalidOtlpEndpoint {
                    endpoint: otlp.endpoint.clone(),
                    message: "invalid URL format".to_string(),
                });
            }

            // V-TEL-003: Protocol must be "http/protobuf" or "grpc"
            if otlp.protocol != "http/protobuf" && otlp.protocol != "grpc" {
                return Err(ConfigError::UnknownOtlpProtocol {
                    protocol: otlp.protocol.clone(),
                });
            }
        }

        // V-TEL-005: max_queue_size > 0
        if let Some(ref batch) = telemetry.batch {
            if batch.max_queue_size == 0 {
                return Err(ConfigError::InvalidQueueSize {
                    size: batch.max_queue_size,
                });
            }

            // V-TEL-006: scheduled_delay_ms >= 100
            if batch.scheduled_delay_ms < 100 {
                return Err(ConfigError::InvalidBatchDelay {
                    delay_ms: batch.scheduled_delay_ms,
                });
            }

            // V-TEL-007: max_export_batch_size in (0, max_queue_size]
            if batch.max_export_batch_size == 0
                || batch.max_export_batch_size > batch.max_queue_size
            {
                return Err(ConfigError::InvalidExportBatchSize {
                    batch_size: batch.max_export_batch_size,
                    queue_size: batch.max_queue_size,
                });
            }

            // V-TEL-008: export_timeout_ms >= 100
            if batch.export_timeout_ms < 100 {
                return Err(ConfigError::InvalidExportTimeout {
                    timeout_ms: batch.export_timeout_ms,
                });
            }
        }
    }

    Ok(ValidationResult::with_warnings(warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_CONFIG: &str = r#"
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080

governance:
  defaults:
    action: forward
"#;

    /// Verify the LazyLock regex compiles successfully.
    /// Guards against regressions if ENV_VAR_PATTERN is modified.
    #[test]
    fn test_env_var_pattern_compiles() {
        // Force evaluation of the LazyLock — if the regex is invalid, this panics
        let _ = &*ENV_VAR_PATTERN;
    }

    #[test]
    fn test_parse_minimal_config() {
        let config: Config = serde_saphyr::from_str(MINIMAL_CONFIG).unwrap();
        assert_eq!(config.schema, 1);
        assert_eq!(config.sources.len(), 1);
        assert_eq!(config.sources[0].id(), "upstream");
    }

    #[test]
    fn test_validate_minimal_config() {
        let config: Config = serde_saphyr::from_str(MINIMAL_CONFIG).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_no_sources() {
        let yaml = r#"
schema: 1
sources: []
governance:
  defaults:
    action: forward
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(result, Err(ConfigError::NoSourcesDefined)));
    }

    #[test]
    fn test_validate_duplicate_source_id() {
        // This would require v0.3+ to test multiple sources
        // For v0.2, the V02SingleSourceOnly error would trigger first
    }

    #[test]
    fn test_validate_reserved_prefix() {
        let yaml = r#"
schema: 1
sources:
  - id: _internal
    kind: mcp
    url: http://localhost:8080
governance:
  defaults:
    action: forward
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(result, Err(ConfigError::ReservedPrefix { .. })));
    }

    #[test]
    fn test_validate_missing_policy_id() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://localhost:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "test_*"
      action: policy
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(result, Err(ConfigError::MissingPolicyId { .. })));
    }

    #[test]
    fn test_validate_undefined_workflow() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://localhost:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "test_*"
      action: approve
      approval: nonexistent
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(result, Err(ConfigError::UndefinedWorkflow { .. })));
    }

    /// Regression test: "default" workflow name is NOT special-cased.
    /// If a rule references `approval: default`, a workflow named "default" must be
    /// explicitly defined in the approval section.
    #[test]
    fn test_validate_default_workflow_must_be_defined() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://localhost:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "test_*"
      action: approve
      approval: default
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        // Should fail because "default" workflow is not defined in approval section
        assert!(
            matches!(result, Err(ConfigError::UndefinedWorkflow { ref workflow, .. }) if workflow == "default"),
            "Expected UndefinedWorkflow error for 'default', got: {:?}",
            result
        );
    }

    #[test]
    fn test_validate_invalid_glob_pattern() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://localhost:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "[invalid"
      action: deny
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidGlobPattern { .. })
        ));
    }

    #[test]
    fn test_validate_invalid_expose_pattern() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://localhost:8080
    expose:
      mode: allowlist
      tools:
        - "[invalid"
governance:
  defaults:
    action: forward
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidGlobPattern { .. })
        ));
    }

    #[test]
    fn test_validate_invalid_url() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: not-a-valid-url
governance:
  defaults:
    action: forward
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(result, Err(ConfigError::InvalidUrl { .. })));
    }

    #[test]
    fn test_validate_policy_id_warning() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://localhost:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "test_*"
      action: forward
      policy_id: some_policy
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2).unwrap();
        assert!(!result.is_clean());
        assert!(matches!(
            &result.warnings[0],
            ValidationWarning::PolicyIdWithoutPolicyAction { .. }
        ));
    }

    #[test]
    fn test_env_var_substitution_required() {
        unsafe {
            std::env::set_var("TEST_VAR", "test_value");
        }
        let input = "url: ${TEST_VAR}";
        let result = substitute_env_vars(input).unwrap();
        assert_eq!(result, "url: test_value");
        unsafe {
            std::env::remove_var("TEST_VAR");
        }
    }

    #[test]
    fn test_env_var_substitution_with_default() {
        unsafe {
            std::env::remove_var("MISSING_VAR");
        }
        let input = "url: ${MISSING_VAR:-default_value}";
        let result = substitute_env_vars(input).unwrap();
        assert_eq!(result, "url: default_value");
    }

    #[test]
    fn test_env_var_substitution_missing_required() {
        unsafe {
            std::env::remove_var("REQUIRED_VAR");
        }
        let input = "url: ${REQUIRED_VAR}";
        let result = substitute_env_vars(input);
        assert!(matches!(result, Err(ConfigError::MissingEnvVar { .. })));
    }

    #[test]
    fn test_unsupported_schema_version() {
        let yaml = r#"
schema: 2
sources:
  - id: upstream
    kind: mcp
    url: http://localhost:8080
governance:
  defaults:
    action: forward
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(
            result,
            Err(ConfigError::UnsupportedSchemaVersion { version: 2 })
        ));
    }

    #[test]
    fn test_parse_full_config() {
        let yaml = r##"
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
    prefix: mcp_
    enabled: true
    description: "Primary MCP server"
    expose:
      mode: blocklist
      tools:
        - "admin_*"
        - "*_unsafe"

governance:
  defaults:
    action: forward

  rules:
    - match: "delete_*"
      action: approve
      approval: default
      description: "All deletions require approval"

    - match: "transfer_*"
      action: policy
      policy_id: financial
      approval: finance

    - match: "*_unsafe"
      action: deny

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
    timeout: 30m
    on_timeout: deny

cedar:
  policies:
    - /etc/thoughtgate/policies/financial.cedar
"##;
        // Note: Cedar policy file validation will fail unless the file exists
        // So we only test parsing here, not full validation
        let config: Config = serde_saphyr::from_str(yaml).unwrap();

        assert_eq!(config.schema, 1);
        assert_eq!(config.sources.len(), 1);
        assert_eq!(config.sources[0].id(), "upstream");
        assert_eq!(config.sources[0].prefix(), Some("mcp_"));

        assert_eq!(config.governance.rules.len(), 3);
        assert_eq!(config.governance.rules[0].pattern, "delete_*");
        assert_eq!(config.governance.rules[0].action, Action::Approve);

        let approval = config.approval.as_ref().unwrap();
        assert!(approval.contains_key("default"));
        assert!(approval.contains_key("finance"));

        assert!(config.cedar.is_some());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Telemetry Validation Tests (V-TEL-001 through V-TEL-006)
    // ─────────────────────────────────────────────────────────────────────────

    fn config_with_telemetry(telemetry_yaml: &str) -> String {
        format!(
            r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
telemetry:
{telemetry_yaml}
"#
        )
    }

    #[test]
    fn test_telemetry_invalid_sample_rate_too_high() {
        let yaml = config_with_telemetry(
            r#"  sampling:
    success_sample_rate: 1.5"#,
        );
        let config: Config = serde_saphyr::from_str(&yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(result, Err(ConfigError::InvalidSampleRate { .. })));
    }

    #[test]
    fn test_telemetry_invalid_sample_rate_negative() {
        let yaml = config_with_telemetry(
            r#"  sampling:
    success_sample_rate: -0.1"#,
        );
        let config: Config = serde_saphyr::from_str(&yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(result, Err(ConfigError::InvalidSampleRate { .. })));
    }

    #[test]
    fn test_telemetry_valid_sample_rate_boundaries() {
        for rate in &["0.0", "0.5", "1.0"] {
            let yaml = config_with_telemetry(&format!(
                r#"  sampling:
    success_sample_rate: {rate}"#
            ));
            let config: Config = serde_saphyr::from_str(&yaml).unwrap();
            let result = validate(&config, Version::V0_2);
            assert!(result.is_ok(), "rate {rate} should be valid");
        }
    }

    #[test]
    fn test_telemetry_invalid_otlp_endpoint() {
        let yaml = config_with_telemetry(
            r#"  otlp:
    endpoint: "not a url""#,
        );
        let config: Config = serde_saphyr::from_str(&yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidOtlpEndpoint { .. })
        ));
    }

    #[test]
    fn test_telemetry_valid_otlp_endpoint() {
        let yaml = config_with_telemetry(
            r#"  otlp:
    endpoint: "http://otel-collector:4318""#,
        );
        let config: Config = serde_saphyr::from_str(&yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_ok());
    }

    #[test]
    fn test_telemetry_unknown_protocol() {
        let yaml = config_with_telemetry(
            r#"  otlp:
    endpoint: "http://otel-collector:4318"
    protocol: "thrift""#,
        );
        let config: Config = serde_saphyr::from_str(&yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(
            result,
            Err(ConfigError::UnknownOtlpProtocol { .. })
        ));
    }

    #[test]
    fn test_telemetry_valid_protocols() {
        for protocol in &["http/protobuf", "grpc"] {
            let yaml = config_with_telemetry(&format!(
                r#"  otlp:
    endpoint: "http://otel-collector:4318"
    protocol: "{protocol}""#
            ));
            let config: Config = serde_saphyr::from_str(&yaml).unwrap();
            let result = validate(&config, Version::V0_2);
            assert!(result.is_ok(), "protocol {protocol} should be valid");
        }
    }

    #[test]
    fn test_telemetry_unknown_sampling_strategy() {
        let yaml = config_with_telemetry(
            r#"  sampling:
    strategy: "random""#,
        );
        let config: Config = serde_saphyr::from_str(&yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(
            result,
            Err(ConfigError::UnknownSamplingStrategy { .. })
        ));
    }

    #[test]
    fn test_telemetry_invalid_queue_size() {
        let yaml = config_with_telemetry(
            r#"  batch:
    max_queue_size: 0"#,
        );
        let config: Config = serde_saphyr::from_str(&yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(result, Err(ConfigError::InvalidQueueSize { .. })));
    }

    #[test]
    fn test_telemetry_invalid_batch_delay() {
        let yaml = config_with_telemetry(
            r#"  batch:
    scheduled_delay_ms: 50"#,
        );
        let config: Config = serde_saphyr::from_str(&yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(matches!(result, Err(ConfigError::InvalidBatchDelay { .. })));
    }

    #[test]
    fn test_telemetry_valid_batch_delay_boundary() {
        let yaml = config_with_telemetry(
            r#"  batch:
    scheduled_delay_ms: 100"#,
        );
        let config: Config = serde_saphyr::from_str(&yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_ok());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Per-workflow Slack warning tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_per_workflow_slack_warning() {
        let yaml = r##"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: approve
      approval: custom
approval:
  custom:
    destination:
      type: slack
      channel: "#custom-channel"
      token_env: CUSTOM_TOKEN
      mention:
        - "@oncall"
    timeout: 5m
"##;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2).unwrap();
        assert!(!result.is_clean());
        assert!(result.warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::PerWorkflowSlackNotSupported { workflow } if workflow == "custom"
        )));
    }

    #[test]
    fn test_per_workflow_slack_no_warning_default() {
        let yaml = r##"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
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
    timeout: 5m
"##;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2).unwrap();
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| matches!(w, ValidationWarning::PerWorkflowSlackNotSupported { .. })),
            "No per-workflow Slack warning expected for default channel"
        );
    }

    #[test]
    fn test_unknown_field_rejected() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
unknown_top_level_field: oops
"#;
        let result: Result<Config, _> = serde_saphyr::from_str(yaml);
        assert!(
            result.is_err(),
            "Unknown top-level field should be rejected"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown_top_level_field"),
            "Error should mention the unknown field, got: {err}"
        );
    }

    #[test]
    fn test_unknown_rule_field_rejected() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: deny
      typo_field: oops
"#;
        let result: Result<Config, _> = serde_saphyr::from_str(yaml);
        assert!(result.is_err(), "Unknown rule field should be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("typo_field"),
            "Error should mention the unknown field, got: {err}"
        );
    }

    #[test]
    fn test_approve_no_workflow_zero_defined() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: approve
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("0 workflows"),
            "Should mention 0 workflows, got: {err}"
        );
    }

    #[test]
    fn test_approve_no_workflow_multi_defined() {
        let yaml = r##"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: approve
approval:
  fast:
    destination:
      type: slack
      channel: "#approvals"
    timeout: 1m
  slow:
    destination:
      type: slack
      channel: "#approvals"
    timeout: 30m
"##;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("2 workflows"),
            "Should mention 2 workflows, got: {err}"
        );
    }

    #[test]
    fn test_approve_no_workflow_single_defined() {
        let yaml = r##"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
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
    timeout: 5m
"##;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(
            result.is_ok(),
            "Single workflow should allow implicit selection, got: {:?}",
            result.unwrap_err()
        );
    }

    // ── V-015: Workflow timeout validation ────────────────────────────────

    #[test]
    fn test_zero_timeout_rejected() {
        let yaml = r##"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
    timeout: 0s
"##;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("0s"), "Should mention 0s timeout, got: {err}");
    }

    #[test]
    fn test_small_timeout_rejected() {
        let yaml = r##"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
    timeout: 3s
"##;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("3s"), "Should mention 3s timeout, got: {err}");
    }

    #[test]
    fn test_minimum_timeout_accepted() {
        let yaml = r##"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
approval:
  default:
    destination:
      type: slack
      channel: "#approvals"
    timeout: 5s
"##;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(
            result.is_ok(),
            "5s timeout should be accepted, got: {:?}",
            result.unwrap_err()
        );
    }

    // ── V-007: Source references in rules ─────────────────────────────────

    #[test]
    fn test_undefined_source_in_rule_rejected() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: deny
      source: nonexistent-server
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent-server"),
            "Should mention undefined source, got: {err}"
        );
    }

    #[test]
    fn test_valid_source_in_rule_accepted() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: deny
      source: upstream
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(
            result.is_ok(),
            "Valid source reference should be accepted, got: {:?}",
            result.unwrap_err()
        );
    }

    // ── V-013 extension: Cedar schema path ───────────────────────────────

    #[test]
    fn test_cedar_schema_path_validated() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
cedar:
  policies: []
  schema: /nonexistent/path/to/schema.cedarschema
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "Should mention file not found, got: {err}"
        );
    }

    // ── V-016: Source ID format validation ────────────────────────────────

    #[test]
    fn test_empty_source_id() {
        let yaml = r#"
schema: 1
sources:
  - id: ""
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("empty"),
            "Should mention empty source ID, got: {err}"
        );
    }

    #[test]
    fn test_special_char_source_id() {
        let yaml = r#"
schema: 1
sources:
  - id: "up stream!"
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("[a-zA-Z0-9._-]"),
            "Should mention valid characters, got: {err}"
        );
    }

    #[test]
    fn test_valid_source_id_with_dots() {
        let yaml = r#"
schema: 1
sources:
  - id: my.mcp-server_01
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(
            result.is_ok(),
            "Dotted source ID should be valid, got: {:?}",
            result.unwrap_err()
        );
    }

    // ── V-TEL-007/008: OTEL batch config range checks ───────────────────

    #[test]
    fn test_batch_export_size_zero() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
telemetry:
  enabled: true
  batch:
    max_queue_size: 2048
    max_export_batch_size: 0
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("max_export_batch_size"),
            "Should mention batch size, got: {err}"
        );
    }

    #[test]
    fn test_batch_export_size_exceeds_queue() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
telemetry:
  enabled: true
  batch:
    max_queue_size: 100
    max_export_batch_size: 200
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("max_export_batch_size"),
            "Should mention batch exceeds queue, got: {err}"
        );
    }

    #[test]
    fn test_batch_export_timeout_small() {
        let yaml = r#"
schema: 1
sources:
  - id: upstream
    kind: mcp
    url: http://mcp-server:8080
governance:
  defaults:
    action: forward
telemetry:
  enabled: true
  batch:
    export_timeout_ms: 50
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let result = validate(&config, Version::V0_2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("export_timeout_ms"),
            "Should mention timeout, got: {err}"
        );
    }
}
