---
sidebar_position: 1
---

# Quickstart

Get ThoughtGate running in under 5 minutes.

## Prerequisites

- Rust 1.75+ or Docker
- An MCP server to proxy

## Option 1: From Source

```bash
# Clone and build
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo build --release -p thoughtgate-proxy

# Create config file
cat > thoughtgate.yaml << 'EOF'
schema: 1

sources:
  - id: upstream
    kind: mcp
    url: http://your-mcp-server:3000

governance:
  defaults:
    action: forward
EOF

# Run
export THOUGHTGATE_CONFIG=./thoughtgate.yaml
./target/release/thoughtgate-proxy
```

## Option 2: Docker

```bash
# Create config file first (see above)

docker run -d \
  -p 7467:7467 \
  -p 7469:7469 \
  -v $(pwd)/thoughtgate.yaml:/etc/thoughtgate/config.yaml \
  -e THOUGHTGATE_CONFIG=/etc/thoughtgate/config.yaml \
  ghcr.io/thoughtgate/thoughtgate:v0.2.2
```

## Verify It Works

```bash
# Health check (admin port)
curl http://localhost:7469/health

# Proxy a request (outbound port)
curl -X POST http://localhost:7467 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc": "2.0", "method": "tools/list", "id": 1}'
```

## Add Slack Approvals

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

- Follow the full [tutorial](/docs/tutorials/first-proxy)
- Learn to [write policies](/docs/how-to/write-policies)
- Read about the [4-Gate architecture](/docs/explanation/architecture)
