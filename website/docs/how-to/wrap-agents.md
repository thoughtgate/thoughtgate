---
sidebar_position: 3
---

# Wrap MCP Agents

How to use `thoughtgate wrap` with each supported agent type.

## How Auto-Detection Works

ThoughtGate detects the agent type from the command basename (the last component of the command path):

```bash
# Auto-detected as Claude Code (basename: "claude-code")
thoughtgate wrap -- claude-code

# Auto-detected as Cursor (basename: "cursor")
thoughtgate wrap -- cursor

# Auto-detected as VS Code (basename: "code")
thoughtgate wrap -- code

# Full paths work too (basename: "claude-code")
thoughtgate wrap -- /usr/local/bin/claude-code
```

If detection fails, ThoughtGate exits with an error. Use `--agent-type` to override.

## Claude Code

```bash
thoughtgate wrap -- claude-code
# or
thoughtgate wrap -- claude
```

**Config file:** `~/.claude.json` (per-project servers under `projects.<cwd>.mcpServers`)

**What happens:**
1. ThoughtGate reads `~/.claude.json` and finds servers for the current working directory
2. Merges any `.mcp.json` servers from the project directory
3. Filters to stdio-only servers (HTTP/SSE servers are skipped)
4. Expands `${VAR}` and `${VAR:-default}` environment variable references
5. Rewrites each server's `command` to route through the ThoughtGate shim
6. Launches Claude Code with the rewritten config

**Notes:**
- Non-stdio servers (`"type": "http"`, `"type": "sse"`) are left unchanged — ThoughtGate only governs stdio servers
- `.mcp.json` entries override `~/.claude.json` entries when they share the same server ID
- The `enabledMcpjsonServers` and `disabledMcpjsonServers` arrays in `~/.claude.json` are respected

## Claude Desktop

```bash
thoughtgate wrap -- claude-desktop
# or for macOS app bundle:
thoughtgate wrap -- /Applications/Claude.app/Contents/MacOS/Claude
```

**Config file:** `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `~/.config/Claude/claude_desktop_config.json` (Linux)

**What happens:**
1. ThoughtGate reads the Claude Desktop config
2. Rewrites all servers under `mcpServers`
3. Launches Claude Desktop

## Cursor

```bash
thoughtgate wrap -- cursor
```

**Config files:**
- Global: `~/.cursor/mcp.json`
- Project: `.cursor/mcp.json` (in working directory)

**What happens:**
1. ThoughtGate reads the global config
2. Merges project-level overrides (project entries override global by server ID)
3. Rewrites all servers under `mcpServers`
4. Launches Cursor

## VS Code

```bash
thoughtgate wrap -- code
# or for Insiders:
thoughtgate wrap -- code-insiders
```

**Config file:** `.vscode/mcp.json` (workspace-level only)

**What happens:**
1. ThoughtGate reads the workspace MCP config
2. Filters to `"type": "stdio"` entries only (SSE and other types are skipped)
3. Rewrites under the `servers` key (note: VS Code uses `servers`, not `mcpServers`)
4. Launches VS Code

**Note:** VS Code's MCP config uses `"servers"` as the top-level key, not `"mcpServers"`.

## Windsurf

```bash
thoughtgate wrap -- windsurf
```

**Config file:** `~/.codeium/windsurf/mcp_config.json`

**What happens:**
1. ThoughtGate reads the Windsurf config
2. Expands `${env:VAR}` environment variable references (Windsurf's syntax)
3. Rewrites all servers under `mcpServers`
4. Launches Windsurf

**Note:** Windsurf uses `${env:VAR}` syntax (with `env:` prefix), unlike Claude Code's `${VAR}` syntax. Plain `${VAR}` without the prefix is left unchanged.

## Zed

```bash
thoughtgate wrap -- zed
```

**Config file:** `~/.config/zed/settings.json`

**What happens:**
1. ThoughtGate reads the Zed settings
2. Parses servers under `context_servers` (Zed's unique key name)
3. Handles Zed's nested command format: `{ "command": { "path": "...", "args": [...] } }`
4. Launches Zed

## Custom Agents

For agents not in the supported list, use `--agent-type custom` with `--config-path`:

```bash
thoughtgate wrap \
  --agent-type custom \
  --config-path ./my-agent-config.json \
  -- ./my-agent
```

The custom agent's config must use the `mcpServers` key format (same as Claude Desktop / Cursor).

## Overriding Detection

### Override Agent Type

```bash
# Force Cursor adapter for a forked build
thoughtgate wrap --agent-type cursor -- /opt/cursor-fork/cursor
```

### Override Config Path

```bash
# Use a specific config file instead of auto-discovered path
thoughtgate wrap --config-path ~/testing/mcp.json -- claude-code
```

## Linux Paths

On Linux, ThoughtGate follows XDG conventions where applicable:

| Agent | macOS Path | Linux Path |
|-------|------------|------------|
| Claude Desktop | `~/Library/Application Support/Claude/` | `~/.config/Claude/` |
| Zed | `~/Library/Application Support/zed/` | `~/.config/zed/` |
| Others | Home directory | Same as macOS |

## Double-Wrap Prevention

If you accidentally try to wrap an already-wrapped agent, ThoughtGate detects it and exits:

```
Error: Config already managed by ThoughtGate — run the agent directly or restore with `thoughtgate unwrap`
```

Detection works two ways:
1. **Config-level:** Checks if any server's `command` already points to the `thoughtgate` binary
2. **Process-level:** Checks for the `THOUGHTGATE_ACTIVE=1` environment variable

## Config Backup and Restore

When wrapping, ThoughtGate creates a backup of the original config:

| File | Purpose |
|------|---------|
| `<config>.thoughtgate-backup` | Byte-for-byte copy of the original config |
| `<config>.thoughtgate-lock` | Advisory lock preventing concurrent modifications |

On exit (normal, SIGTERM, SIGINT, or panic), the original config is automatically restored from the backup.

To skip automatic restoration:

```bash
thoughtgate wrap --no-restore -- claude-code
```

## See Also

- [CLI Reference](/docs/reference/cli) — Complete flag documentation
- [Supported Agents](/docs/reference/supported-agents) — Config format details
- [Use Profiles](/docs/how-to/use-profiles) — Development vs production modes
