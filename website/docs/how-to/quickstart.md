---
sidebar_position: 1
---

# Quickstart

Get ThoughtGate running in under 5 minutes.

## Prerequisites

- Rust 1.87+ or Docker
- An MCP agent with stdio servers configured (for Option 1)
- An MCP server to proxy (for Option 2)

## Option 1: CLI Wrapper (Local Development)

The fastest way to get started. Wraps your existing MCP agent with governance.

```bash
# Clone and build the CLI
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo install --path thoughtgate

# Create a minimal config
cat > thoughtgate.yaml << 'EOF'
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://localhost:3000

governance:
  defaults:
    action: forward
EOF

# Wrap Claude Code (or any supported agent)
thoughtgate wrap -- claude-code
```

ThoughtGate auto-detects the agent type, rewrites its MCP config, and launches it. All tool calls are logged. When you exit, the original config is restored.

### Preview Changes First

Use `--dry-run` to see what ThoughtGate would change without writing anything:

```bash
thoughtgate wrap --dry-run -- claude-code
```

### Add Governance Rules

Update `thoughtgate.yaml` to deny or approve specific tools:

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
    - match: "delete_*"
      action: deny
    - match: "admin_*"
      action: deny
```

### Start in Development Mode

Development mode logs what **would** be blocked without actually blocking:

```bash
thoughtgate wrap --profile development -- claude-code
```

## Option 2: HTTP Sidecar (Docker / Kubernetes)

For server deployments where the agent connects over HTTP.

```bash
# Create config file first (see above)

docker run -d \
  -p 7467:7467 \
  -p 7469:7469 \
  -v $(pwd)/thoughtgate.yaml:/etc/thoughtgate/config.yaml \
  -e THOUGHTGATE_CONFIG=/etc/thoughtgate/config.yaml \
  ghcr.io/thoughtgate/thoughtgate:v0.3.0
```

### Verify It Works

```bash
# Health check (admin port)
curl http://localhost:7469/health

# Proxy a request (outbound port)
curl -X POST http://localhost:7467 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc": "2.0", "method": "tools/list", "id": 1}'
```

### Add Slack Approvals

Update your `thoughtgate.yaml`:

```yaml
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://your-mcp-server:3000

governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: approve

approval:
  default:
    adapter: slack
    channel: "#approvals"
    timeout: 5m
```

Then set the Slack token and restart:

```bash
export THOUGHTGATE_CONFIG=./thoughtgate.yaml
export THOUGHTGATE_SLACK_BOT_TOKEN=xoxb-your-token
./target/release/thoughtgate-proxy
```

## Next Steps

- **CLI wrapper path:** Follow the full [Wrap Your First Agent](/docs/tutorials/wrap-first-agent) tutorial
- **HTTP sidecar path:** Follow the [Your First Proxy](/docs/tutorials/first-proxy) tutorial
- Learn to [write policies](/docs/how-to/write-policies)
- Read about the [4-Gate architecture](/docs/explanation/architecture)
