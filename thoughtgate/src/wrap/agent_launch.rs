//! Wrap orchestration: config discovery → rewrite → governance → agent launch.
//!
//! Implements: REQ-CORE-008/F-008 (Governance Service Startup),
//!             F-009 (Agent Process Launch), F-010 (Wait for Exit),
//!             F-019 (Graceful Shutdown — Wrap), F-020 (Config Restoration Safety)
//!
//! This module provides the main `run_wrap` function that orchestrates the full
//! wrap lifecycle: detect agent type, discover and rewrite config, start the
//! governance HTTP service, launch the agent process, handle signals, and
//! restore config on exit.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use thoughtgate_core::governance::approval::{
    ApprovalAdapter, MockAdapter, PollingConfig, PollingScheduler, SlackAdapter, SlackConfig,
};
use thoughtgate_core::governance::engine::{ApprovalEngine, ApprovalEngineConfig};
use thoughtgate_core::governance::evaluator::GovernanceEvaluator;
use thoughtgate_core::governance::service::{GovernanceServiceState, start_governance_service};
use thoughtgate_core::governance::task::{Principal, TaskStore};
use thoughtgate_core::policy::engine::CedarEngine;
use thoughtgate_core::profile::Profile;
use thoughtgate_core::transport::NoopUpstreamForwarder;

use crate::cli::WrapArgs;
use crate::error::StdioError;
use crate::wrap::config_adapter::{
    AgentType, ClaudeCodeAdapter, ClaudeDesktopAdapter, ConfigAdapter, ConfigError, CursorAdapter,
    McpServerEntry, ShimOptions, VsCodeAdapter, WindsurfAdapter, ZedAdapter, detect_agent_type,
};
use crate::wrap::config_guard::ConfigGuard;

// ─────────────────────────────────────────────────────────────────────────────
// Panic Restore Global (F-020)
// ─────────────────────────────────────────────────────────────────────────────

/// Global storage for backup/config paths used by the panic hook.
///
/// The panic hook performs a raw `std::fs::copy(backup, config)` without
/// acquiring any locks — this minimises panic-in-panic risk. The value is
/// cleared in the normal cleanup path.
///
/// Implements: REQ-CORE-008/F-020
static PANIC_RESTORE: OnceLock<Mutex<Option<(PathBuf, PathBuf)>>> = OnceLock::new();

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum time to wait for the agent to exit after a signal (seconds).
const SIGNAL_SHUTDOWN_TIMEOUT_SECS: u64 = 10;

/// Maximum time to wait for the governance service to finish (seconds).
const GOVERNANCE_DRAIN_TIMEOUT_SECS: u64 = 10;

// ─────────────────────────────────────────────────────────────────────────────
// Adapter Factory
// ─────────────────────────────────────────────────────────────────────────────

