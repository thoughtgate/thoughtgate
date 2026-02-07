# ThoughtGate

Human-in-the-loop approval proxy for MCP (Model Context Protocol) agents.

## Specs

All requirements are in `specs/`:
- `ARCHITECTURE.md` - System overview (v0.1 simplified model)
- `REQ-CORE-001` - Zero-copy streaming (DEFERRED to v0.2+)
- `REQ-CORE-002` - Buffered inspection (DEFERRED to v0.2+)
- `REQ-CORE-003` - MCP transport & routing
- `REQ-CORE-004` - Error handling
- `REQ-CORE-005` - Operational lifecycle
- `REQ-POL-001` - Cedar policy engine (3-way: Forward/Approve/Reject)
- `REQ-GOV-*` - Governance (tasks, approvals, Slack)
- `REQ-CORE-008` - stdio transport & CLI wrapper (v0.3)
- `REQ-OBS-001` - Performance metrics & benchmarking

**Read the relevant REQ file before implementing any feature.**

## Project Structure

> **Note:** v0.2 uses a flat `src/` layout. v0.3 restructures into the workspace below.

```
Cargo.toml                        # [workspace] with 3 members
thoughtgate-core/                 # Library: transport-agnostic governance + telemetry
├── src/
│   ├── governance/               # REQ-GOV-001/002/003: Engine, tasks, pipeline, Slack
│   ├── policy/                   # REQ-POL-001: Cedar policy engine
│   ├── transport/                # REQ-CORE-003: JSON-RPC parsing, classification
│   ├── config/                   # REQ-CFG-001: YAML config, governance rules
│   ├── telemetry/                # Spans, metrics, audit, redaction
│   ├── error/                    # REQ-CORE-004: Error types
│   └── lifecycle/                # REQ-CORE-005: State machine (shared)
thoughtgate-proxy/                # Binary: HTTP+SSE sidecar for K8s
├── src/
│   ├── main.rs                   # Startup, listener, shutdown
│   ├── handlers.rs               # Axum request handlers
│   └── health.rs                 # K8s health/readiness probes
thoughtgate/                      # Binary: CLI wrapper for local dev
└── src/
    ├── main.rs                   # clap dispatch (wrap, shim)
    ├── wrap/                     # REQ-CORE-008: config discovery, rewrite, launch
    └── shim/                     # REQ-CORE-008: per-server stdio proxy
```

## Domain Model

Policy evaluates to ONE action:

| Action | Trigger | Behavior |
|--------|---------|----------|
| **Forward** | `PolicyAction::Forward` | Send to upstream immediately |
| **Approve** | `PolicyAction::Approve` | Create SEP-1686 task, post to Slack, return task ID |
| **Reject** | `PolicyAction::Reject` | Return error immediately |
| **Policy** | `action: policy` | Delegate to Cedar engine, which returns Forward/Approve/Reject |

**Response handling:** All responses are passed through directly. No inspection or streaming distinction.

### Current Constraints
- **SEP-1686 async mode** - Approvals use async tasks (agent polls `tasks/get` for result)
- **Single upstream** - One `THOUGHTGATE_UPSTREAM` per instance
- **In-memory state** - Tasks lost on restart
- **No inspection** - Green/Amber paths deferred to v0.3+

## Code Standards

### Pre-Commit Checklist

**ALWAYS run before committing:**
```bash
cargo fmt              # Format code (REQUIRED - CI will fail without this)
cargo clippy -- -D warnings  # Lint code
cargo test            # Run tests
```

### Git Commit Messages (Conventional Commits)

**Format:** `<type>(<scope>): <subject>`

**Types:**
- `feat` - New feature
- `fix` - Bug fix
- `docs` - Documentation only
- `style` - Code style (formatting, missing semicolons, etc)
- `refactor` - Code change that neither fixes a bug nor adds a feature
- `test` - Adding or updating tests
- `chore` - Maintenance tasks (dependencies, build config, etc)

**Rules:**
- **MUST run `cargo fmt` before every commit** (CI enforces this)
- Subject: Imperative mood ("add" not "added"), lowercase, no period at end
- Body: Wrapped at 72 characters, explaining **why** vs. how
- Footer: Reference issues/requirements (e.g., `Implements: REQ-CORE-004`)

**Examples:**
```
feat(error): add JSON-RPC 2.0 error handling

Implement comprehensive error types for MCP transport layer with
standard and custom error codes. All errors map to JSON-RPC 2.0
compliant responses with security-safe details.

Implements: REQ-CORE-004
Refs: specs/REQ-CORE-004_Error_Handling.md
```

