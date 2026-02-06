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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thoughtgate_core::profile::Profile;

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
    /// Path to ThoughtGate config file.
    pub config_path: PathBuf,
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
// Environment Variable Expansion (F-004)
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
    expand_vars(input, false)
}

/// Expand `${env:VAR}` references in a string (Windsurf style).
///
/// Scans for `${env:...}` patterns and replaces them with the corresponding
/// environment variable value. Plain `${VAR}` patterns without the `env:`
/// prefix are left unchanged.
///
/// Implements: REQ-CORE-008/F-004
pub fn expand_env_colon(input: &str) -> Result<String, ConfigError> {
    expand_vars(input, true)
}

/// Internal implementation of environment variable expansion.
///
/// When `require_env_prefix` is true, only `${env:VAR}` patterns are expanded;
/// plain `${VAR}` is left unchanged. When false, all `${VAR}` patterns are expanded.
///
/// Both modes support the `:-default` syntax for fallback values.
fn expand_vars(input: &str, require_env_prefix: bool) -> Result<String, ConfigError> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_content = String::new();
            let mut found_close = false;
            for inner in chars.by_ref() {
                if inner == '}' {
                    found_close = true;
                    break;
                }
                var_content.push(inner);
            }
            if !found_close {
                // No closing brace — pass through literally.
                result.push('$');
                result.push('{');
                result.push_str(&var_content);
                continue;
            }

            if require_env_prefix {
                if let Some(var_spec) = var_content.strip_prefix("env:") {
                    let value = expand_var_with_default(var_spec)?;
                    result.push_str(&value);
                } else {
                    // Not an env: prefix — pass through unchanged.
                    result.push('$');
                    result.push('{');
                    result.push_str(&var_content);
                    result.push('}');
                }
            } else {
                let value = expand_var_with_default(&var_content)?;
                result.push_str(&value);
            }
        } else {
            result.push(c);
        }
    }

    Ok(result)
}

/// Expand a single variable reference, supporting `VAR:-default` syntax.
///
/// - `VAR` - Returns value of VAR, errors if not set
/// - `VAR:-default` - Returns value of VAR if set, otherwise returns "default"
fn expand_var_with_default(var_spec: &str) -> Result<String, ConfigError> {
    // Check for `:-` default value syntax.
    if let Some((var_name, default_value)) = var_spec.split_once(":-") {
        match std::env::var(var_name) {
            Ok(value) if !value.is_empty() => Ok(value),
            _ => Ok(default_value.to_string()),
        }
    } else {
        std::env::var(var_spec).map_err(|_| ConfigError::UndefinedEnvVar {
            name: var_spec.to_string(),
        })
    }
}

/// Expand environment variables in all string values of a server entry's
/// command and args, using the given expansion function.
fn expand_server_entry_env(
    entry: &mut McpServerEntry,
    expander: fn(&str) -> Result<String, ConfigError>,
) -> Result<(), ConfigError> {
    entry.command = expander(&entry.command)?;
    entry.args = entry
        .args
        .iter()
        .map(|a| expander(a))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Read a config file and parse as JSON.
fn read_config(path: &Path) -> Result<serde_json::Value, ConfigError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ConfigError::NotFound {
                path: path.to_path_buf(),
            }
        } else {
            ConfigError::Io(e)
        }
    })?;
    serde_json::from_str(&content).map_err(|e| ConfigError::Parse {
        reason: e.to_string(),
    })
}

/// Write JSON to a config file with pretty-printing.
fn write_config(path: &Path, value: &serde_json::Value) -> Result<(), ConfigError> {
    let content = serde_json::to_string_pretty(value).map_err(|e| ConfigError::Write {
        reason: e.to_string(),
    })?;
    std::fs::write(path, content).map_err(|e| ConfigError::Write {
        reason: e.to_string(),
    })
}

/// Create backup at `<path>.thoughtgate-backup`, verify, return backup path.
///
/// Implements: REQ-CORE-008/F-005
fn backup_config(config_path: &Path) -> Result<PathBuf, ConfigError> {
    let mut backup_path = config_path.as_os_str().to_os_string();
    backup_path.push(".thoughtgate-backup");
    let backup_path = PathBuf::from(backup_path);

    // Copy original to backup.
    std::fs::copy(config_path, &backup_path)?;

    // Verify backup was written successfully.
    let original = std::fs::read(config_path)?;
    let backed_up = std::fs::read(&backup_path)?;
    if original != backed_up {
        return Err(ConfigError::Write {
            reason: "backup verification failed: content mismatch".to_string(),
        });
    }

    Ok(backup_path)
}

