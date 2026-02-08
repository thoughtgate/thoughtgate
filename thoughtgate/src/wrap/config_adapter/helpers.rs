//! Shared config file helpers: read, write, backup, rewrite, restore.
//!
//! Implements: REQ-CORE-008/F-003, F-005, F-006

use std::path::{Path, PathBuf};

use thoughtgate_core::profile::Profile;

use super::{ConfigError, McpServerEntry, ShimOptions};

/// Write content atomically using temp-file + rename.
///
/// POSIX `rename(2)` is atomic on the same filesystem, so readers never see
/// a half-written file. On failure the temp file is cleaned up best-effort.
fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let temp_path = path.with_extension("tmp");
    std::fs::write(&temp_path, content)?;
    if let Err(e) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(e);
    }
    Ok(())
}

/// Read a config file and parse as JSON.
pub(super) fn read_config(path: &Path) -> Result<serde_json::Value, ConfigError> {
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
///
/// Uses atomic temp-file + rename to prevent corruption on crash.
pub(super) fn write_config(path: &Path, value: &serde_json::Value) -> Result<(), ConfigError> {
    let content = serde_json::to_string_pretty(value).map_err(|e| ConfigError::Write {
        reason: e.to_string(),
    })?;
    atomic_write(path, content.as_bytes()).map_err(|e| ConfigError::Write {
        reason: e.to_string(),
    })
}

/// Create backup at `<path>.thoughtgate-backup`, verify, return backup path.
///
/// Uses atomic temp-file + rename to prevent corruption on crash.
///
/// Implements: REQ-CORE-008/F-005
pub(super) fn backup_config(config_path: &Path) -> Result<PathBuf, ConfigError> {
    let mut backup_path = config_path.as_os_str().to_os_string();
    backup_path.push(".thoughtgate-backup");
    let backup_path = PathBuf::from(backup_path);

    // Read original, then atomic-write to backup.
    let original = std::fs::read(config_path)?;
    atomic_write(&backup_path, &original).map_err(|e| ConfigError::Write {
        reason: format!("backup write failed: {e}"),
    })?;

    // Verify backup was written successfully.
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
pub(super) fn parse_mcp_servers_key(
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
pub(super) fn rewrite_mcp_servers_key(
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

            // Add THOUGHTGATE_SERVER_ID and THOUGHTGATE_CONFIG to env.
            let env_obj = entry_obj
                .entry("env")
                .or_insert_with(|| serde_json::json!({}));
            if let Some(env_map) = env_obj.as_object_mut() {
                env_map.insert(
                    "THOUGHTGATE_SERVER_ID".to_string(),
                    server.id.clone().into(),
                );
                if let Some(ref cfg_path) = options.config_path {
                    env_map.insert(
                        "THOUGHTGATE_CONFIG".to_string(),
                        cfg_path.to_string_lossy().into_owned().into(),
                    );
                }
            }
        }
    }

    Ok(())
}

/// Check if any server command already points to the thoughtgate binary.
///
/// Implements: REQ-CORE-008/F-005 (double-wrap detection)
pub(super) fn detect_double_wrap(
    config: &serde_json::Value,
    key: &str,
    shim_binary: &Path,
) -> bool {
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
pub(super) fn standard_rewrite(
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

/// Shared restore logic: atomic-write backup content over config, remove backup.
pub(super) fn standard_restore(config_path: &Path, backup_path: &Path) -> Result<(), ConfigError> {
    let content = std::fs::read(backup_path)?;
    atomic_write(config_path, &content).map_err(|e| ConfigError::Write {
        reason: format!("restore write failed: {e}"),
    })?;
    // Best-effort removal of backup.
    let _ = std::fs::remove_file(backup_path);
    Ok(())
}
