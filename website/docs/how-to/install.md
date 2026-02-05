---
sidebar_position: 2
---

# Install ThoughtGate

ThoughtGate has two binaries: the **CLI wrapper** (`thoughtgate`) for local development, and the **HTTP sidecar** (`thoughtgate-proxy`) for Kubernetes deployments.

## From Source

### Prerequisites

- Rust 1.87 or later
- Cargo (included with Rust)

### CLI Wrapper (Local Development)

```bash
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo install --path thoughtgate
```

Verify:

```bash
thoughtgate --help
```

### HTTP Sidecar (Server Deployments)

```bash
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo install --path thoughtgate-proxy
```

Verify:

```bash
thoughtgate-proxy --version
```

### Build Both

```bash
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo build --release
```

Binaries are at `target/release/thoughtgate` and `target/release/thoughtgate-proxy`.

## Docker (HTTP Sidecar Only)

### Pull the Image

```bash
docker pull ghcr.io/thoughtgate/thoughtgate:v0.3.0
```

### Available Tags

| Tag | Description |
|-----|-------------|
| `latest` | Latest stable release |
| `v0.3.0` | Specific version |
| `main-abc1234` | Main branch build |

### Run

```bash
docker run -d \
  --name thoughtgate \
  -p 7467:7467 \
  -p 7469:7469 \
  -v $(pwd)/thoughtgate.yaml:/etc/thoughtgate/config.yaml \
  -e THOUGHTGATE_CONFIG=/etc/thoughtgate/config.yaml \
  ghcr.io/thoughtgate/thoughtgate:v0.3.0
```

## Kubernetes

See [Deploy to Kubernetes](/docs/how-to/deploy-kubernetes) for Helm charts and manifests.

## Verify Installation

```bash
# CLI wrapper
thoughtgate --help

# HTTP sidecar
thoughtgate-proxy --version

# Health check (after starting sidecar)
curl http://localhost:7469/health
```

## Port Model

### HTTP Sidecar

ThoughtGate uses an Envoy-inspired 3-port architecture:

| Port | Name | Purpose |
|------|------|---------|
| 7467 | Outbound | Client requests → upstream (main proxy) |
| 7468 | Inbound | Reserved for webhooks |
| 7469 | Admin | Health checks, metrics |

### CLI Wrapper

The governance service uses an **OS-assigned ephemeral port** on `127.0.0.1`. Override with `--governance-port`:

```bash
thoughtgate wrap --governance-port 9090 -- claude-code
```

## System Requirements

| Resource | Minimum | Recommended |
|----------|---------|-------------|
| Memory | 20 MB | 50 MB |
| CPU | 0.1 cores | 0.5 cores |
| Disk | 15 MB (binary) | 15 MB |
