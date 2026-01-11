//! Principal identity inference from Kubernetes environment.
//!
//! Implements: REQ-POL-001/F-006 (Identity Inference)

use super::{PolicyError, Principal};
use std::env;
use std::fs;
use tracing::{info, warn};

/// Infer principal identity from environment.
///
/// Implements: REQ-POL-001/F-006 (Identity Inference)
///
/// # Priority
/// 1. Dev mode (if `THOUGHTGATE_DEV_MODE=true`)
/// 2. Kubernetes ServiceAccount mount
/// 3. Error if neither available
///
/// # Errors
/// Returns `PolicyError::IdentityError` if:
/// - K8s identity required but not available
/// - Required environment variables missing
pub fn infer_principal() -> Result<Principal, PolicyError> {
    // Check for dev mode first (must be explicitly set to "true")
    if env::var("THOUGHTGATE_DEV_MODE").as_deref() == Ok("true") {
        warn!("Using development mode principal - NOT FOR PRODUCTION");
        return Ok(dev_mode_principal());
    }

    // Try Kubernetes identity
    kubernetes_principal()
}

/// Create principal for development mode.
///
/// Implements: REQ-POL-001/F-006.2 (Dev Mode Override)
fn dev_mode_principal() -> Principal {
    let app_name = env::var("THOUGHTGATE_DEV_PRINCIPAL").unwrap_or_else(|_| "dev-app".to_string());

    let namespace =
        env::var("THOUGHTGATE_DEV_NAMESPACE").unwrap_or_else(|_| "development".to_string());

    Principal {
        app_name,
        namespace,
        service_account: "dev-sa".to_string(),
        roles: vec!["dev".to_string()],
    }
}

/// Infer principal from Kubernetes ServiceAccount mount.
///
/// Implements: REQ-POL-001/F-006.1 (K8s Identity)
///
/// Reads identity from:
/// - `$HOSTNAME` → app_name
/// - `/var/run/secrets/kubernetes.io/serviceaccount/namespace` → namespace
/// - `/var/run/secrets/kubernetes.io/serviceaccount/token` → service_account (parsed)
fn kubernetes_principal() -> Result<Principal, PolicyError> {
    // Read hostname (pod name)
    let app_name = env::var("HOSTNAME").map_err(|_| PolicyError::IdentityError {
        details: "HOSTNAME environment variable not set".to_string(),
    })?;

    // Read namespace from ServiceAccount mount
    let namespace = fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
        .map_err(|e| PolicyError::IdentityError {
            details: format!("Cannot read namespace from ServiceAccount mount: {}", e),
        })?
        .trim()
        .to_string();

    // Try to read ServiceAccount from token (best effort)
    let service_account = fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/token")
        .ok()
        .and_then(|token| parse_service_account_from_token(&token))
        .unwrap_or_else(|| "default".to_string());

    info!(
        app_name = %app_name,
        namespace = %namespace,
        service_account = %service_account,
        "Inferred K8s principal identity"
    );

    Ok(Principal {
        app_name,
        namespace,
        service_account,
        roles: vec![], // Roles can be loaded from policy or external source
    })
}

/// Parse ServiceAccount name from JWT token.
///
/// Implements: REQ-POL-001/F-006.1
///
/// This is a best-effort extraction. The token is a JWT with the ServiceAccount
/// name in the `kubernetes.io/serviceaccount/service-account.name` claim.
/// We avoid full JWT parsing to keep dependencies minimal.
fn parse_service_account_from_token(token: &str) -> Option<String> {
    // JWT tokens are base64-encoded JSON separated by dots
    // Format: header.payload.signature
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    // Decode the payload (second part)
    let payload = parts[1];

    // JWT uses base64url encoding (no padding)
    let decoded = base64_decode_no_pad(payload)?;
    let json_str = String::from_utf8(decoded).ok()?;

    // Parse JSON to extract service account name
    // Expected structure: {"kubernetes.io/serviceaccount/service-account.name":"sa-name",...}
    extract_sa_from_json(&json_str)
}

/// Simple base64 decoder for JWT payload (base64url, no padding).
fn base64_decode_no_pad(_input: &str) -> Option<Vec<u8>> {
    // TODO: Add proper base64 decoding or use a lightweight JWT crate
    // For now, just return None - full JWT parsing not critical for v0.1
    // The ServiceAccount name will default to "default" if parsing fails
    None
}