```
fix(proxy): prevent connection leak on timeout

test(k8s): mark integration tests as ignored by default
```

### Safety Rules (ENFORCED)
```rust
// ❌ NEVER in runtime code
.unwrap()
.expect("...")
std::thread::sleep()
std::sync::Mutex

// ❌ NEVER log
Authorization, Cookie, x-api-key, THOUGHTGATE_SLACK_BOT_TOKEN, tool_arguments

// ✅ ALWAYS use
? operator or explicit error handling
tokio::time::sleep()
tokio::sync::Mutex

// ✅ EXCEPTION: .expect() on LazyLock with compile-time literal constants
// where the parse function is not const fn. Must include a // SAFETY: comment.
static FOO: LazyLock<T> = LazyLock::new(|| T::parse("literal").expect("BUG: ..."));
```

### Observability Systems

ThoughtGate uses two observability systems (each for a distinct purpose):

| Purpose | Library | Location |
|---------|---------|----------|
| **Metrics** | `prometheus-client` | `telemetry/prom_metrics.rs` |
| **Distributed Tracing** | OpenTelemetry SDK | `telemetry/otel.rs` |

**Metrics** (`ThoughtGateMetrics` in `prom_metrics.rs`):
- Counters: `thoughtgate_requests_total`, `thoughtgate_tasks_created_total`, `thoughtgate_tasks_completed_total`
- Gauges: `thoughtgate_connections_active`, `thoughtgate_tasks_pending`, `thoughtgate_cedar_policies_loaded`, `thoughtgate_uptime_seconds`
- Histograms: `thoughtgate_request_duration_seconds`, `thoughtgate_upstream_latency_seconds`

**When adding metrics:**
1. Add to `prom_metrics.rs`
2. Use `record_*` helper methods on `ThoughtGateMetrics`
3. Wire via `set_metrics()` or `with_metrics()` builder pattern

**When adding tracing:**
- Use `tracing` macros (`info_span!`, `#[instrument]`) for local spans
- OpenTelemetry exports traces to OTLP endpoints for distributed tracing

### Requirement Traceability
Every public function must link to its requirement:
```rust
/// Parses JSON-RPC 2.0 requests.
///
/// Implements: REQ-CORE-003/F-001
/// Handles: REQ-CORE-003/EC-MCP-001, EC-MCP-004
pub fn parse_jsonrpc(bytes: &[u8]) -> Result<JsonRpcMessage, ParseError>
```

### Key Types (must be consistent across codebase)
```rust
// v0.1 simplified policy actions
pub enum PolicyAction { Forward, Approve { timeout: Duration }, Reject { reason: String } }
pub enum TaskStatus { Working, InputRequired, Executing, Completed, Failed, Rejected, Cancelled, Expired }
pub enum ApprovalDecision { Approved, Rejected { reason: Option<String> } }
```

## Dependencies (Blessed Stack)

Workspace-level shared deps: `tokio`, `serde`, `serde_json`, `tracing`, `thiserror`.

| Crate | Category | Key deps |
|-------|----------|----------|
| `thoughtgate-core` | Governance, policy, telemetry | `cedar-policy`, `arc-swap`, `opentelemetry`, `reqwest`, `axum` |
| `thoughtgate-proxy` | HTTP transport | core + `hyper` 1.x, `hyper-rustls`, `rustls`, `tower`, `axum` |
| `thoughtgate` (CLI) | stdio transport | core + `clap`, `dirs`, `nix`, `reqwest` |
| Testing | All crates | `proptest`, `wiremock`, `insta`, `criterion` |

## Commands

```bash
cargo build                            # Build all crates
cargo build -p thoughtgate-proxy       # Build sidecar only
cargo build -p thoughtgate             # Build CLI only
cargo test                             # Run all tests
cargo test -p thoughtgate-core         # Test core library only
cargo clippy -- -D warnings            # Lint all crates
cargo fuzz run fuzz_jsonrpc            # Fuzz JSON-RPC parser
```

## Implementation Order (v0.1)

1. `REQ-CORE-004` - Error types (everything depends on this)
2. `REQ-POL-001` - Cedar policy engine (3-way: Forward/Approve/Reject)
3. `REQ-CORE-003` - MCP transport & routing
4. `REQ-GOV-001` - Task lifecycle (blocking mode)
5. `REQ-GOV-003` - Slack integration
6. `REQ-GOV-002` - Execution pipeline
7. `REQ-CORE-005` - Lifecycle management

