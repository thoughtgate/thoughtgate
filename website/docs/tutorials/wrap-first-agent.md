---
sidebar_position: 1
---

# Wrap Your First Agent

In this tutorial, you'll wrap Claude Code with ThoughtGate to govern its MCP tool calls. You'll start with a passthrough config, add deny rules, and try development mode — all without modifying Claude Code's source.

## What You'll Learn

- How `thoughtgate wrap` discovers and rewrites agent configs
- How governance rules control tool calls
- How `--dry-run` previews changes safely
- How profiles let you observe before enforcing

## Prerequisites

- **Rust 1.87+** installed (`rustup update` if needed)
- **Claude Code** installed and working (`claude --version`)
- At least one MCP server configured in Claude Code (check `~/.claude.json`)

## Step 1: Install ThoughtGate

Build and install the CLI binary:

```bash
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo install --path thoughtgate
```

Verify the installation:

```bash
thoughtgate --help
```

You should see the `wrap` and `shim` subcommands listed.

## Step 2: Create a Minimal Config

Create `thoughtgate.yaml` in your project directory with a passthrough configuration:

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://localhost:3000  # Unused in stdio mode, but required by schema

governance:
  defaults:
    action: forward  # Forward everything — no blocking yet
```

This config forwards all tool calls without any governance. We'll add rules shortly.

## Step 3: Preview with Dry Run

Before making any changes, use `--dry-run` to see what ThoughtGate would do:

```bash
thoughtgate wrap --dry-run -- claude-code
```

ThoughtGate will:
1. Auto-detect Claude Code from the command name
2. Find `~/.claude.json` and read your MCP server configs
3. Print the config diff showing how each server's `command` would be rewritten

```
Detected agent: ClaudeCode
Config file: /Users/you/.claude.json
Servers found: 2 (filesystem, github)

--- original
+++ rewritten
  "filesystem": {
-   "command": "npx",
-   "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
+   "command": "/Users/you/.cargo/bin/thoughtgate",
+   "args": ["shim", "--server-id", "filesystem", "--governance-endpoint",
+            "http://127.0.0.1:0", "--profile", "production",
+            "--", "npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
  }
```

No files are modified during a dry run.

## Step 4: Wrap the Agent

Run Claude Code through ThoughtGate:

```bash
thoughtgate wrap -- claude-code
```

ThoughtGate will:
1. Back up `~/.claude.json` to `~/.claude.json.thoughtgate-backup`
2. Start the governance HTTP service on an ephemeral port
3. Rewrite your MCP server configs to route through ThoughtGate shims
4. Launch Claude Code
5. Restore the original config when you exit

Use Claude Code normally. All tool calls pass through because `defaults.action: forward`.

## Step 5: Observe Governance Logs

While Claude Code is running, watch the ThoughtGate logs. Every tool call is logged:

```
INFO  governance: tool=read_file server=filesystem action=forward
INFO  governance: tool=list_files server=filesystem action=forward
```

This confirms ThoughtGate is intercepting and forwarding tool calls.

## Step 6: Add a Deny Rule

Exit Claude Code (Ctrl+C or quit normally — ThoughtGate restores your config automatically).

Update `thoughtgate.yaml` to deny admin tools:

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://localhost:3000

governance:
  defaults:
    action: forward
  rules:
    - match: "admin_*"
      action: deny
    - match: "delete_*"
      action: deny
```

Re-wrap and try calling a denied tool:

```bash
thoughtgate wrap -- claude-code
```

If the agent tries to call `delete_user` or `admin_reset`, it receives a `-32003 PolicyDenied` error instead of the tool executing.

## Step 7: Try Development Mode

Development mode lets you observe what **would** be blocked without actually blocking:

```bash
thoughtgate wrap --profile development -- claude-code
```

Now when a denied tool is called, you'll see:

```
WOULD_BLOCK tool=delete_user rule=delete_* action=deny
```

But the call still goes through to the server. This is perfect for testing rules before enforcing them.

## Step 8: Switch to Production

Once you're satisfied with your rules, switch to production (the default):

```bash
thoughtgate wrap -- claude-code
```

In production mode:
- **Deny rules** return errors to the agent
- **Approve rules** would create Slack approval workflows (requires Slack token)
- **Governance errors** fail closed (deny on error)

## What's Next

- [Wrap other agents](/docs/how-to/wrap-agents) — Cursor, VS Code, Windsurf, Zed, Claude Desktop
- [Write governance policies](/docs/how-to/write-policies) — Complex rules with Cedar
- [Use profiles](/docs/how-to/use-profiles) — Deep dive on development vs production
- [Architecture](/docs/explanation/architecture) — How the 4-gate model works
- [CLI Reference](/docs/reference/cli) — Complete flag documentation