/// Parse servers from a JSON object with a given top-level key (e.g., `"mcpServers"`).
///
/// Used by ClaudeDesktop, ClaudeCode, Cursor, and Windsurf adapters which share
/// the `{ "mcpServers": { "<id>": { "command": ..., "args": [...] } } }` structure.
///
/// Implements: REQ-CORE-008/F-003
fn parse_mcp_servers_key(
    config: &serde_json::Value,
    key: &str,
) -> Result<Vec<McpServerEntry>, ConfigError> {
    let servers_obj =
        config
            .get(key)
            .and_then(|v| v.as_object())
            .ok_or_else(|| ConfigError::Parse {
                reason: format!("missing or invalid `{key}` key in config"),
            })?;

    let mut entries = Vec::new();
    for (id, server) in servers_obj {
        let command = server
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ConfigError::Parse {
                reason: format!("server `{id}` missing `command` field"),
            })?
            .to_string();

        let args = server
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let env = server.get("env").and_then(|v| v.as_object()).map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        });

        let enabled = !server
            .get("disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        entries.push(McpServerEntry {
            id: id.clone(),
            command,
            args,
            env,
            enabled,
        });
    }

    Ok(entries)
}

/// Rewrite a config JSON, replacing server commands under the given key.
///
/// For each server, replaces `command` with the shim binary path and builds
/// the appropriate args array with `--server-id`, `--governance-endpoint`,
/// `--profile`, and the original command/args after `--`.
///
/// Implements: REQ-CORE-008/F-006
fn rewrite_mcp_servers_key(
    config: &mut serde_json::Value,
    key: &str,
    servers: &[McpServerEntry],
    shim_binary: &Path,
    options: &ShimOptions,
) -> Result<(), ConfigError> {
    let servers_obj = config
        .get_mut(key)
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| ConfigError::Parse {
            reason: format!("missing or invalid `{key}` key in config"),
        })?;

    let shim_path = shim_binary.to_string_lossy().to_string();
    let profile_str = match options.profile {
        Profile::Production => "production",
        Profile::Development => "development",
    };

    for server in servers {
        if let Some(entry) = servers_obj.get_mut(&server.id) {
            let entry_obj = entry.as_object_mut().ok_or_else(|| ConfigError::Parse {
                reason: format!("server `{}` is not a JSON object", server.id),
            })?;

            // Build shim args.
            let mut shim_args: Vec<serde_json::Value> = vec![
                "shim".into(),
                "--server-id".into(),
                server.id.clone().into(),
                "--governance-endpoint".into(),
                options.governance_endpoint.clone().into(),
                "--profile".into(),
                profile_str.into(),
                "--".into(),
                server.command.clone().into(),
            ];
            for arg in &server.args {
                shim_args.push(arg.clone().into());
            }

            entry_obj.insert("command".to_string(), shim_path.clone().into());
            entry_obj.insert("args".to_string(), serde_json::Value::Array(shim_args));

            // Add THOUGHTGATE_SERVER_ID to env.
            let env_obj = entry_obj
                .entry("env")
                .or_insert_with(|| serde_json::json!({}));
            if let Some(env_map) = env_obj.as_object_mut() {
                env_map.insert(
                    "THOUGHTGATE_SERVER_ID".to_string(),
                    server.id.clone().into(),
                );
            }
        }
    }

    Ok(())
}

/// Check if any server command already points to the thoughtgate binary.
///
/// Implements: REQ-CORE-008/F-005 (double-wrap detection)
fn detect_double_wrap(config: &serde_json::Value, key: &str, shim_binary: &Path) -> bool {
    let shim_str = shim_binary.to_string_lossy();

    let Some(servers_obj) = config.get(key).and_then(|v| v.as_object()) else {
        return false;
    };

    for (_id, server) in servers_obj {
        // Standard format: "command": "thoughtgate"
        if let Some(cmd) = server.get("command").and_then(|v| v.as_str()) {
            if cmd == shim_str || Path::new(cmd).file_name() == Path::new("thoughtgate").file_name()
            {
                return true;
            }
        }
        // Zed object format: "command": { "path": "thoughtgate", ... }
        if let Some(cmd_path) = server
            .get("command")
            .and_then(|v| v.get("path"))
            .and_then(|v| v.as_str())
        {
            if cmd_path == shim_str
                || Path::new(cmd_path).file_name() == Path::new("thoughtgate").file_name()
            {
                return true;
            }
        }
    }

    false
}

