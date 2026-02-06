# REQ-CORE-005: Operational Lifecycle

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-CORE-005` |
| **Title** | Operational Lifecycle |
| **Type** | Core Mechanic |
| **Status** | Draft |
| **Priority** | **High** |
| **Tags** | `#operations` `#health` `#shutdown` `#kubernetes` `#reliability` |

## 1. Context & Decision Rationale

This requirement defines how ThoughtGate manages its operational lifecycle—startup, health monitoring, and graceful shutdown. Proper lifecycle management is essential for:

1. **Kubernetes integration:** Health probes determine pod scheduling and traffic routing
2. **Zero-downtime deployments:** Graceful shutdown prevents request loss during rollouts
3. **Reliability:** Clear startup sequencing prevents serving before ready
4. **Debuggability:** Health endpoints expose internal state for troubleshooting

### 1.1 Version Scope Overview

| Version | Configuration | Approval Tracking | Shutdown Behavior |
|---------|---------------|-------------------|-------------------|
| **v0.2** | YAML config | `PendingApprovalStore` (SEP-1686) | Cancel pending approvals |
| v0.3+ | YAML + Cedar | `TaskStore` (SEP-1686) | Fail tasks with reason |

### 1.2 Deployment Context

ThoughtGate runs as a sidecar proxy in Kubernetes (via `thoughtgate-proxy`). For CLI wrapper process lifecycle (v0.3), see REQ-CORE-008 §7.5. The sidecar must:
- Start quickly and signal readiness
- Accept traffic only when fully initialized
- Drain connections gracefully on shutdown
- Handle pending approvals appropriately during shutdown

## 2. Dependencies

| Requirement | Relationship | v0.2 | v0.3+ |
|-------------|--------------|------|-------|
| REQ-CFG-001 | **Loads from** | YAML configuration | Same |
| REQ-CORE-003 | **Coordinates with** | Connection draining | Same |
| REQ-POL-001 | **Waits for** | ❌ Not used | Policy loading |
| REQ-GOV-001 | **Coordinates with** | Pending approval cleanup | Task state on shutdown |

## 3. Intent

### 3.1 v0.2 Intent

The system must:
1. Perform orderly startup with clear sequencing
2. Load and validate YAML configuration
3. Expose health endpoints for orchestration
4. Handle shutdown signals gracefully
5. Cancel pending approval waits on shutdown
6. Drain in-flight requests without data loss

### 3.2 v0.3+ Intent

The system must additionally:
1. Load and validate Cedar policies
2. Preserve or fail pending tasks appropriately
3. Support hot-reload of policies

## 4. Scope

### 4.1 v0.2 Scope

| Component | Status | Notes |
|-----------|--------|-------|
| Startup sequencing | ✅ In Scope | Load config, init stores, start server |
| YAML configuration loading | ✅ In Scope | REQ-CFG-001 |
| Health probe endpoints | ✅ In Scope | `/health`, `/ready` |
| SIGTERM/SIGINT handling | ✅ In Scope | Graceful shutdown |
| Connection draining | ✅ In Scope | Complete in-flight requests |
| Pending approval cleanup | ✅ In Scope | Cancel waits, return errors |
| Startup/shutdown logging | ✅ In Scope | Observability |
| Cedar policy loading | ❌ Out of Scope | v0.3+ |
| Policy hot-reload | ❌ Out of Scope | v0.3+ |
| Task persistence | ❌ Out of Scope | Future version |

### 4.2 v0.3+ Scope (Future)

| Component | Status | Notes |
|-----------|--------|-------|
| Cedar policy loading | In Scope | From ConfigMap/file |
| Policy validation | In Scope | Against schema |
| Policy hot-reload | In Scope | Watch for changes |
| Task failure on shutdown | In Scope | Transition to failed state |
| stdio process lifecycle | In Scope | REQ-CORE-008: child process mgmt, signal forwarding, config restoration |

## 5. Constraints

### 5.1 Kubernetes Integration

**Port Architecture:**

ThoughtGate uses a 3-port Envoy-style architecture:

