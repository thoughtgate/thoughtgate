//! Policy loading with priority: ConfigMap → Env → Embedded.
//!
//! Implements: REQ-POL-001/F-003 (Policy Loading)

use super::PolicySource;
use std::env;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// Load policies with priority order.
///
/// Implements: REQ-POL-001/F-003 (Policy Loading)
///
/// # Priority
/// 1. ConfigMap file at `$THOUGHTGATE_POLICY_FILE` (default: `/etc/thoughtgate/policies.cedar`)
/// 2. Environment variable `$THOUGHTGATE_POLICIES`
/// 3. Embedded default policies
///
/// # Returns
/// Tuple of (policy_text, source)
///
/// # Note
/// Uses blocking I/O (`std::fs`). Called only at startup (via `CedarEngine::new()`)
/// and during rare hot-reload events (via `reload()`), so blocking is acceptable.
/// Production callers that run on the Tokio runtime should wrap calls in
/// `tokio::task::spawn_blocking` if needed.
pub fn load_policies() -> (String, PolicySource) {
    // 1. Try ConfigMap
    let config_path = env::var("THOUGHTGATE_POLICY_FILE")
        .unwrap_or_else(|_| "/etc/thoughtgate/policies.cedar".to_string());

    if Path::new(&config_path).exists() {
        info!(path = %config_path, "Loading policies from ConfigMap");
        match fs::read_to_string(&config_path) {
            Ok(content) => {
                return (
                    content,
                    PolicySource::ConfigMap {
                        path: config_path,
                        loaded_at: std::time::SystemTime::now(),
                    },
                );
            }
            Err(e) => {
                warn!(
                    path = %config_path,
                    error = %e,
                    "Failed to read policy file, trying environment"
                );
            }
        }
    }

    // 2. Try Environment Variable
    if let Ok(policy_str) = env::var("THOUGHTGATE_POLICIES") {
        info!("Loading policies from environment variable");
        return (
            policy_str,
            PolicySource::Environment {
                loaded_at: std::time::SystemTime::now(),
            },
        );
    }

    // 3. Fallback to Embedded
    warn!("Using embedded default policies - NOT FOR PRODUCTION");
    (embedded_default_policies(), PolicySource::Embedded)
}

/// Load Cedar schema.
///
/// Implements: REQ-POL-001/F-004 (Schema Validation)
///
/// Loads schema from:
/// 1. File at `$THOUGHTGATE_SCHEMA_FILE` (default: `/etc/thoughtgate/schema.cedarschema`)
/// 2. Embedded schema (compile-time)
///
/// # Note
/// Uses blocking I/O (`std::fs`). See `load_policies` for rationale.
pub fn load_schema() -> String {
    let schema_path = env::var("THOUGHTGATE_SCHEMA_FILE")
        .unwrap_or_else(|_| "/etc/thoughtgate/schema.cedarschema".to_string());

    if Path::new(&schema_path).exists() {
        info!(path = %schema_path, "Loading schema from file");
        match fs::read_to_string(&schema_path) {
            Ok(content) => return content,
            Err(e) => {
                warn!(
                    path = %schema_path,
                    error = %e,
                    "Failed to read schema file, using embedded"
                );
            }
        }
    }

    info!("Using embedded Cedar schema");
    embedded_schema()
}

/// Embedded default policies for development.
///
/// Implements: REQ-POL-001/F-007 (Embedded Default Policy)
///
/// **WARNING:** These policies permit ALL actions. Only use for development.
fn embedded_default_policies() -> String {
    include_str!("defaults.cedar").to_string()
}

/// Embedded Cedar schema.
///
/// Implements: REQ-POL-001/F-004
fn embedded_schema() -> String {
    include_str!("schema.cedarschema").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_load_policies_embedded() {
        // Clear env vars to ensure embedded is used
        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
            env::remove_var("THOUGHTGATE_POLICIES");
        }

        let (policies, source) = load_policies();
        assert!(!policies.is_empty());
        assert!(matches!(source, PolicySource::Embedded));
    }

    #[test]
    #[serial]
    fn test_load_policies_from_env() {
        unsafe {
            env::set_var(
                "THOUGHTGATE_POLICIES",
                "permit(principal, action, resource);",
            );
        }

        let (policies, source) = load_policies();
        assert_eq!(policies, "permit(principal, action, resource);");
        assert!(matches!(source, PolicySource::Environment { .. }));

        unsafe {
            env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    #[test]
    fn test_load_schema_embedded() {
        let schema = load_schema();
        assert!(!schema.is_empty());
        assert!(schema.contains("namespace ThoughtGate"));
    }

    #[test]
    fn test_embedded_policies_not_empty() {
        let policies = embedded_default_policies();
        assert!(!policies.is_empty());
    }

    #[test]
    fn test_embedded_schema_not_empty() {
        let schema = embedded_schema();
        assert!(!schema.is_empty());
        assert!(schema.contains("namespace ThoughtGate"));
    }

    // ═══════════════════════════════════════════════════════════
    // Edge Case Tests (EC-POL-004, 005, 006) - Policy Loading
    // ═══════════════════════════════════════════════════════════

    /// EC-POL-004: ConfigMap exists → Load from ConfigMap
    #[test]
    #[serial]
    fn test_ec_pol_004_configmap_exists() {
        use std::io::Write;

        // Create a temporary ConfigMap file
        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join(format!("thoughtgate_test_{}.cedar", std::process::id()));
        let config_content = "permit(principal, action, resource);";

        {
            let mut file = fs::File::create(&temp_path).expect("Failed to create temp file");
            file.write_all(config_content.as_bytes())
                .expect("Failed to write to temp file");
        }

        unsafe {
            env::set_var("THOUGHTGATE_POLICY_FILE", &temp_path);
            env::remove_var("THOUGHTGATE_POLICIES");
        }

        let (policies, source) = load_policies();
        assert_eq!(policies, config_content);
        assert!(matches!(source, PolicySource::ConfigMap { .. }));

        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
        }

        // Cleanup
        let _ = fs::remove_file(&temp_path);
    }

    /// EC-POL-005: ConfigMap missing, Env exists → Load from Env
    #[test]
    #[serial]
    fn test_ec_pol_005_env_fallback() {
        unsafe {
            env::set_var("THOUGHTGATE_POLICY_FILE", "/nonexistent/path/policy.cedar");
            env::set_var(
                "THOUGHTGATE_POLICIES",
                "permit(principal, action, resource);",
            );
        }

        let (policies, source) = load_policies();
        assert_eq!(policies, "permit(principal, action, resource);");
        assert!(matches!(source, PolicySource::Environment { .. }));

        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
            env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    /// EC-POL-006: Both ConfigMap and Env missing → Load embedded
    #[test]
    #[serial]
    fn test_ec_pol_006_embedded_fallback() {
        unsafe {
            env::set_var("THOUGHTGATE_POLICY_FILE", "/nonexistent/path/policy.cedar");
            env::remove_var("THOUGHTGATE_POLICIES");
        }

        let (policies, source) = load_policies();
        assert!(!policies.is_empty());
        assert!(matches!(source, PolicySource::Embedded));
        // Verify it contains expected v0.1 default policies
        assert!(policies.contains("Forward"));
        assert!(policies.contains("Approve"));

        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
        }
    }
}
