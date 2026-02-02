//! Configuration error types.
//!
//! Implements: REQ-CFG-001 Section 8.3 (Error Types)
//!
//! # Traceability
//! - Implements: REQ-CFG-001/V-001 through V-014

use std::path::PathBuf;
use thiserror::Error;

/// Configuration loading and validation errors.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 8.3 (Error Types)
#[derive(Debug, Error)]
pub enum ConfigError {
    // ─────────────────────────────────────────────────────────────────────────
    // Source validation errors (V-001, V-002, V-003, V-010, V-011, V-012)
    // ─────────────────────────────────────────────────────────────────────────
    /// V-010: No sources defined in configuration.
    #[error("no sources defined in configuration")]
    NoSourcesDefined,

    /// V-011: v0.2 only supports a single source.
    #[error("v0.2 supports only a single source, found {count}")]
    V02SingleSourceOnly { count: usize },

    /// V-012: v0.2 only supports kind: mcp.
    #[error("v0.2 supports only kind: mcp, found '{kind}'")]
    V02McpOnly { kind: String },

    /// V-001: Duplicate source ID found.
    #[error("duplicate source ID: '{id}'")]
    DuplicateSourceId { id: String },

    /// V-002: Source ID uses reserved prefix.
    #[error("source ID '{id}' uses reserved prefix '_'")]
    ReservedPrefix { id: String },

    /// V-003: Duplicate prefix across sources.
    #[error("duplicate prefix: '{prefix}'")]
    DuplicatePrefix { prefix: String },

    // ─────────────────────────────────────────────────────────────────────────
    // Rule validation errors (V-004, V-005, V-006, V-009)
    // ─────────────────────────────────────────────────────────────────────────
    /// V-005: action: policy requires policy_id.
    #[error("missing policy_id for action: policy in rule '{pattern}'")]
    MissingPolicyId { pattern: String },

    /// V-006: Referenced approval workflow does not exist.
    #[error("undefined workflow '{workflow}' in rule '{pattern}'")]
    UndefinedWorkflow { workflow: String, pattern: String },

    /// V-009: Invalid glob pattern.
    #[error("invalid glob pattern '{pattern}': {message}")]
    InvalidGlobPattern { pattern: String, message: String },

    // ─────────────────────────────────────────────────────────────────────────
    // Value validation errors (V-007, V-008, V-013, V-014)
    // ─────────────────────────────────────────────────────────────────────────
    /// V-007: Invalid duration format.
    #[error("invalid duration '{value}': {message}")]
    InvalidDuration { value: String, message: String },

    /// V-008: Invalid URL format.
    #[error("invalid URL '{url}': {message}")]
    InvalidUrl { url: String, message: String },

    /// V-013: Cedar policy file not found.
    #[error("policy file not found: {path}")]
    PolicyFileNotFound { path: PathBuf },

    /// V-014: Required environment variable not set.
    #[error("environment variable '{var}' not set (required for field '{field}')")]
    MissingEnvVar { var: String, field: String },

    // ─────────────────────────────────────────────────────────────────────────
    // Schema validation errors
    // ─────────────────────────────────────────────────────────────────────────
    /// Schema version not supported.
    #[error("unsupported schema version {version}, expected 1")]
    UnsupportedSchemaVersion { version: u32 },

    /// Schema field missing.
    #[error("missing required field 'schema'")]
    MissingSchemaVersion,

    // ─────────────────────────────────────────────────────────────────────────
    // I/O and parsing errors
    // ─────────────────────────────────────────────────────────────────────────
    /// YAML parsing error.
    #[error("YAML parse error: {0}")]
    ParseError(#[from] serde_saphyr::Error),

    /// I/O error reading config file.
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// Config file not found at any search location.
    #[error("configuration file not found (searched: {searched:?})")]
    ConfigFileNotFound { searched: Vec<PathBuf> },

    /// Empty configuration file.
    #[error("configuration file is empty")]
    EmptyConfigFile,
}

/// Validation warnings (non-fatal).
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 8.3 (ValidationWarning)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationWarning {
    /// V-004: policy_id specified but action is not policy.
    PolicyIdWithoutPolicyAction { pattern: String },

    /// V-014: Environment variable not set, using default.
    MissingEnvVar { var: String, field: String },

    /// Glob pattern matches zero tools (informational).
    PatternMatchesNothing { pattern: String },

    /// Feature not supported in current version.
    UnsupportedFeature {
        feature: String,
        min_version: String,
    },
}

impl std::fmt::Display for ValidationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PolicyIdWithoutPolicyAction { pattern } => {
                write!(
                    f,
                    "rule '{pattern}' has policy_id but action is not 'policy'"
                )
            }
            Self::MissingEnvVar { var, field } => {
                write!(f, "environment variable '{var}' not set for '{field}'")
            }
            Self::PatternMatchesNothing { pattern } => {
                write!(f, "glob pattern '{pattern}' may match no tools")
            }
            Self::UnsupportedFeature {
                feature,
                min_version,
            } => {
                write!(
                    f,
                    "feature '{feature}' requires version {min_version} or later"
                )
            }
        }
    }
}

/// Result of configuration validation.
#[derive(Debug)]
pub struct ValidationResult {
    /// Non-fatal warnings encountered during validation.
    pub warnings: Vec<ValidationWarning>,
}

impl ValidationResult {
    /// Create a new validation result with no warnings.
    pub fn ok() -> Self {
        Self {
            warnings: Vec::new(),
        }
    }

    /// Create a new validation result with warnings.
    pub fn with_warnings(warnings: Vec<ValidationWarning>) -> Self {
        Self { warnings }
    }

    /// Check if validation passed with no warnings.
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_error_display() {
        let err = ConfigError::DuplicateSourceId {
            id: "upstream".to_string(),
        };
        assert_eq!(err.to_string(), "duplicate source ID: 'upstream'");
    }

    #[test]
    fn test_validation_warning_display() {
        let warn = ValidationWarning::PolicyIdWithoutPolicyAction {
            pattern: "test_*".to_string(),
        };
        assert_eq!(
            warn.to_string(),
            "rule 'test_*' has policy_id but action is not 'policy'"
        );
    }

    #[test]
    fn test_validation_result() {
        let result = ValidationResult::ok();
        assert!(result.is_clean());

        let result_with_warnings =
            ValidationResult::with_warnings(vec![ValidationWarning::PatternMatchesNothing {
                pattern: "foo_*".to_string(),
            }]);
        assert!(!result_with_warnings.is_clean());
    }
}
