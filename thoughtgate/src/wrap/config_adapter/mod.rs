//! Config adapter trait and agent-specific implementations.
//!
//! Implements: REQ-CORE-008 §6.2 (ConfigAdapter), §6.3 (McpServerEntry),
//!             §6.4 (ShimOptions), F-001 (Agent Detection), F-002 (Path Discovery),
//!             F-003 (Config Parsing), F-004 (Env Var Expansion), F-005 (Backup),
//!             F-006 (Command Replacement), F-007 (Shim Resolution)
//!
//! This module provides the `ConfigAdapter` trait and implementations for each
//! supported MCP agent: Claude Desktop, Claude Code, Cursor, VS Code, Windsurf,
//! and Zed.

mod adapters;
mod env;
mod helpers;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thoughtgate_core::profile::Profile;

// Re-export all adapter structs and trait.
pub use adapters::{
    ClaudeCodeAdapter, ClaudeDesktopAdapter, CursorAdapter, VsCodeAdapter, WindsurfAdapter,
    ZedAdapter,
};

// ─────────────────────────────────────────────────────────────────────────────
// Types & Enums
// ─────────────────────────────────────────────────────────────────────────────

/// Supported MCP agent types.
///
/// Implements: REQ-CORE-008/F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentType {
    /// Claude Desktop (macOS/Linux app).
    ClaudeDesktop,
    /// Claude Code CLI (`claude` or `claude-code`).
    ClaudeCode,
    /// Cursor editor.
    Cursor,
    /// Visual Studio Code or VS Code Insiders.
    VsCode,
    /// Windsurf (Codeium).
    Windsurf,
    /// Zed editor.
    Zed,
    /// User-specified custom agent.
    Custom,
}

/// A single MCP server entry parsed from agent config.
///
/// Implements: REQ-CORE-008 §6.3
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    /// Server identifier (key in the config map).
    pub id: String,
    /// Original command to spawn the server.
    pub command: String,
    /// Original arguments.
    pub args: Vec<String>,
    /// Environment variables to set for the server process.
    pub env: Option<HashMap<String, String>>,
    /// Whether this server is enabled.
    pub enabled: bool,
}

/// Configuration passed from `wrap` to each `shim` instance.
///
/// Implements: REQ-CORE-008 §6.4
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShimOptions {
    /// Server identifier.
    pub server_id: String,
    /// URL of the ThoughtGate governance service.
    pub governance_endpoint: String,
    /// Active configuration profile.
    pub profile: Profile,
}

/// Errors that can occur during config operations.
///
/// Implements: REQ-CORE-008 §6.2
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Config file not found at expected location.
    #[error("Config file not found: {path}")]
    NotFound {
        /// The path that was checked.
        path: PathBuf,
    },
    /// Config file could not be parsed.
    #[error("Failed to parse config: {reason}")]
    Parse {
        /// Human-readable description of the parse failure.
        reason: String,
    },
    /// Config file could not be written.
    #[error("Failed to write config: {reason}")]
    Write {
        /// Human-readable description of the write failure.
        reason: String,
    },
    /// Config file is locked by another ThoughtGate instance.
    #[error("Config file is locked by another ThoughtGate instance")]
    Locked,
    /// Config already rewritten by ThoughtGate (double-wrap detection).
    #[error(
        "Config already managed by ThoughtGate — run the agent directly or restore with `thoughtgate unwrap`"
    )]
    AlreadyManaged,
    /// No stdio MCP servers found in config.
    #[error("No stdio MCP servers found in config")]
    NoServers,
    /// Environment variable referenced in config is not defined.
    #[error("Undefined environment variable: {name}")]
    UndefinedEnvVar {
        /// The variable name that was not found.
        name: String,
    },
    /// Underlying IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// ─────────────────────────────────────────────────────────────────────────────
// ConfigAdapter Trait
// ─────────────────────────────────────────────────────────────────────────────

/// Trait for agent-specific config file handling.
///
/// Each supported agent implements this trait to handle its unique config file
/// location, JSON structure, and key naming conventions.
///
/// Implements: REQ-CORE-008 §6.2
pub trait ConfigAdapter: Send + Sync {
    /// Returns the agent type identifier.
    fn agent_type(&self) -> AgentType;

