# Docker Build Caching Guide for ThoughtGate

This guide explains the caching strategies implemented to speed up integration tests.

## Quick Reference

```bash
# Full rebuild (slowest, ~2 minutes)
just test-kind

# Cached build (checks if rebuild needed, ~10-30 seconds)
just test-cached

# No rebuild (fastest, ~5 seconds)
just test-fast

# Check cache status
just cache-status

# Force rebuild everything
just rebuild
```

---

## Caching Strategies

### 1. **Docker Layer Caching**

The Dockerfile is optimized to cache dependency compilation separately from source code:

```dockerfile
# Layer 1: Copy only Cargo.toml/Cargo.lock (rarely changes)
COPY Cargo.toml Cargo.lock ./

# Layer 2: Build dependencies (cached unless deps change)
RUN cargo build --release ...

# Layer 3: Copy source code (changes frequently)
COPY . .

# Layer 4: Build application (only rebuilds if source changed)
RUN cargo build --release ...
```

**Speed improvement:** Dependencies take ~90 seconds to compile but are only rebuilt when `Cargo.toml` changes.

### 2. **BuildKit Caching**

Enable Docker BuildKit for better caching:

```bash
export DOCKER_BUILDKIT=1
```

BuildKit provides:
- Parallel layer building
- More efficient cache invalidation
- Better progress output

This is now enabled by default in all `build` targets.

### 3. **Conditional Build (`build-if-needed`)**

The `build-if-needed` target only rebuilds if:
- Docker image doesn't exist, OR
- Source files are newer than the image

```bash
just build-if-needed
# ✅ Image is up to date, skipping build
```

**Speed improvement:** Skips 2-minute build if nothing changed.

### 4. **Conditional Load (`load-if-needed`)**

The `load-if-needed` target checks if the image is already in Kind:

```bash
just load-if-needed
# ✅ Image already loaded in Kind, skipping
```

**Speed improvement:** Skips 5-10 second image load operation.

---

## Justfile Targets

### Test Targets

| Target | Build | Load | Speed | Use When |
|--------|-------|------|-------|----------|
| `test-kind` | Always | Always | Slow | CI, first run, or after dependency changes |
| `test-cached` | Smart | Smart | Medium | Development (auto-detects changes) |
| `test-fast` | Never | Smart | Fast | Rapid iteration on tests only |

### Build Targets

| Target | Description | Cache Strategy |
|--------|-------------|----------------|
| `build` | Always rebuild | Uses Docker layer cache |
| `build-if-needed` | Conditional rebuild | Checks file timestamps |
| `rebuild` | Force rebuild | Ignores all caches |

### Utility Targets

- `cache-status` - Show current cache state
- `load` - Always load to Kind
- `load-if-needed` - Conditional load
- `clean` - Delete cluster and image

---

## Typical Workflows

### Initial Setup
```bash
# First time setup (slow)
just test-kind
```

### Development Iteration

**When modifying source code:**
```bash
# Smart rebuild (medium speed)
just test-cached

# Or if you know only tests changed:
just test-fast
```

**When modifying test harness only:**
```bash
# Skip Docker build entirely (fast)
just test-fast
```

**When adding dependencies:**
```bash
# Force full rebuild to update dependency cache
just rebuild
kind load docker-image thoughtgate:test --name kind
cargo test --test integration_k8s -- --nocapture
```

### CI/CD Pipeline
```bash
# Always use full rebuild in CI
just test-kind
```

---

## Cache Invalidation

### What Invalidates Docker Layers?

| Change | Layer Invalidated | Rebuild Time |
|--------|------------------|--------------|
| Change `Cargo.toml` | Dependency layer + all after | ~90s |
| Change `Cargo.lock` | Dependency layer + all after | ~90s |
| Change `src/*.rs` | Only source code layer | ~30s |
| Change `Dockerfile` | All layers | ~2 min |
| Change nothing | No layers | ~0s (cached) |

### What Invalidates Kind Load?

