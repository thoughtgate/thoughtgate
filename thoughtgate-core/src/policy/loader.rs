//! Policy loading with priority: ConfigMap → Env → Embedded.
//!
//! Implements: REQ-POL-001/F-003 (Policy Loading)

use super::{PolicyError, PolicySource};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
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
/// # Errors
/// Returns `PolicyError::PolicyLoadError` if a configured policy file exists
/// but cannot be read (fail-closed on I/O errors).
///
/// # Note
/// Uses blocking I/O (`std::fs`). Called only at startup (via `CedarEngine::new()`)
/// and during rare hot-reload events (via `reload()`), so blocking is acceptable.
/// Production callers that run on the Tokio runtime should wrap calls in
/// `tokio::task::spawn_blocking` if needed.
pub fn load_policies() -> Result<(String, PolicySource), PolicyError> {
    load_policies_with_config(None)
}

/// Load policies with optional YAML config paths.
///
/// Implements: REQ-POL-001/F-003 (Policy Loading)
///
/// # Priority
/// 1. ConfigMap file at `$THOUGHTGATE_POLICY_FILE`
/// 2. Environment variable `$THOUGHTGATE_POLICIES`
/// 3. YAML `cedar.policies[]` paths (concatenated)
/// 4. Embedded default policies
///
/// # Returns
/// Tuple of (policy_text, source)
///
/// # Errors
/// Returns `PolicyError::PolicyLoadError` if a configured policy file exists
/// but cannot be read. File-not-found falls through to the next tier.
pub fn load_policies_with_config(
    config_paths: Option<&[PathBuf]>,
) -> Result<(String, PolicySource), PolicyError> {
    // 1. Try ConfigMap
    let config_path = env::var("THOUGHTGATE_POLICY_FILE")
        .unwrap_or_else(|_| "/etc/thoughtgate/policies.cedar".to_string());

    if Path::new(&config_path).exists() {
        info!(path = %config_path, "Loading policies from ConfigMap");
        match fs::read_to_string(&config_path) {
            Ok(content) => {
                return Ok((
                    content,
                    PolicySource::ConfigMap {
                        path: config_path,
                        loaded_at: std::time::SystemTime::now(),
                    },
                ));
            }
            Err(e) => {
                // File exists but can't be read — fail closed.
                // A transient permissions error must NOT silently fall
                // through to embedded permit-all policies.
                return Err(PolicyError::PolicyLoadError {
                    path: config_path,
                    reason: e.to_string(),
                });
            }
        }
    }

    // 2. Try Environment Variable
    if let Ok(policy_str) = env::var("THOUGHTGATE_POLICIES") {
        info!("Loading policies from environment variable");
        return Ok((
            policy_str,
            PolicySource::Environment {
                loaded_at: std::time::SystemTime::now(),
            },
        ));
    }

    // 3. Try YAML config paths
    if let Some(paths) = config_paths {
        if !paths.is_empty() {
            let mut combined = String::new();
            for path in paths {
                match fs::read_to_string(path) {
                    Ok(content) => {
                        if !combined.is_empty() {
                            combined.push('\n');
                        }
                        combined.push_str(&content);
                    }
                    Err(e) => {
                        // YAML-configured path unreadable — fail closed.
                        return Err(PolicyError::PolicyLoadError {
                            path: path.display().to_string(),
                            reason: e.to_string(),
                        });
                    }
                }
            }
            if !combined.is_empty() {
                info!(
                    path_count = paths.len(),
                    "Loading policies from YAML cedar.policies[]"
                );
                return Ok((
                    combined,
                    PolicySource::ConfigMap {
                        path: paths[0].display().to_string(),
                        loaded_at: std::time::SystemTime::now(),
                    },
                ));
            }
        }
    }

    // 4. Fallback to Embedded — only reached when no higher-tier source is configured
    warn!("Using embedded default policies - NOT FOR PRODUCTION");
    Ok((embedded_default_policies(), PolicySource::Embedded))
}

