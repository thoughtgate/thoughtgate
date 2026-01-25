---
sidebar_position: 2
---

# Install ThoughtGate

## From Source

### Prerequisites

- Rust 1.75 or later
- Cargo (included with Rust)

### Build

```bash
git clone https://github.com/thoughtgate/thoughtgate
cd thoughtgate
cargo build --release
```

The binary is at `target/release/thoughtgate`.

### Install Globally

```bash
cargo install --path .
```

## Docker

### Pull the Image

```bash
docker pull ghcr.io/thoughtgate/thoughtgate:v0.2.0
```

### Available Tags

| Tag | Description |
|-----|-------------|
| `latest` | Latest stable release |
| `v0.2.0` | Specific version |
| `main-abc1234` | Main branch build |

### Run

```bash
docker run -d \
  --name thoughtgate \
  -p 7467:7467 \
  -p 7469:7469 \
  -v $(pwd)/thoughtgate.yaml:/etc/thoughtgate/config.yaml \
  -e THOUGHTGATE_CONFIG=/etc/thoughtgate/config.yaml \
  ghcr.io/thoughtgate/thoughtgate:v0.2.0
```

## Kubernetes

See [Deploy to Kubernetes](/docs/how-to/deploy-kubernetes) for Helm charts and manifests.

## Verify Installation

```bash
# Check version
thoughtgate --version

# Check health (after starting)
curl http://localhost:7469/health
```

## Port Model

ThoughtGate uses an Envoy-inspired 3-port architecture:

| Port | Name | Purpose |
|------|------|---------|
| 7467 | Outbound | Client requests → upstream (main proxy) |
| 7468 | Inbound | Reserved for webhooks (v0.3+) |
| 7469 | Admin | Health checks, metrics |

## System Requirements

| Resource | Minimum | Recommended |
|----------|---------|-------------|
| Memory | 20 MB | 50 MB |
| CPU | 0.1 cores | 0.5 cores |
| Disk | 15 MB (binary) | 15 MB |
