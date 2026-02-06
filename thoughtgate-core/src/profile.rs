//! Configuration profiles controlling enforcement behaviour.
//!
//! Implements: REQ-CORE-008 §6.7 (Configuration Profile Types)
//!
//! Profiles are transport-agnostic — development mode's `WOULD_BLOCK` semantics
//! apply identically to HTTP and stdio transports.

use serde::{Deserialize, Serialize};

/// Named configuration profile controlling enforcement behaviour.
///
/// Implements: REQ-CORE-008/F-022
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// All inspectors enforcing, approvals required, version pinning enforced.
    Production,
    /// Inspectors log-only, approvals auto-approved with audit trail,
    /// version pinning disabled.
    Development,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_serde_roundtrip() {
        let json = serde_json::to_string(&Profile::Production).unwrap();
        assert_eq!(json, r#""production""#);
        let parsed: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Profile::Production);

        let json = serde_json::to_string(&Profile::Development).unwrap();
        assert_eq!(json, r#""development""#);
        let parsed: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Profile::Development);
    }
}