| Port | Env Variable | Default | Purpose |
|------|--------------|---------|---------|
| Outbound | `THOUGHTGATE_OUTBOUND_PORT` | 7467 | MCP traffic (agent → upstream) |
| Inbound | (reserved) | 7468 | Future callbacks (not wired) |
| Admin | `THOUGHTGATE_ADMIN_PORT` | 7469 | Health, ready, metrics |

**Probe Configuration:**
```yaml
livenessProbe:
  httpGet:
    path: /health
    port: 7469          # Admin port
  initialDelaySeconds: 5
  periodSeconds: 10
  failureThreshold: 3

readinessProbe:
  httpGet:
    path: /ready
    port: 7469          # Admin port
  initialDelaySeconds: 2
  periodSeconds: 5
  failureThreshold: 2
```

**Timing Requirements:**
| Phase | Maximum Duration |
|-------|------------------|
| Startup to healthy | 10 seconds |
| Startup to ready | 15 seconds |
| Graceful shutdown | 30 seconds (configurable) |

### 5.2 Signal Handling

| Signal | Action |
|--------|--------|
| SIGTERM | Begin graceful shutdown |
| SIGINT | Begin graceful shutdown |
| SIGQUIT | Immediate shutdown (dump state) |

### 5.3 Configuration

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| Outbound port | 7467 | `THOUGHTGATE_OUTBOUND_PORT` |
| Admin port | 7469 | `THOUGHTGATE_ADMIN_PORT` |
| Shutdown timeout | 30s | `THOUGHTGATE_SHUTDOWN_TIMEOUT_SECS` |
| Drain timeout | 25s | `THOUGHTGATE_DRAIN_TIMEOUT_SECS` |
| Startup timeout | 15s | `THOUGHTGATE_STARTUP_TIMEOUT_SECS` |
| Require upstream at startup | false | `THOUGHTGATE_REQUIRE_UPSTREAM_AT_STARTUP` |
| Upstream URL | (required) | `UPSTREAM_URL` / `THOUGHTGATE_UPSTREAM` |
| Upstream health interval | 30s | `THOUGHTGATE_UPSTREAM_HEALTH_INTERVAL_SECS` |
| Environment | `production` | `THOUGHTGATE_ENVIRONMENT` |
| Log level | info | `RUST_LOG` |
| Log format | json | *(always JSON; not configurable)* |

