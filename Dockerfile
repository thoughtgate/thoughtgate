# Build stage - Alpine for musl static linking
FROM rust:1.93-alpine AS builder

# Install musl-dev for static linking
RUN apk add --no-cache musl-dev

WORKDIR /app

# Copy only dependency manifests first (for caching)
COPY Cargo.toml Cargo.lock ./

# Create dummy source files to build dependencies
# Note: Must match real project module structure to avoid E0761 conflicts
RUN mkdir -p src/bin src/error && \
    echo "fn main() {}" > src/main.rs && \
    echo "pub mod error; pub mod logging_layer; pub mod proxy_service;" > src/lib.rs && \
    echo "pub struct ProxyService;" > src/proxy_service.rs && \
    echo "pub enum ProxyError {}" > src/error/mod.rs && \
    echo "pub struct LoggingLayer;" > src/logging_layer.rs

# Build dependencies (this layer will be cached unless Cargo.toml/Cargo.lock change)
RUN cargo build --release --locked --bin thoughtgate || true

# Now copy actual source code
COPY . .

# Remove dummy binaries to force rebuild with real source
RUN rm -rf target/release/thoughtgate target/release/deps/thoughtgate*

# Build with real source (only this layer rebuilds when source changes)
# Note: mock_mcp is intentionally NOT built - use Dockerfile.test for tests
RUN cargo build --release --locked --bin thoughtgate

# Production stage - distroless for minimal attack surface
# Includes: CA certificates, /etc/passwd, timezone data
FROM gcr.io/distroless/static-debian12:nonroot

# Copy the statically-linked binary
COPY --from=builder /app/target/release/thoughtgate /thoughtgate

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

ENTRYPOINT ["/thoughtgate"]
