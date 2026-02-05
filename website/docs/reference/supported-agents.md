---
sidebar_position: 3
---

# Supported Agents

Complete reference for all MCP agent types supported by `thoughtgate wrap`, including config file locations, JSON formats, and special behaviors.

## Summary

| Agent | Command | Config Path (macOS) | Config Path (Linux) | JSON Key | Env Expansion |
|-------|---------|---------------------|---------------------|-----------|----|
| Claude Code | `claude`, `claude-code` | `~/.claude.json` + `.mcp.json` | Same | `projects.<cwd>.mcpServers` | `${VAR}`, `${VAR:-default}` |
| Claude Desktop | `claude-desktop`, `Claude.app` | `~/Library/Application Support/Claude/claude_desktop_config.json` | `~/.config/Claude/claude_desktop_config.json` | `mcpServers` | None |
| Cursor | `cursor` | `~/.cursor/mcp.json` + `.cursor/mcp.json` | Same | `mcpServers` | None |
| VS Code | `code`, `code-insiders` | `.vscode/mcp.json` | Same | `servers` | None |
| Windsurf | `windsurf` | `~/.codeium/windsurf/mcp_config.json` | Same | `mcpServers` | `${env:VAR}` |
| Zed | `zed` | `~/.config/zed/settings.json` | Same | `context_servers` | None |

## Per-Agent Details

### Claude Code

**Config locations:**
1. **User config:** `~/.claude.json` — per-project servers under `projects.<cwd>.mcpServers`
2. **Project config:** `.mcp.json` in the working directory — merged with user config

**JSON structure (`~/.claude.json`):**

```json
{
  "projects": {
    "/Users/alice/my-project": {
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
      },
      "enabledMcpjsonServers": [],
      "disabledMcpjsonServers": []
    }
  }
}
```

**JSON structure (`.mcp.json`):**

```json
{
  "mcpServers": {
    "sqlite": {
      "command": "uvx",
      "args": ["mcp-server-sqlite", "--db-path", "${DB_PATH:-/tmp/dev.db}"]
    }
  }
}
```

**Special behaviors:**
- Non-stdio servers (`"type": "http"`, `"type": "sse"`, `"type": "webSocket"`) are **skipped** — ThoughtGate only wraps stdio servers
- `.mcp.json` entries override `~/.claude.json` entries by server ID
- `enabledMcpjsonServers` / `disabledMcpjsonServers` arrays control which `.mcp.json` servers are active
- Supports `${VAR}` and `${VAR:-default}` environment variable expansion in `command` and `args`

### Claude Desktop

**Config location:** `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `~/.config/Claude/claude_desktop_config.json` (Linux)

**JSON structure:**

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
      "env": { "NODE_ENV": "production" }
    }
  }
}
```

**Special behaviors:**
- Auto-detected from `Claude.app` in the command path or `claude-desktop` basename
- All servers under `mcpServers` are treated as stdio

### Cursor

**Config locations:**
1. **Global:** `~/.cursor/mcp.json`
2. **Project:** `.cursor/mcp.json` in the working directory

**JSON structure:**

```json
{
  "mcpServers": {
    "sqlite": {
      "command": "uvx",
      "args": ["mcp-server-sqlite", "--db-path", "/tmp/test.db"]
    }
  }
}
```

**Special behaviors:**
- Project-level entries override global entries by server ID
- Both files use the same `mcpServers` key format

### VS Code

**Config location:** `.vscode/mcp.json` in the workspace

**JSON structure:**

```json
{
  "servers": {
    "filesystem": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    },
    "remote-api": {
      "type": "sse",
      "url": "http://api-server:8080/sse"
    }
  }
}
```

**Special behaviors:**
- Uses `servers` key (not `mcpServers`)
- Each server has an explicit `type` field
- **Only `"type": "stdio"` servers are wrapped** — SSE and other types are ignored
- No environment variable expansion

### Windsurf

**Config location:** `~/.codeium/windsurf/mcp_config.json`

**JSON structure:**

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "${env:HOME}/documents"]
    }
  }
}
```

**Special behaviors:**
- Uses `${env:VAR}` syntax for environment variable expansion (different from Claude Code's `${VAR}` syntax)
- Plain `${VAR}` patterns without the `env:` prefix are left unchanged

### Zed

**Config location:** `~/.config/zed/settings.json`

**JSON structure:**

```json
{
  "context_servers": {
    "filesystem": {
      "command": {
        "path": "npx",
        "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
      },
      "env": { "NODE_ENV": "production" }
    }
  }
}
```

**Special behaviors:**
- Uses `context_servers` key (unique naming)
- Command is nested: `{ "command": { "path": "...", "args": [...] } }` instead of `{ "command": "...", "args": [...] }`
- Also supports simple string commands: `{ "command": "my-server" }`

## Config Rewrite Mechanics

When `thoughtgate wrap` rewrites a config, each stdio server entry is transformed:

**Before:**

```json
{
  "filesystem": {
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
  }
}
```

**After:**

```json
{
  "filesystem": {
    "command": "/path/to/thoughtgate",
    "args": [
      "shim",
      "--server-id", "filesystem",
      "--governance-endpoint", "http://127.0.0.1:54321",
      "--profile", "production",
      "--",
      "npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"
    ],
    "env": {
      "THOUGHTGATE_SERVER_ID": "filesystem"
    }
  }
}
```

The original command and args are preserved after the `--` separator, allowing the shim to spawn the original server process.

## Overriding Detection

If auto-detection fails or picks the wrong agent:

```bash
# Explicit agent type
thoughtgate wrap --agent-type cursor -- /usr/local/bin/my-cursor-fork

# Explicit config path
thoughtgate wrap --config-path ~/custom/mcp.json -- claude-code

# Both overrides (required for custom agents)
thoughtgate wrap --agent-type custom --config-path ./mcp-config.json -- my-agent
```

## Double-Wrap Prevention

ThoughtGate detects if a config has already been rewritten by checking if any server's `command` points to the `thoughtgate` binary. If detected, it exits with:

```
Error: Config already managed by ThoughtGate — run the agent directly or restore with `thoughtgate unwrap`
```

The `THOUGHTGATE_ACTIVE=1` environment variable also prevents nested wrapping at the process level.

## Config Backup and Restore

| File | Purpose |
|------|---------|
| `<config>.thoughtgate-backup` | Byte-for-byte copy of the original config. Created before rewrite, verified after copy. |
| `<config>.thoughtgate-lock` | Advisory lock file preventing concurrent ThoughtGate instances from modifying the same config. |

On exit (normal, SIGTERM, SIGINT, or panic), ThoughtGate automatically restores the original config from the backup file. Use `--no-restore` to skip restoration.
