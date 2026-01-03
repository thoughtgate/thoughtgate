FROM rust:1.92 AS builder
WORKDIR /app

# Copy only dependency manifests first (for caching)
COPY Cargo.toml Cargo.lock ./

# Create dummy source files to build dependencies
RUN mkdir -p src/bin && \
    echo "fn main() {}" > src/main.rs && \
    echo "fn main() {}" > src/bin/mock_llm.rs && \
    mkdir -p src && \
    echo "pub mod error; pub mod logging_layer; pub mod proxy_service;" > src/lib.rs && \
    echo "pub struct ProxyService;" > src/proxy_service.rs && \
    echo "pub enum ProxyError {}" > src/error.rs && \
    echo "pub struct LoggingLayer;" > src/logging_layer.rs

# Build dependencies (this layer will be cached unless Cargo.toml/Cargo.lock change)
RUN cargo build --release --bin thoughtgate || true && \
    cargo build --release --bin mock_llm --features mock || true

# Now copy actual source code
COPY . .

# Remove dummy binaries to force rebuild with real source
RUN rm -rf target/release/thoughtgate target/release/mock_llm target/release/deps/thoughtgate* target/release/deps/mock_llm*

# Build with real source (only this layer rebuilds when source changes)
RUN cargo build --release --bin thoughtgate && \
    cargo build --release --bin mock_llm --features mock

FROM debian:13-slim
# Install CA certificates for HTTPS connections
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

# Copy both binaries
COPY --from=builder /app/target/release/thoughtgate /
COPY --from=builder /app/target/release/mock_llm /

# Create entrypoint wrapper that sets ulimits
RUN echo '#!/bin/sh\nulimit -n 65535\nexec "$@"' > /entrypoint.sh && \
    chmod +x /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh", "/thoughtgate"]
