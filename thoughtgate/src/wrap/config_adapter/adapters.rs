//! Agent-specific ConfigAdapter implementations.
//!
//! Implements: REQ-CORE-008/F-002, F-003, F-004, F-005, F-006

use std::path::{Path, PathBuf};

use thoughtgate_core::profile::Profile;

use super::env::expand_server_entry_env;
use super::helpers::{
    backup_config, detect_double_wrap, parse_mcp_servers_key, read_config, standard_restore,
    standard_rewrite, write_config,
};
use super::{
    AgentType, ConfigAdapter, ConfigError, McpServerEntry, ShimOptions, expand_dollar_brace,
    expand_env_colon,
};

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
    pub(super) fn parse_project_servers(
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
    pub(super) fn is_mcp_json_server_enabled(
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
    pub(super) fn detect_project_double_wrap(
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
