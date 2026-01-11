//! Cedar policy engine implementation.
//!
//! Implements: REQ-POL-001/F-001, F-005 (Policy Evaluation & Hot-Reload)
//!
//! # v0.1 Simplified Model
//!
//! The engine evaluates Cedar policies and returns `PolicyAction`:
//! - `Forward`: Send request to upstream immediately
//! - `Approve`: Require human approval before forwarding
//! - `Reject`: Deny the request
//!
//! Evaluation order: Forward → Approve → (default) Reject

use super::{
    PolicyAction, PolicyError, PolicyRequest, PolicySource, PolicyStats, Resource, loader,
};
use arc_swap::ArcSwap;
use cedar_policy::{
    Authorizer, Context, Decision, Entities, EntityId, EntityTypeName, EntityUid, PolicySet,
    Request, Schema,
};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Cedar policy engine for ThoughtGate.
///
/// Implements: REQ-POL-001 (Cedar Policy Engine)
pub struct CedarEngine {
    /// Cedar authorizer
    authorizer: Authorizer,

    /// Current policy set (atomic for hot-reload)
    policies: ArcSwap<PolicySet>,

    /// Cedar schema for validation
    schema: Schema,

    /// Policy source information
    source: Arc<ArcSwap<PolicySource>>,

    /// Statistics counters
    stats: Arc<Stats>,
}

struct Stats {
    evaluation_count: AtomicU64,
    reload_count: AtomicU64,
    last_reload: arc_swap::ArcSwap<Option<std::time::SystemTime>>,
}

impl CedarEngine {
    /// Create a new Cedar engine.
    ///
    /// Implements: REQ-POL-001/F-003 (Policy Loading)
    ///
    /// # Errors
    /// Returns `PolicyError` if:
    /// - Schema parsing fails
    /// - Policy parsing fails
    /// - Schema validation fails
    pub fn new() -> Result<Self, PolicyError> {
        info!("Initializing Cedar policy engine");

        // Load schema
        let schema_str = loader::load_schema();
        let schema = Schema::from_str(&schema_str).map_err(|e| PolicyError::SchemaValidation {
            details: format!("Failed to parse schema: {}", e),
        })?;

        // Load policies
        let (policy_str, source) = loader::load_policies();
        let policies = Self::parse_policies(&policy_str, &schema)?;

        info!(
            source = ?source,
            policy_count = policies.policies().count(),
            "Cedar engine initialized"
        );

        Ok(Self {
            authorizer: Authorizer::new(),
            policies: ArcSwap::new(Arc::new(policies)),
            schema,
            source: Arc::new(ArcSwap::new(Arc::new(source))),
            stats: Arc::new(Stats {
                evaluation_count: AtomicU64::new(0),
                reload_count: AtomicU64::new(0),
                last_reload: arc_swap::ArcSwap::new(Arc::new(None)),
            }),
        })
    }

    /// Evaluate a policy request.
    ///
    /// Implements: REQ-POL-001/F-001 (Policy Evaluation - v0.1 Simplified)
    ///
    /// # Decision Logic (v0.1)
    /// Check Forward → Approve → Reject
    ///
    /// # Returns
    /// - `PolicyAction::Forward` if Forward action is permitted
    /// - `PolicyAction::Approve` if only Approve action is permitted
    /// - `PolicyAction::Reject` if no action is permitted
    pub fn evaluate(&self, request: &PolicyRequest) -> PolicyAction {
        self.stats.evaluation_count.fetch_add(1, Ordering::Relaxed);

        let policies = self.policies.load();

        // v0.1: Check actions in priority order: Forward → Approve
        let actions = ["Forward", "Approve"];

        for action_name in &actions {
            if self.is_action_permitted(request, action_name, &policies) {
                debug!(
                    principal = %request.principal.app_name,
                    resource = ?request.resource,
                    action = action_name,
                    "Policy permit"
                );

                return match *action_name {
                    "Forward" => PolicyAction::Forward,
                    "Approve" => PolicyAction::Approve {
                        timeout: Duration::from_secs(300), // Default 5 minutes
                    },
                    _ => unreachable!(),
                };
            }
        }

        // No action permitted - Reject
        warn!(
            principal = %request.principal.app_name,
            resource = ?request.resource,
            "Policy denied - no permitted action"
        );

        PolicyAction::Reject {
            reason: "No policy permits this request".to_string(),
        }
    }

