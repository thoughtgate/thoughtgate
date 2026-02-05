---
sidebar_position: 5
---

# Use Profiles

ThoughtGate supports two configuration profiles that control how governance decisions are enforced. Profiles let you safely roll out governance rules by starting in development mode (observe-only) before switching to production mode (enforcing).

## When to Use Each Profile

| Scenario | Profile | Why |
|----------|---------|-----|
| First time wrapping an agent | `development` | See what would be blocked without disrupting work |
| Testing new governance rules | `development` | Validate rules before enforcing |
| Production deployment | `production` | Full enforcement with Slack approvals |
| CI/CD pipelines | `production` | Ensure governance is enforced |

## Behavior Comparison

| Behavior | Production | Development |
|----------|-----------|-------------|
| Governance enforcement | **Blocking** — denied requests return errors | **Log-only** — denied requests are forwarded with a warning |
| Approval workflows | **Required** — Slack approval must be granted | **Auto-approved** — logged but not blocked |
| Decision log prefix | `BLOCKED` | `WOULD_BLOCK` |
| NDJSON smuggling detection | **Reject** — smuggled messages are dropped | **WOULD_REJECT** — logged but forwarded |
| Governance errors | **Fail-closed** — errors result in denial | **Fail-open** — errors result in forwarding |

## Starting with Development Mode

Use `--profile development` to observe governance decisions without blocking:

```bash
thoughtgate wrap --profile development -- claude-code
```

In your logs, you'll see entries like:

```
WOULD_BLOCK tool=delete_user rule=delete_* action=deny
WOULD_BLOCK tool=admin_console rule=admin_* action=deny
```

These tell you which requests **would** be blocked in production, without actually blocking them.

## Reading WOULD_BLOCK Logs

Each `WOULD_BLOCK` entry includes:

| Field | Description |
|-------|-------------|
| `tool` | The tool name that triggered the rule |
| `rule` | The governance rule pattern that matched |
| `action` | What would happen in production (`deny`, `approve`) |

Use these logs to:
1. Verify your rules match the expected tools
2. Check for false positives (rules that are too broad)
3. Ensure critical tools are governed

## Switching to Production

Once you're confident in your rules, switch to production:

```bash
thoughtgate wrap --profile production -- claude-code
```

Or simply omit `--profile` (production is the default):

```bash
thoughtgate wrap -- claude-code
```

## Profile Effects

### Governance Decisions

In **production**, a deny rule returns an error to the agent:

```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": -32003,
    "message": "Denied by ThoughtGate policy"
  },
  "id": 1
}
```

In **development**, the same request is forwarded to the upstream server with a log warning.

### Approval Adapter

In **production**, approval rules create Slack messages and wait for human reactions (👍/👎).

In **development**, approvals are auto-approved immediately with an audit log entry. No Slack messages are posted. This means you **don't need a Slack token** in development mode.

### NDJSON Smuggling Detection

In **production**, detected smuggling attempts cause the message to be rejected.

In **development**, smuggling is logged as `WOULD_REJECT` but the message is still forwarded.

### Governance Errors

If ThoughtGate encounters an error evaluating a governance rule:

- **Production:** Fails closed — the request is denied (fail-safe)
- **Development:** Fails open — the request is forwarded with an error log

## See Also

- [CLI Reference](/docs/reference/cli) — `--profile` flag documentation
- [Wrap MCP Agents](/docs/how-to/wrap-agents) — Agent-specific wrapping guides
- [Security Model](/docs/explanation/security-model) — Profile security implications