- Deleting the Kind cluster
- Deleting the image from Kind's containerd
- Building a new image with same tag (Kind doesn't auto-update)

---

## Advanced: Cargo Build Cache

For even faster builds during local development, you can use `sccache`:

### Install sccache
```bash
cargo install sccache
```

### Configure
```bash
export RUSTC_WRAPPER=sccache
export SCCACHE_DIR=$HOME/.cache/sccache
```

### Update Dockerfile
```dockerfile
ENV RUSTC_WRAPPER=sccache
ENV SCCACHE_DIR=/sccache
RUN --mount=type=cache,target=/sccache cargo build --release
```

This caches individual Rust compilation units across builds.

**Speed improvement:** Can reduce clean build from 90s to 20s.

---

## Debugging Cache Issues

### Check Docker Cache
```bash
docker system df
docker builder prune  # Clean build cache if it gets too large
```

### Check if Image Exists
```bash
docker image inspect thoughtgate:test
```

### Check if Image in Kind
```bash
docker exec kind-control-plane crictl images | grep thoughtgate
```

### View Build Cache Usage
```bash
docker buildx du
```

### Force Clean Rebuild
```bash
just clean
docker builder prune --all
just test-kind
```

---

## Performance Benchmarks

Measured on MacBook Pro M1 Max:

| Scenario | test-kind | test-cached | test-fast |
|----------|-----------|-------------|-----------|
| Clean build | 2m 10s | 2m 10s | N/A |
| Source change only | 2m 10s | 35s | 8s |
| No changes | 2m 10s | 8s | 8s |
| Test change only | 2m 10s | 8s | 8s |

**Key takeaway:** Use `test-cached` or `test-fast` for development iterations.

---

## Best Practices

### ✅ Do

- Use `test-cached` for development
- Use `test-fast` when only changing test code
- Use `test-kind` in CI/CD pipelines
- Run `just cache-status` to understand current state
- Clean up with `just clean` periodically

### ❌ Don't

- Don't use `test-kind` for every local iteration (too slow)
- Don't skip `build-if-needed` logic in Justfile
- Don't modify `.dockerignore` to include `target/` (breaks caching)
- Don't use `--no-cache` unless necessary (defeats caching)

---

## Troubleshooting

### "Image not found" error
```bash
# Rebuild and load
just build load
```

### "Tests fail but image seems cached"
```bash
# Force rebuild
just rebuild
just load
cargo test --test integration_k8s -- --nocapture
```

### "Kind cluster is slow"
```bash
# Check if too many old namespaces exist
kubectl get ns | grep thoughtgate

# Clean up old namespaces
kubectl get ns -o name | grep thoughtgate | xargs kubectl delete

# Or restart cluster
just clean
kind create cluster --name kind
```

### "Docker build is slow"
```bash
# Check if BuildKit is enabled
docker buildx ls

# Enable BuildKit
export DOCKER_BUILDKIT=1

# Or add to ~/.zshrc or ~/.bashrc
echo 'export DOCKER_BUILDKIT=1' >> ~/.zshrc
```

---

## Future Optimizations

### Remote Docker BuildKit Cache
Use a remote cache for team sharing:

```bash
docker buildx create --use --name=remote --driver=docker-container

docker buildx build \
  --cache-from type=registry,ref=myregistry/thoughtgate:cache \
  --cache-to type=registry,ref=myregistry/thoughtgate:cache,mode=max \
  -t thoughtgate:test .
```

### GitHub Actions Cache
For CI/CD:

```yaml
- name: Set up Docker Buildx
  uses: docker/setup-buildx-action@v2

- name: Cache Docker layers
  uses: actions/cache@v3
  with:
    path: /tmp/.buildx-cache
    key: ${{ runner.os }}-buildx-${{ github.sha }}
    restore-keys: |
      ${{ runner.os }}-buildx-

- name: Build
  uses: docker/build-push-action@v4
  with:
    cache-from: type=local,src=/tmp/.buildx-cache
    cache-to: type=local,dest=/tmp/.buildx-cache-new,mode=max
```

---

## Summary

The caching system provides three speed tiers:

1. **Full Build** (`test-kind`): 2+ minutes - Use in CI or after dependency changes
2. **Smart Build** (`test-cached`): 8-35 seconds - Use during development
3. **Fast Test** (`test-fast`): 5-8 seconds - Use for test-only iterations

Choose the right target for your workflow to maximize productivity.