/// Shared rewrite logic for adapters using the standard `mcpServers`-style key.
///
/// Reads config → checks double-wrap → backs up → rewrites → writes.
fn standard_rewrite(
    config_path: &Path,
    key: &str,
    servers: &[McpServerEntry],
    shim_binary: &Path,
    options: &ShimOptions,
) -> Result<PathBuf, ConfigError> {
    let mut config = read_config(config_path)?;

    if detect_double_wrap(&config, key, shim_binary) {
        return Err(ConfigError::AlreadyManaged);
    }

    let backup_path = backup_config(config_path)?;
    rewrite_mcp_servers_key(&mut config, key, servers, shim_binary, options)?;
    write_config(config_path, &config)?;

    Ok(backup_path)
}

/// Shared restore logic: copy backup over config, remove backup.
fn standard_restore(config_path: &Path, backup_path: &Path) -> Result<(), ConfigError> {
    std::fs::copy(backup_path, config_path)?;
    // Best-effort removal of backup.
    let _ = std::fs::remove_file(backup_path);
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Claude Desktop Adapter
// ─────────────────────────────────────────────────────────────────────────────

/// Config adapter for Claude Desktop.
///
/// Claude Desktop stores MCP servers under `"mcpServers"` in its config file.
/// Config path: `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS)
/// or `~/.config/Claude/claude_desktop_config.json` (Linux).
///
/// Implements: REQ-CORE-008/F-002, F-003
pub struct ClaudeDesktopAdapter;

impl ConfigAdapter for ClaudeDesktopAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::ClaudeDesktop
    }

    /// Discovers Claude Desktop config path.
    ///
    /// Implements: REQ-CORE-008/F-002
    fn discover_config_path(&self) -> Option<PathBuf> {
        let config_dir = dirs::config_dir()?;
        let path = config_dir.join("Claude").join("claude_desktop_config.json");
        if path.exists() { Some(path) } else { None }
    }

    fn parse_servers(&self, config_path: &Path) -> Result<Vec<McpServerEntry>, ConfigError> {
        let config = read_config(config_path)?;
        parse_mcp_servers_key(&config, "mcpServers")
    }

    fn rewrite_config(
        &self,
        config_path: &Path,
        servers: &[McpServerEntry],
        shim_binary: &Path,
        options: &ShimOptions,
    ) -> Result<PathBuf, ConfigError> {
        standard_rewrite(config_path, "mcpServers", servers, shim_binary, options)
    }

    fn restore_config(&self, config_path: &Path, backup_path: &Path) -> Result<(), ConfigError> {
        standard_restore(config_path, backup_path)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Claude Code Adapter
// ─────────────────────────────────────────────────────────────────────────────

/// Config adapter for Claude Code.
///
/// Claude Code stores MCP servers in two locations:
/// 1. Per-project in `~/.claude.json` under `projects.<project_path>.mcpServers`
/// 2. Project-local `.mcp.json` files (controlled by `enabledMcpjsonServers`/`disabledMcpjsonServers`)
///
/// Supports `${VAR}` and `${VAR:-default}` environment variable expansion.
///
/// Implements: REQ-CORE-008/F-002, F-003, F-004
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    /// Get the project path key for the current working directory.
    fn get_project_key() -> Option<String> {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    }

    /// Parse MCP servers from a project's mcpServers object in ~/.claude.json.
    ///
    /// Structure: `{ "projects": { "<path>": { "mcpServers": { ... } } } }`
    fn parse_project_servers(
        config: &serde_json::Value,
        project_path: &str,
    ) -> Result<Vec<McpServerEntry>, ConfigError> {
        let servers_obj = config
            .get("projects")
            .and_then(|p| p.get(project_path))
            .and_then(|proj| proj.get("mcpServers"))
            .and_then(|v| v.as_object());

        let Some(servers_obj) = servers_obj else {
            // No servers configured for this project - not an error, just empty.
            return Ok(Vec::new());
        };

        let mut entries = Vec::new();
        for (id, server) in servers_obj {
            // Skip non-stdio servers (http, sse, webSocket).
            let server_type = server
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("stdio");
            if server_type != "stdio" {
                continue;
            }

            let command = server
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConfigError::Parse {
                    reason: format!("server `{id}` missing `command` field"),
                })?
                .to_string();

            let args = server
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let env = server.get("env").and_then(|v| v.as_object()).map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            });

            let enabled = !server
                .get("disabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            entries.push(McpServerEntry {
                id: id.clone(),
                command,
                args,
                env,
                enabled,
            });
        }

        Ok(entries)
    }

    /// Check if a .mcp.json server is enabled for this project.
    fn is_mcp_json_server_enabled(
        config: &serde_json::Value,
        project_path: &str,
        server_id: &str,
    ) -> bool {
        let project = config.get("projects").and_then(|p| p.get(project_path));

        let Some(project) = project else {
            return true; // No project config = default enabled
        };

        // Check if explicitly disabled.
        if let Some(disabled) = project
            .get("disabledMcpjsonServers")
            .and_then(|v| v.as_array())
        {
            if disabled.iter().any(|v| v.as_str() == Some(server_id)) {
                return false;
            }
        }

        // Check if explicitly enabled (if enabledMcpjsonServers is non-empty, only those are enabled).
        if let Some(enabled) = project
            .get("enabledMcpjsonServers")
            .and_then(|v| v.as_array())
        {
            if !enabled.is_empty() {
                return enabled.iter().any(|v| v.as_str() == Some(server_id));
            }
        }

        true // Default: enabled
    }

    /// Rewrite the per-project mcpServers in ~/.claude.json.
    fn rewrite_project_servers(
        config: &mut serde_json::Value,
        project_path: &str,
        servers: &[McpServerEntry],
        shim_binary: &Path,
        options: &ShimOptions,
    ) -> Result<(), ConfigError> {
        let shim_path = shim_binary.to_string_lossy().to_string();
        let profile_str = match options.profile {
            Profile::Production => "production",
            Profile::Development => "development",
        };

        // Navigate to projects.<project_path>.mcpServers, creating if needed.
        let projects = config
            .as_object_mut()
            .ok_or_else(|| ConfigError::Parse {
                reason: "config is not a JSON object".to_string(),
            })?
            .entry("projects")
            .or_insert_with(|| serde_json::json!({}));

        let project = projects
            .as_object_mut()
            .ok_or_else(|| ConfigError::Parse {
                reason: "`projects` is not a JSON object".to_string(),
            })?
            .entry(project_path)
            .or_insert_with(|| serde_json::json!({}));

        let servers_obj = project
            .as_object_mut()
            .ok_or_else(|| ConfigError::Parse {
                reason: format!("project `{project_path}` is not a JSON object"),
            })?
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));

        let servers_map = servers_obj
            .as_object_mut()
            .ok_or_else(|| ConfigError::Parse {
                reason: "`mcpServers` is not a JSON object".to_string(),
            })?;

        for server in servers {
            if let Some(entry) = servers_map.get_mut(&server.id) {
                let entry_obj = entry.as_object_mut().ok_or_else(|| ConfigError::Parse {
                    reason: format!("server `{}` is not a JSON object", server.id),
                })?;

                // Build shim args.
                let mut shim_args: Vec<serde_json::Value> = vec![
                    "shim".into(),
                    "--server-id".into(),
                    server.id.clone().into(),
                    "--governance-endpoint".into(),
                    options.governance_endpoint.clone().into(),
                    "--profile".into(),
                    profile_str.into(),
                    "--".into(),
                    server.command.clone().into(),
                ];
                for arg in &server.args {
                    shim_args.push(arg.clone().into());
                }

                entry_obj.insert("command".to_string(), shim_path.clone().into());
                entry_obj.insert("args".to_string(), serde_json::Value::Array(shim_args));

                // Add THOUGHTGATE_SERVER_ID to env.
                let env_obj = entry_obj
                    .entry("env")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(env_map) = env_obj.as_object_mut() {
                    env_map.insert(
                        "THOUGHTGATE_SERVER_ID".to_string(),
                        server.id.clone().into(),
                    );
                }
            }
        }

        Ok(())
    }

    /// Check if any server in the project already points to thoughtgate (double-wrap detection).
    fn detect_project_double_wrap(
        config: &serde_json::Value,
        project_path: &str,
        shim_binary: &Path,
    ) -> bool {
        let shim_str = shim_binary.to_string_lossy();

        let servers_obj = config
            .get("projects")
            .and_then(|p| p.get(project_path))
            .and_then(|proj| proj.get("mcpServers"))
            .and_then(|v| v.as_object());

        let Some(servers_obj) = servers_obj else {
            return false;
        };

        for (_id, server) in servers_obj {
            if let Some(cmd) = server.get("command").and_then(|v| v.as_str()) {
                if cmd == shim_str
                    || Path::new(cmd).file_name() == Path::new("thoughtgate").file_name()
                {
                    return true;
                }
            }
        }

        false
    }
}

