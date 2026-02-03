//! CLI argument types for `thoughtgate wrap` and `thoughtgate shim`.
//!
//! Implements: REQ-CORE-008 §6.1 (CLI Interface)
//!
//! These types are defined separately from `main.rs` so that integration tests
//! can construct them directly for testing the wrap orchestration.

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use thoughtgate_core::profile::Profile;

use crate::wrap::config_adapter::AgentType;

// ─────────────────────────────────────────────────────────────────────────────
// Wrap Subcommand Args
// ─────────────────────────────────────────────────────────────────────────────

/// Arguments for `thoughtgate wrap`.
///
/// Wraps an MCP agent: discovers config, rewrites server commands to use shim
/// proxies, starts the governance service, launches the agent, and restores
/// config on exit.
///
/// Implements: REQ-CORE-008 §6.1
#[derive(Args, Debug)]
pub struct WrapArgs {
    /// Override auto-detected agent type.
    #[arg(long, value_enum)]
    pub agent_type: Option<CliAgentType>,

    /// Override auto-detected config file path.
    #[arg(long)]
    pub config_path: Option<PathBuf>,

    /// Configuration profile.
    #[arg(long, value_enum, default_value = "production")]
    pub profile: CliProfile,

    /// ThoughtGate config file.
    #[arg(long, default_value = "thoughtgate.yaml")]
    pub thoughtgate_config: PathBuf,

    /// Port for governance service (0 = OS-assigned ephemeral).
    #[arg(long, default_value_t = 0)]
    pub governance_port: u16,

    /// Don't restore original config on exit.
    #[arg(long)]
    pub no_restore: bool,

    /// Print rewritten config diff without writing.
    #[arg(long)]
    pub dry_run: bool,

    /// Enable debug logging.
    #[arg(long)]
    pub verbose: bool,

    /// Agent command and arguments (after `--`).
    #[arg(trailing_var_arg = true, required = true)]
    pub command: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Shim Subcommand Args
// ─────────────────────────────────────────────────────────────────────────────

/// Arguments for `thoughtgate shim`.
///
/// Per-server stdio shim proxy. Not intended for direct user invocation —
/// injected into MCP config by `thoughtgate wrap`.
///
/// Implements: REQ-CORE-008 §6.1
#[derive(Args, Debug)]
pub struct ShimArgs {
    /// Server identifier (from config rewrite).
    #[arg(long)]
    pub server_id: String,

    /// ThoughtGate governance service URL.
    #[arg(long)]
    pub governance_endpoint: String,

    /// Configuration profile.
    #[arg(long, value_enum)]
    pub profile: CliProfile,

    /// Server command and arguments (after `--`).
    #[arg(trailing_var_arg = true, required = true)]
    pub command: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Value Enums (clap-compatible)
// ─────────────────────────────────────────────────────────────────────────────

/// CLI-level profile selection.
///
/// Maps 1:1 to `thoughtgate_core::profile::Profile`.
///
/// Implements: REQ-CORE-008/F-022
#[derive(Clone, Debug, ValueEnum)]
pub enum CliProfile {
    /// All governance decisions are enforcing.
    Production,
    /// Governance decisions are logged but not enforced.
    Development,
}

/// CLI-level agent type selection.
///
/// Maps 1:1 to `crate::wrap::config_adapter::AgentType`.
///
/// Implements: REQ-CORE-008/F-001
#[derive(Clone, Debug, ValueEnum)]
pub enum CliAgentType {
    /// Claude Desktop (macOS/Linux app).
    ClaudeDesktop,
    /// Claude Code CLI.
    ClaudeCode,
    /// Cursor editor.
    Cursor,
    /// Visual Studio Code.
    Vscode,
    /// Windsurf (Codeium).
    Windsurf,
    /// Zed editor.
    Zed,
    /// User-specified custom agent.
    Custom,
}

// ─────────────────────────────────────────────────────────────────────────────
// Conversions
// ─────────────────────────────────────────────────────────────────────────────

impl From<CliProfile> for Profile {
    fn from(p: CliProfile) -> Self {
        match p {
            CliProfile::Production => Profile::Production,
            CliProfile::Development => Profile::Development,
        }
    }
}

impl From<CliAgentType> for AgentType {
    fn from(a: CliAgentType) -> Self {
        match a {
            CliAgentType::ClaudeDesktop => AgentType::ClaudeDesktop,
            CliAgentType::ClaudeCode => AgentType::ClaudeCode,
            CliAgentType::Cursor => AgentType::Cursor,
            CliAgentType::Vscode => AgentType::VsCode,
            CliAgentType::Windsurf => AgentType::Windsurf,
            CliAgentType::Zed => AgentType::Zed,
            CliAgentType::Custom => AgentType::Custom,
        }
    }
}
