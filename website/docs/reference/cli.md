---
sidebar_position: 1
---

# CLI Reference

Complete reference for the `thoughtgate` command-line interface.

## `thoughtgate wrap`

Wraps an MCP agent: discovers its config file, rewrites server commands to route through ThoughtGate shim proxies, starts the governance service, launches the agent, and restores the original config on exit.

### Usage

```bash
thoughtgate wrap [OPTIONS] -- <COMMAND>...
```

### Arguments

| Argument | Description |
|----------|-------------|
| `<COMMAND>...` | Agent command and arguments (after `--`). Required. |

### Options

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--agent-type <TYPE>` | enum | auto-detect | Override auto-detected agent type. Values: `claude-code`, `claude-desktop`, `cursor`, `vscode`, `windsurf`, `zed`, `custom`. |
| `--config-path <PATH>` | path | auto-discover | Override auto-detected agent config file path. |
| `--profile <PROFILE>` | enum | `production` | Configuration profile. Values: `production`, `development`. |
| `--thoughtgate-config <PATH>` | path | `thoughtgate.yaml` | Path to ThoughtGate governance config file. |
| `--governance-port <PORT>` | u16 | `0` | Port for the governance HTTP service. `0` = OS-assigned ephemeral port. |
| `--no-restore` | flag | `false` | Don't restore original agent config on exit. |
| `--dry-run` | flag | `false` | Print rewritten config diff without writing. Useful for previewing changes. |
| `--verbose` | flag | `false` | Enable debug logging. |

### Examples

```bash
# Basic wrap (auto-detects Claude Code, uses thoughtgate.yaml)
thoughtgate wrap -- claude-code

# Dry run — preview config changes without writing
thoughtgate wrap --dry-run -- claude-code

# Development profile (log-only, no blocking)
thoughtgate wrap --profile development -- claude-code

# Explicit agent type with custom config path
thoughtgate wrap --agent-type cursor --config-path ~/.cursor/mcp.json -- cursor

# Custom ThoughtGate config
thoughtgate wrap --thoughtgate-config /etc/thoughtgate/production.yaml -- claude-code

# Verbose logging for debugging
thoughtgate wrap --verbose -- claude-code
```

## `thoughtgate shim`

Per-server stdio shim proxy. **Not intended for direct invocation** — this subcommand is injected into agent config files by `thoughtgate wrap`.

### Usage

```bash
thoughtgate shim --server-id <ID> --governance-endpoint <URL> --profile <PROFILE> -- <COMMAND>...
```

### Options

| Flag | Type | Description |
|------|------|-------------|
| `--server-id <ID>` | string | Server identifier (from config rewrite). |
| `--governance-endpoint <URL>` | string | ThoughtGate governance service URL. |
| `--profile <PROFILE>` | enum | Configuration profile (`production` or `development`). |
| `<COMMAND>...` | args | Original server command and arguments (after `--`). |

## Environment Variables

### Set by ThoughtGate

These are injected into the subprocess environment by `thoughtgate wrap`:

| Variable | Description |
|----------|-------------|
| `THOUGHTGATE_ACTIVE=1` | Indicates the process is managed by ThoughtGate. Used for double-wrap detection. |
| `THOUGHTGATE_SERVER_ID` | Server identifier for the current shim. Injected into agent config `env` block. |
| `THOUGHTGATE_GOVERNANCE_ENDPOINT` | URL of the governance HTTP service (e.g., `http://127.0.0.1:54321`). |

:::note
These variables are **scrubbed** from the agent subprocess environment to prevent leakage to child processes.
:::

### Read by ThoughtGate

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTGATE_CONFIG` | — | Path to YAML config (sidecar mode only; CLI uses `--thoughtgate-config`). |
| `THOUGHTGATE_OUTBOUND_PORT` | `7467` | Proxy port (sidecar mode only). |
| `THOUGHTGATE_ADMIN_PORT` | `7469` | Admin port (sidecar mode only). |
| `THOUGHTGATE_SLACK_BOT_TOKEN` | — | Slack Bot OAuth token for approval workflows. |
| `THOUGHTGATE_SLACK_CHANNEL` | `#approvals` | Default Slack channel for approvals. |
| `THOUGHTGATE_APPROVAL_TIMEOUT_SECS` | `300` | Default approval timeout in seconds. |
| `THOUGHTGATE_REQUEST_TIMEOUT_SECS` | `300` | Default upstream request timeout in seconds. |
| `THOUGHTGATE_MAX_BATCH_SIZE` | `100` | Maximum Slack batch polling size. |
| `THOUGHTGATE_ENVIRONMENT` | `production` | Deployment environment label (for telemetry). |
| `THOUGHTGATE_TELEMETRY_ENABLED` | `false` | Enable OTLP trace export. |

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Agent exited successfully, config restored. |
| `1` | ThoughtGate error (config parse failure, governance service failed to start). |
| `2` | CLI argument error. |
| Other | Propagated from the wrapped agent process. |

## Signal Handling

ThoughtGate installs signal handlers for graceful shutdown:

| Signal | Behavior |
|--------|----------|
| `SIGTERM` | Restore original config, shut down governance service, forward to agent. |
| `SIGINT` (Ctrl+C) | Same as SIGTERM. |
| Panic | Panic hook restores config before unwinding. |

On shutdown, ThoughtGate:

1. Restores the original agent config from `.thoughtgate-backup`
2. Sends shutdown signal to all shim processes
3. Waits for shim graceful shutdown (stdin close → SIGTERM → SIGKILL)
4. Exits with the agent's exit code

## Agent Auto-Detection

When `--agent-type` is not specified, ThoughtGate detects the agent from the command basename:

| Basename | Detected Type |
|----------|---------------|
| `claude`, `claude-code` | Claude Code |
| `claude-desktop`, `*/Claude.app/*` | Claude Desktop |
| `cursor` | Cursor |
| `code`, `code-insiders` | VS Code |
| `windsurf` | Windsurf |
| `zed` | Zed |

If detection fails, ThoughtGate exits with an error suggesting `--agent-type`.

See [Supported Agents](/docs/reference/supported-agents) for config file locations and formats.