    /// Discovers the config file path for this agent.
    ///
    /// Returns `None` if config file not found at expected location.
    ///
    /// Implements: REQ-CORE-008/F-002
    fn discover_config_path(&self) -> Option<PathBuf>;

    /// Parses MCP server entries from the config file.
    ///
    /// Implements: REQ-CORE-008/F-003
    fn parse_servers(&self, config_path: &Path) -> Result<Vec<McpServerEntry>, ConfigError>;

    /// Rewrites the config file, replacing server commands with shim proxies.
    ///
    /// Returns the backup file path.
    ///
    /// Implements: REQ-CORE-008/F-005, F-006
    fn rewrite_config(
        &self,
        config_path: &Path,
        servers: &[McpServerEntry],
        shim_binary: &Path,
        options: &ShimOptions,
    ) -> Result<PathBuf, ConfigError>;

    /// Restores the original config from backup.
    fn restore_config(&self, config_path: &Path, backup_path: &Path) -> Result<(), ConfigError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Agent Type Detection (F-001)
// ─────────────────────────────────────────────────────────────────────────────

/// Detect agent type from the command name or path.
///
/// Extracts the basename (final path component) and matches against known
/// agent binary names. Returns `None` if no pattern matches.
///
/// Implements: REQ-CORE-008/F-001
pub fn detect_agent_type(command: &str) -> Option<AgentType> {
    let path = Path::new(command);
    let basename = path.file_name()?.to_str()?;

    // Claude Desktop: basename contains "Claude" AND path contains ".app"
    if basename.contains("Claude") && command.contains(".app") {
        return Some(AgentType::ClaudeDesktop);
    }

    match basename {
        "claude-code" | "claude" => Some(AgentType::ClaudeCode),
        "claude-desktop" => Some(AgentType::ClaudeDesktop),
        "cursor" => Some(AgentType::Cursor),
        "code" | "code-insiders" => Some(AgentType::VsCode),
        "windsurf" => Some(AgentType::Windsurf),
        "zed" => Some(AgentType::Zed),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Environment Variable Expansion (F-004) — public wrappers
// ─────────────────────────────────────────────────────────────────────────────

/// Expand `${VAR}` and `${VAR:-default}` references in a string (Claude Code style).
///
/// Scans for `${...}` patterns and replaces them with the corresponding
/// environment variable value. Supports default values with `:-` syntax:
/// - `${VAR}` - Expands to value of VAR, errors if not set
/// - `${VAR:-default}` - Expands to VAR if set, otherwise uses "default"
///
/// Implements: REQ-CORE-008/F-004
pub fn expand_dollar_brace(input: &str) -> Result<String, ConfigError> {
    env::expand_vars(input, false)
}

/// Expand `${env:VAR}` references in a string (Windsurf style).
///
/// Scans for `${env:...}` patterns and replaces them with the corresponding
/// environment variable value. Plain `${VAR}` patterns without the `env:`
/// prefix are left unchanged.
///
/// Implements: REQ-CORE-008/F-004
pub fn expand_env_colon(input: &str) -> Result<String, ConfigError> {
    env::expand_vars(input, true)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Re-import helpers for the test that calls standard_rewrite directly.
    use helpers::standard_rewrite;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn temp_config_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thoughtgate-test-{}", uuid_v4_simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Simple pseudo-UUID for test isolation (avoids adding uuid dep).
    fn uuid_v4_simple() -> String {
        use std::time::SystemTime;
        let d = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        format!("{}-{}", d.as_secs(), d.subsec_nanos())
    }

    // ── Agent Detection Tests ────────────────────────────────────────────

    #[test]
    fn test_detect_claude_code() {
        assert_eq!(
            detect_agent_type("claude-code"),
            Some(AgentType::ClaudeCode)
        );
    }

    #[test]
    fn test_detect_claude_standalone() {
        assert_eq!(detect_agent_type("claude"), Some(AgentType::ClaudeCode));
    }

    #[test]
    fn test_detect_claude_desktop_macos() {
        assert_eq!(
            detect_agent_type("/Applications/Claude.app/Contents/MacOS/Claude"),
            Some(AgentType::ClaudeDesktop)
        );
    }

    #[test]
    fn test_detect_cursor() {
        assert_eq!(detect_agent_type("cursor"), Some(AgentType::Cursor));
    }

    #[test]
    fn test_detect_vscode() {
        assert_eq!(detect_agent_type("code"), Some(AgentType::VsCode));
    }

    #[test]
    fn test_detect_vscode_insiders() {
        assert_eq!(detect_agent_type("code-insiders"), Some(AgentType::VsCode));
    }

    #[test]
    fn test_detect_windsurf() {
        assert_eq!(detect_agent_type("windsurf"), Some(AgentType::Windsurf));
    }

    #[test]
    fn test_detect_zed() {
        assert_eq!(detect_agent_type("zed"), Some(AgentType::Zed));
    }

    #[test]
    fn test_detect_unknown() {
        assert_eq!(detect_agent_type("my-custom-agent"), None);
    }

    #[test]
    fn test_detect_with_path_prefix() {
        assert_eq!(
            detect_agent_type("/usr/local/bin/claude-code"),
            Some(AgentType::ClaudeCode)
        );
    }

    // ── Config Parsing Tests ─────────────────────────────────────────────

    #[test]
    fn test_parse_claude_desktop_config() {
        let adapter = ClaudeDesktopAdapter;
        let path = fixture_path("claude_desktop_config.json");
        let servers = adapter.parse_servers(&path).unwrap();

        assert_eq!(servers.len(), 2);

        let fs_server = servers.iter().find(|s| s.id == "filesystem").unwrap();
        assert_eq!(fs_server.command, "npx");
        assert_eq!(
            fs_server.args,
            vec!["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        );
        assert!(fs_server.env.is_some());
        assert_eq!(
            fs_server.env.as_ref().unwrap().get("NODE_ENV").unwrap(),
            "production"
        );
        assert!(fs_server.enabled);

        let gh_server = servers.iter().find(|s| s.id == "github").unwrap();
        assert_eq!(gh_server.command, "npx");
    }

    #[test]
    fn test_parse_vscode_config() {
        let adapter = VsCodeAdapter;
        let path = fixture_path("vscode_mcp_config.json");
        let servers = adapter.parse_servers(&path).unwrap();

        // Only stdio servers should be returned; SSE server filtered out.
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, "filesystem");
        assert_eq!(servers[0].command, "npx");
    }

    #[test]
    fn test_parse_cursor_config() {
        let adapter = CursorAdapter;
        let path = fixture_path("cursor_mcp_config.json");
        let servers = adapter.parse_servers(&path).unwrap();

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, "sqlite");
        assert_eq!(servers[0].command, "uvx");
        assert_eq!(
            servers[0].args,
            vec!["mcp-server-sqlite", "--db-path", "/tmp/test.db"]
        );
    }

    #[test]
    fn test_parse_windsurf_config() {
        let adapter = WindsurfAdapter;
        let path = fixture_path("windsurf_mcp_config.json");
        let servers = adapter.parse_servers(&path).unwrap();

        assert_eq!(servers.len(), 2);

        let fs_server = servers.iter().find(|s| s.id == "filesystem").unwrap();
        assert_eq!(fs_server.command, "npx");
        assert_eq!(
            fs_server.args,
            vec![
                "-y",
                "@modelcontextprotocol/server-filesystem",
                "/home/user"
            ]
        );
        assert!(fs_server.enabled);

        let db_server = servers.iter().find(|s| s.id == "database").unwrap();
        assert_eq!(db_server.command, "uvx");
    }

    #[test]
    fn test_parse_zed_config() {
        let adapter = ZedAdapter;
        let path = fixture_path("zed_settings.json");
        let servers = adapter.parse_servers(&path).unwrap();

        assert_eq!(servers.len(), 2);

        // Structured command with path + args.
        let fs_server = servers.iter().find(|s| s.id == "filesystem").unwrap();
        assert_eq!(fs_server.command, "npx");
        assert_eq!(
            fs_server.args,
            vec!["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        );

        // Simple string command with no args.
        let simple = servers.iter().find(|s| s.id == "simple-server").unwrap();
        assert_eq!(simple.command, "mcp-server-simple");
        assert!(simple.args.is_empty());
    }

    // ── Config Rewrite Tests ─────────────────────────────────────────────

    #[test]
    fn test_rewrite_produces_valid_json() {
        let dir = temp_config_dir();
        let config_path = dir.join("config.json");
        let fixture = std::fs::read_to_string(fixture_path("claude_desktop_config.json")).unwrap();
        std::fs::write(&config_path, &fixture).unwrap();

        let adapter = ClaudeDesktopAdapter;
        let servers = adapter.parse_servers(&config_path).unwrap();
        let shim_binary = PathBuf::from("/usr/local/bin/thoughtgate");
        let options = ShimOptions {
            server_id: String::new(),
            governance_endpoint: "http://127.0.0.1:19090".to_string(),
            profile: Profile::Production,
        };

        let backup = adapter
            .rewrite_config(&config_path, &servers, &shim_binary, &options)
            .unwrap();

        // Verify rewritten config is valid JSON.
        let rewritten = std::fs::read_to_string(&config_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert!(parsed.get("mcpServers").is_some());

        // Verify backup exists.
        assert!(backup.exists());

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rewrite_preserves_env() {
        let dir = temp_config_dir();
        let config_path = dir.join("config.json");
        let fixture = std::fs::read_to_string(fixture_path("claude_desktop_config.json")).unwrap();
        std::fs::write(&config_path, &fixture).unwrap();

        let adapter = ClaudeDesktopAdapter;
        let servers = adapter.parse_servers(&config_path).unwrap();
        let shim_binary = PathBuf::from("/usr/local/bin/thoughtgate");
        let options = ShimOptions {
            server_id: String::new(),
            governance_endpoint: "http://127.0.0.1:19090".to_string(),
            profile: Profile::Production,
        };

        adapter
            .rewrite_config(&config_path, &servers, &shim_binary, &options)
            .unwrap();

        let rewritten: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        let fs_env = rewritten["mcpServers"]["filesystem"]["env"]
            .as_object()
            .unwrap();

        // Original env preserved.
        assert_eq!(
            fs_env.get("NODE_ENV").unwrap().as_str().unwrap(),
            "production"
        );
        // THOUGHTGATE_SERVER_ID added.
        assert_eq!(
            fs_env
                .get("THOUGHTGATE_SERVER_ID")
                .unwrap()
                .as_str()
                .unwrap(),
            "filesystem"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rewrite_shim_args_structure() {
        let dir = temp_config_dir();
        let config_path = dir.join("config.json");
        let fixture = std::fs::read_to_string(fixture_path("claude_desktop_config.json")).unwrap();
        std::fs::write(&config_path, &fixture).unwrap();

        let adapter = ClaudeDesktopAdapter;
        let servers = adapter.parse_servers(&config_path).unwrap();
        let shim_binary = PathBuf::from("/usr/local/bin/thoughtgate");
        let options = ShimOptions {
            server_id: String::new(),
            governance_endpoint: "http://127.0.0.1:19090".to_string(),
            profile: Profile::Production,
        };

        adapter
            .rewrite_config(&config_path, &servers, &shim_binary, &options)
            .unwrap();

        let rewritten: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();

        let fs_entry = &rewritten["mcpServers"]["filesystem"];
        assert_eq!(
            fs_entry["command"].as_str().unwrap(),
            "/usr/local/bin/thoughtgate"
        );

        let args: Vec<&str> = fs_entry["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            args,
            vec![
                "shim",
                "--server-id",
                "filesystem",
                "--governance-endpoint",
                "http://127.0.0.1:19090",
                "--profile",
                "production",
                "--",
                "npx",
                "-y",
                "@modelcontextprotocol/server-filesystem",
                "/tmp",
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_zed_rewrite_injects_env_without_existing_env() {
        let dir = temp_config_dir();
        let config_path = dir.join("settings.json");
        let fixture = std::fs::read_to_string(fixture_path("zed_settings.json")).unwrap();
        std::fs::write(&config_path, &fixture).unwrap();

        let adapter = ZedAdapter;
        let servers = adapter.parse_servers(&config_path).unwrap();
        let shim_binary = PathBuf::from("/usr/local/bin/thoughtgate");
        let options = ShimOptions {
            server_id: String::new(),
            governance_endpoint: "http://127.0.0.1:19090".to_string(),
            profile: Profile::Production,
        };

        adapter
            .rewrite_config(&config_path, &servers, &shim_binary, &options)
            .unwrap();

        let rewritten: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();

        // `simple-server` has no env key in the fixture — verify it was created.
        let simple_env = rewritten["context_servers"]["simple-server"]["env"]
            .as_object()
            .expect("env should be created for servers without existing env");
        assert_eq!(
            simple_env
                .get("THOUGHTGATE_SERVER_ID")
                .unwrap()
                .as_str()
                .unwrap(),
            "simple-server"
        );

        // `filesystem` has an existing env — verify it was preserved.
        let fs_env = rewritten["context_servers"]["filesystem"]["env"]
            .as_object()
            .unwrap();
        assert_eq!(
            fs_env.get("NODE_ENV").unwrap().as_str().unwrap(),
            "production"
        );
        assert_eq!(
            fs_env
                .get("THOUGHTGATE_SERVER_ID")
                .unwrap()
                .as_str()
                .unwrap(),
            "filesystem"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_double_wrap_detection() {
        let dir = temp_config_dir();
        let config_path = dir.join("config.json");
        let fixture = std::fs::read_to_string(fixture_path("claude_desktop_config.json")).unwrap();
        std::fs::write(&config_path, &fixture).unwrap();

        let adapter = ClaudeDesktopAdapter;
        let servers = adapter.parse_servers(&config_path).unwrap();
        let shim_binary = PathBuf::from("/usr/local/bin/thoughtgate");
        let options = ShimOptions {
            server_id: String::new(),
            governance_endpoint: "http://127.0.0.1:19090".to_string(),
            profile: Profile::Production,
        };

        // First rewrite succeeds.
        adapter
            .rewrite_config(&config_path, &servers, &shim_binary, &options)
            .unwrap();

        // Second rewrite detects double-wrap.
        let result = adapter.rewrite_config(&config_path, &servers, &shim_binary, &options);
        assert!(matches!(result, Err(ConfigError::AlreadyManaged)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_double_wrap_zed_object_format() {
        let dir = temp_config_dir();
        let config_path = dir.join("settings.json");
        let fixture = std::fs::read_to_string(fixture_path("zed_settings.json")).unwrap();
        std::fs::write(&config_path, &fixture).unwrap();

        let adapter = ZedAdapter;
        let servers = adapter.parse_servers(&config_path).unwrap();
        let shim_binary = PathBuf::from("/usr/local/bin/thoughtgate");
        let options = ShimOptions {
            server_id: String::new(),
            governance_endpoint: "http://127.0.0.1:19090".to_string(),
            profile: Profile::Production,
        };

        // First rewrite succeeds.
        adapter
            .rewrite_config(&config_path, &servers, &shim_binary, &options)
            .unwrap();

        // Second rewrite detects double-wrap via object command format.
        let result = adapter.rewrite_config(&config_path, &servers, &shim_binary, &options);
        assert!(matches!(result, Err(ConfigError::AlreadyManaged)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cursor_project_level_rewrite() {
        // Verify that standard_rewrite correctly wraps a project-level
        // Cursor config (same format as global). CursorAdapter::rewrite_config
        // calls standard_rewrite on both global and project-level configs.
        let dir = temp_config_dir();
        let project_config = dir.join("mcp.json");

        // Project-level config has a server that could bypass governance.
        let config = serde_json::json!({
            "mcpServers": {
                "dangerous-tool": {
                    "command": "npx",
                    "args": ["-y", "dangerous-mcp-server"]
                }
            }
        });
        std::fs::write(
            &project_config,
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let servers = vec![McpServerEntry {
            id: "dangerous-tool".to_string(),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "dangerous-mcp-server".to_string()],
            env: None,
            enabled: true,
        }];

        let shim_binary = PathBuf::from("/usr/local/bin/thoughtgate");
        let options = ShimOptions {
            server_id: String::new(),
            governance_endpoint: "http://127.0.0.1:19090".to_string(),
            profile: Profile::Production,
        };

        // Rewrite the project-level config.
        standard_rewrite(
            &project_config,
            "mcpServers",
            &servers,
            &shim_binary,
            &options,
        )
        .unwrap();

        let rewritten: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&project_config).unwrap()).unwrap();

        // Verify the command was replaced with thoughtgate shim.
        assert_eq!(
            rewritten["mcpServers"]["dangerous-tool"]["command"]
                .as_str()
                .unwrap(),
            "/usr/local/bin/thoughtgate"
        );

        // Verify THOUGHTGATE_SERVER_ID is set.
        assert_eq!(
            rewritten["mcpServers"]["dangerous-tool"]["env"]["THOUGHTGATE_SERVER_ID"]
                .as_str()
                .unwrap(),
            "dangerous-tool"
        );

        // Verify double-wrap detection works on the rewritten project config.
        let result = standard_rewrite(
            &project_config,
            "mcpServers",
            &servers,
            &shim_binary,
            &options,
        );
        assert!(matches!(result, Err(ConfigError::AlreadyManaged)));

        // Verify backup was created.
        let mut backup_path = project_config.as_os_str().to_os_string();
        backup_path.push(".thoughtgate-backup");
        assert!(PathBuf::from(&backup_path).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Environment Variable Expansion Tests ─────────────────────────────

    #[test]
    fn test_expand_dollar_brace_known_var() {
        // Use a variable we know exists in every test environment.
        let home = std::env::var("HOME").unwrap();
        let result = expand_dollar_brace("${HOME}/configs").unwrap();
        assert_eq!(result, format!("{home}/configs"));
    }

    #[test]
    fn test_expand_dollar_brace_undefined() {
        let result = expand_dollar_brace("${NONEXISTENT_VAR_XYZ_12345}");
        assert!(matches!(
            result,
            Err(ConfigError::UndefinedEnvVar { ref name }) if name == "NONEXISTENT_VAR_XYZ_12345"
        ));
    }

    #[test]
    fn test_expand_env_colon_known_var() {
        let home = std::env::var("HOME").unwrap();
        let result = expand_env_colon("${env:HOME}/configs").unwrap();
        assert_eq!(result, format!("{home}/configs"));
    }

    #[test]
    fn test_expand_env_colon_undefined() {
        let result = expand_env_colon("${env:NONEXISTENT_VAR_XYZ_12345}");
        assert!(matches!(
            result,
            Err(ConfigError::UndefinedEnvVar { ref name }) if name == "NONEXISTENT_VAR_XYZ_12345"
        ));
    }

    #[test]
    fn test_expand_no_vars() {
        let result = expand_dollar_brace("plain string without vars").unwrap();
        assert_eq!(result, "plain string without vars");
    }

    #[test]
    fn test_expand_multiple_vars() {
        // SAFETY: Test runs single-threaded; no other thread reads these vars.
        unsafe {
            std::env::set_var("THOUGHTGATE_TEST_A", "alpha");
            std::env::set_var("THOUGHTGATE_TEST_B", "beta");
        }
        let result = expand_dollar_brace("${THOUGHTGATE_TEST_A}/${THOUGHTGATE_TEST_B}").unwrap();
        assert_eq!(result, "alpha/beta");
        unsafe {
            std::env::remove_var("THOUGHTGATE_TEST_A");
            std::env::remove_var("THOUGHTGATE_TEST_B");
        }
    }

    #[test]
    fn test_expand_with_default_value_undefined() {
        // Variable not set, should use default.
        let result = expand_dollar_brace("${NONEXISTENT_VAR_ABC:-fallback_value}").unwrap();
        assert_eq!(result, "fallback_value");
    }

    #[test]
    fn test_expand_with_default_value_defined() {
        // SAFETY: Test runs single-threaded.
        unsafe {
            std::env::set_var("THOUGHTGATE_TEST_DEFAULT", "actual_value");
        }
        let result = expand_dollar_brace("${THOUGHTGATE_TEST_DEFAULT:-fallback}").unwrap();
        assert_eq!(result, "actual_value");
        unsafe {
            std::env::remove_var("THOUGHTGATE_TEST_DEFAULT");
        }
    }

    #[test]
    fn test_expand_with_default_empty_var() {
        // SAFETY: Test runs single-threaded.
        unsafe {
            std::env::set_var("THOUGHTGATE_TEST_EMPTY", "");
        }
        // Empty string should trigger default.
        let result = expand_dollar_brace("${THOUGHTGATE_TEST_EMPTY:-default_for_empty}").unwrap();
        assert_eq!(result, "default_for_empty");
        unsafe {
            std::env::remove_var("THOUGHTGATE_TEST_EMPTY");
        }
    }

    #[test]
    fn test_expand_with_default_in_path() {
        let result = expand_dollar_brace("/home/${USER:-nobody}/.config/${APP:-myapp}").unwrap();
        // USER is usually set, APP is not.
        let user = std::env::var("USER").unwrap_or_else(|_| "nobody".to_string());
        assert_eq!(result, format!("/home/{user}/.config/myapp"));
    }

    // ── Claude Code Adapter Tests ─────────────────────────────────────────

    #[test]
    fn test_parse_claude_code_project_servers() {
        let config: serde_json::Value = serde_json::json!({
            "projects": {
                "/tmp/test-project": {
                    "mcpServers": {
                        "filesystem": {
                            "type": "stdio",
                            "command": "npx",
                            "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
                            "env": { "NODE_ENV": "production" }
                        },
                        "notion": {
                            "type": "http",
                            "url": "https://mcp.notion.com/mcp"
                        }
                    }
                }
            }
        });

        let servers =
            ClaudeCodeAdapter::parse_project_servers(&config, "/tmp/test-project").unwrap();

        // Only stdio servers should be returned (notion is http, filtered out).
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, "filesystem");
        assert_eq!(servers[0].command, "npx");
        assert_eq!(
            servers[0].args,
            vec!["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        );
        assert!(servers[0].env.is_some());
        assert_eq!(
            servers[0].env.as_ref().unwrap().get("NODE_ENV").unwrap(),
            "production"
        );
    }

    #[test]
    fn test_parse_claude_code_empty_project() {
        let config: serde_json::Value = serde_json::json!({
            "projects": {
                "/tmp/other-project": {
                    "mcpServers": {}
                }
            }
        });

        let servers =
            ClaudeCodeAdapter::parse_project_servers(&config, "/tmp/test-project").unwrap();

        // Project not found = empty, not error.
        assert!(servers.is_empty());
    }

    #[test]
    fn test_claude_code_mcp_json_server_enabled_default() {
        let config: serde_json::Value = serde_json::json!({
            "projects": {
                "/tmp/test-project": {
                    "mcpServers": {},
                    "enabledMcpjsonServers": [],
                    "disabledMcpjsonServers": []
                }
            }
        });

        // Empty lists = default enabled.
        assert!(ClaudeCodeAdapter::is_mcp_json_server_enabled(
            &config,
            "/tmp/test-project",
            "any-server"
        ));
    }

    #[test]
    fn test_claude_code_mcp_json_server_disabled() {
        let config: serde_json::Value = serde_json::json!({
            "projects": {
                "/tmp/test-project": {
                    "mcpServers": {},
                    "enabledMcpjsonServers": [],
                    "disabledMcpjsonServers": ["disabled-server"]
                }
            }
        });

        assert!(!ClaudeCodeAdapter::is_mcp_json_server_enabled(
            &config,
            "/tmp/test-project",
            "disabled-server"
        ));
        assert!(ClaudeCodeAdapter::is_mcp_json_server_enabled(
            &config,
            "/tmp/test-project",
            "other-server"
        ));
    }

    #[test]
    fn test_claude_code_mcp_json_server_explicit_enabled() {
        let config: serde_json::Value = serde_json::json!({
            "projects": {
                "/tmp/test-project": {
                    "mcpServers": {},
                    "enabledMcpjsonServers": ["allowed-server"],
                    "disabledMcpjsonServers": []
                }
            }
        });

        // When enabledMcpjsonServers is non-empty, only those are enabled.
        assert!(ClaudeCodeAdapter::is_mcp_json_server_enabled(
            &config,
            "/tmp/test-project",
            "allowed-server"
        ));
        assert!(!ClaudeCodeAdapter::is_mcp_json_server_enabled(
            &config,
            "/tmp/test-project",
            "other-server"
        ));
    }

    #[test]
    fn test_claude_code_double_wrap_detection() {
        let config: serde_json::Value = serde_json::json!({
            "projects": {
                "/tmp/test-project": {
                    "mcpServers": {
                        "wrapped": {
                            "type": "stdio",
                            "command": "/usr/local/bin/thoughtgate",
                            "args": ["shim", "--server-id", "wrapped"]
                        }
                    }
                }
            }
        });

        let shim = PathBuf::from("/usr/local/bin/thoughtgate");
        assert!(ClaudeCodeAdapter::detect_project_double_wrap(
            &config,
            "/tmp/test-project",
            &shim
        ));

        // Different project = no wrap.
        assert!(!ClaudeCodeAdapter::detect_project_double_wrap(
            &config,
            "/tmp/other-project",
            &shim
        ));
    }
}
