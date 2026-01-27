---
sidebar_position: 1
---

# Your First ThoughtGate Proxy

In this tutorial, you'll set up ThoughtGate to proxy requests between an AI agent and an MCP server, with governance rules that require approval for destructive operations.

## What You'll Learn

- How to run ThoughtGate as a proxy
- How to write YAML governance rules
- How to configure Slack approvals
- How the SEP-1686 task flow works

## Prerequisites

- Rust 1.75+ installed
- An MCP server running (or use the mock server)
- A Slack workspace with bot token (for approvals)

## Step 1: Build ThoughtGate

Clone the repository and build the release binary:

```bash
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo build --release
```

The binary will be at `target/release/thoughtgate`.

## Step 2: Start a Mock MCP Server

For testing, ThoughtGate includes a mock MCP server:

```bash
cargo build --release --bin mock_mcp --features mock
MOCK_MCP_PORT=3000 ./target/release/mock_mcp
```

This server responds to MCP tool calls with mock data.

## Step 3: Create a Configuration File

Create `thoughtgate.yaml` with governance rules:

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://localhost:3000

governance:
  defaults:
    action: forward  # Allow by default
  rules:
    # Require approval for destructive operations
    - match: "delete_*"
      action: approve
    - match: "drop_*"
      action: approve
    # Block admin tools entirely
    - match: "admin_*"
      action: deny

approval:
  default:
    adapter: slack
    channel: "#thoughtgate-approvals"
    timeout: 5m
```

## Step 4: Configure Slack Integration

1. Create a Slack app at [api.slack.com/apps](https://api.slack.com/apps)
2. Add Bot Token Scopes:
   - `chat:write` — Post approval messages
   - `reactions:read` — Detect approval reactions
   - `channels:history` — Poll channel for reactions
   - `users:read` — Resolve user display names
3. Install to your workspace
4. Copy the Bot OAuth Token
5. Create a channel for approvals (e.g., `#thoughtgate-approvals`)
6. Invite the bot: `/invite @YourBotName`

## Step 5: Start ThoughtGate

Set environment variables and start the proxy:

```bash
export THOUGHTGATE_CONFIG=./thoughtgate.yaml
export SLACK_BOT_TOKEN=xoxb-your-token

./target/release/thoughtgate
```

You should see:

```
ThoughtGate listening on 0.0.0.0:7467
Admin server on 0.0.0.0:7469
Upstream: http://localhost:3000
```

## Step 6: Test the Proxy

Send a `tools/list` request (should pass through):

```bash
curl -X POST http://localhost:7467 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc": "2.0", "method": "tools/list", "id": 1}'
```

Send a safe `tools/call` request (should pass through):

```bash
curl -X POST http://localhost:7467 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {"name": "get_weather", "arguments": {"city": "London"}},
    "id": 2
  }'
```

## Step 7: Trigger an Approval

Send a destructive `tools/call` request:

```bash
curl -X POST http://localhost:7467 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {"name": "delete_user", "arguments": {"user_id": "12345"}},
    "id": 3
  }'
```

With SEP-1686 async tasks, you'll receive a task ID immediately:

```json
{
  "jsonrpc": "2.0",
  "result": {
    "taskId": "tg_abc123xyz",
    "status": "working"
  },
  "id": 3
}
```

Check your Slack channel — you should see a message like:

```
🔒 Approval Required: delete_user

Tool: delete_user
Principal: unknown
Arguments:
{
  "user_id": "12345"
}

React with 👍 to approve or 👎 to reject

Task ID: tg_abc123xyz • Expires: 2024-01-15 10:30 UTC
```

## Step 8: Poll for the Result

The agent polls for the result using `tasks/get`:

```bash
curl -X POST http://localhost:7467 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tasks/get",
    "params": {"taskId": "tg_abc123xyz"},
    "id": 4
  }'
```

While waiting for approval:

```json
{
  "jsonrpc": "2.0",
  "result": {
    "taskId": "tg_abc123xyz",
    "status": "working",
    "statusMessage": "Waiting for human approval"
  },
  "id": 4
}
```

React with 👍 in Slack, then poll again:

```json
{
  "jsonrpc": "2.0",
  "result": {
    "taskId": "tg_abc123xyz",
    "status": "completed"
  },
  "id": 4
}
```

Then fetch the result with `tasks/result`:

```bash
curl -X POST http://localhost:7467 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tasks/result",
    "params": {"taskId": "tg_abc123xyz"},
    "id": 5
  }'
```

## Step 9: Verify Health

Check the health endpoints on the admin port:

```bash
curl http://localhost:7469/health
curl http://localhost:7469/ready
```

## What's Next?

- Learn how to [write more complex policies](/docs/how-to/write-policies)
- Understand the [4-Gate architecture](/docs/explanation/architecture)
- [Deploy to Kubernetes](/docs/how-to/deploy-kubernetes) for production
