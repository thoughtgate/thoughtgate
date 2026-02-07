//! Governance component factory.
//!
//! Creates the TaskHandler, CedarEngine, and optional ApprovalEngine
//! used by the MCP handler.

use std::sync::Arc;

use tracing::{debug, info};

use thoughtgate_core::config::Config;
use thoughtgate_core::error::ThoughtGateError;
use thoughtgate_core::governance::{
    ApprovalAdapter, ApprovalEngine, ApprovalEngineConfig, SlackAdapter, TaskHandler, TaskStore,
};
use thoughtgate_core::policy::engine::CedarEngine;
use thoughtgate_core::telemetry::ThoughtGateMetrics;
use thoughtgate_core::transport::upstream::UpstreamForwarder;
use tokio_util::sync::CancellationToken;

/// Create governance components with optional Prometheus metrics for gauge updates.
///
/// This variant accepts `ThoughtGateMetrics` to wire the `tasks_pending` gauge
/// (REQ-OBS-002 §6.4/MG-002).
///
/// # Arguments
///
/// * `upstream` - Upstream forwarder for approved requests
/// * `config` - Optional YAML config (enables Gate 1, 2, 4)
/// * `shutdown` - Cancellation token for graceful shutdown
/// * `tg_metrics` - Optional Prometheus metrics for gauge updates
///
/// # Returns
///
/// Tuple of (TaskHandler, CedarEngine, optional ApprovalEngine)
#[allow(clippy::type_complexity)]
pub async fn create_governance_components_with_metrics(
    upstream: Arc<dyn UpstreamForwarder>,
    config: Option<&Config>,
    shutdown: CancellationToken,
    tg_metrics: Option<Arc<ThoughtGateMetrics>>,
) -> Result<(TaskHandler, Arc<CedarEngine>, Option<Arc<ApprovalEngine>>), ThoughtGateError> {
    // Create task store with optional metrics wiring (REQ-OBS-002 §6.4/MG-002)
    let task_store = if let Some(ref metrics) = tg_metrics {
        Arc::new(TaskStore::with_metrics(
            thoughtgate_core::governance::task::TaskStoreConfig::default(),
            metrics.clone(),
        ))
    } else {
        Arc::new(TaskStore::with_defaults())
    };
    let task_handler = TaskHandler::new(task_store.clone());

    // Create Cedar policy engine (Gate 3) with optional metrics wiring (REQ-OBS-002 §6.4/MG-003)
    let cedar_engine = {
        let cedar_config = config.and_then(|c| c.cedar.as_ref());
        let engine = CedarEngine::new_with_config(cedar_config).map_err(|e| {
            ThoughtGateError::ServiceUnavailable {
                reason: format!("Failed to create Cedar engine: {}", e),
            }
        })?;
        if let Some(ref metrics) = tg_metrics {
            Arc::new(engine.with_metrics(metrics.clone()))
        } else {
            Arc::new(engine)
        }
    };

    // Create ApprovalEngine only if config uses approval rules (Gate 4)
    // This avoids requiring Slack credentials when approvals are not used
    let needs_approval = config
        .map(|c| c.requires_approval_engine())
        .unwrap_or(false);

    let approval_engine = if needs_approval {
        info!("Approval rules detected, initializing ApprovalEngine");

        // Create approval adapter based on environment
        let adapter: Arc<dyn ApprovalAdapter> =
            match std::env::var("THOUGHTGATE_APPROVAL_ADAPTER").as_deref() {
                Ok("mock") => {
                    // Use mock adapter for testing
                    Arc::new(thoughtgate_core::governance::approval::mock::MockAdapter::from_env())
                }
                _ => {
                    // Default to Slack adapter
                    let slack_config = thoughtgate_core::governance::SlackConfig::from_env()
                        .map_err(|e| ThoughtGateError::ServiceUnavailable {
                            reason: format!("Failed to create Slack config: {}", e),
                        })?;
                    let slack_adapter = SlackAdapter::new(slack_config).map_err(|e| {
                        ThoughtGateError::ServiceUnavailable {
                            reason: format!("Failed to create Slack adapter: {}", e),
                        }
                    })?;
                    Arc::new(slack_adapter)
                }
            };

        let engine_config = ApprovalEngineConfig::from_env();

        let engine = ApprovalEngine::new(
            task_store,
            adapter,
            upstream,
            cedar_engine.clone(),
            engine_config,
            shutdown,
        );

        // Wire metrics for task counters (REQ-OBS-002 §6.4/MC-007, MC-008)
        let engine = if let Some(ref metrics) = tg_metrics {
            engine.with_metrics(metrics.clone())
        } else {
            engine
        };

        // Spawn background polling loop for approval decisions
        // Implements: REQ-GOV-003/F-002, REQ-GOV-001/F-008
        engine.spawn_background_tasks().await;
        info!("ApprovalEngine background tasks started");

        Some(Arc::new(engine))
    } else {
        if config.is_some() {
            debug!("No approval rules detected, skipping ApprovalEngine initialization");
        }
        None
    };

    Ok((task_handler, cedar_engine, approval_engine))
}