    /// Check if a specific action is permitted.
    fn is_action_permitted(
        &self,
        request: &PolicyRequest,
        action_name: &str,
        policies: &PolicySet,
    ) -> bool {
        // Build Cedar request
        let cedar_request = match self.build_cedar_request(request, action_name) {
            Ok(req) => req,
            Err(e) => {
                error!(error = %e, "Failed to build Cedar request");
                return false;
            }
        };

        // Evaluate with empty entities
        // Note: For v0.1, we don't populate the entity store.
        // This works for policies using entity UIDs (principal == ThoughtGate::App::"name")
        // but not for attribute-based policies (principal.namespace == "prod").
        // Full entity support will be added in a future version.
        let entities = Entities::empty();
        let response = self
            .authorizer
            .is_authorized(&cedar_request, policies, &entities);

        response.decision() == Decision::Allow
    }

    /// Build Cedar request from PolicyRequest.
    fn build_cedar_request(
        &self,
        request: &PolicyRequest,
        action_name: &str,
    ) -> Result<Request, PolicyError> {
        // Build principal UID
        let principal_uid = EntityUid::from_type_name_and_id(
            EntityTypeName::from_str("ThoughtGate::App").map_err(|e| PolicyError::CedarError {
                details: format!("Invalid entity type: {}", e),
            })?,
            EntityId::from_str(&request.principal.app_name).map_err(|e| {
                PolicyError::CedarError {
                    details: format!("Invalid principal ID: {}", e),
                }
            })?,
        );

        // Build resource UID
        let (resource_type, resource_id) = match &request.resource {
            Resource::ToolCall { name, .. } => ("ThoughtGate::ToolCall", name.clone()),
            Resource::McpMethod { method, .. } => ("ThoughtGate::McpMethod", method.clone()),
        };

        let resource_uid = EntityUid::from_type_name_and_id(
            EntityTypeName::from_str(resource_type).map_err(|e| PolicyError::CedarError {
                details: format!("Invalid resource type: {}", e),
            })?,
            EntityId::from_str(&resource_id).map_err(|e| PolicyError::CedarError {
                details: format!("Invalid resource ID: {}", e),
            })?,
        );

        // Build action UID
        let action_uid = EntityUid::from_type_name_and_id(
            EntityTypeName::from_str("ThoughtGate::Action").map_err(|e| {
                PolicyError::CedarError {
                    details: format!("Invalid action type: {}", e),
                }
            })?,
            EntityId::from_str(action_name).map_err(|e| PolicyError::CedarError {
                details: format!("Invalid action ID: {}", e),
            })?,
        );

        // Build context (for approval grant if present)
        let context = if let Some(ctx) = &request.context {
            if let Some(grant) = &ctx.approval_grant {
                use std::collections::HashMap;
                let mut record_fields = HashMap::new();
                record_fields.insert(
                    "task_id".to_string(),
                    cedar_policy::RestrictedExpression::new_string(grant.task_id.clone()),
                );
                record_fields.insert(
                    "approved_by".to_string(),
                    cedar_policy::RestrictedExpression::new_string(grant.approved_by.clone()),
                );
                record_fields.insert(
                    "approved_at".to_string(),
                    cedar_policy::RestrictedExpression::new_long(grant.approved_at),
                );

                let record = cedar_policy::RestrictedExpression::new_record(record_fields)
                    .map_err(|e| PolicyError::CedarError {
                        details: format!("Failed to create approval grant record: {}", e),
                    })?;

                Context::from_pairs(vec![("approval_grant".to_string(), record)]).map_err(|e| {
                    PolicyError::CedarError {
                        details: format!("Failed to create context: {}", e),
                    }
                })?
            } else {
                Context::empty()
            }
        } else {
            Context::empty()
        };

        // Create Cedar request
        Request::new(principal_uid, action_uid, resource_uid, context, None).map_err(|e| {
            PolicyError::CedarError {
                details: format!("Failed to create Cedar request: {}", e),
            }
        })
    }

    /// Parse policies and validate against schema.
    ///
    /// Implements: REQ-POL-001/F-004 (Schema Validation)
    fn parse_policies(policy_str: &str, schema: &Schema) -> Result<PolicySet, PolicyError> {
        // Parse policies
        let policies = PolicySet::from_str(policy_str).map_err(|e| PolicyError::ParseError {
            details: format!("{}", e),
            line: None, // Cedar doesn't provide line numbers in error
        })?;

        // Validate against schema
        let validator = cedar_policy::Validator::new(schema.clone());
        let validation_result =
            validator.validate(&policies, cedar_policy::ValidationMode::default());

        if validation_result.validation_passed() {
            Ok(policies)
        } else {
            let errors: Vec<String> = validation_result
                .validation_errors()
                .map(|e| format!("{}", e))
                .collect();

            Err(PolicyError::SchemaValidation {
                details: errors.join("; "),
            })
        }
    }

