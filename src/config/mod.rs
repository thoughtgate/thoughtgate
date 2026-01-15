//! Configuration module for ThoughtGate.
//!
//! Implements: REQ-CFG-001 (Configuration Schema)
//!
//! This module provides:
//! - YAML configuration parsing
//! - Environment variable substitution
//! - Configuration validation (V-001 through V-014)
//! - Governance rule matching
//! - Tool exposure filtering
//! - Centralized default values
//!
//! # Example
//!
//! ```ignore
//! use thoughtgate::config::{load_and_validate, Version};
//!
//! let (config, warnings) = load_and_validate(
//!     Path::new("config.yaml"),
//!     Version::V0_2,
//! )?;
//!
//! // Evaluate governance rules
//! let result = config.governance.evaluate("delete_user", "upstream");
//! match result.action {
//!     Action::Approve => { /* request human approval */ }
//!     Action::Forward => { /* forward to upstream */ }
//!     Action::Deny => { /* reject immediately */ }
//!     Action::Policy => { /* evaluate Cedar policy */ }
//! }
//! ```
//!
//! # Traceability
//! - Implements: REQ-CFG-001 (Configuration Schema)

mod defaults;
mod duration_format;
mod error;
mod loader;
mod schema;

// Re-export public API
pub use defaults::ThoughtGateDefaults;
pub use error::{ConfigError, ValidationResult, ValidationWarning};
pub use loader::{
    Version, default_config_paths, find_config_file, load_and_validate, load_config,
    substitute_env_vars, validate,
};
pub use schema::{
    Action, ApprovalDestination, CedarConfig, Config, ExposeConfig, Governance, GovernanceDefaults,
    HumanWorkflow, MatchResult, Rule, Source, SourceFilter, TimeoutAction, WebhookAuth,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Integration test: Full config loading flow
    #[test]
    fn test_full_loading_flow() {
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
    timeout: 10m
    on_timeout: deny
"##;

        // Parse
        let config: Config = serde_saphyr::from_str(yaml).unwrap();

        // Validate
        let result = validate(&config, Version::V0_2).unwrap();
        assert!(result.is_clean());

        // Test governance evaluation
        let match_result = config.governance.evaluate("delete_user", "upstream");
        assert_eq!(match_result.action, Action::Approve);
        assert_eq!(match_result.matched_rule, Some("delete_*".to_string()));
        assert_eq!(match_result.approval_workflow, Some("default".to_string()));

        // Test default action
        let match_result = config.governance.evaluate("read_file", "upstream");
        assert_eq!(match_result.action, Action::Forward);
        assert_eq!(match_result.matched_rule, None);
    }

    /// Test expose config filtering
    #[test]
    fn test_expose_filtering() {
        let allowlist = ExposeConfig::Allowlist {
            tools: vec!["read_*".to_string(), "list_*".to_string()],
        };
        assert!(allowlist.is_visible("read_file"));
        assert!(allowlist.is_visible("list_users"));
        assert!(!allowlist.is_visible("delete_user"));

        let blocklist = ExposeConfig::Blocklist {
            tools: vec!["admin_*".to_string()],
        };
        assert!(blocklist.is_visible("read_file"));
        assert!(!blocklist.is_visible("admin_config"));
    }

    /// Test defaults
    #[test]
    fn test_defaults_invariants() {
        let defaults = ThoughtGateDefaults::default();
        assert!(defaults.validate().is_ok());
    }
}