/// Load Cedar schema, optionally using a YAML-configured path.
///
/// Implements: REQ-POL-001/F-004 (Schema Validation)
///
/// # Priority
/// 1. File at `$THOUGHTGATE_SCHEMA_FILE`
/// 2. YAML `cedar.schema` path
/// 3. Embedded schema (compile-time)
///
/// # Errors
/// Returns `PolicyError::PolicyLoadError` if a configured schema file exists
/// but cannot be read.
pub fn load_schema_with_config(config_schema: Option<&Path>) -> Result<String, PolicyError> {
    let schema_path = env::var("THOUGHTGATE_SCHEMA_FILE")
        .unwrap_or_else(|_| "/etc/thoughtgate/schema.cedarschema".to_string());

    if Path::new(&schema_path).exists() {
        info!(path = %schema_path, "Loading schema from file");
        match fs::read_to_string(&schema_path) {
            Ok(content) => return Ok(content),
            Err(e) => {
                return Err(PolicyError::PolicyLoadError {
                    path: schema_path,
                    reason: e.to_string(),
                });
            }
        }
    }

    // Try YAML-configured schema path
    if let Some(path) = config_schema {
        if path.exists() {
            info!(path = %path.display(), "Loading schema from YAML cedar.schema");
            match fs::read_to_string(path) {
                Ok(content) => return Ok(content),
                Err(e) => {
                    return Err(PolicyError::PolicyLoadError {
                        path: path.display().to_string(),
                        reason: e.to_string(),
                    });
                }
            }
        }
    }

    info!("Using embedded Cedar schema");
    Ok(embedded_schema())
}