/// Extract ServiceAccount name from JWT payload JSON.
fn extract_sa_from_json(json: &str) -> Option<String> {
    // Look for the service account name claim
    // This is a simple string search, not full JSON parsing
    let key = "\"kubernetes.io/serviceaccount/service-account.name\":\"";
    if let Some(start) = json.find(key) {
        let value_start = start + key.len();
        if let Some(end) = json[value_start..].find('"') {
            return Some(json[value_start..value_start + end].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_dev_mode_principal() {
        // Clear any env vars from other tests
        unsafe {
            env::remove_var("THOUGHTGATE_DEV_PRINCIPAL");
            env::remove_var("THOUGHTGATE_DEV_NAMESPACE");
        }

        let principal = dev_mode_principal();
        assert_eq!(principal.app_name, "dev-app");
        assert_eq!(principal.namespace, "development");
        assert_eq!(principal.service_account, "dev-sa");
        assert_eq!(principal.roles, vec!["dev"]);
    }

    #[test]
    #[serial]
    fn test_dev_mode_with_overrides() {
        // Using unsafe blocks for test env var manipulation
        unsafe {
            env::set_var("THOUGHTGATE_DEV_PRINCIPAL", "my-test-app");
            env::set_var("THOUGHTGATE_DEV_NAMESPACE", "testing");
        }

        let principal = dev_mode_principal();
        assert_eq!(principal.app_name, "my-test-app");
        assert_eq!(principal.namespace, "testing");

        unsafe {
            env::remove_var("THOUGHTGATE_DEV_PRINCIPAL");
            env::remove_var("THOUGHTGATE_DEV_NAMESPACE");
        }
    }

    #[test]
    fn test_extract_sa_from_json() {
        let json =
            r#"{"kubernetes.io/serviceaccount/service-account.name":"my-sa","other":"value"}"#;
        let result = extract_sa_from_json(json);
        assert_eq!(result, Some("my-sa".to_string()));
    }

    #[test]
    fn test_extract_sa_from_json_not_found() {
        let json = r#"{"other":"value"}"#;
        let result = extract_sa_from_json(json);
        assert_eq!(result, None);
    }

    // ═══════════════════════════════════════════════════════════
    // Edge Case Tests (EC-POL-013, 014, 015, 016)
    // ═══════════════════════════════════════════════════════════

    /// EC-POL-013: K8s identity available → Infer principal
    /// Note: This test verifies dev mode override. Full K8s ServiceAccount testing
    /// requires integration tests with actual K8s mounts or mocked file system.
    #[test]
    #[serial]
    fn test_ec_pol_013_k8s_identity_structure() {
        // This test verifies the structure using dev mode
        // Full integration test should be in tests/integration_k8s.rs
        unsafe {
            env::set_var("THOUGHTGATE_DEV_MODE", "true");
            env::set_var("THOUGHTGATE_DEV_PRINCIPAL", "test-pod");
            env::set_var("THOUGHTGATE_DEV_NAMESPACE", "production");
        }

        let result = infer_principal();
        assert!(result.is_ok());

        let principal = result.unwrap();
        assert_eq!(principal.app_name, "test-pod");
        assert_eq!(principal.namespace, "production");

        unsafe {
            env::remove_var("THOUGHTGATE_DEV_MODE");
            env::remove_var("THOUGHTGATE_DEV_PRINCIPAL");
            env::remove_var("THOUGHTGATE_DEV_NAMESPACE");
        }
    }

    /// EC-POL-014: K8s identity missing, dev mode → Use dev principal
    #[test]
    #[serial]
    fn test_ec_pol_014_dev_mode_fallback() {
        unsafe {
            env::set_var("THOUGHTGATE_DEV_MODE", "true");
            env::set_var("THOUGHTGATE_DEV_PRINCIPAL", "my-dev-app");
            env::set_var("THOUGHTGATE_DEV_NAMESPACE", "dev-namespace");
        }

        let result = infer_principal();
        assert!(result.is_ok());

        let principal = result.unwrap();
        assert_eq!(principal.app_name, "my-dev-app");
        assert_eq!(principal.namespace, "dev-namespace");
        assert_eq!(principal.service_account, "dev-sa");
        assert_eq!(principal.roles, vec!["dev"]);

        unsafe {
            env::remove_var("THOUGHTGATE_DEV_MODE");
            env::remove_var("THOUGHTGATE_DEV_PRINCIPAL");
            env::remove_var("THOUGHTGATE_DEV_NAMESPACE");
        }
    }

    /// EC-POL-015: K8s identity missing, no dev mode → Fail startup
    #[test]
    #[serial]
    fn test_ec_pol_015_no_identity_fails() {
        unsafe {
            env::remove_var("THOUGHTGATE_DEV_MODE");
            env::remove_var("HOSTNAME");
        }

        let result = infer_principal();
        assert!(result.is_err());

        if let Err(PolicyError::IdentityError { details }) = result {
            assert!(details.contains("HOSTNAME"));
        } else {
            panic!("Expected IdentityError for missing K8s identity");
        }
    }

    /// EC-POL-016: Role-based policy matching
    /// Note: This tests the Principal structure supports roles.
    /// Actual role hierarchy matching would be tested in engine tests with Cedar policies.
    #[test]
    fn test_ec_pol_016_principal_with_roles() {
        let principal = Principal {
            app_name: "admin-app".to_string(),
            namespace: "production".to_string(),
            service_account: "admin-sa".to_string(),
            roles: vec!["admin".to_string(), "operator".to_string()],
        };

        assert_eq!(principal.roles.len(), 2);
        assert!(principal.roles.contains(&"admin".to_string()));
        assert!(principal.roles.contains(&"operator".to_string()));

        // In a real policy evaluation, Cedar would check:
        // principal in ThoughtGate::Role::"admin"
        // This test just verifies the data structure is correct
    }

    /// Test that dev mode requires explicit "true" value
    #[test]
    #[serial]
    fn test_dev_mode_requires_true() {
        // Setting to "false" should NOT enable dev mode
        unsafe {
            env::set_var("THOUGHTGATE_DEV_MODE", "false");
            env::remove_var("HOSTNAME");
        }

        let result = infer_principal();
        // Should fail because K8s identity is missing and dev mode is not enabled
        assert!(result.is_err());

        // Setting to "1" should NOT enable dev mode
        unsafe {
            env::set_var("THOUGHTGATE_DEV_MODE", "1");
        }

        let result = infer_principal();
        // Should fail because K8s identity is missing and dev mode is not enabled
        assert!(result.is_err());

        // Setting to "true" SHOULD enable dev mode
        unsafe {
            env::set_var("THOUGHTGATE_DEV_MODE", "true");
        }

        let result = infer_principal();
        // Should succeed with dev principal
        assert!(result.is_ok());

        unsafe {
            env::remove_var("THOUGHTGATE_DEV_MODE");
        }
    }
}