/// Create a config adapter for the given agent type.
///
/// `AgentType::Custom` falls back to the Claude Desktop adapter (generic
/// `mcpServers` structure).
///
/// Implements: REQ-CORE-008/F-001
fn adapter_for_agent_type(agent_type: AgentType) -> Box<dyn ConfigAdapter> {
    match agent_type {
        AgentType::ClaudeDesktop => Box::new(ClaudeDesktopAdapter),
        AgentType::ClaudeCode => Box::new(ClaudeCodeAdapter),
        AgentType::Cursor => Box::new(CursorAdapter),
        AgentType::VsCode => Box::new(VsCodeAdapter),
        AgentType::Windsurf => Box::new(WindsurfAdapter),
        AgentType::Zed => Box::new(ZedAdapter),
        AgentType::Custom => Box::new(ClaudeDesktopAdapter),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve the path to the current binary (used as the shim binary in config
/// rewrites).
///
/// Primary: `current_exe()` → `canonicalize()` to resolve symlinks.
/// Fallback: `which::which("thoughtgate")` for PATH-based discovery.
///
/// Implements: REQ-CORE-008/F-007
fn resolve_shim_binary() -> Result<PathBuf, StdioError> {
    // Primary: current_exe() → canonicalize() to resolve symlinks.
    if let Ok(exe) = std::env::current_exe() {
        match std::fs::canonicalize(&exe) {
            Ok(canonical) => return Ok(canonical),
            Err(e) => {
                tracing::debug!(
                    exe = %exe.display(),
                    error = %e,
                    "canonicalize failed, trying which fallback"
                );
            }
        }
    }

    // Fallback: which::which("thoughtgate").
    which::which("thoughtgate").map_err(|e| {
        StdioError::StdioIo(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("could not resolve thoughtgate binary: {e}"),
        ))
    })
}

/// Convert a `ConfigError` to a `StdioError`, attaching the config path for
/// context.
fn config_err_to_stdio(e: ConfigError, config_path: &Path) -> StdioError {
    match e {
        ConfigError::NotFound { path } => StdioError::ConfigNotFound { path },
        ConfigError::Locked => StdioError::ConfigLocked {
            path: config_path.to_path_buf(),
        },
        ConfigError::Parse { reason } => StdioError::ConfigParseError {
            path: config_path.to_path_buf(),
            reason,
        },
        ConfigError::Write { reason } => StdioError::ConfigWriteError {
            path: config_path.to_path_buf(),
            reason,
        },
        ConfigError::AlreadyManaged => StdioError::ConfigParseError {
            path: config_path.to_path_buf(),
            reason: "config already managed by ThoughtGate".to_string(),
        },
        ConfigError::NoServers => StdioError::ConfigParseError {
            path: config_path.to_path_buf(),
            reason: "no stdio MCP servers found".to_string(),
        },
        ConfigError::UndefinedEnvVar { name } => StdioError::ConfigParseError {
            path: config_path.to_path_buf(),
            reason: format!("undefined environment variable: {name}"),
        },
        ConfigError::Io(e) => StdioError::StdioIo(e),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Dry-Run
// ─────────────────────────────────────────────────────────────────────────────

/// Print the rewritten config without modifying the real file.
///
/// Copies config to a temp directory, rewrites the copy, prints the result,
/// and cleans up.
///
/// Implements: REQ-CORE-008 §6.1 (`--dry-run`)
fn run_dry_run(
    adapter: &dyn ConfigAdapter,
    config_path: &Path,
    servers: &[McpServerEntry],
    shim_binary: &Path,
    profile: Profile,
) -> Result<i32, StdioError> {
    let original = std::fs::read_to_string(config_path).map_err(StdioError::StdioIo)?;

    // Create temp dir and copy config there for isolated rewrite.
    let temp_dir = std::env::temp_dir().join("thoughtgate-dry-run");
    std::fs::create_dir_all(&temp_dir).map_err(StdioError::StdioIo)?;
    let temp_config = temp_dir.join("config.json");
    std::fs::copy(config_path, &temp_config).map_err(StdioError::StdioIo)?;

    let shim_options = ShimOptions {
        server_id: String::new(),
        governance_endpoint: "http://127.0.0.1:0".to_string(),
        profile,
    };

    let _backup = adapter
        .rewrite_config(&temp_config, servers, shim_binary, &shim_options)
        .map_err(|e| config_err_to_stdio(e, config_path))?;

    let rewritten = std::fs::read_to_string(&temp_config).map_err(StdioError::StdioIo)?;

    let diff = similar::TextDiff::from_lines(&original, &rewritten);
    print!(
        "{}",
        diff.unified_diff().context_radius(3).header(
            &format!("original: {}", config_path.display()),
            "rewritten (dry-run)",
        )
    );

    // Clean up temp directory (best-effort).
    let _ = std::fs::remove_dir_all(&temp_dir);

    Ok(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Signal Shutdown (F-019)
// ─────────────────────────────────────────────────────────────────────────────

/// Handle signal-initiated shutdown: trigger governance shutdown, wait for
/// agent to exit, kill if needed.
///
/// Implements: REQ-CORE-008/F-019 (Graceful Shutdown — Wrap)
async fn handle_signal_shutdown(
    child: &mut tokio::process::Child,
    gov_state: &GovernanceServiceState,
    governance_endpoint: &str,
) -> i32 {
    // Step 1: Signal governance shutdown (in-process).
    gov_state.trigger_shutdown();

    // Step 2: POST /governance/shutdown for any external consumers.
    let client = reqwest::Client::new();
    let _ = client
        .post(format!("{governance_endpoint}/governance/shutdown"))
        .send()
        .await;

    // Step 3: Wait for agent to exit (it may handle the signal itself).
    match tokio::time::timeout(
        Duration::from_secs(SIGNAL_SHUTDOWN_TIMEOUT_SECS),
        child.wait(),
    )
    .await
    {
        Ok(Ok(status)) => status.code().unwrap_or(1),
        Ok(Err(_)) => 1,
        Err(_) => {
            // Agent didn't exit within timeout — kill it.
            tracing::warn!("agent did not exit within timeout, killing");
            let _ = child.kill().await;
            child
                .wait()
                .await
                .map(|s| s.code().unwrap_or(1))
                .unwrap_or(1)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Main Orchestration (F-008, F-009, F-010)
// ─────────────────────────────────────────────────────────────────────────────

/// Run the `thoughtgate wrap` orchestration.
///
/// Full lifecycle: detect agent → discover config → parse servers → rewrite
/// config → start governance service → install signal handlers → spawn agent
/// → wait for exit → cleanup → restore config.
///
/// # Arguments
///
/// * `args` - Parsed CLI arguments from `WrapArgs`.
///
/// # Returns
///
/// The agent's exit code (propagated to process exit).
///
/// Implements: REQ-CORE-008/F-008, F-009, F-010, F-019, F-020
pub async fn run_wrap(args: WrapArgs) -> Result<i32, StdioError> {
    // ── F-001: Resolve agent type ──────────────────────────────────────────
    let agent_type = if let Some(ref cli_type) = args.agent_type {
        cli_type.clone().into()
    } else {
        let agent_cmd = args
            .command
            .first()
            .ok_or_else(|| StdioError::UnknownAgentType {
                command: String::new(),
            })?;
        detect_agent_type(agent_cmd).ok_or_else(|| StdioError::UnknownAgentType {
            command: agent_cmd.clone(),
        })?
    };

    tracing::info!(?agent_type, "resolved agent type");

    // ── Get config adapter ─────────────────────────────────────────────────
    let adapter = adapter_for_agent_type(agent_type);

    // ── F-002: Discover config path ────────────────────────────────────────
    let config_path = if let Some(ref path) = args.config_path {
        if !path.exists() {
            return Err(StdioError::ConfigNotFound { path: path.clone() });
        }
        path.clone()
    } else {
        adapter
            .discover_config_path()
            .ok_or_else(|| StdioError::ConfigNotFound {
                path: PathBuf::from("<auto-detect failed>"),
            })?
    };

    tracing::info!(config_path = %config_path.display(), "discovered config path");

    // ── F-003: Parse servers ───────────────────────────────────────────────
    let servers = adapter
        .parse_servers(&config_path)
        .map_err(|e| config_err_to_stdio(e, &config_path))?;

    tracing::info!(
        server_count = servers.len(),
        "parsed MCP server entries from config"
    );

    // ── F-007: Resolve shim binary ─────────────────────────────────────────
    let shim_binary = resolve_shim_binary()?;
    tracing::debug!(shim_binary = %shim_binary.display(), "resolved shim binary path");

    // ── Profile ────────────────────────────────────────────────────────────
    let profile: Profile = args.profile.clone().into();

    // ── Dry-run branch ─────────────────────────────────────────────────────
    if args.dry_run {
        return run_dry_run(&*adapter, &config_path, &servers, &shim_binary, profile);
    }

    // ── Load ThoughtGate config (if provided) ──────────────────────────────
    let tg_config = if args.thoughtgate_config.as_os_str().is_empty() {
        None
    } else if args.thoughtgate_config.exists() {
        match thoughtgate_core::config::load_and_validate(
            &args.thoughtgate_config,
            thoughtgate_core::config::Version::V0_2,
        ) {
            Ok((config, warnings)) => {
                if !warnings.is_clean() {
                    tracing::warn!(
                        warnings = ?warnings,
                        "config validation warnings"
                    );
                }
                tracing::info!(
                    config_path = %args.thoughtgate_config.display(),
                    "loaded ThoughtGate config"
                );
                Some(Arc::new(config))
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    config_path = %args.thoughtgate_config.display(),
                    "failed to load ThoughtGate config — governance will use stub mode"
                );
                None
            }
        }
    } else {
        tracing::debug!(
            config_path = %args.thoughtgate_config.display(),
            "ThoughtGate config not found — governance will use stub mode"
        );
        None
    };

    // ── Create governance evaluator (if config loaded) ───────────────────
    let (gov_state, scheduler_state) =
        setup_governance(tg_config, profile, &args.thoughtgate_config).await?;

    // ── F-008: Start governance service ────────────────────────────────────
    let (gov_port, gov_handle) = start_governance_service(args.governance_port, gov_state.clone())
        .await
        .map_err(StdioError::StdioIo)?;

    // Validate port before constructing URL (defensive check for edge case)
    if gov_port == 0 {
        return Err(StdioError::ServerSpawnError {
            server_id: "governance".to_string(),
            reason: "governance service bound to port 0".to_string(),
        });
    }
    let governance_endpoint = format!("http://127.0.0.1:{gov_port}");

    tracing::info!(
        gov_port,
        "governance service started on 127.0.0.1:{gov_port}"
    );

    // ── Acquire config lock before rewriting ─────────────────────────────
    // Lock must be acquired *before* rewrite_config to prevent a TOCTOU race
    // where two concurrent instances both read the original config, then
    // overwrite each other's rewrites.
    let mut guard =
        ConfigGuard::lock(&config_path).map_err(|e| config_err_to_stdio(e, &config_path))?;

    // ── F-005/F-006: Rewrite config (under lock) ─────────────────────────
    let shim_options = ShimOptions {
        server_id: String::new(), // per-server; rewrite sets each server_id
        governance_endpoint: governance_endpoint.clone(),
        profile,
    };

    let backup_path = adapter
        .rewrite_config(&config_path, &servers, &shim_binary, &shim_options)
        .map_err(|e| config_err_to_stdio(e, &config_path))?;

    // Now that the backup exists, enable restore-on-drop.
    guard.set_backup(&backup_path);

    tracing::info!(
        backup_path = %backup_path.display(),
        "config rewritten under lock, backup created"
    );

    // If --no-restore, disable the guard's automatic restore on drop.
    if args.no_restore {
        guard.skip_restore();
        tracing::info!("--no-restore set, config will NOT be restored on exit");
    }

    // ── F-020: Install panic hook for config restoration safety ────────────
    PANIC_RESTORE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .replace((backup_path.clone(), config_path.clone()));

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(lock) = PANIC_RESTORE.get() {
            if let Ok(paths) = lock.lock() {
                if let Some((backup, config)) = paths.as_ref() {
                    let _ = std::fs::copy(backup, config);
                }
            }
        }
        // Use write! instead of eprintln! to avoid double-panic if stderr
        // is closed (eprintln! panics on write failure).
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("thoughtgate panicked: {info}\n"),
        );
        prev_hook(info);
    }));

    // ── F-019: Register signal handlers BEFORE spawning agent ──────────────
    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(StdioError::StdioIo)?;

    #[cfg(unix)]
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(StdioError::StdioIo)?;

    // ── F-009: Spawn agent process ─────────────────────────────────────────
    let agent_command = &args.command[0];
    let agent_args = &args.command[1..];

    tracing::info!(agent_command, ?agent_args, "spawning agent process");

    let mut child = tokio::process::Command::new(agent_command)
        .args(agent_args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .env("THOUGHTGATE_ACTIVE", "1")
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| StdioError::ServerSpawnError {
            server_id: "agent".to_string(),
            reason: e.to_string(),
        })?;

    tracing::info!("agent process spawned, waiting for exit");

    // ── F-010: Wait for agent exit with signal handling ────────────────────
    let exit_code;

    #[cfg(unix)]
    {
        tokio::select! {
            status = child.wait() => {
                let status = status.map_err(StdioError::StdioIo)?;
                exit_code = status.code().unwrap_or(1);
                tracing::info!(exit_code, "agent exited");
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, initiating shutdown");
                exit_code = handle_signal_shutdown(
                    &mut child, &gov_state, &governance_endpoint,
                ).await;
            }
            _ = sigint.recv() => {
                tracing::info!("received SIGINT, initiating shutdown");
                exit_code = handle_signal_shutdown(
                    &mut child, &gov_state, &governance_endpoint,
                ).await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let status = child.wait().await.map_err(StdioError::StdioIo)?;
        exit_code = status.code().unwrap_or(1);
        tracing::info!(exit_code, "agent exited");
    }

    // ── F-010: Cleanup ─────────────────────────────────────────────────────
    wrap_cleanup(scheduler_state, &gov_state, gov_handle).await;

    // Explicit restore (belt and suspenders — Drop also calls restore).
    if !args.no_restore {
        if let Err(e) = guard.restore() {
            tracing::error!(error = %e, "failed to restore config");
        }
    }
    drop(guard);

    // Clear panic hook restore paths.
    if let Some(lock) = PANIC_RESTORE.get() {
        if let Ok(mut paths) = lock.lock() {
            *paths = None;
        }
    }

    tracing::info!(exit_code, "wrap complete");
    Ok(exit_code)
}

/// Create the governance evaluator and optional approval engine/scheduler.
///
/// Implements: REQ-CORE-008/F-008 (Governance Service Startup)
async fn setup_governance(
    tg_config: Option<Arc<thoughtgate_core::config::Config>>,
    profile: Profile,
    config_path: &Path,
) -> Result<(Arc<GovernanceServiceState>, Option<CancellationToken>), StdioError> {
    let config = match tg_config {
        Some(c) => c,
        None => return Ok((Arc::new(GovernanceServiceState::new()), None)),
    };

    // Infer principal with graceful fallback for non-K8s environments.
    let policy_principal =
        tokio::task::spawn_blocking(thoughtgate_core::policy::principal::infer_principal)
            .await
            .ok()
            .and_then(|r| r.ok());

    let principal = match policy_principal {
        Some(p) => Principal::from_policy(
            &p.app_name,
            &p.namespace,
            &p.service_account,
            p.roles.clone(),
        ),
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "anonymous".to_string());
            tracing::info!(
                user = %user,
                "principal inference failed — using local fallback"
            );
            Principal::from_policy(&user, "local", "local", vec![])
        }
    };

    let cedar_engine = match CedarEngine::new() {
        Ok(engine) => {
            tracing::info!("Cedar policy engine initialized");
            Some(Arc::new(engine))
        }
        Err(e) => {
            tracing::info!(
                error = %e,
                "Cedar engine not available — Cedar policy rules will use fallback"
            );
            None
        }
    };

    let task_store = Arc::new(TaskStore::with_defaults());

    let mut evaluator = GovernanceEvaluator::new(
        config.clone(),
        cedar_engine.clone(),
        task_store.clone(),
        principal,
        profile,
    );

    let scheduler_state = if config.requires_approval_engine() {
        let cancel_token = CancellationToken::new();

        let adapter: Arc<dyn ApprovalAdapter> = match SlackConfig::from_env()
            .and_then(SlackAdapter::new)
        {
            Ok(adapter) => {
                tracing::info!("Slack adapter initialized for approval polling");
                Arc::new(adapter)
            }
            Err(e) => {
                if profile == Profile::Production {
                    return Err(StdioError::ConfigParseError {
                        path: config_path.to_path_buf(),
                        reason: format!(
                            "approval adapter required in production but failed to initialize: {e}"
                        ),
                    });
                }
                tracing::info!(
                    error = %e,
                    "Slack adapter unavailable — using mock adapter (auto-approve in development)"
                );
                Arc::new(MockAdapter::new(Duration::from_secs(1), true))
            }
        };

        if let Some(ref cedar) = cedar_engine {
            let noop_upstream = Arc::new(NoopUpstreamForwarder);
            let engine = ApprovalEngine::new(
                task_store.clone(),
                adapter,
                noop_upstream,
                cedar.clone(),
                ApprovalEngineConfig::from_env(),
                cancel_token.clone(),
            );
            engine.spawn_background_tasks().await;
            evaluator = evaluator.with_approval_engine(Arc::new(engine));
            tracing::info!("approval engine started (with Cedar pipeline)");
        } else {
            let scheduler = Arc::new(PollingScheduler::new(
                adapter,
                task_store.clone(),
                PollingConfig::default(),
                cancel_token.clone(),
            ));
            evaluator.set_scheduler(scheduler.clone());
            tokio::spawn({
                let scheduler = scheduler.clone();
                async move {
                    scheduler.run().await;
                }
            });
            tracing::info!("approval polling scheduler started (legacy, no Cedar)");
        }

        Some(cancel_token)
    } else {
        tracing::debug!("no approval workflows configured — scheduler not started");
        None
    };

    let evaluator = Arc::new(evaluator);
    let state = Arc::new(GovernanceServiceState::with_evaluator(
        evaluator, task_store,
    ));

    Ok((state, scheduler_state))
}

/// Stop scheduler and shut down governance service.
///
/// Implements: REQ-CORE-008/F-010 (Cleanup)
async fn wrap_cleanup(
    scheduler_state: Option<CancellationToken>,
    gov_state: &GovernanceServiceState,
    gov_handle: tokio::task::JoinHandle<Result<(), std::io::Error>>,
) {
    if let Some(cancel_token) = scheduler_state {
        cancel_token.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;
        tracing::debug!("approval scheduler stopped");
    }

    gov_state.trigger_shutdown();
    tracing::info!("governance shutdown triggered");

    let _ = tokio::time::timeout(
        Duration::from_secs(GOVERNANCE_DRAIN_TIMEOUT_SECS),
        gov_handle,
    )
    .await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_for_claude_desktop() {
        let adapter = adapter_for_agent_type(AgentType::ClaudeDesktop);
        assert_eq!(adapter.agent_type(), AgentType::ClaudeDesktop);
    }

    #[test]
    fn test_adapter_for_claude_code() {
        let adapter = adapter_for_agent_type(AgentType::ClaudeCode);
        assert_eq!(adapter.agent_type(), AgentType::ClaudeCode);
    }

    #[test]
    fn test_adapter_for_cursor() {
        let adapter = adapter_for_agent_type(AgentType::Cursor);
        assert_eq!(adapter.agent_type(), AgentType::Cursor);
    }

    #[test]
    fn test_adapter_for_vscode() {
        let adapter = adapter_for_agent_type(AgentType::VsCode);
        assert_eq!(adapter.agent_type(), AgentType::VsCode);
    }

    #[test]
    fn test_adapter_for_windsurf() {
        let adapter = adapter_for_agent_type(AgentType::Windsurf);
        assert_eq!(adapter.agent_type(), AgentType::Windsurf);
    }

    #[test]
    fn test_adapter_for_zed() {
        let adapter = adapter_for_agent_type(AgentType::Zed);
        assert_eq!(adapter.agent_type(), AgentType::Zed);
    }

    #[test]
    fn test_adapter_for_custom_falls_back_to_claude_desktop() {
        let adapter = adapter_for_agent_type(AgentType::Custom);
        // Custom uses ClaudeDesktopAdapter as fallback.
        assert_eq!(adapter.agent_type(), AgentType::ClaudeDesktop);
    }

    #[test]
    fn test_resolve_shim_binary_returns_path() {
        let path = resolve_shim_binary();
        assert!(path.is_ok(), "should resolve current exe path");
        let path = path.unwrap();
        assert!(!path.to_string_lossy().is_empty());
    }

    /// F-007: Resolved path is absolute (canonicalize guarantees this).
    #[test]
    fn test_resolve_shim_binary_returns_canonical_path() {
        let path = resolve_shim_binary();
        assert!(path.is_ok());
        let path = path.unwrap();
        assert!(path.is_absolute());
    }

    #[test]
    fn test_config_err_not_found() {
        let err = config_err_to_stdio(
            ConfigError::NotFound {
                path: PathBuf::from("/tmp/missing.json"),
            },
            Path::new("/tmp/config.json"),
        );
        match err {
            StdioError::ConfigNotFound { path } => {
                assert_eq!(path, PathBuf::from("/tmp/missing.json"));
            }
            other => panic!("expected ConfigNotFound, got: {other:?}"),
        }
    }

    #[test]
    fn test_config_err_locked() {
        let err = config_err_to_stdio(ConfigError::Locked, Path::new("/tmp/config.json"));
        match err {
            StdioError::ConfigLocked { path } => {
                assert_eq!(path, PathBuf::from("/tmp/config.json"));
            }
            other => panic!("expected ConfigLocked, got: {other:?}"),
        }
    }

    #[test]
    fn test_config_err_parse() {
        let err = config_err_to_stdio(
            ConfigError::Parse {
                reason: "bad json".to_string(),
            },
            Path::new("/tmp/c.json"),
        );
        match err {
            StdioError::ConfigParseError { reason, .. } => {
                assert_eq!(reason, "bad json");
            }
            other => panic!("expected ConfigParseError, got: {other:?}"),
        }
    }

    #[test]
    fn test_config_err_write() {
        let err = config_err_to_stdio(
            ConfigError::Write {
                reason: "disk full".to_string(),
            },
            Path::new("/tmp/c.json"),
        );
        match err {
            StdioError::ConfigWriteError { reason, .. } => {
                assert_eq!(reason, "disk full");
            }
            other => panic!("expected ConfigWriteError, got: {other:?}"),
        }
    }

    #[test]
    fn test_config_err_already_managed() {
        let err = config_err_to_stdio(ConfigError::AlreadyManaged, Path::new("/tmp/config.json"));
        match err {
            StdioError::ConfigParseError { reason, .. } => {
                assert!(reason.contains("already managed"));
            }
            other => panic!("expected ConfigParseError, got: {other:?}"),
        }
    }

    #[test]
    fn test_config_err_no_servers() {
        let err = config_err_to_stdio(ConfigError::NoServers, Path::new("/tmp/config.json"));
        match err {
            StdioError::ConfigParseError { reason, .. } => {
                assert!(reason.contains("no stdio MCP servers"));
            }
            other => panic!("expected ConfigParseError, got: {other:?}"),
        }
    }

    #[test]
    fn test_config_err_undefined_env_var() {
        let err = config_err_to_stdio(
            ConfigError::UndefinedEnvVar {
                name: "FOO".to_string(),
            },
            Path::new("/tmp/c.json"),
        );
        match err {
            StdioError::ConfigParseError { reason, .. } => {
                assert!(reason.contains("FOO"));
            }
            other => panic!("expected ConfigParseError, got: {other:?}"),
        }
    }
}