/// Load Cedar schema from env vars only.
///
/// Implements: REQ-POL-001/F-004 (Schema Validation)
///
/// # Note
/// Uses blocking I/O (`std::fs`). See `load_policies` for rationale.
pub fn load_schema() -> Result<String, PolicyError> {
    load_schema_with_config(None)
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

        let (policies, source) = load_policies().unwrap();
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

        let (policies, source) = load_policies().unwrap();
        assert_eq!(policies, "permit(principal, action, resource);");
        assert!(matches!(source, PolicySource::Environment { .. }));

        unsafe {
            env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    #[test]
    fn test_load_schema_embedded() {
        let schema = load_schema().unwrap();
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

        let (policies, source) = load_policies().unwrap();
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

        let (policies, source) = load_policies().unwrap();
        assert_eq!(policies, "permit(principal, action, resource);");
        assert!(matches!(source, PolicySource::Environment { .. }));

        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
            env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    #[test]
    #[serial]
    fn test_load_policies_with_config_paths() {
        use std::io::Write;

        // Clear env vars so config paths are used
        unsafe {
            env::set_var("THOUGHTGATE_POLICY_FILE", "/nonexistent/path/policy.cedar");
            env::remove_var("THOUGHTGATE_POLICIES");
        }

        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join(format!("thoughtgate_cfg_test_{}.cedar", std::process::id()));
        {
            let mut file = fs::File::create(&temp_path).expect("create temp file");
            file.write_all(b"permit(principal, action, resource);")
                .expect("write");
        }

        let (policies, source) =
            load_policies_with_config(Some(std::slice::from_ref(&temp_path))).unwrap();
        assert_eq!(policies, "permit(principal, action, resource);");
        assert!(matches!(source, PolicySource::ConfigMap { .. }));

        let _ = fs::remove_file(&temp_path);
        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
        }
    }

    #[test]
    #[serial]
    fn test_load_policies_env_overrides_config() {
        use std::io::Write;

        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join(format!("thoughtgate_cfg_env_{}.cedar", std::process::id()));
        {
            let mut file = fs::File::create(&temp_path).expect("create temp file");
            file.write_all(b"forbid(principal, action, resource);")
                .expect("write");
        }

        // Env var takes priority over config paths
        unsafe {
            env::set_var("THOUGHTGATE_POLICY_FILE", "/nonexistent/path/policy.cedar");
            env::set_var(
                "THOUGHTGATE_POLICIES",
                "permit(principal, action, resource);",
            );
        }

        let (policies, source) =
            load_policies_with_config(Some(std::slice::from_ref(&temp_path))).unwrap();
        assert_eq!(policies, "permit(principal, action, resource);");
        assert!(matches!(source, PolicySource::Environment { .. }));

        let _ = fs::remove_file(&temp_path);
        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
            env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    #[test]
    #[serial]
    fn test_load_policies_config_when_no_env() {
        use std::io::Write;

        unsafe {
            env::set_var("THOUGHTGATE_POLICY_FILE", "/nonexistent/path/policy.cedar");
            env::remove_var("THOUGHTGATE_POLICIES");
        }

        let temp_dir = std::env::temp_dir();
        let p1 = temp_dir.join(format!("thoughtgate_multi1_{}.cedar", std::process::id()));
        let p2 = temp_dir.join(format!("thoughtgate_multi2_{}.cedar", std::process::id()));
        {
            let mut f1 = fs::File::create(&p1).expect("create");
            f1.write_all(b"// policy file 1\n").expect("write");
            let mut f2 = fs::File::create(&p2).expect("create");
            f2.write_all(b"// policy file 2\n").expect("write");
        }

        let (policies, _source) =
            load_policies_with_config(Some(&[p1.clone(), p2.clone()])).unwrap();
        assert!(policies.contains("policy file 1"));
        assert!(policies.contains("policy file 2"));

        let _ = fs::remove_file(&p1);
        let _ = fs::remove_file(&p2);
        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
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

        let (policies, source) = load_policies().unwrap();
        assert!(!policies.is_empty());
        assert!(matches!(source, PolicySource::Embedded));
        // Verify it contains expected v0.1 default policies
        assert!(policies.contains("Forward"));
        assert!(policies.contains("Approve"));

        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
        }
    }

    /// ConfigMap file exists but is unreadable → hard error
    #[test]
    #[serial]
    fn test_configmap_exists_but_unreadable_fails_hard() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join(format!(
            "thoughtgate_unreadable_{}.cedar",
            std::process::id()
        ));

        // Create file, then make it unreadable
        fs::write(&temp_path, "permit(principal, action, resource);").expect("create file");
        fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o000)).expect("chmod");

        unsafe {
            env::set_var("THOUGHTGATE_POLICY_FILE", &temp_path);
            env::remove_var("THOUGHTGATE_POLICIES");
        }

        let result = load_policies();
        assert!(result.is_err(), "Should fail when ConfigMap is unreadable");
        let err = result.unwrap_err();
        assert!(
            matches!(err, PolicyError::PolicyLoadError { .. }),
            "Expected PolicyLoadError, got: {err:?}"
        );

        // Cleanup: restore permissions so we can delete
        let _ = fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o644));
        let _ = fs::remove_file(&temp_path);
        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
        }
    }

    /// YAML-configured path unreadable → hard error
    #[test]
    #[serial]
    fn test_yaml_path_unreadable_fails_hard() {
        use std::os::unix::fs::PermissionsExt;

        unsafe {
            env::set_var("THOUGHTGATE_POLICY_FILE", "/nonexistent/path/policy.cedar");
            env::remove_var("THOUGHTGATE_POLICIES");
        }

        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join(format!(
            "thoughtgate_yaml_unread_{}.cedar",
            std::process::id()
        ));

        fs::write(&temp_path, "permit(principal, action, resource);").expect("create file");
        fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o000)).expect("chmod");

        let result = load_policies_with_config(Some(std::slice::from_ref(&temp_path)));
        assert!(result.is_err(), "Should fail when YAML path is unreadable");

        // Cleanup
        let _ = fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o644));
        let _ = fs::remove_file(&temp_path);
        unsafe {
            env::remove_var("THOUGHTGATE_POLICY_FILE");
        }
    }
}
