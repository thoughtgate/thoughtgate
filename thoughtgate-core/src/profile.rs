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

/// Per-feature behaviour determined by active profile.
///
/// Implements: REQ-CORE-008/F-022, F-023
#[derive(Debug, Clone)]
pub struct ProfileBehaviour {
    /// Whether governance decisions block (true) or log-only (false).
    pub enforcement_blocking: bool,
    /// Whether approval workflows require human interaction.
    pub approvals_required: bool,
    /// Log prefix for governance decisions (`"BLOCKED"` vs `"WOULD_BLOCK"`).
    pub decision_log_prefix: &'static str,
}

impl Profile {
    /// Returns the behaviour configuration for this profile.
    ///
    /// Implements: REQ-CORE-008/F-022, F-023
    pub fn behaviour(&self) -> ProfileBehaviour {
        match self {
            Profile::Production => ProfileBehaviour {
                enforcement_blocking: true,
                approvals_required: true,
                decision_log_prefix: "BLOCKED",
            },
            Profile::Development => ProfileBehaviour {
                enforcement_blocking: false,
                approvals_required: false,
                decision_log_prefix: "WOULD_BLOCK",
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_production_behaviour() {
        let b = Profile::Production.behaviour();
        assert!(b.enforcement_blocking);
        assert!(b.approvals_required);
        assert_eq!(b.decision_log_prefix, "BLOCKED");
    }

    #[test]
    fn test_development_behaviour() {
        let b = Profile::Development.behaviour();
        assert!(!b.enforcement_blocking);
        assert!(!b.approvals_required);
        assert_eq!(b.decision_log_prefix, "WOULD_BLOCK");
    }

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
