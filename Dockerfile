# Build stage - Alpine for musl static linking
FROM rust:1.93-alpine AS builder

# Install musl-dev for static linking
RUN apk add --no-cache musl-dev

WORKDIR /app

# Copy only dependency manifests first (for caching)
COPY Cargo.toml Cargo.lock ./
COPY thoughtgate-core/Cargo.toml thoughtgate-core/Cargo.toml
COPY thoughtgate-proxy/Cargo.toml thoughtgate-proxy/Cargo.toml
COPY thoughtgate/Cargo.toml thoughtgate/Cargo.toml

# Create workspace-aware dummy sources for dependency caching
RUN mkdir -p thoughtgate-core/src thoughtgate-proxy/src/bin thoughtgate/src && \
    echo "fn main() {}" > thoughtgate-proxy/src/main.rs && \
    echo "" > thoughtgate-core/src/lib.rs && \
    echo "fn main() {}" > thoughtgate/src/main.rs && \
    echo "fn main() {}" > thoughtgate-proxy/src/bin/mock_mcp.rs

# Build dependencies (this layer will be cached unless Cargo.toml/Cargo.lock change)
RUN cargo build --release -p thoughtgate-proxy --locked || true

# Now copy actual source code
COPY . .

# Remove dummy binaries to force rebuild with real source
RUN rm -rf target/release/thoughtgate-proxy target/release/deps/thoughtgate*

# Build with real source (only this layer rebuilds when source changes)
# Note: mock_mcp is intentionally NOT built - use Dockerfile.test for tests
RUN cargo build --release -p thoughtgate-proxy --locked

# Production stage - distroless for minimal attack surface
# Includes: CA certificates, /etc/passwd, timezone data
FROM gcr.io/distroless/static-debian12:nonroot

# Copy the statically-linked binary
COPY --from=builder /app/target/release/thoughtgate-proxy /thoughtgate-proxy

# nonroot image already runs as UID 65532
# NOTE: Set file descriptor limits via container runtime (--ulimit nofile=65535:65535)

# Expose default ports (outbound: 7467, inbound: 7468, admin: 7469)
EXPOSE 7467 7468 7469

# Note: No HEALTHCHECK instruction - distroless images don't include a shell.
# Use Kubernetes liveness/readiness probes against http://localhost:7469/health instead.
# Example K8s probe:
#   livenessProbe:
#     httpGet:
#       path: /health
#       port: 7469
#     initialDelaySeconds: 5
#     periodSeconds: 10

ENTRYPOINT ["/thoughtgate-proxy"]
