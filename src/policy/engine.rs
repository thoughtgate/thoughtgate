//! Cedar policy engine implementation.
//!
//! Implements: REQ-POL-001/F-001, F-005 (Policy Evaluation & Hot-Reload)
//!
//! # v0.2 Model
//!
//! Cedar is Gate 3 in the 4-Gate model. It returns Permit/Forbid only—
//! routing decisions are handled by YAML governance rules (Gate 2).
//!
//! The engine is invoked when a governance rule specifies `action: policy`.
//! The `policy_id` from the YAML rule is bound to `context.policy_id`.
//!
//! # v0.1 Compatibility
//!
//! The legacy `evaluate()` method returning `PolicyAction` is retained
//! for backward compatibility but marked deprecated.

#[allow(deprecated)] // v0.1 types needed for backward compatibility
use super::{
    PolicyAction, PolicyError, PolicyRequest, PolicySource, PolicyStats, Resource, loader,
    types::{
        CedarContext, CedarDecision, CedarRequest, CedarResource, CedarStats, PolicyAnnotations,
        PolicyInfo,
    },
};
use arc_swap::ArcSwap;
use cedar_policy::{
    Authorizer, Context, Decision, Entities, EntityId, EntityTypeName, EntityUid, PolicySet,
    Request, Schema,
};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Cedar policy engine for ThoughtGate.
///
/// Implements: REQ-POL-001 (Cedar Policy Engine)
///
/// # v0.2 Usage
///
/// ```ignore
/// let engine = CedarEngine::new()?;
///
/// let request = CedarRequest {
///     principal,
///     resource: CedarResource::ToolCall { name, server, arguments },
///     context: CedarContext { policy_id, source_id, time },
/// };
///
/// match engine.evaluate_v2(&request) {
///     CedarDecision::Permit { determining_policies } => {
///         // Continue to Gate 4
///     }
///     CedarDecision::Forbid { reason, .. } => {
///         // Return -32003 PolicyDenied
///     }
/// }
/// ```
pub struct CedarEngine {
    /// Cedar authorizer
    authorizer: Authorizer,

    /// Current policy set (atomic for hot-reload)
    policies: ArcSwap<PolicySet>,

    /// Cedar schema for validation
    schema: Schema,

    /// Policy annotations (cached at load time)
    annotations: ArcSwap<PolicyAnnotations>,

    /// Policy source information
    source: Arc<ArcSwap<PolicySource>>,

    /// v0.2 statistics counters
    stats_v2: Arc<StatsV2>,

    /// Legacy v0.1 statistics counters
    stats: Arc<Stats>,
}

/// v0.2 statistics.
struct StatsV2 {
    evaluation_count: AtomicU64,
    permit_count: AtomicU64,
    forbid_count: AtomicU64,
    total_eval_time_us: AtomicU64,
}