impl ConfigAdapter for ClaudeCodeAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::ClaudeCode
    }

    /// Discovers Claude Code user-level config path.
    ///
    /// Implements: REQ-CORE-008/F-002
    fn discover_config_path(&self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        let path = home.join(".claude.json");
        if path.exists() { Some(path) } else { None }
    }

    fn parse_servers(&self, config_path: &Path) -> Result<Vec<McpServerEntry>, ConfigError> {
        let config = read_config(config_path)?;

        // Get current project path for per-project server lookup.
        let project_key = Self::get_project_key();

        let mut entries = Vec::new();

        // 1. Parse servers from ~/.claude.json projects.<cwd>.mcpServers
        if let Some(ref project_path) = project_key {
            entries = Self::parse_project_servers(&config, project_path)?;
        }

        // 2. Check for project-level .mcp.json and merge (project file overrides).
        if let Ok(cwd) = std::env::current_dir() {
            let mcp_json_path = cwd.join(".mcp.json");
            if mcp_json_path.exists() {
                if let Ok(mcp_config) = read_config(&mcp_json_path) {
                    if let Ok(mcp_entries) = parse_mcp_servers_key(&mcp_config, "mcpServers") {
                        for mcp_entry in mcp_entries {
                            // Check if this .mcp.json server is enabled for the project.
                            let is_enabled = project_key.as_ref().is_none_or(|pk| {
                                Self::is_mcp_json_server_enabled(&config, pk, &mcp_entry.id)
                            });

                            if !is_enabled {
                                continue;
                            }

                            // .mcp.json entries override ~/.claude.json entries by id.
                            if let Some(existing) =
                                entries.iter_mut().find(|e| e.id == mcp_entry.id)
                            {
                                *existing = mcp_entry;
                            } else {
                                entries.push(mcp_entry);
                            }
                        }
                    }
                }
            }
        }

        // 3. Apply ${VAR} and ${VAR:-default} expansion (F-004).
        for entry in &mut entries {
            expand_server_entry_env(entry, expand_dollar_brace)?;
        }

        // Filter to only enabled servers.
        entries.retain(|e| e.enabled);

        if entries.is_empty() {
            return Err(ConfigError::NoServers);
        }

        Ok(entries)
    }

    fn rewrite_config(
        &self,
        config_path: &Path,
        servers: &[McpServerEntry],
        shim_binary: &Path,
        options: &ShimOptions,
    ) -> Result<PathBuf, ConfigError> {
        let project_key = Self::get_project_key().ok_or_else(|| ConfigError::Parse {
            reason: "could not determine current working directory".to_string(),
        })?;

        let mut config = read_config(config_path)?;

        if Self::detect_project_double_wrap(&config, &project_key, shim_binary) {
            return Err(ConfigError::AlreadyManaged);
        }

        let backup_path = backup_config(config_path)?;
        Self::rewrite_project_servers(&mut config, &project_key, servers, shim_binary, options)?;
        write_config(config_path, &config)?;

        Ok(backup_path)
    }

    fn restore_config(&self, config_path: &Path, backup_path: &Path) -> Result<(), ConfigError> {
        standard_restore(config_path, backup_path)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cursor Adapter
// ─────────────────────────────────────────────────────────────────────────────

/// Config adapter for Cursor.
///
/// Cursor uses `~/.cursor/mcp.json` (global) and `.cursor/mcp.json` (project).
/// Both use `"mcpServers"` key. Project-level entries override global by id.
///
/// Implements: REQ-CORE-008/F-002, F-003
pub struct CursorAdapter;

impl ConfigAdapter for CursorAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Cursor
    }

    /// Discovers Cursor global config path.
    ///
    /// Implements: REQ-CORE-008/F-002
    fn discover_config_path(&self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        let path = home.join(".cursor").join("mcp.json");
        if path.exists() { Some(path) } else { None }
    }

    fn parse_servers(&self, config_path: &Path) -> Result<Vec<McpServerEntry>, ConfigError> {
        let config = read_config(config_path)?;
        let mut entries = parse_mcp_servers_key(&config, "mcpServers")?;

        // Check for project-level .cursor/mcp.json and merge.
        if let Ok(cwd) = std::env::current_dir() {
            let project_path = cwd.join(".cursor").join("mcp.json");
            if project_path.exists() {
                if let Ok(project_config) = read_config(&project_path) {
                    if let Ok(project_entries) =
                        parse_mcp_servers_key(&project_config, "mcpServers")
                    {
                        for proj_entry in project_entries {
                            if let Some(existing) =
                                entries.iter_mut().find(|e| e.id == proj_entry.id)
                            {
                                *existing = proj_entry;
                            } else {
                                entries.push(proj_entry);
                            }
                        }
                    }
                }
            }
        }

        Ok(entries)
    }

    fn rewrite_config(
        &self,
        config_path: &Path,
        servers: &[McpServerEntry],
        shim_binary: &Path,
        options: &ShimOptions,
    ) -> Result<PathBuf, ConfigError> {
        let backup_path =
            standard_rewrite(config_path, "mcpServers", servers, shim_binary, options)?;

        // Also rewrite project-level config if it exists, to prevent
        // project overrides from bypassing governance.
        if let Ok(cwd) = std::env::current_dir() {
            let project_path = cwd.join(".cursor").join("mcp.json");
            if project_path.exists() {
                if let Ok(project_config) = read_config(&project_path) {
                    if project_config
                        .get("mcpServers")
                        .and_then(|v| v.as_object())
                        .is_some()
                    {
                        match standard_rewrite(
                            &project_path,
                            "mcpServers",
                            servers,
                            shim_binary,
                            options,
                        ) {
                            Ok(_) | Err(ConfigError::AlreadyManaged) => {}
                            Err(e) => {
                                tracing::warn!(
                                    path = %project_path.display(),
                                    error = %e,
                                    "Failed to rewrite project-level Cursor config; \
                                     project servers may bypass governance"
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(backup_path)
    }

    fn restore_config(&self, config_path: &Path, backup_path: &Path) -> Result<(), ConfigError> {
        standard_restore(config_path, backup_path)?;

        // Best-effort restore project-level config.
        if let Ok(cwd) = std::env::current_dir() {
            let project_path = cwd.join(".cursor").join("mcp.json");
            let mut project_backup = project_path.as_os_str().to_os_string();
            project_backup.push(".thoughtgate-backup");
            let project_backup = PathBuf::from(project_backup);
            if project_backup.exists() {
                let _ = standard_restore(&project_path, &project_backup);
            }
        }

        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// VS Code Adapter
// ─────────────────────────────────────────────────────────────────────────────

/// Config adapter for VS Code.
///
/// VS Code uses `.vscode/mcp.json` in the workspace. Uses `"servers"` key
/// (not `"mcpServers"`). Each server has a `"type"` field; only `"stdio"`
/// entries are relevant.
///
/// Implements: REQ-CORE-008/F-002, F-003
pub struct VsCodeAdapter;

impl ConfigAdapter for VsCodeAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::VsCode
    }

    /// Discovers VS Code workspace config path.
    ///
    /// Implements: REQ-CORE-008/F-002
    fn discover_config_path(&self) -> Option<PathBuf> {
        let cwd = std::env::current_dir().ok()?;
        let path = cwd.join(".vscode").join("mcp.json");
        if path.exists() { Some(path) } else { None }
    }

    fn parse_servers(&self, config_path: &Path) -> Result<Vec<McpServerEntry>, ConfigError> {
        let config = read_config(config_path)?;

        let servers_obj = config
            .get("servers")
            .and_then(|v| v.as_object())
            .ok_or_else(|| ConfigError::Parse {
                reason: "missing or invalid `servers` key in VS Code config".to_string(),
            })?;

        let mut entries = Vec::new();
        for (id, server) in servers_obj {
            // Only process stdio-type servers.
            let server_type = server.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if server_type != "stdio" {
                continue;
            }

            let command = server
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConfigError::Parse {
                    reason: format!("stdio server `{id}` missing `command` field"),
                })?
                .to_string();

            let args = server
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let env = server.get("env").and_then(|v| v.as_object()).map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            });

            entries.push(McpServerEntry {
                id: id.clone(),
                command,
                args,
                env,
                enabled: true,
            });
        }

        Ok(entries)
    }

    fn rewrite_config(
        &self,
        config_path: &Path,
        servers: &[McpServerEntry],
        shim_binary: &Path,
        options: &ShimOptions,
    ) -> Result<PathBuf, ConfigError> {
        let mut config = read_config(config_path)?;

        if detect_double_wrap(&config, "servers", shim_binary) {
            return Err(ConfigError::AlreadyManaged);
        }

        let backup_path = backup_config(config_path)?;

        // Rewrite only stdio servers under "servers" key.
        let servers_obj = config
            .get_mut("servers")
            .and_then(|v| v.as_object_mut())
            .ok_or_else(|| ConfigError::Parse {
                reason: "missing `servers` key".to_string(),
            })?;

        let shim_path = shim_binary.to_string_lossy().to_string();
        let profile_str = match options.profile {
            Profile::Production => "production",
            Profile::Development => "development",
        };

        for server in servers {
            if let Some(entry) = servers_obj.get_mut(&server.id) {
                let entry_obj = entry.as_object_mut().ok_or_else(|| ConfigError::Parse {
                    reason: format!("server `{}` is not a JSON object", server.id),
                })?;

                let mut shim_args: Vec<serde_json::Value> = vec![
                    "shim".into(),
                    "--server-id".into(),
                    server.id.clone().into(),
                    "--governance-endpoint".into(),
                    options.governance_endpoint.clone().into(),
                    "--profile".into(),
                    profile_str.into(),
                    "--".into(),
                    server.command.clone().into(),
                ];
                for arg in &server.args {
                    shim_args.push(arg.clone().into());
                }

                entry_obj.insert("command".to_string(), shim_path.clone().into());
                entry_obj.insert("args".to_string(), serde_json::Value::Array(shim_args));

                let env_obj = entry_obj
                    .entry("env")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(env_map) = env_obj.as_object_mut() {
                    env_map.insert(
                        "THOUGHTGATE_SERVER_ID".to_string(),
                        server.id.clone().into(),
                    );
                }
            }
        }

        write_config(config_path, &config)?;
        Ok(backup_path)
    }

    fn restore_config(&self, config_path: &Path, backup_path: &Path) -> Result<(), ConfigError> {
        standard_restore(config_path, backup_path)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Windsurf Adapter
// ─────────────────────────────────────────────────────────────────────────────

/// Config adapter for Windsurf (Codeium).
///
/// Windsurf uses `~/.codeium/windsurf/mcp_config.json` with `"mcpServers"` key.
/// Supports `${env:VAR}` environment variable expansion.
///
/// Implements: REQ-CORE-008/F-002, F-003, F-004
pub struct WindsurfAdapter;

impl ConfigAdapter for WindsurfAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Windsurf
    }

    /// Discovers Windsurf config path.
    ///
    /// Implements: REQ-CORE-008/F-002
    fn discover_config_path(&self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        let path = home
            .join(".codeium")
            .join("windsurf")
            .join("mcp_config.json");
        if path.exists() { Some(path) } else { None }
    }

    fn parse_servers(&self, config_path: &Path) -> Result<Vec<McpServerEntry>, ConfigError> {
        let config = read_config(config_path)?;
        let mut entries = parse_mcp_servers_key(&config, "mcpServers")?;

        // Apply ${env:VAR} expansion (F-004).
        for entry in &mut entries {
            expand_server_entry_env(entry, expand_env_colon)?;
        }

        Ok(entries)
    }

    fn rewrite_config(
        &self,
        config_path: &Path,
        servers: &[McpServerEntry],
        shim_binary: &Path,
        options: &ShimOptions,
    ) -> Result<PathBuf, ConfigError> {
        standard_rewrite(config_path, "mcpServers", servers, shim_binary, options)
    }

    fn restore_config(&self, config_path: &Path, backup_path: &Path) -> Result<(), ConfigError> {
        standard_restore(config_path, backup_path)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Zed Adapter
// ─────────────────────────────────────────────────────────────────────────────

/// Config adapter for Zed.
///
/// Zed uses `~/.config/zed/settings.json` with `"context_servers"` key.
/// The structure differs from other agents: servers are nested under
/// `context_servers.<id>.command` with `path` and `args` fields.
///
/// Implements: REQ-CORE-008/F-002, F-003
pub struct ZedAdapter;

impl ConfigAdapter for ZedAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Zed
    }

    /// Discovers Zed config path.
    ///
    /// Implements: REQ-CORE-008/F-002
    fn discover_config_path(&self) -> Option<PathBuf> {
        let config_dir = dirs::config_dir()?;
        let path = config_dir.join("zed").join("settings.json");
        if path.exists() { Some(path) } else { None }
    }

    fn parse_servers(&self, config_path: &Path) -> Result<Vec<McpServerEntry>, ConfigError> {
        let config = read_config(config_path)?;

        let servers_obj = config
            .get("context_servers")
            .and_then(|v| v.as_object())
            .ok_or_else(|| ConfigError::Parse {
                reason: "missing or invalid `context_servers` key in Zed config".to_string(),
            })?;

        let mut entries = Vec::new();
        for (id, server) in servers_obj {
            // Zed nests command info: { "command": { "path": "...", "args": [...] } }
            // or may have { "command": "..." } directly.
            let (command, args) = if let Some(cmd_obj) = server.get("command") {
                if let Some(cmd_str) = cmd_obj.as_str() {
                    // Simple string command.
                    (cmd_str.to_string(), Vec::new())
                } else if let Some(cmd_map) = cmd_obj.as_object() {
                    let path = cmd_map
                        .get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| ConfigError::Parse {
                            reason: format!(
                                "context server `{id}` has command object without `path`"
                            ),
                        })?
                        .to_string();
                    let args = cmd_map
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    (path, args)
                } else {
                    return Err(ConfigError::Parse {
                        reason: format!("context server `{id}` has invalid `command` field type"),
                    });
                }
            } else {
                return Err(ConfigError::Parse {
                    reason: format!("context server `{id}` missing `command` field"),
                });
            };

            let env = server.get("env").and_then(|v| v.as_object()).map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            });

            entries.push(McpServerEntry {
                id: id.clone(),
                command,
                args,
                env,
                enabled: true,
            });
        }

        Ok(entries)
    }

    fn rewrite_config(
        &self,
        config_path: &Path,
        servers: &[McpServerEntry],
        shim_binary: &Path,
        options: &ShimOptions,
    ) -> Result<PathBuf, ConfigError> {
        let mut config = read_config(config_path)?;

        if detect_double_wrap(&config, "context_servers", shim_binary) {
            return Err(ConfigError::AlreadyManaged);
        }

        let backup_path = backup_config(config_path)?;

        let servers_obj = config
            .get_mut("context_servers")
            .and_then(|v| v.as_object_mut())
            .ok_or_else(|| ConfigError::Parse {
                reason: "missing `context_servers` key".to_string(),
            })?;

        let shim_path = shim_binary.to_string_lossy().to_string();
        let profile_str = match options.profile {
            Profile::Production => "production",
            Profile::Development => "development",
        };

        for server in servers {
            if let Some(entry) = servers_obj.get_mut(&server.id) {
                let entry_obj = entry.as_object_mut().ok_or_else(|| ConfigError::Parse {
                    reason: format!("server `{}` is not a JSON object", server.id),
                })?;

                // Build shim args for Zed's command.args format.
                let mut shim_args: Vec<serde_json::Value> = vec![
                    "shim".into(),
                    "--server-id".into(),
                    server.id.clone().into(),
                    "--governance-endpoint".into(),
                    options.governance_endpoint.clone().into(),
                    "--profile".into(),
                    profile_str.into(),
                    "--".into(),
                    server.command.clone().into(),
                ];
                for arg in &server.args {
                    shim_args.push(arg.clone().into());
                }

                // Zed uses { "command": { "path": "...", "args": [...] } }
                entry_obj.insert(
                    "command".to_string(),
                    serde_json::json!({
                        "path": shim_path,
                        "args": shim_args,
                    }),
                );

                // Add THOUGHTGATE_SERVER_ID to env (create env if absent).
                let env_obj = entry_obj
                    .entry("env")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(env_map) = env_obj.as_object_mut() {
                    env_map.insert(
                        "THOUGHTGATE_SERVER_ID".to_string(),
                        server.id.clone().into(),
                    );
                }
            }
        }

        write_config(config_path, &config)?;
        Ok(backup_path)
    }

    fn restore_config(&self, config_path: &Path, backup_path: &Path) -> Result<(), ConfigError> {
        standard_restore(config_path, backup_path)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
            config_path: PathBuf::from("thoughtgate.yaml"),
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
            config_path: PathBuf::from("thoughtgate.yaml"),
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
            config_path: PathBuf::from("thoughtgate.yaml"),
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
            config_path: PathBuf::from("thoughtgate.yaml"),
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
            config_path: PathBuf::from("thoughtgate.yaml"),
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
            config_path: PathBuf::from("thoughtgate.yaml"),
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
            config_path: PathBuf::from("thoughtgate.yaml"),
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