    /// Reload policies from source.
    ///
    /// Implements: REQ-POL-001/F-005 (Hot-Reload)
    ///
    /// On success, atomically swaps in new policies.
    /// On failure, keeps old policies and returns error.
    pub fn reload(&self) -> Result<(), PolicyError> {
        info!("Reloading policies");

        let (policy_str, source) = loader::load_policies();
        let new_policies = Self::parse_policies(&policy_str, &self.schema)?;

        // Atomic swap
        self.policies.store(Arc::new(new_policies));
        self.source.store(Arc::new(source));
        self.stats.reload_count.fetch_add(1, Ordering::Relaxed);
        self.stats
            .last_reload
            .store(Arc::new(Some(std::time::SystemTime::now())));

        info!("Policies reloaded successfully");
        Ok(())
    }

    /// Get policy source information.
    ///
    /// Implements: REQ-POL-001/F-003 (Policy Loading)
    ///
    /// Returns information about where the currently loaded policies came from:
    /// ConfigMap, Environment variable, or embedded defaults.
    pub fn policy_source(&self) -> PolicySource {
        (**self.source.load()).clone()
    }

    /// Get policy statistics.
    ///
    /// Implements: REQ-POL-001/F-005 (Hot-Reload)
    ///
    /// Returns runtime statistics including policy count, evaluation count,
    /// reload count, and last reload timestamp.
    pub fn stats(&self) -> PolicyStats {
        let policies = self.policies.load();

        PolicyStats {
            policy_count: policies.policies().count(),
            last_reload: *self.stats.last_reload.load().as_ref(),
            reload_count: self.stats.reload_count.load(Ordering::Relaxed),
            evaluation_count: self.stats.evaluation_count.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Principal;
    use serial_test::serial;

    fn test_principal() -> Principal {
        Principal {
            app_name: "test-app".to_string(),
            namespace: "default".to_string(),
            service_account: "default".to_string(),
            roles: vec![],
        }
    }

    fn test_tool_call(name: &str) -> Resource {
        Resource::ToolCall {
            name: name.to_string(),
            server: "test-server".to_string(),
        }
    }

    #[test]
    #[serial]
    fn test_engine_creation() {
        let engine = CedarEngine::new();
        assert!(engine.is_ok());
    }

    #[test]
    #[serial]
    fn test_evaluate_with_default_policies() {
        let engine = CedarEngine::new().expect("Failed to create engine");

        let request = PolicyRequest {
            principal: test_principal(),
            resource: test_tool_call("test_tool"),
            context: None,
        };

        // Default policies permit all actions, so first check (Forward) should return Forward
        let action = engine.evaluate(&request);
        assert!(matches!(action, PolicyAction::Forward));
    }

    #[test]
    #[serial]
    fn test_stats() {
        let engine = CedarEngine::new().expect("Failed to create engine");

        let request = PolicyRequest {
            principal: test_principal(),
            resource: test_tool_call("test_tool"),
            context: None,
        };

        // Evaluate a few times
        engine.evaluate(&request);
        engine.evaluate(&request);
        engine.evaluate(&request);

        let stats = engine.stats();
        assert_eq!(stats.evaluation_count, 3);
    }

    // ═══════════════════════════════════════════════════════════
    // Edge Case Tests (EC-POL-001 to EC-POL-003) - v0.1 Simplified
    // ═══════════════════════════════════════════════════════════

    /// EC-POL-001: Forward permitted → Return Forward
    #[test]
    #[serial]
    fn test_ec_pol_001_forward_permitted() {
        // Create engine with custom policy that only permits Forward
        let policy_str = r#"
            permit(
                principal == ThoughtGate::App::"test-app",
                action == ThoughtGate::Action::"Forward",
                resource == ThoughtGate::ToolCall::"test_tool"
            );
        "#;

        unsafe {
            std::env::set_var("THOUGHTGATE_POLICIES", policy_str);
        }

        let engine = CedarEngine::new().expect("Failed to create engine");

        let request = PolicyRequest {
            principal: test_principal(),
            resource: test_tool_call("test_tool"),
            context: None,
        };

        let action = engine.evaluate(&request);
        assert!(matches!(action, PolicyAction::Forward));

        unsafe {
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    /// EC-POL-002: Only Approve permitted → Return Approve
    #[test]
    #[serial]
    fn test_ec_pol_002_approve_only() {
        let policy_str = r#"
            permit(
                principal == ThoughtGate::App::"test-app",
                action == ThoughtGate::Action::"Approve",
                resource == ThoughtGate::ToolCall::"test_tool"
            );
        "#;

        unsafe {
            std::env::set_var("THOUGHTGATE_POLICIES", policy_str);
        }

        let engine = CedarEngine::new().expect("Failed to create engine");

        let request = PolicyRequest {
            principal: test_principal(),
            resource: test_tool_call("test_tool"),
            context: None,
        };

        let action = engine.evaluate(&request);
        assert!(matches!(action, PolicyAction::Approve { .. }));

        unsafe {
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    /// EC-POL-003: No action permitted → Return Reject
    #[test]
    #[serial]
    fn test_ec_pol_003_no_action_permitted() {
        let policy_str = r#"
            // Permit a different principal, not test-app
            permit(
                principal == ThoughtGate::App::"other-app",
                action == ThoughtGate::Action::"Forward",
                resource == ThoughtGate::ToolCall::"test_tool"
            );
        "#;

        unsafe {
            std::env::set_var("THOUGHTGATE_POLICIES", policy_str);
        }

        let engine = CedarEngine::new().expect("Failed to create engine");

        let request = PolicyRequest {
            principal: test_principal(),
            resource: test_tool_call("test_tool"),
            context: None,
        };

        let action = engine.evaluate(&request);
        assert!(matches!(action, PolicyAction::Reject { .. }));

        unsafe {
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    /// EC-POL-007: Invalid policy syntax → Keep old policies
    #[test]
    #[serial]
    fn test_ec_pol_007_invalid_syntax() {
        // First create engine with valid policies
        let valid_policy = r#"
            permit(principal, action, resource);
        "#;

        unsafe {
            std::env::set_var("THOUGHTGATE_POLICIES", valid_policy);
        }

        let engine = CedarEngine::new().expect("Failed to create engine");

        // Verify it works
        let request = PolicyRequest {
            principal: test_principal(),
            resource: test_tool_call("test_tool"),
            context: None,
        };
        assert!(matches!(engine.evaluate(&request), PolicyAction::Forward));

        // Now try to reload with invalid syntax
        unsafe {
            std::env::set_var("THOUGHTGATE_POLICIES", "invalid syntax {{{");
        }

        let result = engine.reload();
        assert!(result.is_err());

        // Engine should still work with old policies
        assert!(matches!(engine.evaluate(&request), PolicyAction::Forward));

        unsafe {
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    /// EC-POL-008: Schema violation → Keep old policies
    #[test]
    #[serial]
    fn test_ec_pol_008_schema_violation() {
        // First create engine with valid policies
        let valid_policy = r#"
            permit(principal, action, resource);
        "#;

        unsafe {
            std::env::set_var("THOUGHTGATE_POLICIES", valid_policy);
        }

        let engine = CedarEngine::new().expect("Failed to create engine");

        // Try to reload with schema-violating policy
        let invalid_policy = r#"
            permit(
                principal == ThoughtGate::InvalidEntity::"test",
                action == ThoughtGate::Action::"Forward",
                resource
            );
        "#;

        unsafe {
            std::env::set_var("THOUGHTGATE_POLICIES", invalid_policy);
        }

        let result = engine.reload();
        assert!(result.is_err());

        // Engine should still work with old policies
        let request = PolicyRequest {
            principal: test_principal(),
            resource: test_tool_call("test_tool"),
            context: None,
        };
        assert!(matches!(engine.evaluate(&request), PolicyAction::Forward));

        unsafe {
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    /// EC-POL-009: Policy reload → Statistics updated
    #[test]
    #[serial]
    fn test_ec_pol_009_reload_updates_stats() {
        unsafe {
            std::env::set_var(
                "THOUGHTGATE_POLICIES",
                "permit(principal, action, resource);",
            );
        }

        let engine = CedarEngine::new().expect("Failed to create engine");

        let stats_before = engine.stats();
        assert_eq!(stats_before.reload_count, 0);

        // Reload policies
        let result = engine.reload();
        assert!(result.is_ok());

        let stats_after = engine.stats();
        assert_eq!(stats_after.reload_count, 1);

        unsafe {
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }
}