**Deferred to v0.3+:**
- `REQ-CORE-001` - Green path streaming
- `REQ-CORE-002` - Amber path buffering/inspection
- `REQ-CFG-002` - Config hot-reload (file watcher + ArcSwap + SIGHUP)

## Implementation Order (v0.3)

1. Workspace restructure (split monolith into `thoughtgate-core` + `thoughtgate-proxy` + `thoughtgate`)
2. `REQ-CORE-008` - stdio transport & CLI wrapper

## Performance Metrics (REQ-OBS-001)

Performance is tracked via Bencher.dev with the following metrics:

| Category | Metrics | Target |
|----------|---------|--------|
| **Binary** | `binary/size` | < 15 MB |
| **Memory** | `memory/idle_rss`, `memory/constrained_rss` | < 20 MB idle, < 100 MB constrained |
| **Latency** | `latency/p50`, `latency/p95`, `latency/p99` | p50 < 5ms, p95 < 15ms |
| **Throughput** | `throughput/rps_10vu`, `throughput/rps_constrained` | > 10K RPS, > 5K constrained |
| **Overhead** | `overhead/latency_p50`, `overhead/percent_p50` | < 2ms, < 10% |
| **Policy** | `policy/eval_p50`, `policy/eval_p99` | p50 < 100µs, p99 < 500µs |
| **Startup** | `startup/to_healthy`, `startup/to_ready` | < 100ms healthy, < 150ms ready |

### Benchmarking Commands

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark
cargo bench --bench ttfb
cargo bench --bench policy_eval

# Collect metrics manually
./scripts/collect_metrics.sh --binary-only
./scripts/measure_overhead.sh
./scripts/measure_constrained.sh --image thoughtgate:test
```

### CI Integration

- **Bencher.dev** tracks metrics on `main`, `releases`, and `pr-*` branches
- PRs get comparison comments with regression detection
- CI fails on significant regressions (> 25% latency, > 50% memory)

## Quick Reference

### Error Codes
```
-32700  ParseError           -32003  PolicyDenied
-32600  InvalidRequest       -32005  TaskExpired
-32601  MethodNotFound       -32006  TaskCancelled
-32602  InvalidParams        -32007  ApprovalRejected
-32603  InternalError        -32008  ApprovalTimeout
-32000  UpstreamConnFailed   -32009  RateLimited
-32001  UpstreamTimeout      -32011  PolicyDrift
-32002  UpstreamError        -32012  TransformDrift
                             -32013  ServiceUnavailable
                             -32014  GovernanceRuleDenied
                             -32015  ToolNotExposed
                             -32016  ConfigurationError
                             -32017  WorkflowNotFound
```

### Environment Variables
```bash
THOUGHTGATE_UPSTREAM=http://mcp-server:3000  # Required
THOUGHTGATE_OUTBOUND_PORT=7467               # Main proxy port (default: 7467)
THOUGHTGATE_ADMIN_PORT=7469                  # Admin/health port (default: 7469)
THOUGHTGATE_CONFIG=/etc/thoughtgate/config.yaml  # Optional YAML config
THOUGHTGATE_POLICIES=/etc/thoughtgate/policies/
THOUGHTGATE_SLACK_BOT_TOKEN=xoxb-...
THOUGHTGATE_SLACK_CHANNEL=#approvals
THOUGHTGATE_APPROVAL_TIMEOUT_SECS=300
THOUGHTGATE_REQUEST_TIMEOUT_SECS=300          # Per-request pipeline timeout (default: 300s)
THOUGHTGATE_UPSTREAM_TIMEOUT_SECS=30          # Upstream HTTP client timeout (default: 30s)
THOUGHTGATE_MAX_BATCH_SIZE=100
THOUGHTGATE_ENVIRONMENT=production
```

## Requirement-Specific Notes

### REQ-CORE-003 (JSON-RPC)
```rust
// Preserve exact ID type - never coerce
#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    Number(i64),
    String(String),
    Null,  // Notification, no response
}
```

### REQ-POL-001 (Cedar Policy)
```rust
// v0.1 simplified: 3 actions only
// Forward -> send to upstream
// Approve -> block until Slack approval
// Reject -> return error
```

### REQ-GOV-003 (Slack)
```rust
// CRITICAL: Use batch polling
// ❌ reactions.get per task (O(n) API calls)
// ✅ conversations.history once (O(1) API calls)
```