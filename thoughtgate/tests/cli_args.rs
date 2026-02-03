//! CLI argument parsing tests.
//!
//! Tests that WrapArgs and ShimArgs parse correctly from command-line strings
//! and that conversion impls (CliProfile → Profile, CliAgentType → AgentType)
//! produce the expected values.

use clap::{Parser, Subcommand};

use thoughtgate::cli::{CliAgentType, CliProfile, ShimArgs, WrapArgs};
use thoughtgate::wrap::config_adapter::AgentType;
use thoughtgate_core::profile::Profile;

// ─────────────────────────────────────────────────────────────────────────────
// Test Harness
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal CLI parser that mirrors main.rs's Cli, usable from integration tests.
#[derive(Parser)]
#[command(name = "thoughtgate")]
struct TestCli {
    #[command(subcommand)]
    command: TestCommands,
}

#[derive(Subcommand)]
enum TestCommands {
    Wrap(WrapArgs),
    Shim(ShimArgs),
}

/// Parse a command-line string into TestCli.
fn parse(args: &[&str]) -> Result<TestCli, clap::Error> {
    TestCli::try_parse_from(args)
}

// ─────────────────────────────────────────────────────────────────────────────
// WrapArgs Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_wrap_basic_defaults() {
    let cli = parse(&["thoughtgate", "wrap", "--", "claude-code"]).unwrap();
    match cli.command {
        TestCommands::Wrap(args) => {
            assert!(args.agent_type.is_none());
            assert!(args.config_path.is_none());
            assert!(matches!(args.profile, CliProfile::Production));
            assert_eq!(args.governance_port, 0);
            assert!(!args.no_restore);
            assert!(!args.dry_run);
            assert!(!args.verbose);
            assert_eq!(args.command, vec!["claude-code"]);
        }
        _ => panic!("expected Wrap command"),
    }
}

#[test]
fn test_wrap_all_options() {
    let cli = parse(&[
        "thoughtgate",
        "wrap",
        "--agent-type",
        "cursor",
        "--config-path",
        "/tmp/config.json",
        "--profile",
        "development",
        "--governance-port",
        "9090",
        "--no-restore",
        "--dry-run",
        "--verbose",
        "--",
        "cursor",
        "--flag",
        "value",
    ])
    .unwrap();

    match cli.command {
        TestCommands::Wrap(args) => {
            assert!(matches!(args.agent_type, Some(CliAgentType::Cursor)));
            assert_eq!(
                args.config_path.as_deref(),
                Some(std::path::Path::new("/tmp/config.json"))
            );
            assert!(matches!(args.profile, CliProfile::Development));
            assert_eq!(args.governance_port, 9090);
            assert!(args.no_restore);
            assert!(args.dry_run);
            assert!(args.verbose);
            assert_eq!(args.command, vec!["cursor", "--flag", "value"]);
        }
        _ => panic!("expected Wrap command"),
    }
}

#[test]
fn test_wrap_requires_command() {
    let result = parse(&["thoughtgate", "wrap"]);
    assert!(result.is_err(), "wrap without command should fail");
}

#[test]
fn test_wrap_agent_type_claude_desktop() {
    let cli = parse(&[
        "thoughtgate",
        "wrap",
        "--agent-type",
        "claude-desktop",
        "--",
        "claude",
    ])
    .unwrap();
    match cli.command {
        TestCommands::Wrap(args) => {
            assert!(matches!(args.agent_type, Some(CliAgentType::ClaudeDesktop)));
        }
        _ => panic!("expected Wrap command"),
    }
}

#[test]
fn test_wrap_agent_type_all_variants() {
    let variants = [
        ("claude-desktop", "ClaudeDesktop"),
        ("claude-code", "ClaudeCode"),
        ("cursor", "Cursor"),
        ("vscode", "Vscode"),
        ("windsurf", "Windsurf"),
        ("zed", "Zed"),
        ("custom", "Custom"),
    ];

    for (cli_val, _name) in &variants {
        let cli = parse(&[
            "thoughtgate",
            "wrap",
            "--agent-type",
            cli_val,
            "--",
            "agent",
        ]);
        assert!(
            cli.is_ok(),
            "agent type '{cli_val}' should parse successfully"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ShimArgs Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_shim_basic() {
    let cli = parse(&[
        "thoughtgate",
        "shim",
        "--server-id",
        "filesystem",
        "--governance-endpoint",
        "http://127.0.0.1:9090",
        "--profile",
        "production",
        "--",
        "node",
        "server.js",
    ])
    .unwrap();

    match cli.command {
        TestCommands::Shim(args) => {
            assert_eq!(args.server_id, "filesystem");
            assert_eq!(args.governance_endpoint, "http://127.0.0.1:9090");
            assert!(matches!(args.profile, CliProfile::Production));
            assert_eq!(args.command, vec!["node", "server.js"]);
        }
        _ => panic!("expected Shim command"),
    }
}

#[test]
fn test_shim_development_profile() {
    let cli = parse(&[
        "thoughtgate",
        "shim",
        "--server-id",
        "test",
        "--governance-endpoint",
        "http://127.0.0.1:8080",
        "--profile",
        "development",
        "--",
        "cat",
    ])
    .unwrap();

    match cli.command {
        TestCommands::Shim(args) => {
            assert!(matches!(args.profile, CliProfile::Development));
        }
        _ => panic!("expected Shim command"),
    }
}

#[test]
fn test_shim_requires_server_id() {
    let result = parse(&[
        "thoughtgate",
        "shim",
        "--governance-endpoint",
        "http://127.0.0.1:9090",
        "--profile",
        "production",
        "--",
        "cat",
    ]);
    assert!(result.is_err(), "shim without --server-id should fail");
}

#[test]
fn test_shim_requires_governance_endpoint() {
    let result = parse(&[
        "thoughtgate",
        "shim",
        "--server-id",
        "test",
        "--profile",
        "production",
        "--",
        "cat",
    ]);
    assert!(
        result.is_err(),
        "shim without --governance-endpoint should fail"
    );
}

#[test]
fn test_shim_requires_command() {
    let result = parse(&[
        "thoughtgate",
        "shim",
        "--server-id",
        "test",
        "--governance-endpoint",
        "http://127.0.0.1:9090",
        "--profile",
        "production",
    ]);
    assert!(result.is_err(), "shim without command should fail");
}

// ─────────────────────────────────────────────────────────────────────────────
// Conversion Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_cli_profile_to_profile_production() {
    let profile: Profile = CliProfile::Production.into();
    assert_eq!(profile, Profile::Production);
}

#[test]
fn test_cli_profile_to_profile_development() {
    let profile: Profile = CliProfile::Development.into();
    assert_eq!(profile, Profile::Development);
}

#[test]
fn test_cli_agent_type_to_agent_type() {
    assert_eq!(
        AgentType::from(CliAgentType::ClaudeDesktop),
        AgentType::ClaudeDesktop
    );
    assert_eq!(
        AgentType::from(CliAgentType::ClaudeCode),
        AgentType::ClaudeCode
    );
    assert_eq!(AgentType::from(CliAgentType::Cursor), AgentType::Cursor);
    assert_eq!(AgentType::from(CliAgentType::Vscode), AgentType::VsCode);
    assert_eq!(AgentType::from(CliAgentType::Windsurf), AgentType::Windsurf);
    assert_eq!(AgentType::from(CliAgentType::Zed), AgentType::Zed);
    assert_eq!(AgentType::from(CliAgentType::Custom), AgentType::Custom);
}
