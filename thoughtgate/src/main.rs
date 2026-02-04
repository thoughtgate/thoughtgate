//! ThoughtGate CLI entry point.
//!
//! Implements: REQ-CORE-008 §6.1 (CLI Interface)
//!
//! Dispatches to `wrap` (config rewrite + governance + agent launch) or
//! `shim` (per-server stdio proxy) subcommands.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use thoughtgate_core::profile::Profile;

use thoughtgate::cli::{ShimArgs, WrapArgs};
use thoughtgate::shim::proxy::run_shim;
use thoughtgate::wrap;
use thoughtgate::wrap::config_adapter::ShimOptions;

// ─────────────────────────────────────────────────────────────────────────────
// CLI Definitions
// ─────────────────────────────────────────────────────────────────────────────

/// ThoughtGate: human-in-the-loop approval proxy for MCP agents.
#[derive(Parser)]
#[command(name = "thoughtgate", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Wrap an MCP agent: rewrite config, start governance, launch agent.
    Wrap(WrapArgs),
    /// Per-server stdio shim proxy (injected by `wrap`, not for direct use).
    Shim(ShimArgs),
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry Point
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let code = match cli.command {
        Commands::Wrap(args) => {
            init_tracing(args.verbose);
            match wrap::run_wrap(args).await {
                Ok(code) => code,
                Err(e) => {
                    tracing::error!(error = %e, "wrap failed");
                    eprintln!("thoughtgate wrap: {e}");
                    1
                }
            }
        }
        Commands::Shim(args) => {
            init_tracing(false);
            run_shim_from_args(args).await
        }
    };

    std::process::exit(code);
}

/// Run the shim proxy from parsed CLI args.
///
/// Converts `ShimArgs` to the `ShimOptions` expected by `run_shim`.
async fn run_shim_from_args(args: ShimArgs) -> i32 {
    let profile: Profile = args.profile.into();

    let opts = ShimOptions {
        server_id: args.server_id,
        governance_endpoint: args.governance_endpoint,
        profile,
        config_path: PathBuf::from("/dev/null"),
    };

    if args.command.is_empty() {
        eprintln!("thoughtgate shim: no server command specified");
        return 1;
    }

    let server_command = args.command[0].clone();
    let server_args = args.command[1..].to_vec();

    match run_shim(opts, None, server_command, server_args).await {
        Ok(code) => code,
        Err(e) => {
            tracing::error!(error = %e, "shim failed");
            eprintln!("thoughtgate shim: {e}");
            1
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tracing Init
// ─────────────────────────────────────────────────────────────────────────────

/// Initialise tracing subscriber with stderr output.
///
/// When `verbose` is true, sets filter to `debug`. Otherwise, respects
/// `RUST_LOG` environment variable (defaulting to no output).
fn init_tracing(verbose: bool) {
    use tracing_subscriber::EnvFilter;

    let filter = if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