/// Legacy v0.1 statistics.
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

        // Parse annotations
        let annotations = Self::parse_annotations(&policies);

        info!(
            source = ?source,
            policy_count = policies.policies().count(),
            annotated_count = annotations.len(),
            "Cedar engine initialized"
        );

        Ok(Self {
            authorizer: Authorizer::new(),
            policies: ArcSwap::new(Arc::new(policies)),
            schema,
            annotations: ArcSwap::new(Arc::new(annotations)),
            source: Arc::new(ArcSwap::new(Arc::new(source))),
            stats_v2: Arc::new(StatsV2 {
                evaluation_count: AtomicU64::new(0),
                permit_count: AtomicU64::new(0),
                forbid_count: AtomicU64::new(0),
                total_eval_time_us: AtomicU64::new(0),
            }),
            stats: Arc::new(Stats {
                evaluation_count: AtomicU64::new(0),
                reload_count: AtomicU64::new(0),
                last_reload: arc_swap::ArcSwap::new(Arc::new(None)),
            }),
        })
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // v0.2 API (REQ-POL-001/F-001)
    // ═══════════════════════════════════════════════════════════════════════════

    /// Evaluate a Cedar request (v0.2 API).
    ///
    /// Implements: REQ-POL-001/F-001 (Policy Evaluation)
    ///
    /// Cedar returns Permit or Forbid only. If no policy permits the request,
    /// it is denied (default-deny).
    ///
    /// # Decision Logic
    ///
    /// 1. Build Cedar request with context (policy_id, time, arguments)
    /// 2. Evaluate against policy set
    /// 3. If ANY forbid matches → Forbid
    /// 4. If ANY permit matches (no forbid) → Permit
    /// 5. If NO policy matches → Forbid (default-deny)
    ///
    /// # Returns
    ///
    /// - `CedarDecision::Permit` - Continue to Gate 4
    /// - `CedarDecision::Forbid` - Deny with -32003 PolicyDenied
    // NOTE: This is synchronous CPU-bound work on the Tokio runtime.
    // Current benchmarks show p50 < 100µs for typical tool-level policies,
    // which is faster than the ~10-20µs context switch overhead of spawn_blocking.
    // If policy complexity increases and p99 exceeds 200µs, consider offloading
    // to tokio::task::spawn_blocking. See REQ-OBS-001 policy eval metrics.
    #[tracing::instrument(skip(self, request))]
    pub fn evaluate_v2(&self, request: &CedarRequest) -> CedarDecision {
        let start = std::time::Instant::now();
        self.stats_v2
            .evaluation_count
            .fetch_add(1, Ordering::Relaxed);

        let policies = self.policies.load();

        // Determine action based on resource type
        let action_name = match &request.resource {
            CedarResource::ToolCall { .. } => "tools/call",
            CedarResource::McpMethod { .. } => "mcp/method",
        };

        // Build and evaluate Cedar request
        let cedar_request = match self.build_cedar_request_v2(request, action_name) {
            Ok(req) => req,
            Err(e) => {
                error!(error = %e, "Failed to build Cedar request");
                self.stats_v2.forbid_count.fetch_add(1, Ordering::Relaxed);
                return CedarDecision::Forbid {
                    reason: format!("Failed to build request: {}", e),
                    policy_ids: vec![],
                };
            }
        };

        // Build entities with attributes for attribute-based policies
        let entities = match self.build_entities_v2(request) {
            Ok(e) => e,
            Err(e) => {
                error!(error = %e, "Failed to build entities");
                self.stats_v2.forbid_count.fetch_add(1, Ordering::Relaxed);
                return CedarDecision::Forbid {
                    reason: format!("Failed to build entities: {}", e),
                    policy_ids: vec![],
                };
            }
        };

        // Evaluate
        let response = self
            .authorizer
            .is_authorized(&cedar_request, &policies, &entities);

        let elapsed = start.elapsed();
        self.stats_v2
            .total_eval_time_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);

        // Convert response to CedarDecision
        match response.decision() {
            Decision::Allow => {
                self.stats_v2.permit_count.fetch_add(1, Ordering::Relaxed);

                // Extract determining policy IDs
                let determining_policies: Vec<String> = response
                    .diagnostics()
                    .reason()
                    .map(|id| id.to_string())
                    .collect();

                debug!(
                    principal = %request.principal.app_name,
                    resource = %request.resource.name(),
                    policy_id = %request.context.policy_id,
                    determining = ?determining_policies,
                    duration_us = elapsed.as_micros(),
                    "Cedar permit"
                );

                CedarDecision::Permit {
                    determining_policies,
                }
            }
            Decision::Deny => {
                self.stats_v2.forbid_count.fetch_add(1, Ordering::Relaxed);

                // Extract forbidding policy IDs
                let policy_ids: Vec<String> = response
                    .diagnostics()
                    .reason()
                    .map(|id| id.to_string())
                    .collect();

                // Build reason from errors if any
                let errors: Vec<String> = response
                    .diagnostics()
                    .errors()
                    .map(|e| e.to_string())
                    .collect();

                let reason = if errors.is_empty() {
                    if policy_ids.is_empty() {
                        "No policy permits this action (default-deny)".to_string()
                    } else {
                        format!("Forbidden by policy: {}", policy_ids.join(", "))
                    }
                } else {
                    format!("Policy evaluation errors: {}", errors.join("; "))
                };

                warn!(
                    principal = %request.principal.app_name,
                    resource = %request.resource.name(),
                    policy_id = %request.context.policy_id,
                    reason = %reason,
                    duration_us = elapsed.as_micros(),
                    "Cedar forbid"
                );

                CedarDecision::Forbid { reason, policy_ids }
            }
        }
    }

    /// Build Cedar request from v0.2 CedarRequest.
    ///
    /// Implements: REQ-POL-001/F-002 (Policy ID Binding), F-003 (Argument Inspection)
    fn build_cedar_request_v2(
        &self,
        request: &CedarRequest,
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
            CedarResource::ToolCall { name, .. } => ("ThoughtGate::ToolCall", name.clone()),
            CedarResource::McpMethod { method, .. } => ("ThoughtGate::McpMethod", method.clone()),
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

        // Build context with policy_id, source_id, and time
        let context = self.build_context_v2(&request.context)?;

        // Create Cedar request
        Request::new(principal_uid, action_uid, resource_uid, context, None).map_err(|e| {
            PolicyError::CedarError {
                details: format!("Failed to create Cedar request: {}", e),
            }
        })
    }

    /// Build Cedar context from CedarContext.
    ///
    /// Implements: REQ-POL-001/F-002 (Policy ID Binding), F-004 (Time-Based Rules)
    fn build_context_v2(&self, ctx: &CedarContext) -> Result<Context, PolicyError> {
        use cedar_policy::RestrictedExpression;

        // Build time context record
        let mut time_fields = HashMap::new();
        time_fields.insert(
            "hour".to_string(),
            RestrictedExpression::new_long(ctx.time.hour as i64),
        );
        time_fields.insert(
            "day_of_week".to_string(),
            RestrictedExpression::new_long(ctx.time.day_of_week as i64),
        );
        time_fields.insert(
            "timestamp".to_string(),
            RestrictedExpression::new_long(ctx.time.timestamp),
        );

        let time_record =
            RestrictedExpression::new_record(time_fields).map_err(|e| PolicyError::CedarError {
                details: format!("Failed to create time record: {}", e),
            })?;

        // Build main context record
        let mut context_fields = HashMap::new();
        context_fields.insert(
            "policy_id".to_string(),
            RestrictedExpression::new_string(ctx.policy_id.clone()),
        );
        context_fields.insert(
            "source_id".to_string(),
            RestrictedExpression::new_string(ctx.source_id.clone()),
        );
        context_fields.insert("time".to_string(), time_record);

        Context::from_pairs(context_fields.into_iter().collect::<Vec<_>>()).map_err(|e| {
            PolicyError::CedarError {
                details: format!("Failed to create context: {}", e),
            }
        })
    }

    /// Build entities for v0.2 evaluation.
    ///
    /// Implements: REQ-POL-001/F-003 (Argument Inspection), F-005 (Principal-Based Rules)
    fn build_entities_v2(&self, request: &CedarRequest) -> Result<Entities, PolicyError> {
        use cedar_policy::{Entity, RestrictedExpression};

        let mut entities = vec![];

        // Build role UIDs first (needed for principal parent membership)
        let mut role_uids = std::collections::HashSet::new();
        for role in &request.principal.roles {
            let role_uid = EntityUid::from_type_name_and_id(
                EntityTypeName::from_str("ThoughtGate::Role").map_err(|e| {
                    PolicyError::CedarError {
                        details: format!("Invalid role type: {}", e),
                    }
                })?,
                EntityId::from_str(role).map_err(|e| PolicyError::CedarError {
                    details: format!("Invalid role ID: {}", e),
                })?,
            );
            role_uids.insert(role_uid);
        }

        // Build principal entity with attributes and role membership
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

        let mut principal_attrs = HashMap::new();
        principal_attrs.insert(
            "name".to_string(),
            RestrictedExpression::new_string(request.principal.app_name.clone()),
        );
        principal_attrs.insert(
            "namespace".to_string(),
            RestrictedExpression::new_string(request.principal.namespace.clone()),
        );
        principal_attrs.insert(
            "service_account".to_string(),
            RestrictedExpression::new_string(request.principal.service_account.clone()),
        );

        // Include role UIDs as parents so `principal in Role::"admin"` checks work
        let principal_entity =
            Entity::new(principal_uid.clone(), principal_attrs, role_uids.clone()).map_err(
                |e| PolicyError::CedarError {
                    details: format!("Failed to create principal entity: {}", e),
                },
            )?;
        entities.push(principal_entity);

        // Build resource entity with attributes (including arguments for ToolCall)
        let (resource_type, resource_id, resource_attrs) = match &request.resource {
            CedarResource::ToolCall {
                name,
                server,
                arguments,
            } => {
                let mut attrs = HashMap::new();
                attrs.insert(
                    "name".to_string(),
                    RestrictedExpression::new_string(name.clone()),
                );
                attrs.insert(
                    "server".to_string(),
                    RestrictedExpression::new_string(server.clone()),
                );

                // Convert JSON arguments to Cedar record
                let arguments_expr = self.json_to_cedar_expr(arguments)?;
                attrs.insert("arguments".to_string(), arguments_expr);

                ("ThoughtGate::ToolCall", name.clone(), attrs)
            }
            CedarResource::McpMethod { method, server } => {
                let mut attrs = HashMap::new();
                attrs.insert(
                    "method".to_string(),
                    RestrictedExpression::new_string(method.clone()),
                );
                attrs.insert(
                    "server".to_string(),
                    RestrictedExpression::new_string(server.clone()),
                );
                ("ThoughtGate::McpMethod", method.clone(), attrs)
            }
        };

        let resource_uid = EntityUid::from_type_name_and_id(
            EntityTypeName::from_str(resource_type).map_err(|e| PolicyError::CedarError {
                details: format!("Invalid resource type: {}", e),
            })?,
            EntityId::from_str(&resource_id).map_err(|e| PolicyError::CedarError {
                details: format!("Invalid resource ID: {}", e),
            })?,
        );

        let resource_entity = Entity::new(
            resource_uid,
            resource_attrs,
            std::collections::HashSet::new(),
        )
        .map_err(|e| PolicyError::CedarError {
            details: format!("Failed to create resource entity: {}", e),
        })?;
        entities.push(resource_entity);

        // Add role entities (UIDs already built above)
        for role in &request.principal.roles {
            let role_uid = EntityUid::from_type_name_and_id(
                EntityTypeName::from_str("ThoughtGate::Role").map_err(|e| {
                    PolicyError::CedarError {
                        details: format!("Invalid role type: {}", e),
                    }
                })?,
                EntityId::from_str(role).map_err(|e| PolicyError::CedarError {
                    details: format!("Invalid role ID: {}", e),
                })?,
            );

            let mut role_attrs = HashMap::new();
            role_attrs.insert(
                "name".to_string(),
                RestrictedExpression::new_string(role.clone()),
            );

            let role_entity = Entity::new(role_uid, role_attrs, std::collections::HashSet::new())
                .map_err(|e| PolicyError::CedarError {
                details: format!("Failed to create role entity: {}", e),
            })?;
            entities.push(role_entity);
        }

        Entities::from_entities(entities, None).map_err(|e| PolicyError::CedarError {
            details: format!("Failed to create entities: {}", e),
        })
    }

    /// Convert JSON value to Cedar expression.
    ///
    /// Implements: REQ-POL-001/F-003 (Argument Inspection)
    fn json_to_cedar_expr(
        &self,
        value: &serde_json::Value,
    ) -> Result<cedar_policy::RestrictedExpression, PolicyError> {
        use cedar_policy::RestrictedExpression;

        match value {
            serde_json::Value::Null => {
                // Cedar doesn't have null, use empty record
                RestrictedExpression::new_record(HashMap::new()).map_err(|e| {
                    PolicyError::CedarError {
                        details: format!("Failed to create empty record for null: {}", e),
                    }
                })
            }
            serde_json::Value::Bool(b) => Ok(RestrictedExpression::new_bool(*b)),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(RestrictedExpression::new_long(i))
                } else if let Some(f) = n.as_f64() {
                    // Cedar doesn't have floats - round to nearest integer
                    let rounded = f.round() as i64;
                    if (f - rounded as f64).abs() > f64::EPSILON {
                        warn!(
                            original = f,
                            rounded = rounded,
                            "Float value rounded to integer for Cedar"
                        );
                    }
                    Ok(RestrictedExpression::new_long(rounded))
                } else {
                    Err(PolicyError::CedarError {
                        details: "Number too large for Cedar".to_string(),
                    })
                }
            }
            serde_json::Value::String(s) => Ok(RestrictedExpression::new_string(s.clone())),
            serde_json::Value::Array(arr) => {
                let exprs: Result<Vec<_>, _> =
                    arr.iter().map(|v| self.json_to_cedar_expr(v)).collect();
                Ok(RestrictedExpression::new_set(exprs?))
            }
            serde_json::Value::Object(obj) => {
                let fields: Result<HashMap<_, _>, _> = obj
                    .iter()
                    .map(|(k, v)| self.json_to_cedar_expr(v).map(|e| (k.clone(), e)))
                    .collect();
                RestrictedExpression::new_record(fields?).map_err(|e| PolicyError::CedarError {
                    details: format!("Failed to create record: {}", e),
                })
            }
        }
    }

    /// Get policy annotations.
    ///
    /// Implements: REQ-POL-001/§8.0 (Policy Annotations)
    pub fn annotations(&self) -> Arc<PolicyAnnotations> {
        self.annotations.load_full()
    }

    /// Get v0.2 statistics.
    ///
    /// Implements: REQ-POL-001/§6.3 (CedarStats)
    pub fn stats_v2(&self) -> CedarStats {
        let eval_count = self.stats_v2.evaluation_count.load(Ordering::Relaxed);
        let total_time = self.stats_v2.total_eval_time_us.load(Ordering::Relaxed);

        CedarStats {
            evaluation_count: eval_count,
            permit_count: self.stats_v2.permit_count.load(Ordering::Relaxed),
            forbid_count: self.stats_v2.forbid_count.load(Ordering::Relaxed),
            avg_eval_time_us: if eval_count > 0 {
                total_time / eval_count
            } else {
                0
            },
        }
    }

    /// Get v0.2 policy info.
    ///
    /// Implements: REQ-POL-001/§6.3 (PolicyInfo)
    pub fn policy_info(&self) -> PolicyInfo {
        let policies = self.policies.load();
        let annotations = self.annotations.load();

        PolicyInfo {
            paths: vec![], // TODO: Track paths from loader
            policy_count: policies.policies().count(),
            last_reload: *self.stats.last_reload.load().as_ref(),
            annotated_policy_count: annotations.len(),
        }
    }

    /// Parse policy annotations at load time.
    ///
    /// Implements: REQ-POL-001/§8.0 (Using Policy Annotations for Workflow Routing)
    fn parse_annotations(policies: &PolicySet) -> PolicyAnnotations {
        let mut annotations = PolicyAnnotations::new();

        for policy in policies.policies() {
            // Check for @thoughtgate_approval annotation
            if let Some(workflow) = policy.annotation("thoughtgate_approval") {
                let policy_id = policy.id().to_string();
                // Remove quotes from the annotation value if present
                let workflow = workflow.trim_matches('"').to_string();
                annotations.add_workflow(policy_id, workflow);
            }
        }

        annotations
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // v0.1 Legacy API (Deprecated)
    // ═══════════════════════════════════════════════════════════════════════════

    /// Evaluate a policy request (v0.1 API).
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
    ///
    /// # Deprecated
    /// Use `evaluate_v2()` for v0.2 Cedar evaluation.
    #[deprecated(since = "0.2.0", note = "Use evaluate_v2() for v0.2 Cedar evaluation")]
    #[allow(deprecated)]
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

    /// Check if a specific action is permitted (v0.1).
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

    /// Build Cedar request from PolicyRequest (v0.1).
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
        let new_annotations = Self::parse_annotations(&new_policies);

        // Atomic swap
        self.policies.store(Arc::new(new_policies));
        self.annotations.store(Arc::new(new_annotations));
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

    /// Get policy statistics (v0.1).
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
    use crate::policy::{Principal, TimeContext};
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

    // ═══════════════════════════════════════════════════════════════════════════
    // v0.2 Tests
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    #[serial]
    fn test_evaluate_v2_permit() {
        // Set up a permit-all policy for tools/call
        let policy = r#"
            permit(
                principal,
                action == ThoughtGate::Action::"tools/call",
                resource
            );
        "#;

        unsafe {
            std::env::set_var("THOUGHTGATE_POLICIES", policy);
        }

        let engine = CedarEngine::new().expect("Failed to create engine");

        let request = CedarRequest {
            principal: test_principal(),
            resource: CedarResource::ToolCall {
                name: "test_tool".to_string(),
                server: "test-server".to_string(),
                arguments: serde_json::json!({}),
            },
            context: CedarContext {
                policy_id: "test_policy".to_string(),
                source_id: "test-server".to_string(),
                time: TimeContext::from_timestamp(0),
            },
        };

        let decision = engine.evaluate_v2(&request);
        assert!(decision.is_permit());

        unsafe {
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    #[test]
    #[serial]
    fn test_evaluate_v2_forbid_default_deny() {
        // Set up a policy that doesn't match anything
        let policy = r#"
            permit(
                principal == ThoughtGate::App::"other-app",
                action == ThoughtGate::Action::"tools/call",
                resource
            );
        "#;

        unsafe {
            std::env::set_var("THOUGHTGATE_POLICIES", policy);
        }

        let engine = CedarEngine::new().expect("Failed to create engine");

        let request = CedarRequest {
            principal: test_principal(),
            resource: CedarResource::ToolCall {
                name: "test_tool".to_string(),
                server: "test-server".to_string(),
                arguments: serde_json::json!({}),
            },
            context: CedarContext {
                policy_id: "test_policy".to_string(),
                source_id: "test-server".to_string(),
                time: TimeContext::from_timestamp(0),
            },
        };

        let decision = engine.evaluate_v2(&request);
        assert!(decision.is_forbid());

        unsafe {
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    #[test]
    #[serial]
    fn test_evaluate_v2_stats() {
        let policy = r#"
            permit(principal, action == ThoughtGate::Action::"tools/call", resource);
        "#;

        unsafe {
            std::env::set_var("THOUGHTGATE_POLICIES", policy);
        }

        let engine = CedarEngine::new().expect("Failed to create engine");

        let request = CedarRequest {
            principal: test_principal(),
            resource: CedarResource::ToolCall {
                name: "test_tool".to_string(),
                server: "test-server".to_string(),
                arguments: serde_json::json!({}),
            },
            context: CedarContext {
                policy_id: "test_policy".to_string(),
                source_id: "test-server".to_string(),
                time: TimeContext::from_timestamp(0),
            },
        };

        // Evaluate multiple times
        engine.evaluate_v2(&request);
        engine.evaluate_v2(&request);
        engine.evaluate_v2(&request);

        let stats = engine.stats_v2();
        assert_eq!(stats.evaluation_count, 3);
        assert_eq!(stats.permit_count, 3);
        assert_eq!(stats.forbid_count, 0);

        unsafe {
            std::env::remove_var("THOUGHTGATE_POLICIES");
        }
    }

    #[test]
    fn test_time_context_construction() {
        let time = TimeContext::from_timestamp(1705329000);
        assert_eq!(time.hour, 14);
        assert_eq!(time.day_of_week, 1); // Monday
    }

    #[test]
    fn test_json_to_cedar_expr() {
        let engine = CedarEngine::new().expect("Failed to create engine");

        // Test various JSON types
        let json = serde_json::json!({
            "amount": 5000,
            "currency": "USD",
            "enabled": true,
            "items": [1, 2, 3]
        });

        let result = engine.json_to_cedar_expr(&json);
        assert!(result.is_ok());
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // v0.1 Legacy Tests
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    #[serial]
    fn test_engine_creation() {
        let engine = CedarEngine::new();
        assert!(engine.is_ok());
    }

    #[test]
    #[serial]
    #[allow(deprecated)]
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
    #[allow(deprecated)]
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

    // ═══════════════════════════════════════════════════════════════════════════
    // Edge Case Tests (EC-POL-001 to EC-POL-003) - v0.1 Simplified
    // ═══════════════════════════════════════════════════════════════════════════

    /// EC-POL-001: Forward permitted → Return Forward
    #[test]
    #[serial]
    #[allow(deprecated)]
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
    #[allow(deprecated)]
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
    #[allow(deprecated)]
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
    #[allow(deprecated)]
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
    #[allow(deprecated)]
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
