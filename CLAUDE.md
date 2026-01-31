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

**Read the relevant REQ file before implementing any feature.**

## Project Structure

```
src/
├── error/        # REQ-CORE-004: Error types
├── transport/    # REQ-CORE-003: MCP JSON-RPC, routing, upstream
├── policy/       # REQ-POL-001: Cedar policy engine
├── governance/   # REQ-GOV-001/002/003: Tasks, pipeline, Slack
└── lifecycle/    # REQ-CORE-005: Startup, shutdown, health
```

## Domain Model (v0.1 Simplified)

Policy evaluates to ONE action:

| Action | Trigger | Behavior |
|--------|---------|----------|
| **Forward** | `PolicyAction::Forward` | Send to upstream immediately |
| **Approve** | `PolicyAction::Approve` | Block until Slack approval, then forward |
| **Reject** | `PolicyAction::Reject` | Return error immediately |

**Response handling:** All responses are passed through directly. No inspection or streaming distinction in v0.1.

### v0.1 Constraints
- **Blocking approval mode** - Hold HTTP connection until approval (no SEP-1686 tasks)
- **Zombie prevention** - Check connection liveness before executing approved tools
- **Single upstream** - One `THOUGHTGATE_UPSTREAM` per instance
- **In-memory state** - Tasks lost on restart
- **No inspection** - Green/Amber paths deferred to v0.2+

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
```

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

| Category | Crates |
|----------|--------|
| Runtime | `tokio` (full) |
| HTTP | `hyper` 1.x, `hyper-util`, `axum` 0.7, `tower`, `reqwest` |
| Data | `bytes`, `serde`, `serde_json` |
| Policy | `cedar-policy`, `arc-swap` |
| Errors | `thiserror` (lib), `anyhow` (bin) |
| Observability | `tracing`, `tracing-subscriber`, `metrics` |
| Testing | `tokio-test`, `proptest`, `mockall`, `wiremock` |

## Commands

```bash
cargo build                    # Build
cargo test                     # Run tests
cargo clippy -- -D warnings    # Lint
cargo fuzz run fuzz_jsonrpc    # Fuzz JSON-RPC parser
```

## Implementation Order (v0.1)

1. `REQ-CORE-004` - Error types (everything depends on this)
2. `REQ-POL-001` - Cedar policy engine (3-way: Forward/Approve/Reject)
3. `REQ-CORE-003` - MCP transport & routing
4. `REQ-GOV-001` - Task lifecycle (blocking mode)
5. `REQ-GOV-003` - Slack integration
6. `REQ-GOV-002` - Execution pipeline
7. `REQ-CORE-005` - Lifecycle management

**Deferred to v0.2+:**
- `REQ-CORE-001` - Green path streaming
- `REQ-CORE-002` - Amber path buffering/inspection
- `REQ-CFG-002` - Config hot-reload (file watcher + ArcSwap + SIGHUP)

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
-32700  ParseError           -32003  PolicyDenied (Reject action)
-32600  InvalidRequest       -32007  ApprovalRejected
-32601  MethodNotFound       -32008  ApprovalTimeout
-32602  InvalidParams        -32009  RateLimited
-32603  InternalError        -32013  ServiceUnavailable
-32000  UpstreamConnFailed
-32001  UpstreamTimeout
-32002  UpstreamError
```

### Environment Variables
```bash
THOUGHTGATE_UPSTREAM=http://mcp-server:3000  # Required
THOUGHTGATE_LISTEN=0.0.0.0:8080
THOUGHTGATE_CEDAR_POLICY_PATH=/etc/thoughtgate/policy.cedar
THOUGHTGATE_SLACK_BOT_TOKEN=xoxb-...
THOUGHTGATE_SLACK_CHANNEL=#approvals
THOUGHTGATE_APPROVAL_TIMEOUT_SECS=300
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