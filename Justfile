CLUSTER := "kind"
IMAGE := "thoughtgate:test"

# Run integration tests (always rebuild)
test-kind: ensure-cluster build load
    @echo "🚀 Running Integration Tests..."
    cargo test --test integration_k8s -- --nocapture

# Run tests with cached image (skip build if image exists and sources unchanged)
test-cached: ensure-cluster build-if-needed load-if-needed
    @echo "🚀 Running Integration Tests (cached)..."
    cargo test --test integration_k8s -- --nocapture

# Run tests without rebuilding (use existing image)
test-fast: ensure-cluster load-if-needed
    @echo "🚀 Running Integration Tests (no rebuild)..."
    cargo test --test integration_k8s -- --nocapture

# Build Docker image (unconditional)
build:
    @echo "🔨 Building Docker image..."
    DOCKER_BUILDKIT=1 docker build -t {{IMAGE}} .

# Build only if source files changed or image doesn't exist
build-if-needed:
    #!/usr/bin/env bash
    set -euo pipefail
    
    # Check if image exists
    if ! docker image inspect {{IMAGE}} >/dev/null 2>&1; then
        echo "🔨 Image not found, building..."
        DOCKER_BUILDKIT=1 docker build -t {{IMAGE}} .
        exit 0
    fi
    
    # Get image creation time
    IMAGE_TIME=$(docker image inspect {{IMAGE}} --format='{{{{.Created}}' | xargs -I {} date -j -f "%Y-%m-%dT%H:%M:%S" {} "+%s" 2>/dev/null || echo "0")
    
    # Find newest source file
    NEWEST_FILE=$(find src tests Cargo.toml Cargo.lock Dockerfile -type f -exec stat -f "%m %N" {} \; 2>/dev/null | sort -rn | head -1 | cut -d' ' -f1)
    
    if [ "${NEWEST_FILE:-0}" -gt "${IMAGE_TIME}" ]; then
        echo "🔨 Source files changed, rebuilding..."
        DOCKER_BUILDKIT=1 docker build -t {{IMAGE}} .
    else
        echo "✅ Image is up to date, skipping build"
    fi

# Load image into Kind cluster (unconditional)
load:
    @echo "📦 Loading image into Kind cluster..."
    kind load docker-image {{IMAGE}} --name {{CLUSTER}}

# Load image only if not already in Kind
load-if-needed:
    #!/usr/bin/env bash
    set -euo pipefail
    
    # Check if image exists in Kind
    if docker exec {{CLUSTER}}-control-plane crictl images | grep -q "{{IMAGE}}"; then
        echo "✅ Image already loaded in Kind, skipping"
    else
        echo "📦 Loading image into Kind cluster..."
        kind load docker-image {{IMAGE}} --name {{CLUSTER}}
    fi

# Ensure Kind cluster exists
ensure-cluster:
    @kind get clusters | grep -q ^{{CLUSTER}}$ || kind create cluster --name {{CLUSTER}}

# Clean up cluster and image
clean:
    kind delete cluster --name {{CLUSTER}}
    docker rmi {{IMAGE}} || true

# Force rebuild (ignore cache)
rebuild:
    @echo "🔨 Force rebuilding (no cache)..."
    DOCKER_BUILDKIT=1 docker build --no-cache -t {{IMAGE}} .

# Show cache status
cache-status:
    @echo "📊 Cache Status:"
    @echo "----------------"
    @if docker image inspect {{IMAGE}} >/dev/null 2>&1; then \
        echo "✅ Docker image exists: {{IMAGE}}"; \
        docker image inspect {{IMAGE}} --format='   Created: {{{{.Created}}'; \
        docker image inspect {{IMAGE}} --format='   Size: {{{{.Size}} bytes'; \
    else \
        echo "❌ Docker image not found: {{IMAGE}}"; \
    fi
    @echo ""
    @if docker exec {{CLUSTER}}-control-plane crictl images 2>/dev/null | grep -q "{{IMAGE}}"; then \
        echo "✅ Image loaded in Kind cluster"; \
    else \
        echo "❌ Image not loaded in Kind cluster"; \
    fi