> **Implementation note:** The implementation uses `RUST_LOG` (standard Rust convention via `tracing-subscriber`'s `EnvFilter`) instead of `THOUGHTGATE_LOG_LEVEL`. The `THOUGHTGATE_LOG_FORMAT` env var is not implemented — the log format is always JSON.

**Note:** Health, ready, and metrics endpoints are served on the admin port (default 7469), separate from the main proxy port (default 7467).

**Log Levels:**
- `error`: Unrecoverable failures, panics
- `warn`: Recoverable failures, rejections, timeouts
- `info`: Request lifecycle, state transitions
- `debug`: Detailed flow (disabled in production)
- `trace`: Byte-level details (disabled in production)

## 6. Interfaces

### 6.1 Health Endpoint

**Request:**
```
GET /health HTTP/1.1
```

**Response (Healthy):**
```json
HTTP/1.1 200 OK
Content-Type: application/json

{
  "status": "healthy",
  "version": "0.2.2",
  "uptime_seconds": 3600
}
```

**Response (Unhealthy):**
```json
HTTP/1.1 503 Service Unavailable
Content-Type: application/json

{
  "status": "unhealthy",
  "reason": "upstream_unreachable",
  "details": "Cannot connect to MCP server"
}
```

### 6.2 Readiness Endpoint

**Request:**
```
GET /ready HTTP/1.1
```

**Response (Ready) - v0.2:**
```json
HTTP/1.1 200 OK
Content-Type: application/json

{
  "status": "ready",
  "checks": {
    "config_loaded": true,
    "upstream_reachable": true,
    "approval_store_initialized": true
  }
}
```

**Response (Not Ready) - v0.2:**
```json
HTTP/1.1 503 Service Unavailable
Content-Type: application/json

{
  "status": "not_ready",
  "checks": {
    "config_loaded": true,
    "upstream_reachable": false,
    "approval_store_initialized": true
  },
  "reason": "upstream_unreachable"
}
```

**Response (Ready) - v0.3+:**
```json
HTTP/1.1 200 OK
Content-Type: application/json

{
  "status": "ready",
  "checks": {
    "config_loaded": true,
    "policies_loaded": true,
    "upstream_reachable": true,
    "task_store_initialized": true
  }
}
```

### 6.3 Internal State

```rust
pub enum LifecycleState {
    Starting,       // Initialization in progress
    Ready,          // Accepting traffic
    ShuttingDown,   // Draining, rejecting new requests
    Stopped,        // Shutdown complete
}

/// Lifecycle Manager (v0.3 implementation)
///
/// Uses ArcSwap for lock-free state reads and CancellationToken for
/// cooperative shutdown signalling (replaces broadcast::Sender).
pub struct LifecycleManager {
    state: ArcSwap<LifecycleState>,
    started_at: Instant,
    cancellation_token: CancellationToken,
    active_requests: AtomicUsize,
    // Readiness subsystem checks
    upstream_health: AtomicBool,
    config_loaded: AtomicBool,
    approval_store_initialized: AtomicBool,
    // Metadata
    version: &'static str,
    tg_metrics: Option<Arc<ThoughtGateMetrics>>,
}
```

## 7. Behavior Specification

### F-001: Startup Sequencing (v0.2)

The system MUST initialize in this order:

```
┌─────────────────────────────────────────────────────────────────┐
│                    v0.2 STARTUP SEQUENCE                        │
│                                                                 │
│  1. Load configuration                                          │
│     • Parse environment variables                               │
│     • Load YAML config file (REQ-CFG-001)                       │
│     • Validate required settings                                │
│     • Set state: Starting                                       │
│                                                                 │
│  2. Initialize observability                                    │
│     • Setup logging (tracing subscriber)                        │
│     • Setup metrics (Prometheus registry)                       │
│                                                                 │
│  3. Initialize approval store                                   │
│     • Create PendingApprovalStore                               │
│     • Initialize Slack adapter (REQ-GOV-003)                    │
│                                                                 │
│  4. Connect to upstream                                         │
│     • Verify upstream is reachable (if required)                │
│     • Initialize HTTP client                                    │
│                                                                 │
│  5. Start HTTP servers                                          │
│     • Start admin server on port 7469 (health/ready/metrics)   │
│     • Reserve inbound port 7468 (for future callbacks)         │
│     • Bind outbound port 7467 (MCP traffic)                    │
│     • Health endpoint available immediately                     │
│     • Set state: Ready                                          │
│     • Main endpoints accept traffic                             │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

**v0.3+ additions to startup:**
```
│  4. Load Cedar policies (v0.3+)                                 │
│     • Load from ConfigMap/file                                  │
│     • Validate against schema                                   │
│     • Start hot-reload watcher                                  │
│                                                                 │
│  5. Initialize task store (v0.3+)                               │
│     • Create in-memory store                                    │
│     • Start TTL cleanup task                                    │
```

**Startup Dependency Graph with Failure Modes:**

| Step | Component | Failure Mode | Recovery |
|------|-----------|--------------|----------|
| 1 | Load configuration (REQ-CFG-001) | **FAIL FAST** | Fix config, restart |
| 2 | Initialize observability | **FAIL FAST** | Check log/metrics config |
| 3a | Initialize TaskStore (REQ-GOV-001) | Always succeeds | In-memory, no deps |
| 3b | Initialize Slack adapter (REQ-GOV-003) | **WARNING** | Start without approval capability |
| 4 | Connect to upstream (REQ-CORE-003) | **WARNING** | Start, but NOT Ready |
| 5 | Initialize Cedar engine (REQ-POL-001) | **FAIL FAST** (v0.3+) | Fix policies, restart |
| 6a | Bind admin port 7469 (REQ-CORE-003) | **FAIL FAST** | Check port conflicts |
| 6b | Reserve inbound port 7468 | **WARN** | Port may already be in use |
| 6c | Bind outbound port 7467 (REQ-CORE-003) | **FAIL FAST** | Check port conflicts |
| 7 | Set Ready state | Only after all above | — |

**Key Principle:** Components that affect request routing (config, Cedar) must fail fast. Components that affect specific features (Slack, upstream) can degrade gracefully.

- **F-001.1:** Fail fast if configuration is invalid
- **F-001.2:** Log each startup phase with timing
- **F-001.3:** Health endpoint available before ready
- **F-001.4:** Do not accept traffic until ready
- **F-001.5:** Emit `startup_duration_seconds` metric

### F-002: Health Check (v0.2)

```rust
pub async fn health_check(manager: &LifecycleManager) -> HealthResponse {
    // Health check is simple: are we running and not stopped?
    let state = manager.state.load();
    
    match state {
        LifecycleState::Stopped => HealthResponse {
            status: "unhealthy",
            reason: Some("stopped"),
        },
        _ => HealthResponse {
            status: "healthy",
            version: env!("CARGO_PKG_VERSION"),
            uptime_seconds: manager.started_at.elapsed().as_secs(),
        },
    }
}
```

- **F-002.1:** Return 200 if process is alive and not stopped
- **F-002.2:** Return 503 if process is stopped
- **F-002.3:** Include version and uptime in response
- **F-002.4:** Must complete in < 100ms

### F-003: Readiness Check (v0.2)

```rust
pub struct ReadinessChecks {
    pub config_loaded: bool,
    pub upstream_reachable: bool,
    pub approval_store_initialized: bool,
}

pub async fn readiness_check(
    manager: &LifecycleManager,
    config: &Config,
    upstream: &UpstreamClient,
    approval_store: &PendingApprovalStore,
) -> ReadinessResponse {
    let checks = ReadinessChecks {
        config_loaded: config.is_valid(),
        upstream_reachable: upstream.health_check().await.is_ok(),
        approval_store_initialized: approval_store.is_initialized(),
    };
    
    let ready = checks.config_loaded 
        && checks.upstream_reachable 
        && checks.approval_store_initialized;
    
    ReadinessResponse {
        status: if ready { "ready" } else { "not_ready" },
        checks,
        reason: if !ready { Some(first_failing_check(&checks)) } else { None },
    }
}
```

> **Note:** `PendingApprovalStore::is_initialized()` is defined in REQ-GOV-001 §6.2.
> For the in-memory store, this returns `true` after construction (always initialized).

- **F-003.1:** Check all required subsystems
- **F-003.2:** Return 200 only if ALL checks pass
- **F-003.3:** Return 503 with details if any check fails
- **F-003.4:** Cache upstream check result (don't hit upstream on every probe)
- **F-003.5:** Must complete in < 200ms

### F-004: Graceful Shutdown (v0.2)

```
┌─────────────────────────────────────────────────────────────────┐
│                    v0.2 SHUTDOWN SEQUENCE                       │
│                                                                 │
│  1. Receive signal (SIGTERM/SIGINT)                             │
│     • Log signal received                                       │
│     • Set state: ShuttingDown                                   │
│     • Stop accepting new requests                               │
│                                                                 │
│  2. Cancel pending approvals                                    │
│     • Signal all PendingApproval waits to cancel                │
│     • Return -32603 to blocked clients                          │
│     • Log each cancelled approval                               │
│                                                                 │
│  3. Drain in-flight requests                                    │
│     • Wait for active_requests to reach 0                       │
│     • Or timeout after drain_timeout                            │
│     • Log progress every second                                 │
│                                                                 │
│  4. Cleanup resources                                           │
│     • Close upstream connections                                │
│     • Flush metrics                                             │
│     • Flush logs                                                │
│                                                                 │
│  5. Exit                                                        │
│     • Set state: Stopped                                        │
│     • Exit with code 0 (clean) or 1 (forced)                    │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

- **F-004.1:** Stop accepting new requests immediately on signal
- **F-004.2:** Cancel all pending approval waits
- **F-004.3:** Allow in-flight requests to complete (up to drain timeout)
- **F-004.4:** Return 503 for requests arriving during shutdown
- **F-004.5:** Log pending request counts during drain
- **F-004.6:** Force shutdown if drain timeout exceeded
- **F-004.7:** Exit with code 0 on clean shutdown, non-zero on forced

### F-005: Request Draining (v0.2)

```rust
pub async fn drain_requests(
    manager: &LifecycleManager,
    timeout: Duration,
) -> DrainResult {
    let deadline = Instant::now() + timeout;
    
    loop {
        let active = manager.active_requests.load(Ordering::SeqCst);
        
        if active == 0 {
            return DrainResult::Complete;
        }
        
        if Instant::now() > deadline {
            tracing::warn!(
                active_requests = active,
                "Drain timeout exceeded, forcing shutdown"
            );
            return DrainResult::Timeout { remaining: active };
        }
        
        tracing::info!(
            active_requests = active,
            "Draining requests..."
        );

        // Note: implementation logs progress every 5s (not every iteration)
        // to reduce log volume during drain
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
```

### F-006: Pending Approval Cleanup on Shutdown (v0.2)

In v0.2, pending approvals are tracked as SEP-1686 tasks waiting for Slack responses:

```rust
pub async fn cancel_pending_approvals(
    approval_store: &PendingApprovalStore,
    cancellation_token: &CancellationToken,
) {
    let count = approval_store.count();

    if count > 0 {
        tracing::info!(
            pending_approvals = count,
            "Cancelling pending approvals due to shutdown"
        );

        // Signal all waiters to cancel via CancellationToken
        cancellation_token.cancel();
        
        // Each waiter will:
        // 1. Receive shutdown signal
        // 2. Return ApprovalOutcome::Shutdown (server-initiated shutdown)
        // 3. Pipeline returns -32603 to client
        
        // Log each cancellation
        for entry in approval_store.iter() {
            tracing::info!(
                correlation_id = %entry.id,
                tool = %entry.tool_name,
                "Cancelled pending approval due to shutdown"
            );
        }
    }
}
```

**v0.2 Behavior (SEP-1686 Task Mode):**
- On shutdown, find all tasks in `Working` or `InputRequired`
- Transition each to `Failed` with reason `service_shutdown`
- Log each failed task with task_id and original tool
- Agent will see failure when polling `tasks/result` and can resubmit

| Option | Behavior | When to Use |
|--------|----------|-------------|
| **Fail** | Transition to `Failed` with `service_shutdown` | Default (v0.2) |
| **Wait** | Brief wait for pending approvals | If approvals expected soon |
| **Persist** | Save to external store | Future version (v0.3+) |

### F-007: Pending Task Handling on Shutdown (v0.3+ Reference)

In v0.3+, additional options may be available:

- **Persist:** Save pending tasks to external store for recovery
- **Migrate:** Hand off pending tasks to another instance
- **Drain:** Wait for all pending tasks with configurable timeout

### F-008: Upstream Health Check

- **F-008.1:** Periodic health check to upstream (configurable interval, default 30s)
- **F-008.2:** Cache result for readiness probe
- **F-008.3:** Simple connectivity check (TCP connect or HTTP HEAD)
- **F-008.4:** Update metric on health change

```rust
async fn check_upstream_health(client: &UpstreamClient) -> bool {
    // Simple TCP connect check, or HTTP HEAD if supported
    client.health_check().await.is_ok()
}
```

## 8. Non-Functional Requirements

### NFR-001: Observability

**Metrics:**
```
thoughtgate_lifecycle_state{state="starting|ready|shutting_down"}
thoughtgate_uptime_seconds
thoughtgate_startup_duration_seconds
thoughtgate_shutdown_duration_seconds
thoughtgate_active_requests
thoughtgate_pending_approvals              # v0.2
thoughtgate_pending_tasks                  # v0.3+
thoughtgate_upstream_health{status="healthy|unhealthy"}
thoughtgate_drain_timeout_total
thoughtgate_approvals_cancelled_shutdown   # v0.2
```

**Logging:**
```json
{"level":"info","message":"Starting ThoughtGate","version":"0.2.2"}
{"level":"info","message":"Configuration loaded","source":"config.yaml","workflows":3}
{"level":"info","message":"Upstream connected","url":"http://mcp-server:3000"}
{"level":"info","message":"ThoughtGate ready","startup_duration_ms":1234}
{"level":"info","message":"Shutdown signal received","signal":"SIGTERM"}
{"level":"info","message":"Cancelling pending approvals","count":2}
{"level":"info","message":"Cancelled pending approval","correlation_id":"abc-123","tool":"delete_user"}
{"level":"info","message":"Draining requests","active_requests":5}
{"level":"info","message":"Shutdown complete","duration_ms":2500}
```

### NFR-002: Performance

| Metric | Target |
|--------|--------|
| Startup time | < 10s to ready |
| Health check latency | < 100ms |
| Readiness check latency | < 200ms |
| Shutdown (no pending) | < 5s |

### NFR-003: Reliability

- Health endpoint must never panic
- Shutdown must always complete (forced if needed)
- No resource leaks on restart cycles
- Pending approvals must receive error on shutdown (no hanging connections)

## 9. Verification Plan

### 9.1 v0.2 Edge Case Matrix

| Scenario | Expected Behavior | Test ID |
|----------|-------------------|---------|
| Clean startup | Ready in < 10s | EC-OPS-001 |
| Missing config file | Fail fast with clear error | EC-OPS-002 |
| Invalid config YAML | Fail fast with validation error | EC-OPS-003 |
| Upstream unreachable at start | Start, but not ready | EC-OPS-004 |
| SIGTERM received | Begin graceful shutdown | EC-OPS-005 |
| Requests during shutdown | Return 503 | EC-OPS-006 |
| Drain completes | Exit 0 | EC-OPS-007 |
| Drain timeout | Force exit, log warning | EC-OPS-008 |
| Pending approvals at shutdown | Cancel waits, return -32603 | EC-OPS-009 |
| Health check during startup | Return 503 until ready | EC-OPS-010 |
| Upstream becomes unreachable | Readiness fails, health OK | EC-OPS-011 |
| Rapid restart cycles | No resource leaks | EC-OPS-012 |
| SIGQUIT received | Immediate shutdown | EC-OPS-013 |
| Client waiting for approval during shutdown | Receives -32603 error | EC-OPS-014 |
| SIGTERM during startup | Abort startup, exit cleanly | EC-OPS-015 |
| SIGKILL during operation | Immediate termination (no graceful) | EC-OPS-016 |
| Config file deleted while running | Keep old config, log error | EC-OPS-017 |
| Drain timeout = 0 | Immediate force shutdown | EC-OPS-018 |
| OOM condition | Log error, exit 137 | EC-OPS-019 |
| Health probe during shutdown | Return 200 (process still alive; correct K8s liveness semantic) | EC-OPS-020 |

### 9.2 v0.2 Assertions

**Unit Tests:**
- `test_startup_sequence_order` — Phases execute in correct order
- `test_config_loading` — YAML config loads and validates
- `test_readiness_checks` — All checks evaluated correctly
- `test_shutdown_state_transitions` — State machine correct
- `test_pending_approval_cancellation` — Approvals cancelled on shutdown

**Integration Tests:**
- `test_kubernetes_probes` — Probes work with K8s-style requests
- `test_graceful_shutdown` — Requests complete during drain
- `test_drain_timeout` — Forced shutdown after timeout
- `test_approval_cancelled_on_shutdown` — Blocked clients receive error

**Chaos Tests:**
- `test_rapid_restart_cycles` — 100 start/stop cycles, no leaks
- `test_shutdown_under_load` — Shutdown while handling 1000 req/s
- `test_shutdown_with_pending_approvals` — Multiple pending approvals cancelled

## 10. Implementation Reference

### Lifecycle Manager (v0.3)

```rust
pub struct LifecycleManager {
    state: ArcSwap<LifecycleState>,
    cancellation_token: CancellationToken,
    started_at: Instant,
    active_requests: Arc<AtomicUsize>,
    // Readiness checks
    upstream_health: AtomicBool,
    config_loaded: AtomicBool,
    approval_store_initialized: AtomicBool,
    // Metadata
    version: &'static str,
    tg_metrics: Option<Arc<ThoughtGateMetrics>>,
}

impl LifecycleManager {
    pub fn new(version: &'static str) -> Self {
        Self {
            state: ArcSwap::new(Arc::new(LifecycleState::Starting)),
            cancellation_token: CancellationToken::new(),
            started_at: Instant::now(),
            active_requests: Arc::new(AtomicUsize::new(0)),
            upstream_health: AtomicBool::new(false),
            config_loaded: AtomicBool::new(false),
            approval_store_initialized: AtomicBool::new(false),
            version,
            tg_metrics: None,
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(**self.state.load(), LifecycleState::Ready)
    }

    pub fn is_shutting_down(&self) -> bool {
        matches!(
            **self.state.load(),
            LifecycleState::ShuttingDown | LifecycleState::Stopped
        )
    }

    pub fn begin_shutdown(&self) {
        self.state.store(Arc::new(LifecycleState::ShuttingDown));
        self.cancellation_token.cancel();
    }

    /// Returns a CancellationToken that components can await for shutdown.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.cancellation_token.clone()
    }

    pub fn track_request(&self) -> RequestGuard {
        self.active_requests.fetch_add(1, Ordering::SeqCst);
        RequestGuard {
            counter: Arc::clone(&self.active_requests),
        }
    }
}

pub struct RequestGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}
```

### Signal Handler

```rust
async fn setup_signal_handlers(lifecycle: Arc<LifecycleManager>) {
    let mut sigterm = signal(SignalKind::terminate()).unwrap();
    let mut sigint = signal(SignalKind::interrupt()).unwrap();
    
    tokio::select! {
        _ = sigterm.recv() => {
            tracing::info!("Received SIGTERM");
        }
        _ = sigint.recv() => {
            tracing::info!("Received SIGINT");
        }
    }
    
    lifecycle.begin_shutdown();
}
```

### Shutdown with Approval Cancellation (v0.2)

```rust
async fn graceful_shutdown(
    lifecycle: Arc<LifecycleManager>,
    approval_store: Arc<PendingApprovalStore>,
    config: &Config,
) -> ExitCode {
    // 1. Signal shutdown
    lifecycle.begin_shutdown();
    
    // 2. Cancel pending approvals (they'll receive shutdown signal)
    let pending = approval_store.count();
    if pending > 0 {
        tracing::info!(pending_approvals = pending, "Cancelling pending approvals");
        // The shutdown broadcast already sent in begin_shutdown()
        // Each waiter is listening and will return
    }
    
    // 3. Drain in-flight requests
    let drain_result = drain_requests(&lifecycle, config.drain_timeout).await;
    
    // 4. Cleanup
    // ... close connections, flush metrics, etc.
    
    // 5. Exit
    match drain_result {
        DrainResult::Complete => ExitCode::SUCCESS,
        DrainResult::Timeout { remaining } => {
            tracing::warn!(remaining_requests = remaining, "Forced shutdown");
            ExitCode::FAILURE
        }
    }
}
```

### Anti-Patterns to Avoid

- **❌ Blocking health checks:** Use async, never block
- **❌ Side effects in probes:** Probes must be read-only
- **❌ Ignoring drain timeout:** Always force shutdown eventually
- **❌ Leaking connections:** Close all connections on shutdown
- **❌ Sync shutdown in async context:** Use proper async shutdown
- **❌ Hanging approval waits:** Always signal pending approvals on shutdown

## 11. Definition of Done

### 11.1 v0.2 Definition of Done

- [ ] Startup sequencing implemented with logging
- [ ] YAML configuration loading (REQ-CFG-001)
- [ ] `/health` endpoint implemented and tested
- [ ] `/ready` endpoint with v0.2 checks (config, upstream, approval_store)
- [ ] SIGTERM/SIGINT handlers installed
- [ ] Pending approval cancellation on shutdown
- [ ] Request draining with timeout
- [ ] Metrics for lifecycle events
- [ ] All edge cases (EC-OPS-001 to EC-OPS-014) covered
- [ ] Tested with Kubernetes probe configuration
- [ ] No resource leaks after 100 restart cycles
- [ ] Blocked clients receive -32603 on shutdown

### 11.2 v0.3+ Definition of Done (Future)

- [ ] Cedar policy loading and validation
- [ ] Policy hot-reload watcher
- [ ] Task store initialization with TTL cleanup
- [ ] Pending task failure on shutdown (with state transition)
- [ ] `/ready` endpoint with policy check
