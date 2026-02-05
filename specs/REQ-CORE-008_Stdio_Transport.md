# REQ-CORE-008: stdio Transport & CLI Wrapper

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-CORE-008` |
| **Title** | stdio Transport & CLI Wrapper |
| **Type** | Core Mechanic |
| **Status** | Draft |
| **Priority** | **Critical** |
| **Version** | v0.3 |
| **Tags** | `#mcp` `#transport` `#stdio` `#cli-wrapper` `#config-rewrite` `#ndjson` `#process-lifecycle` |

## 1. Context & Decision Rationale

This requirement defines ThoughtGate's **stdio transport mode**, enabling MCP governance for local development environments where agents spawn MCP servers as child processes communicating via stdin/stdout pipes.

### 1.1 Motivation

MCP supports two transports:

| Transport | Protocol | Use Case | ThoughtGate Support |
|-----------|----------|----------|---------------------|
| **HTTP+SSE (Streamable HTTP)** | POST-based with SSE responses | Cloud/container deployments | v0.2 (shipped) |
| **stdio** | Newline-delimited JSON-RPC over stdin/stdout | Local development | This requirement |

ThoughtGate v0.2 supports HTTP+SSE only. Without stdio support, evaluating ThoughtGate requires Kubernetes — a barrier that kills casual trial. The CLI wrapper (`thoughtgate wrap -- claude-code`) enables "5 minutes to value" for developers using Claude Desktop, Claude Code, Cursor, VS Code, and other local MCP clients.

### 1.2 Security Motivation

stdio transport is where the majority of MCP attacks currently land and where the *least* existing protection exists. The "Localhost Fallacy" — the assumption that local processes are inherently trustworthy — is the root cause of multiple real-world incidents:

- MCP servers spawned via stdio inherit the user's full process privileges with no sandboxing
- stdio has no authentication, encryption, or access control
- Message framing via newlines is vulnerable to smuggling when tool outputs contain embedded newlines
- A compromised MCP server has the same filesystem/network access as the user

The CLI wrapper inserts ThoughtGate's entire 4-gate governance flow into this unprotected gap.

### 1.3 Architectural Principle: Perimeter Consistency

**Key Decision: ThoughtGate wraps the agent, not individual MCP servers.**

This mirrors the sidecar model from HTTP mode. In HTTP mode, ThoughtGate sits between the agent and upstream servers. In stdio mode, ThoughtGate becomes the parent process that owns the agent's MCP perimeter:

```
HTTP mode (sidecar):
┌──────┐     ┌───────────────┐     ┌────────────┐
│Agent │────▶│ ThoughtGate   │────▶│ MCP Server │
│      │◀────│ :8080 (HTTP)  │◀────│ (upstream) │
└──────┘     └───────────────┘     └────────────┘

stdio mode (wrapper):
┌─────────────────────────────────────────────────────┐
│ ThoughtGate (parent process)                        │
│                                                     │
│   ┌──────┐     ┌───────┐     ┌────────────┐        │
│   │Agent │────▶│ shim  │────▶│ MCP Server │        │
│   │(child│◀────│ proxy │◀────│ (child)    │        │
│   │ proc)│     └───────┘     └────────────┘        │
│   └──────┘     ┌───────┐     ┌────────────┐        │
│        │──────▶│ shim  │────▶│ MCP Server │        │
│        │◀──────│ proxy │◀────│ (child)    │        │
│                └───────┘     └────────────┘        │
└─────────────────────────────────────────────────────┘
```

Both modes present the same abstraction: ThoughtGate owns the agent's perimeter.

### 1.4 Interception Strategy: Command Replacement

Every production MCP security tool (mcp-guardian, mcp-proxy, mcp-guard, Invariant Gateway) uses the same injection pattern: **replace the original server command in the agent's MCP config with a proxy binary that spawns the real server as a child process**. This works because all MCP clients share a common config schema with `command`/`args`/`env` fields.

ThoughtGate adopts this proven pattern. The `thoughtgate wrap` command:
1. Detects the target application and locates its MCP config file
2. Backs up the original config
3. Rewrites each server's `command` to `thoughtgate shim` with the original command as trailing args
4. Launches the target application
5. On exit, restores the original config

The agent connects to what it thinks are MCP servers, but each "server" is a ThoughtGate shim that proxies the real server with full governance.

### 1.5 Binary Architecture

This requirement spans two subcommands within the single `thoughtgate` CLI binary:

| Subcommand | Role | Lifecycle |
|------------|------|-----------|
| `thoughtgate wrap` | Config discovery, backup, rewriting, app launch, cleanup | Runs once per session |
| `thoughtgate shim` | Per-server stdio proxy with governance | Spawned N times (one per MCP server) |

The shim logic is embedded in the `thoughtgate` binary as a library module, not a separate binary. Config rewrites `command` to `thoughtgate shim --server-id x -- original-command args`.

### 1.6 Workspace Architecture

ThoughtGate uses a three-crate Cargo workspace. This split ensures transport-agnostic logic (governance, Cedar, JSON-RPC classification, profiles, telemetry) is reusable across both the HTTP sidecar and the stdio CLI binaries.

| Crate | Type | Owns | Depends On |
|-------|------|------|-----------|
| `thoughtgate-core` | library | Profile, ProfileBehaviour, JsonRpcMessageKind, GovernanceEngine trait, governance HTTP service handler, Cedar evaluation, governance API types (request/response), StreamDirection, OTel metric instruments, span builders, redaction pipeline | `cedar-policy`, `serde`, `serde_json`, `tokio`, `tracing`, `thiserror`, `opentelemetry`, `aho-corasick`, `regex` |
| `thoughtgate-proxy` | binary | HTTP sidecar proxy (v0.2 transport), Kubernetes sidecar injection | `thoughtgate-core`, `axum`, `hyper`, `tower` |
| `thoughtgate` | binary | `wrap` + `shim` subcommands, ConfigAdapter trait + impls, StdioMessage (with `raw` field), NDJSON parser, ShimOptions, McpServerEntry, process lifecycle types, FramingError, StdioError, ConfigGuard | `thoughtgate-core`, `clap`, `nix`, `tokio`, `reqwest`, `dirs` |

```
thoughtgate/
├── Cargo.toml                       # [workspace]
├── thoughtgate-core/                # Transport-agnostic library
│   └── src/
│       ├── lib.rs
│       ├── profile.rs               # Profile, ProfileBehaviour (§6.7)
│       ├── jsonrpc.rs               # JsonRpcMessageKind, classify_jsonrpc() (§6.5)
│       ├── governance/
│       │   ├── api.rs               # GovernanceEvaluateRequest/Response (§7.4 F-016)
│       │   ├── engine.rs            # GovernanceEngine trait (REQ-CORE-003 §6.5)
│       │   ├── cedar.rs             # Cedar evaluation (REQ-POL-001)
│       │   └── service.rs           # Governance HTTP service handler
│       ├── telemetry/
│       │   ├── mod.rs
│       │   ├── attributes.rs        # OTel semantic attribute constants
│       │   ├── spans.rs             # Span builders (McpToolCallSpan, etc.)
│       │   ├── context.rs           # W3C trace context propagation
│       │   ├── redact.rs            # Redaction pipeline
│       │   ├── audit.rs             # OCSF audit event types
│       │   ├── metrics.rs           # Metric instrument definitions (NFR-002)
│       │   └── init.rs              # OTel SDK initialisation
│       └── error.rs                 # Shared governance errors
│
├── thoughtgate-proxy/               # Binary: HTTP sidecar (not modified by this spec)
│   └── src/
│       └── main.rs
│
└── thoughtgate/                     # Binary: wrap + shim subcommands
    └── src/
        ├── main.rs                  # clap dispatch
        ├── wrap/
        │   ├── config_adapter.rs    # ConfigAdapter trait + impls (§6.2)
        │   ├── config_guard.rs      # ConfigGuard RAII (§10.2)
        │   ├── agent_launch.rs      # F-008, F-009, F-010
        │   └── mod.rs
        ├── shim/
        │   ├── ndjson.rs            # NDJSON framing, StdioMessage (§6.5)
        │   ├── proxy.rs             # Bidirectional proxy (F-012)
        │   ├── lifecycle.rs         # ServerProcessState, ShutdownRequest (§6.6)
        │   └── mod.rs
        └── error.rs                 # StdioError, FramingError (§6.5, §6.8)
```

**Rationale for three crates:**

1. **`thoughtgate-core` must exist as a library** because both binaries (`thoughtgate-proxy` for HTTP sidecar and `thoughtgate` for stdio CLI) need governance logic, Cedar evaluation, profile types, and telemetry. Without it, either one binary depends on the other or types are duplicated.

2. **Two separate binaries** (`thoughtgate-proxy` and `thoughtgate`) rather than one binary with subcommands because:
   - The HTTP sidecar runs as a long-lived Kubernetes pod; the CLI wraps short-lived agent sessions
   - Different operational contexts: sidecar is injected by mutation webhook; CLI is invoked by users
   - Separate binaries allow independent versioning and deployment
   - Container image size: stdio CLI doesn't need HTTP server dependencies

3. **Feature flags were rejected** in favour of explicit crate boundaries because conditional compilation complicates AI-assisted development (Claude Code cannot reason about `#[cfg(feature)]` gating across files) and adds build matrix complexity.

> **Note:** The governance service handler — the axum HTTP service that shims connect to on `127.0.0.1` (dynamic port by default) — is defined as library code in `thoughtgate-core/src/governance/service.rs`. The startup/binding/lifecycle orchestration lives in `thoughtgate/src/wrap/`. This mirrors how `axum::Router` is defined as a library construct but instantiated by the binary.

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-CORE-003 | **Extends** | Adds stdio as transport alongside HTTP+SSE. Shares JSON-RPC parsing, method routing |
| REQ-CORE-004 | **Uses** | Error types for stdio-specific failures (process crash, pipe broken, framing error) |
| REQ-CORE-005 | **Coordinates with** | Lifecycle: graceful shutdown must terminate all child processes |
| REQ-POL-001 | **Receives from** | Routing decisions (Green/Amber/Approval/Red) — transport-agnostic |
| REQ-GOV-001 | **Provides to** | Task lifecycle for approval workflows (same as HTTP mode) |
| REQ-GOV-002 | **Provides to** | Approval execution pipeline (same as HTTP mode) |
| REQ-GOV-003 | **Provides to** | Approval integration (Slack) (same as HTTP mode) |
| REQ-CFG-001 | **Extends** | Configuration schema extended with `wrapper` and `profiles` sections |

## 3. Intent

The system must:
1. Discover and parse MCP configuration files for supported agents
2. Rewrite server entries to route through ThoughtGate shim proxies
3. Spawn and manage the target agent as a child process
4. For each MCP server, proxy bidirectional NDJSON stdio streams with governance
5. Apply the full 4-gate governance flow (Visibility → Governance → Cedar Policy → Approval) identically to HTTP mode
6. Manage process lifecycle (graceful shutdown, crash recovery, signal forwarding)
7. Restore original configuration on exit
8. Validate message framing integrity as a security defence

## 4. Scope

### 4.1 In Scope

- Config discovery and parsing for: Claude Desktop, Claude Code, Cursor, VS Code, Windsurf, Zed
- Config backup and atomic rewrite
- Config restoration on exit (normal, crash, signal)
- `thoughtgate shim` subcommand for per-server stdio proxying
- NDJSON message framing (newline-delimited JSON-RPC 2.0)
- Bidirectional message interception (agent→server and server→agent)
- Full 4-gate governance flow on intercepted messages
- Server process spawning and lifecycle management (Unix/macOS)
- Signal forwarding (SIGTERM, SIGINT)
- Graceful shutdown with child process cleanup
- Message framing validation (reject smuggled/malformed messages)
- Configuration profiles (`production` / `development`)
- Structured audit logging per intercepted message

### 4.2 Out of Scope

- **Windows support** — deferred to future version. Windows requires named pipes, Job Objects, and `GenerateConsoleCtrlEvent` instead of POSIX signals. Estimated 2-3 weeks additional effort.
- **HTTP_PROXY interception** — deferred. Setting `HTTP_PROXY` to intercept agent outbound HTTP (for A2A or external API calls) is architecturally sound but adds scope. Focus v0.3 on stdio MCP governance only.
- **Auto-detection of running agents** — ThoughtGate requires explicit `wrap` invocation. No daemon mode, no background injection.
- **GUI / system tray integration** — CLI only for v0.3.
- **Server restart on crash** — if an MCP server process dies, the shim reports the error and exits. No automatic restart with backoff (defer to v0.4+).
- **Remote stdio** — SSH-tunnelled or network-bridged stdio transport. Local processes only.
- **Agent-initiated config changes during session** — If the user installs or removes an MCP server via the agent's UI while ThoughtGate is running, the agent overwrites the rewritten config. On exit, ThoughtGate restores its backup — losing the newly added server. This is a fundamental limitation of the config-rewrite injection model shared by all tools in this category. **Workaround:** exit the wrapped session and re-run `thoughtgate wrap`. Future versions may watch the config file for external modifications and merge changes.

## 5. Constraints

### 5.1 Runtime & Dependencies

| Constraint | Value | Rationale |
|------------|-------|-----------|
| **Async runtime** | Tokio (full features) | Required for async process I/O, signal handling |
| **Target platforms** | macOS (aarch64, x86_64), Linux (x86_64, aarch64) | Primary developer platforms. Windows deferred. |
| **Rust edition** | 2024 | Latest stable edition; aligns with new workspace |
| **Binary location** | Single `thoughtgate` binary | `wrap` and `shim` are subcommands, not separate binaries |

### 5.2 Protocol Constraints

| Constraint | Value | Rationale |
|------------|-------|-----------|
| **Message framing** | NDJSON (newline-delimited JSON) | MCP stdio specification. NOT LSP-style `Content-Length` headers. |
| **JSON-RPC version** | 2.0 | MCP requirement |
| **Message direction** | Bidirectional | Servers can send requests TO clients (`sampling/createMessage`, `roots/list`) |
| **Max message size** | 10MB | Defence against memory exhaustion from large base64-encoded tool responses |
| **Encoding** | UTF-8 | JSON-RPC requirement |

### 5.3 Filesystem Constraints

| Constraint | Value | Rationale |
|------------|-------|-----------|
| **Config backup** | `.thoughtgate-backup` suffix | Must survive unexpected termination |
| **Backup written before rewrite** | Always | Crash between backup and rewrite = safe (original preserved) |
| **Restore on exit** | Best-effort | Register cleanup via signal handlers AND `Drop` trait |
| **No concurrent wrapping** | One `thoughtgate wrap` per agent config file | Use file lock (flock) on `<config_path>.thoughtgate-lock` — a separate lock file that survives atomic saves by editors |

### 5.4 Governance Constraints

| Constraint | Value | Rationale |
|------------|-------|-----------|
| **Governance mode** | Identical to HTTP | Same 4-gate flow, same Cedar policies, same approval adapters |
| **Approval delay handling** | Backpressure via bounded channel | Shim stops reading from agent stdin when approval pending |
| **Channel buffer** | 32 messages | Provides reasonable buffering without unbounded memory growth |
| **Fail-closed** | If governance engine errors, deny the message | Security default |

## 6. Interfaces

### 6.1 CLI Interface

```
thoughtgate wrap [OPTIONS] -- <COMMAND> [ARGS...]

OPTIONS:
    --agent-type <TYPE>       Override auto-detected agent type
                              [claude-desktop, claude-code, cursor, vscode, windsurf, zed, custom]
    --config-path <PATH>      Override auto-detected config file path
    --profile <PROFILE>       Configuration profile [default: production]
                              [production, development]
    --thoughtgate-config <PATH>  ThoughtGate config file [default: thoughtgate.yaml]
    --governance-port <PORT>  Port for governance service [default: 0 (OS-assigned)]
                              Use a fixed port for debugging or firewall rules.
    --no-restore              Don't restore original config on exit
    --dry-run                 Print the rewritten config diff without writing.
                              Useful for verifying config changes before committing.
    --verbose                 Enable debug logging
    --version                 Print version and exit

EXAMPLES:
    thoughtgate wrap -- claude-code
    thoughtgate wrap --agent-type cursor -- cursor .
    thoughtgate wrap --profile development -- claude-code
    thoughtgate wrap --dry-run -- claude-code
    thoughtgate wrap --config-path ~/.config/custom/mcp.json --agent-type custom -- my-agent
```

```
thoughtgate shim [OPTIONS] -- <SERVER_COMMAND> [SERVER_ARGS...]

OPTIONS:
    --server-id <ID>          Server identifier (from config rewrite)
    --governance-endpoint <URL>  ThoughtGate governance service URL
                                (injected by `wrap` during config rewrite, includes dynamic port)
    --profile <PROFILE>       Configuration profile [production, development]

NOTE: This subcommand is not intended for direct user invocation.
      It is injected into MCP config by `thoughtgate wrap`.
```

### 6.2 Config Adapter Trait

```rust
/// Trait for agent-specific config file handling.
/// Each supported agent implements this trait.
pub trait ConfigAdapter: Send + Sync {
    /// Returns the agent type identifier.
    fn agent_type(&self) -> AgentType;

    /// Discovers the config file path for this agent.
    /// Returns None if config file not found at expected location.
    fn discover_config_path(&self) -> Option<PathBuf>;

    /// Parses MCP server entries from the config file.
    fn parse_servers(&self, config_path: &Path) -> Result<Vec<McpServerEntry>, ConfigError>;

    /// Rewrites the config file, replacing server commands with shim proxies.
    /// Returns the backup file path.
    fn rewrite_config(
        &self,
        config_path: &Path,
        servers: &[McpServerEntry],
        shim_binary: &Path,
        options: &ShimOptions,
    ) -> Result<PathBuf, ConfigError>;

    /// Restores the original config from backup.
    fn restore_config(
        &self,
        config_path: &Path,
        backup_path: &Path,
    ) -> Result<(), ConfigError>;
}
```

### 6.3 MCP Server Entry

```rust
/// Represents a single MCP server entry parsed from agent config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    /// Server identifier (key in mcpServers map).
    pub id: String,

    /// Original command to spawn the server.
    pub command: String,

    /// Original arguments.
    pub args: Vec<String>,

    /// Environment variables to set for the server process.
    pub env: Option<HashMap<String, String>>,

    /// Whether this server is enabled (some clients support toggling).
    pub enabled: bool,
}
```

### 6.4 Shim Options

```rust
/// Configuration passed from `wrap` to each `shim` instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShimOptions {
    /// Server identifier.
    pub server_id: String,

    /// URL of the ThoughtGate governance service.
    pub governance_endpoint: String,

    /// Active configuration profile.
    pub profile: Profile,

    /// Path to ThoughtGate config file.
    pub config_path: PathBuf,
}
```

### 6.5 JSON-RPC Message Types

The stdio proxy operates on two layers of message abstraction: a **transport-agnostic** classification layer (shared with HTTP mode) and a **stdio-specific** message type that retains the raw NDJSON line for zero-copy forwarding.

#### 6.5.1 Transport-Agnostic Classification (in `thoughtgate-core`)

These types live in `thoughtgate-core/src/jsonrpc.rs` and are shared with the HTTP transport (REQ-CORE-003). This eliminates the duplicate `JsonRpcId` definitions that previously existed in both specs.

```rust
/// JSON-RPC ID can be string, integer, or null.
/// Shared across all transports.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    String(String),
    Number(i64),
    Null,
}

/// Transport-agnostic JSON-RPC 2.0 message classification.
/// Determined by presence/absence of `id` and `method` fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonRpcMessageKind {
    /// Has both `id` and `method` → request
    Request { id: JsonRpcId, method: String },
    /// Has `id` but no `method` → response
    Response { id: JsonRpcId },
    /// Has `method` but no `id` → notification
    Notification { method: String },
}

/// Classify a parsed JSON-RPC value without taking ownership.
/// Used by both HTTP and stdio transports.
pub fn classify_jsonrpc(value: &serde_json::Value) -> Result<JsonRpcMessageKind, JsonRpcClassifyError> {
    // Validate "jsonrpc": "2.0" presence
    let version = value.get("jsonrpc").and_then(|v| v.as_str());
    if version != Some("2.0") {
        return Err(JsonRpcClassifyError::InvalidVersion);
    }

    let id = value.get("id").map(|v| serde_json::from_value::<JsonRpcId>(v.clone()))
        .transpose()
        .map_err(|_| JsonRpcClassifyError::InvalidId)?;
    let method = value.get("method").and_then(|v| v.as_str()).map(String::from);

    match (id, method) {
        (Some(id), Some(method)) => Ok(JsonRpcMessageKind::Request { id, method }),
        (Some(id), None) => Ok(JsonRpcMessageKind::Response { id }),
        (None, Some(method)) => Ok(JsonRpcMessageKind::Notification { method }),
        (None, None) => Err(JsonRpcClassifyError::Unclassifiable),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JsonRpcClassifyError {
    #[error("Missing or invalid jsonrpc version field")]
    InvalidVersion,
    #[error("Invalid id field")]
    InvalidId,
    #[error("Message has neither id nor method")]
    Unclassifiable,
}
```

#### 6.5.2 stdio-Specific Message Type (in `thoughtgate`)

This type lives in `thoughtgate/src/shim/ndjson.rs`. It wraps the core classification with the raw NDJSON line for zero-copy forwarding and stdio-specific parsed fields.

```rust
/// A parsed NDJSON line from a stdio stream. Retains the raw line
/// for forwarding without re-serialisation.
#[derive(Debug, Clone)]
pub struct StdioMessage {
    /// Transport-agnostic classification (from thoughtgate-core).
    pub kind: JsonRpcMessageKind,
    /// Parsed params/result/error for governance inspection.
    pub params: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub error: Option<serde_json::Value>,
    /// Original NDJSON line — forwarded byte-for-byte when permitted.
    pub raw: String,
}
```

#### 6.5.3 Framing Errors (in `thoughtgate`)

These error types are stdio-specific and live in `thoughtgate/src/error.rs`.

```rust
/// Framing error detected during stdio message parsing.
#[derive(Debug, thiserror::Error)]
pub enum FramingError {
    #[error("Message exceeds maximum size of {max_bytes} bytes")]
    MessageTooLarge { max_bytes: usize },

    #[error("Malformed JSON: {reason}")]
    MalformedJson { reason: String },

    #[error("Missing required jsonrpc field")]
    MissingVersion,

    #[error("Unsupported JSON-RPC version: {version}")]
    UnsupportedVersion { version: String },

    #[error("Pipe closed unexpectedly")]
    BrokenPipe,

    #[error("JSON-RPC batch requests (arrays) are not supported")]
    UnsupportedBatch,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

### 6.6 Process Lifecycle Types

```rust
/// State of a managed MCP server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerProcessState {
    /// Process is starting up.
    Starting,
    /// Process is running and stdio proxy is active.
    Running,
    /// Process exited normally.
    Exited { code: i32 },
    /// Process was killed by signal.
    Signalled { signal: i32 },
    /// Process failed to start.
    FailedToStart { reason: String },
}

/// Shutdown request with configurable grace period.
#[derive(Debug, Clone)]
pub struct ShutdownRequest {
    /// Time to wait after closing stdin before sending SIGTERM.
    pub stdin_close_grace: Duration,
    /// Time to wait after SIGTERM before sending SIGKILL.
    pub sigterm_grace: Duration,
}

impl Default for ShutdownRequest {
    fn default() -> Self {
        Self {
            stdin_close_grace: Duration::from_secs(5),
            sigterm_grace: Duration::from_secs(2),
        }
    }
}
```

### 6.7 Configuration Profile Types

> **Crate location:** `thoughtgate-core/src/profile.rs`. Profile and ProfileBehaviour are transport-agnostic — development mode's `WOULD_BLOCK` semantics apply identically to HTTP mode. The `--profile` CLI flag in `thoughtgate` passes the parsed `Profile` enum to `thoughtgate-core`'s governance engine.

```rust
/// Named configuration profile controlling enforcement behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// All inspectors enforcing, approvals required, version pinning enforced.
    Production,
    /// Inspectors log-only, approvals auto-approved with audit trail,
    /// version pinning disabled.
    Development,
}

/// Per-feature behaviour determined by active profile.
#[derive(Debug, Clone)]
pub struct ProfileBehaviour {
    /// Whether governance decisions block (true) or log-only (false).
    pub enforcement_blocking: bool,
    /// Whether approval workflows require human interaction.
    pub approvals_required: bool,
    /// Log prefix for governance decisions.
    pub decision_log_prefix: &'static str,  // "BLOCKED" vs "WOULD_BLOCK"
}

impl Profile {
    pub fn behaviour(&self) -> ProfileBehaviour {
        match self {
            Profile::Production => ProfileBehaviour {
                enforcement_blocking: true,
                approvals_required: true,
                decision_log_prefix: "BLOCKED",
            },
            Profile::Development => ProfileBehaviour {
                enforcement_blocking: false,
                approvals_required: false,
                decision_log_prefix: "WOULD_BLOCK",
            },
        }
    }
}
```

### 6.8 Errors

> **Crate location:** `StdioError` and `FramingError` are stdio-specific and live in `thoughtgate/src/error.rs`. `StreamDirection` is transport-agnostic (HTTP mode also distinguishes inbound/outbound) and lives in `thoughtgate-core/src/lib.rs`.

```rust
/// Stream direction — shared across transports.
/// Crate: thoughtgate-core
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamDirection {
    AgentToServer,
    ServerToAgent,
}

/// Errors specific to stdio transport and CLI wrapper.
/// Crate: thoughtgate
#[derive(Debug, thiserror::Error)]
pub enum StdioError {
    #[error("Agent type could not be detected from command: {command}")]
    UnknownAgentType { command: String },

    #[error("Config file not found at {path}")]
    ConfigNotFound { path: PathBuf },

    #[error("Config file is locked by another ThoughtGate instance")]
    ConfigLocked { path: PathBuf },

    #[error("Failed to parse config: {reason}")]
    ConfigParseError { path: PathBuf, reason: String },

    #[error("Failed to write config: {reason}")]
    ConfigWriteError { path: PathBuf, reason: String },

    #[error("Failed to restore config from backup: {reason}")]
    RestoreError { backup_path: PathBuf, reason: String },

    #[error("Server process failed to start: {reason}")]
    ServerSpawnError { server_id: String, reason: String },

    #[error("Server process exited unexpectedly with code {code}")]
    ServerCrashed { server_id: String, code: i32 },

    #[error("Framing error on {direction} stream: {source}")]
    Framing {
        server_id: String,
        direction: StreamDirection,
        source: FramingError,
    },

    #[error("stdio I/O error: {0}")]
    StdioIo(std::io::Error),

    #[error("Governance engine unavailable")]
    GovernanceUnavailable,

    #[error("Agent process exited with code {code}")]
    AgentExited { code: i32 },
}
```

## 7. Functional Requirements

### 7.1 Config Discovery & Parsing

- **F-001 (Agent Type Detection):** When `--agent-type` is not specified, infer the agent type from the command name after `--`. Detection rules:

    | Command pattern | Detected type |
    |----------------|---------------|
    | `claude` (standalone), `claude-code` | `claude-code` |
    | Contains `Claude` and is a `.app` path (macOS) | `claude-desktop` |
    | `cursor` | `cursor` |
    | `code`, `code-insiders` | `vscode` |
    | `windsurf` | `windsurf` |
    | `zed` | `zed` |

    If no pattern matches and `--agent-type` is not provided, exit with `StdioError::UnknownAgentType` and a helpful message listing supported types.

- **F-002 (Config Path Discovery):** Each `ConfigAdapter` implementation discovers the config file at the platform-specific default location:

    | Agent | macOS Path | Linux Path |
    |-------|-----------|------------|
    | Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` | `~/.config/Claude/claude_desktop_config.json` |
    | Claude Code | `~/.claude.json` (user-level), `.mcp.json` (project-level, CWD) | Same |
    | Cursor | `~/.cursor/mcp.json` (global), `.cursor/mcp.json` (project) | Same |
    | VS Code | `.vscode/mcp.json` (workspace) | Same |
    | Windsurf | `~/.codeium/windsurf/mcp_config.json` | Same |
    | Zed | `~/.config/zed/settings.json` | Same |

    When both user-level and project-level configs exist, merge them (project overrides user). The `--config-path` flag overrides all auto-detection.

- **F-003 (Config Parsing):** Parse the JSON config file into `Vec<McpServerEntry>`. Handle client-specific differences:
    - Claude Desktop / Claude Code / Cursor / Windsurf: `mcpServers` key with `{command, args, env}` objects
    - VS Code: `servers` key (not `mcpServers`), with `{command, args, env}` under `type: "stdio"`
    - Zed: `context_servers` key with different nesting

    Reject configs with zero stdio servers (exit with helpful message).

- **F-004 (Environment Variable Expansion):** Expand environment variable references in parsed config values:
    - Claude Code: `${VAR}` syntax
    - Windsurf: `${env:VAR}` syntax
    - VS Code: `${input:VAR}` (skip — requires interactive prompt, not supported)

### 7.2 Config Rewriting

- **F-005 (Atomic Backup):** Before modifying any config file:
    1. Acquire an advisory file lock (`flock`) on a separate lock file at `<config_path>.thoughtgate-lock` (not on the config file itself — editors like VS Code perform atomic saves by writing a temp file and renaming over the target, which changes the inode and silently breaks locks held on the original file)
    2. **Double-wrap detection:** Check if any server entry's `command` already resolves to the `thoughtgate` binary (by comparing against `current_exe()` canonical path or checking for a `"__thoughtgate_managed": true` marker in the config). If detected, exit with a clear error: "Config already managed by ThoughtGate. Run the agent directly or restore the original config with `thoughtgate unwrap`."
    3. Copy original to `<config_path>.thoughtgate-backup`
    4. Verify backup was written successfully (read back and compare)
    5. Only then proceed to rewrite

- **F-006 (Command Replacement):** For each `McpServerEntry`, rewrite the config:

    ```json
    // Before:
    {
      "mcpServers": {
        "filesystem": {
          "command": "npx",
          "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
          "env": { "NODE_ENV": "production" }
        }
      }
    }

    // After:
    {
      "mcpServers": {
        "filesystem": {
          "command": "/path/to/thoughtgate",
          "args": [
            "shim",
            "--server-id", "filesystem",
            "--governance-endpoint", "http://127.0.0.1:19090",
            "--profile", "production",
            "--",
            "npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"
          ],
          "env": {
            "NODE_ENV": "production",
            "THOUGHTGATE_SERVER_ID": "filesystem"
          }
        }
      }
    }
    ```

    The `command` field points to the `thoughtgate` binary's absolute path (resolved via `std::env::current_exe()`). The original command and args follow `--` as positional arguments.

- **F-007 (Shim Binary Resolution):** Resolve the absolute path of the `thoughtgate` binary for config injection. Use `std::env::current_exe()` as primary, with `which::which("thoughtgate")` as fallback. **Canonicalize the result** with `std::fs::canonicalize()` to resolve symlinks — on macOS, `current_exe()` may return a symlink path that becomes invalid if the symlink target changes (e.g., after `brew upgrade`). Fail if neither resolves (the binary must be findable after the agent launches in a potentially different working directory).

- **F-007a (Dry Run):** When `--dry-run` is specified, `thoughtgate wrap` performs config discovery, parsing, and rewrite generation, then prints a unified diff of the original vs. rewritten config to stdout and exits with code 0. No backup is created, no config is modified, no processes are spawned. This enables users to verify config changes before committing.

- **F-007b (JSON-RPC Batch Rejection):** JSON-RPC 2.0 permits batch requests (a JSON array of request objects). MCP does not use batch requests and ThoughtGate does not support them. If a parsed NDJSON line is a JSON array, the shim MUST reject it with a `FramingError::UnsupportedBatch` error, log at `WARN` level, and skip the message. This is explicitly specified to prevent silent misclassification.

### 7.3 Agent Launch & Lifecycle

- **F-008 (Governance Service Startup):** Before launching the agent, start a lightweight HTTP service on `127.0.0.1` that shim instances connect to for governance decisions. By default, bind to port 0 (OS-assigned ephemeral port) to eliminate port collision errors. The actual bound port is passed to shim instances via the config rewrite. Users may override with `--governance-port <PORT>` for debugging or firewall rules. This service:
    - Loads ThoughtGate configuration (`thoughtgate.yaml`)
    - Initialises the Cedar policy engine
    - Initialises approval adapters (Slack)
    - Exposes a health endpoint at `/healthz`
    - Shares state (policy engine, task store, approval adapters) across all shim connections

- **F-009 (Agent Process Launch):** After config rewrite and governance service startup:
    1. Spawn the agent command as a child process
    2. Inherit the user's environment (augmented with `THOUGHTGATE_ACTIVE=1`)
    3. Do NOT capture the agent's stdin/stdout/stderr — the user interacts with the agent directly
    4. Store the child PID for lifecycle management

- **F-010 (Wait for Exit):** `thoughtgate wrap` blocks until the agent process exits, then:
    1. Collect exit code
    2. Send shutdown signal to governance service (set internal shutdown flag)
    3. Wait for all shim instances to drain (up to `stdin_close_grace`)
    4. Terminate remaining server processes
    5. Restore config from backup (unless `--no-restore`)
    6. Release file lock and remove `<config_path>.thoughtgate-lock`
    7. Exit with the agent's exit code

### 7.4 Shim: stdio Proxy

- **F-011 (Server Process Spawn):** The shim spawns the original MCP server command as a child process:
    ```rust
    Command::new(&server_command)
        .args(&server_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())  // Server stderr goes to terminal
        .kill_on_drop(true)
        .process_group(0)          // New process group (Unix only)
        .spawn()
    ```
    `kill_on_drop(true)` ensures the server is terminated if the shim panics or is dropped.
    `.process_group(0)` prevents Ctrl+C from propagating directly to the server — the shim controls shutdown.

    > **Security note:** `stderr(Stdio::inherit())` means a compromised MCP server can write arbitrary content to the user's terminal. This content will appear interleaved with ThoughtGate's own log output with no visual distinction. This is a known limitation of the stdio wrapping model — prefixing stderr lines would break servers that use stderr for structured logging. Users should be aware that terminal output from wrapped sessions may include untrusted content from MCP servers.

- **F-012 (Bidirectional Proxy):** The shim runs two concurrent Tokio tasks:

    **Agent→Server (outbound):**
    1. Read NDJSON lines from own stdin (agent writes to shim's stdin)
    2. Parse each line as `StdioMessage`
    3. Apply 4-gate governance flow via HTTP call to governance service
    4. If permitted: write original line to server's stdin
    5. If denied: write JSON-RPC error response to own stdout (agent reads it)
    6. If approval required: buffer message, await approval, then forward or deny

    **Server→Agent (inbound):**
    1. Read NDJSON lines from server's stdout
    2. Parse each line as `StdioMessage`
    3. Log for audit trail (locally in the shim — does NOT call the governance evaluate endpoint in v0.3)
    4. Apply governance evaluation for server→agent messages (v0.5+ — when Inspector Framework is enabled)
    5. Write original line to own stdout (agent reads it)

    > **Note:** In v0.3, server→agent messages are forwarded unconditionally after local audit logging. The `direction` field in `GovernanceEvaluateRequest` exists to support future server→agent governance (v0.5+) without API changes. The shim does not call `POST /governance/evaluate` for inbound messages in this version.

    Use `tokio::select!` to handle both directions plus child process exit concurrently.

- **F-013 (NDJSON Message Parsing):** Parse messages using `BufReader::read_line()`:
    - Each complete JSON-RPC message occupies exactly one line
    - Newlines within JSON string values MUST be escaped as `\n` (per spec)
    - After reading a line, validate:
      1. Line is valid UTF-8
      2. Line is valid JSON
      3. JSON contains `"jsonrpc": "2.0"` field
      4. Line does not exceed 10MB
    - On validation failure: log the error with raw bytes (hex-encoded if not UTF-8), return `FramingError`, and skip the message (do not crash)

- **F-014 (Message Framing Validation):** As a security defence against message smuggling:
    - After reading each NDJSON line, split the raw byte buffer on literal `0x0A` (newline) bytes. If the split produces more than one non-empty segment, check whether any segment after the first parses as valid JSON containing a `"jsonrpc"` key. If so, flag as a smuggling attempt.
    - This detects the attack where a malicious server embeds a complete JSON-RPC message inside a string value with a literal (unescaped) newline, causing a naive line-based parser to treat the embedded content as a separate message.
    - Log all framing violations at `WARN` level with server_id context and hex-encoded raw bytes
    - In `production` profile: reject the entire line
    - In `development` profile: log as `WOULD_REJECT` and forward
    - **False positive mitigation:** Only flag when the secondary segment contains a `"jsonrpc"` key — legitimate multi-line content (base64 blobs, markdown) will not trigger this heuristic

- **F-015 (Request ID Tracking):** Maintain a bidirectional request ID map per server:
    - Track outstanding request IDs sent by the agent (outbound requests awaiting responses)
    - Track outstanding request IDs sent by the server (inbound requests like `sampling/createMessage` awaiting agent responses)
    - Use this map to correlate responses with requests for audit logging
    - Detect orphaned requests (request sent but no response within configurable timeout)

- **F-016 (Governance Integration):** The shim communicates with the governance service via HTTP on localhost. The request/response types are defined as shared structs in `thoughtgate-core/src/governance/api.rs` so that both the shim (client) and governance service (server) import the same types — wire format changes are caught at compile time.

    **Governance API types (in `thoughtgate-core`):**
    ```rust
    /// Request from shim to governance service.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct GovernanceEvaluateRequest {
        pub server_id: String,
        pub direction: StreamDirection,
        pub method: String,
        /// JSON-RPC request/response ID, if present. Used for audit trail correlation.
        pub id: Option<JsonRpcId>,
        pub params: Option<serde_json::Value>,
        pub message_type: MessageType,
        pub profile: Profile,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum MessageType {
        Request,
        Response,
        Notification,
    }

    /// Response from governance service to shim.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct GovernanceEvaluateResponse {
        pub decision: GovernanceDecision,
        pub task_id: Option<String>,
        pub policy_id: Option<String>,
        pub reason: Option<String>,
        /// Suggested polling interval in milliseconds for PendingApproval decisions.
        /// The shim should poll GET /governance/task/{task_id} at this interval.
        pub poll_interval_ms: Option<u64>,
        /// When true, the shim must initiate graceful shutdown (F-018).
        /// Set by the governance service when `wrap` triggers shutdown (F-019).
        pub shutdown: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum GovernanceDecision {
        Forward,
        Deny,
        PendingApproval,
    }
    ```

    **HTTP endpoints:**
    - `POST /governance/evaluate` — submit a message for 4-gate evaluation (body: `GovernanceEvaluateRequest`)
    - Response: `GovernanceEvaluateResponse`
    - For `PendingApproval`: poll `GET /governance/task/{task_id}` until resolved
    - `POST /governance/heartbeat` — periodic shim liveness signal (body: `{ "server_id": "..." }`)
    - Response: `{ "shutdown": false }` (or `true` when wrap has triggered shutdown)
    - Shims MUST send a heartbeat every 5 seconds when idle (no messages flowing). If the response contains `"shutdown": true`, the shim initiates graceful shutdown (F-018). If the heartbeat request fails with connection refused or timeout, the shim treats this as an unclean governance service exit and initiates shutdown immediately.
    - Backpressure: while awaiting approval, stop reading from agent stdin (bounded channel fills, read task blocks)
    - Timeout: configurable per-profile (production: 5min, development: 10s auto-approve)
    - **Shutdown coordination:** When any `GovernanceEvaluateResponse` or heartbeat response contains `shutdown: true`, the shim stops accepting new messages from the agent and initiates F-018. This is the mechanism by which F-019 step 2 ("governance service signals all connected shims to drain") is implemented — the governance service sets an internal shutdown flag that is piggy-backed onto the next evaluate or heartbeat response for each connected shim.

- **F-016a (Shim Readiness Check):** Before entering the proxy loop, the shim MUST poll the governance service's `/healthz` endpoint with exponential backoff (initial: 50ms, max: 2s, max attempts: 20). If the governance service is not reachable within the backoff window, exit with `StdioError::GovernanceUnavailable`. This prevents fail-closed denials during normal startup races between shim instances and the governance service.

- **F-016b (Governance Request Timeout):** All HTTP calls from shim to governance service MUST have explicit `reqwest` timeouts:
    - `POST /governance/evaluate`: 500ms connect + 2s total request timeout
    - `GET /governance/task/{task_id}` (approval polling): 500ms connect + 5s total per poll
    - On timeout: fail-closed deny (production) or log `WOULD_DENY` and forward (development)

- **F-017 (Protocol Passthrough):** The following messages MUST be forwarded without governance evaluation:
    - `initialize` request and response (capability negotiation)
    - `initialized` notification
    - `notifications/cancelled` (cancel propagation)
    - `notifications/progress` (progress updates)
    - `notifications/resources/updated` (resource change signal)
    - `notifications/resources/list_changed` (resource list change signal)
    - `notifications/tools/list_changed` (tool list change signal)
    - `notifications/prompts/list_changed` (prompt list change signal)
    - `ping` / `pong`

    The shim MUST NOT modify `initialize` request/response content — protocol version negotiation is transparent.

### 7.5 Process Lifecycle Management

- **F-018 (Graceful Shutdown — Shim):** When the shim receives a shutdown signal (stdin EOF from agent, `shutdown: true` in a governance response or heartbeat, or SIGTERM from OS):
    1. Close stdin to the server process (signals EOF)
    2. Wait up to `stdin_close_grace` (default: 5s) for server to exit cleanly
    3. If still running: send SIGTERM to the server's process group
    4. Wait up to `sigterm_grace` (default: 2s)
    5. If still running: send SIGKILL
    6. Call `child.wait()` to prevent zombies
    7. Exit with 0 (clean shutdown) or server's exit code

- **F-019 (Graceful Shutdown — Wrap):** When `thoughtgate wrap` receives SIGTERM or SIGINT:
    1. Signal the governance service to set its internal shutdown flag
    2. Connected shims discover the shutdown flag via their next `POST /governance/evaluate` response or `POST /governance/heartbeat` response (idle shims discover within one heartbeat interval, ≤5s). Each shim initiates F-018 independently and in parallel.
    3. Wait for all shims to report exit (up to 10s wall-clock time, covering all shims concurrently)
    4. Terminate any remaining server processes
    5. Restore config from backup
    6. Exit

- **F-020 (Config Restoration Safety):** Register config restoration via multiple mechanisms for robustness:
    1. Normal exit path in `main()`
    2. `Drop` implementation on a `ConfigGuard` RAII struct
    3. Signal handler (SIGTERM, SIGINT) registered via `tokio::signal`
    4. Panic hook (`std::panic::set_hook`) as additional safety net

    If restoration fails (e.g., backup file missing), log `ERROR` with the backup path and instructions for manual restoration. Never panic during restoration.

> **Implementation Note (v0.3):** Items 1–3 are implemented (normal exit path,
> `ConfigGuard` RAII with `Drop`, and `tokio::signal` handlers for SIGTERM/SIGINT).
> Item 4 uses `std::panic::set_hook` (not `ctrlc` crate) — the panic hook ensures
> config restoration even when a panic unwinds past the normal Drop path.

- **F-021 (Zombie Prevention):** Always call `child.wait()` after termination of any child process. The `kill_on_drop(true)` on `tokio::process::Child` provides a safety net, but explicit waiting is required for clean exit code collection and log messages.

### 7.6 Configuration Profiles

- **F-022 (Profile Selection):** The `--profile` flag on both `wrap` and `shim` subcommands selects the active profile:
    - `production` (default): all governance decisions are enforcing
    - `development`: governance decisions are logged but not enforced

- **F-023 (Development Profile Behaviour):** In `development` profile:
    - Cedar policy denials are logged as `WOULD_BLOCK` but messages are forwarded
    - Approval workflows are auto-approved with a 0s delay, with `auto_approved: true` in audit log
    - Secret scanning detects but does not redact (logs `WOULD_REDACT`)
    - Message framing violations are logged as `WOULD_REJECT` but messages are forwarded
    - All decisions are still recorded in the structured audit log

- **F-024 (Profile in Audit Log):** Every audit log entry includes the active profile. This enables post-hoc analysis: "what would have been blocked in production?"

## 8. Non-Functional Requirements

### NFR-001: Performance

| Metric | Target | Rationale |
|--------|--------|-----------|
| Message forwarding latency (P99) | < 2ms | For passthrough messages (no governance delay) |
| Governance evaluation latency (P99) | < 10ms | Cedar policy evaluation via localhost HTTP |
| Config rewrite time | < 200ms | One-time cost at startup |
| Memory per proxied server | < 15MB | Shim + buffers + request ID tracking |
| Startup time (until agent launched) | < 1s | Governance service start + config rewrite |
| Message parsing throughput | > 10,000 msg/s | Per shim instance — NDJSON parsing is lightweight |

### NFR-002: Observability

**Metrics (exposed via governance service):**
```
thoughtgate_stdio_messages_total{server_id, direction, method}
thoughtgate_stdio_governance_decisions_total{server_id, decision, profile}
thoughtgate_stdio_framing_errors_total{server_id, error_type}
thoughtgate_stdio_server_state{server_id, state}
thoughtgate_stdio_approval_latency_seconds{server_id}
thoughtgate_stdio_active_servers
```

**Structured Logging:**
```json
{
  "timestamp": "2025-03-15T14:30:00.123Z",
  "level": "info",
  "component": "shim",
  "server_id": "filesystem",
  "direction": "agent_to_server",
  "method": "tools/call",
  "tool": "read_file",
  "message_id": "req-42",
  "governance_decision": "forward",
  "profile": "production",
  "latency_us": 450
}
```

**Decision logging for development profile:**
```json
{
  "timestamp": "2025-03-15T14:30:00.456Z",
  "level": "warn",
  "component": "shim",
  "server_id": "github",
  "direction": "agent_to_server",
  "method": "tools/call",
  "tool": "delete_repo",
  "message_id": "req-43",
  "governance_decision": "WOULD_BLOCK",
  "profile": "development",
  "policy_reason": "Cedar deny: delete operations require approval",
  "forwarded": true
}
```

### NFR-003: Reliability

- **Process isolation:** Server crash does not crash the shim. Shim crash does not crash other shims or the agent.
- **Fail-closed:** If governance service is unreachable, deny messages (production) or log and forward (development).
- **No zombies:** All child processes are waited on. `kill_on_drop(true)` + explicit `wait()`.
- **Config always restorable:** Backup written before rewrite. Multiple restoration mechanisms.
- **Concurrent access safe:** File lock prevents multiple `thoughtgate wrap` instances from corrupting the same config file.

## 9. Verification Plan

### 9.1 Edge Case Matrix

| Scenario | Expected Behaviour | Test ID |
|----------|-------------------|---------|
| Config file not found at expected path | Exit with helpful error listing expected path | EC-STDIO-001 |
| Config file is empty (0 bytes) | Exit with parse error | EC-STDIO-002 |
| Config file has zero MCP servers | Exit with message: "No MCP servers found in config" | EC-STDIO-003 |
| Config file has servers but all disabled | Exit with message: "All MCP servers are disabled" | EC-STDIO-004 |
| Another `thoughtgate wrap` already running (lock held) | Exit with `ConfigLocked` error | EC-STDIO-005 |
| Backup file already exists (stale from previous crash) | Overwrite stale backup, log warning | EC-STDIO-006 |
| Config rewrite fails (permission denied) | Restore from backup, exit with error | EC-STDIO-007 |
| Agent process fails to start (command not found) | Restore config, exit with error | EC-STDIO-008 |
| Server process fails to start (command not found) | Shim returns error response for all requests, logs error | EC-STDIO-009 |
| Server process crashes mid-session | Shim returns JSON-RPC error to pending requests, exits | EC-STDIO-010 |
| Agent process crashes mid-session | Wrap triggers shutdown: terminate servers, restore config | EC-STDIO-011 |
| SIGTERM during active session | Graceful shutdown: drain → SIGTERM → SIGKILL → restore | EC-STDIO-012 |
| SIGINT (Ctrl+C) during active session | Same as SIGTERM | EC-STDIO-013 |
| Message exceeds 10MB limit | Reject with `FramingError::MessageTooLarge`, skip message | EC-STDIO-014 |
| Malformed JSON on stdio stream | Log error, skip message, continue reading | EC-STDIO-015 |
| Missing `jsonrpc` field | Log error, skip message, continue reading | EC-STDIO-016 |
| Valid JSON but not JSON-RPC (missing method and id) | Drop message (production) or log `WOULD_DROP` and forward (development). Fail-closed per §5.4. | EC-STDIO-017 |
<!-- v0.3 Note: Unclassifiable messages (neither id nor method) are rejected via
FramingError::MalformedJson in ndjson.rs. In proxy.rs, malformed messages are
logged at WARN and skipped in both profiles. The fully-structured WOULD_DROP
format with development-mode forwarding is deferred to v0.4. -->
| Embedded newline smuggling attempt | Detect and reject (production) or warn (development) | EC-STDIO-018 |
| Server sends request to agent (`sampling/createMessage`) | Route server→agent, track response ID | EC-STDIO-019 |
| Server sends `notifications/tools/list_changed` | Forward to agent without governance (on F-017 passthrough whitelist) | EC-STDIO-020 |
| Approval required but Slack is unreachable | Timeout → deny (production), auto-approve (development) | EC-STDIO-021 |
| Approval pending when Ctrl+C received | Cancel pending approvals, proceed with shutdown | EC-STDIO-022 |
| VS Code config with `servers` key (not `mcpServers`) | Parse correctly via VS Code adapter | EC-STDIO-023 |
| Zed config with `context_servers` key | Parse correctly via Zed adapter | EC-STDIO-024 |
| Claude Code with both `~/.claude.json` and `.mcp.json` | Merge: project overrides user-level | EC-STDIO-025 |
| Config contains `${VAR}` env expansion | Expand at parse time, error if undefined and no default | EC-STDIO-026 |
| Server stdout produces partial line (no trailing newline) | Buffer until newline received | EC-STDIO-027 |
| Server produces output on stderr | Pass through to terminal (not intercepted) | EC-STDIO-028 |
| `thoughtgate` binary not on PATH after agent launches | Fail at shim launch — detected during config rewrite by using absolute path | EC-STDIO-029 |
| Protocol version mismatch (agent and server disagree) | Pass through transparently — ThoughtGate does not participate in version negotiation | EC-STDIO-030 |
| `notifications/cancelled` while approval pending | Abort approval workflow, forward cancellation | EC-STDIO-031 |
<!-- v0.3 Note: notifications/cancelled is on the F-017 passthrough whitelist and
forwarded without governance. Aborting pending approval workflows on cancellation
requires the F-015 request ID correlation map (implemented in v0.3) plus an approval
task cancellation API in the governance service. The abort-on-cancel flow is deferred
to v0.4. -->
| Large base64 tool response (~5MB) | Forward (under 10MB limit), no special handling | EC-STDIO-032 |
| Development profile with Cedar deny | Log `WOULD_BLOCK`, forward message, record in audit | EC-STDIO-033 |
| Config already rewritten by ThoughtGate (double-wrap) | Exit with clear error, do not nest shim inside shim | EC-STDIO-034 |
| JSON-RPC batch request (JSON array) on stdio | Reject with `FramingError::UnsupportedBatch`, log, skip | EC-STDIO-035 |
| Governance service not ready when shim starts | Poll `/healthz` with backoff; exit if unreachable after retries | EC-STDIO-036 |
| Governance `POST /evaluate` times out (service hung) | Fail-closed deny (production) or log and forward (development) | EC-STDIO-037 |
| `--dry-run` flag specified | Print config diff to stdout, exit 0, no file changes | EC-STDIO-038 |
| Agent UI adds new MCP server during wrapped session | New server not governed; original config restored on exit (known limitation) | EC-STDIO-039 |
| Governance port 19090 already in use (explicit `--governance-port` override) | Exit with clear error identifying the port conflict | EC-STDIO-040 |
| Shim idle when wrap receives SIGTERM | Heartbeat discovers `shutdown: true` within 5s, initiates F-018 | EC-STDIO-041 |
| Governance service crashes (connection refused on heartbeat) | Shim treats as unclean shutdown, initiates F-018 immediately | EC-STDIO-042 |
| Editor performs atomic save on config file during session | Lock on `.thoughtgate-lock` is unaffected by inode change | EC-STDIO-043 |
| Stale `.thoughtgate-lock` file from previous crash | `flock` on the file succeeds (advisory lock was released on process exit); proceed normally | EC-STDIO-044 |

### 9.2 Assertions

**Unit Tests:**
- NDJSON parser handles all message types (request, response, notification)
- NDJSON parser rejects malformed JSON, oversized messages, missing `jsonrpc` field
- Config adapters parse sample config files for each supported agent
- Config rewrite produces valid JSON with correct shim command injection
- Profile behaviour matrix is correct for both profiles
- Request ID tracker correctly correlates requests and responses
- Shutdown sequence executes in correct order

**Integration Tests:**
- End-to-end: launch mock agent + mock MCP server, verify messages flow through shim with governance
- Config backup and restore cycle survives simulated crash (kill -9)
- Concurrent `thoughtgate wrap` on same config file: second instance exits with lock error
- Approval workflow: shim buffers message during approval, forwards on approve, denies on reject
- Development profile: denied messages are logged but forwarded
- Signal handling: SIGTERM triggers graceful shutdown with config restoration

**Property Tests:**
- Any valid JSON-RPC message survives round-trip through the shim unmodified (governance permitting)
- Config rewrite → restore cycle produces byte-identical original
- No zombie processes after any shutdown path (normal, SIGTERM, SIGKILL of wrap)

## 10. Implementation Reference

### 10.1 Recommended Crate Dependencies

**Workspace root (`Cargo.toml`):**
```toml
[workspace]
members = [
    "thoughtgate-core",
    "thoughtgate-proxy",
    "thoughtgate",
]
resolver = "2"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
thiserror = "2"
opentelemetry = { version = "0.27", features = ["trace", "metrics"] }
```

**`thoughtgate-core/Cargo.toml`:**
```toml
[package]
name = "thoughtgate-core"
version = "0.3.0"
edition = "2024"

[dependencies]
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
tracing.workspace = true
thiserror.workspace = true
opentelemetry.workspace = true
opentelemetry-otlp = "0.27"
opentelemetry_sdk = "0.27"
tracing-opentelemetry = "0.28"
cedar-policy = "4"
arc-swap = "1"              # Atomic policy swap for hot-reload
axum = "0.8"                # Governance service handler
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }
aho-corasick = "1"          # Redaction pipeline
regex = "1"                 # Redaction patterns
```

**`thoughtgate-proxy/Cargo.toml`:**
```toml
[package]
name = "thoughtgate-proxy"
version = "0.3.0"
edition = "2024"

# HTTP sidecar binary — not modified by REQ-CORE-008
[dependencies]
thoughtgate-core = { path = "../thoughtgate-core" }
tokio.workspace = true
tracing.workspace = true
axum = "0.8"
hyper = "1"
tower = "0.4"
```

**`thoughtgate/Cargo.toml`:**
```toml
[package]
name = "thoughtgate"
version = "0.3.0"
edition = "2024"

[dependencies]
thoughtgate-core = { path = "../thoughtgate-core" }
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
tracing.workspace = true
tracing-subscriber = "0.3"
thiserror.workspace = true
clap = { version = "4", features = ["derive"] }
dirs = "5"
glob = "0.3"                # Config file discovery patterns
reqwest = { version = "0.12", features = ["json"] }

[target.'cfg(unix)'.dependencies]
nix = { version = "0.29", features = ["signal", "process"] }
```

### 10.2 Key Patterns

**NDJSON Reading:**
```rust
// Use BufReader::read_line(), NOT lines() iterator.
// lines() strips the newline and hides EOF vs empty line distinction.
let mut buf = String::new();
loop {
    buf.clear();
    let bytes_read = reader.read_line(&mut buf).await?;
    if bytes_read == 0 {
        break; // EOF — pipe closed
    }
    if buf.trim().is_empty() {
        continue; // Skip empty lines (shouldn't occur, but be lenient)
    }
    let msg = parse_stdio_message(&buf)?;
    // ... process msg
}
```

**Bidirectional Proxy with Select:**
```rust
tokio::select! {
    result = agent_to_server_task => {
        // Agent stdin closed or error
        initiate_shutdown(&mut server_child).await;
    }
    result = server_to_agent_task => {
        // Server stdout closed (server exited)
        // Send error responses for any pending requests
    }
    status = server_child.wait() => {
        // Server process exited
        tracing::info!(server_id, ?status, "server process exited");
    }
}
```

**Config Restoration Guard (RAII):**
```rust
struct ConfigGuard {
    config_path: PathBuf,
    backup_path: Option<PathBuf>,
    _lock_file: File,             // Advisory lock held for lifetime; released on drop
    restored: AtomicBool,
    skip: AtomicBool,             // Set by skip_restore() for --no-restore
}

impl ConfigGuard {
    /// Two-phase construction: first acquire the lock, then set the backup path.
    fn lock(config_path: PathBuf, lock_file: File) -> Self {
        Self {
            config_path,
            backup_path: None,
            _lock_file: lock_file,
            restored: AtomicBool::new(false),
            skip: AtomicBool::new(false),
        }
    }

    /// Set the backup path after backup has been written successfully.
    fn set_backup(&mut self, backup_path: PathBuf) {
        self.backup_path = Some(backup_path);
    }

    /// Skip restoration on drop (for --no-restore flag).
    fn skip_restore(&self) {
        self.skip.store(true, Ordering::SeqCst);
    }

    fn restore(&self) {
        if self.skip.load(Ordering::SeqCst) {
            return;
        }
        if self.restored.swap(true, Ordering::SeqCst) {
            return; // Already restored
        }
        if let Some(ref backup) = self.backup_path {
            if let Err(e) = std::fs::copy(backup, &self.config_path) {
                tracing::error!(?e, backup=?backup,
                    "Failed to restore config. Manual restore: cp {} {}",
                    backup.display(), self.config_path.display());
            } else {
                let _ = std::fs::remove_file(backup);
                tracing::info!("Config restored from backup");
            }
        }
        // Note: lock file is NOT deleted on drop (TOCTOU prevention).
        // The advisory lock is released when _lock_file is dropped.
    }
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        self.restore();
    }
}
```

### 10.3 Anti-Patterns

| Anti-Pattern | Why | Correct Approach |
|--------------|-----|------------------|
| Using `lines()` iterator for NDJSON | Hides EOF detection, allocates per line | Use `read_line()` into reusable buffer |
| Modifying `initialize` messages | Breaks protocol negotiation, causes silent failures | Pass through all `initialize` traffic unmodified |
| Sending SIGKILL immediately | Server may be writing state, corrupts data | Close stdin → wait → SIGTERM → wait → SIGKILL |
| Spawning server in same process group | Ctrl+C kills server before shim can clean up | Use `.process_group(0)` on Unix |
| Relative path for shim binary in config | Agent may launch with different CWD | Always use absolute path via `current_exe()` |
| Blocking on governance in the parser task | Stalls the entire proxy pipeline | Use bounded channels: separate parse/governance/forward |
| Restoring config in signal handler only | Drop/panic paths skip signal handler | Use RAII `ConfigGuard` + signal handler + `ctrlc` handler |
| Defining `JsonRpcId` in thoughtgate | HTTP transport needs the same type — causes duplication | Use `thoughtgate-core::jsonrpc::JsonRpcId` |
| Putting `Profile` in thoughtgate | Development mode semantics apply to HTTP mode too | Use `thoughtgate-core::profile::Profile` |
| Forwarding unclassifiable messages (no id or method) | Bypasses governance entirely, violates fail-closed | Drop in production, `WOULD_DROP` in development |
| No timeout on governance HTTP calls | Hung governance service stalls the entire proxy pipeline | Set explicit connect + request timeouts on `reqwest` client |
| No readiness check before proxy loop | Startup race → first messages fail-closed denied | Poll `/healthz` with exponential backoff before accepting traffic |
| Treating JSON arrays as single messages | JSON-RPC batch requests bypass per-message governance | Check `value.is_array()` and reject with `UnsupportedBatch` |
| Wrapping an already-wrapped config | Nests shim inside shim, doubling latency and confusing shutdown | Detect ThoughtGate binary in `command` field before rewriting |
| Relying on evaluate response alone for shutdown | Idle shims never call evaluate, never discover shutdown | Use periodic heartbeat (5s) as independent shutdown discovery channel |
| Locking the config file directly with `flock` | Atomic-save editors (VS Code, vim) change inode, silently breaking the lock | Lock a separate `.thoughtgate-lock` file |
| Hardcoding governance port 19090 | Port collisions on first run violate "5 minutes to value" | Default to port 0 (OS-assigned); pass actual port via config rewrite |

### 10.4 Crate Dependency Diagram

```
    ┌──────────────────────┐      ┌──────────────────────┐
    │   thoughtgate-proxy   │      │      thoughtgate      │
    │   (binary crate)      │      │   (binary crate)      │
    │                       │      │                       │
    │   HTTP sidecar        │      │   src/main.rs         │
    │   (v0.2 transport)    │      │   src/wrap/           │
    │                       │      │   src/shim/           │
    │                       │      │   src/error.rs        │
    └───────────┬───────────┘      └───────────┬───────────┘
                │                              │
                │                              │
                └──────────────┬───────────────┘
                               │
                               ▼
                ┌──────────────────────────────┐
                │      thoughtgate-core        │
                │      (library crate)         │
                │                              │
                │  profile.rs                  │
                │  jsonrpc.rs                  │
                │  governance/                 │
                │    api.rs                    │
                │    engine.rs                 │
                │    cedar.rs                  │
                │    service.rs                │
                │  telemetry/                  │
                │    mod.rs                    │
                │    attributes.rs             │
                │    spans.rs                  │
                │    redact.rs                 │
                │    audit.rs                  │
                │    metrics.rs                │
                │    init.rs                   │
                └──────────────────────────────┘

    Legend:
    ─▶  depends on (Cargo dependency)
```

**Key constraint:** Both binaries (`thoughtgate-proxy` and `thoughtgate`) depend on `thoughtgate-core` but NOT on each other. This allows independent deployment and versioning. The HTTP sidecar doesn't need CLI wiring; the CLI doesn't need sidecar-specific HTTP handling.

## 11. Definition of Done

- [ ] `thoughtgate wrap -- claude-code` launches Claude Code with config rewritten, governance active
- [ ] `thoughtgate wrap -- cursor .` works for Cursor
- [ ] Config adapters correctly parse sample configs for all 6 supported agents
- [ ] Config backup is created before rewrite; original is restored on normal exit
- [ ] Config is restored on SIGTERM, SIGINT, and agent crash
- [ ] Concurrent `thoughtgate wrap` on same config exits with lock error
- [ ] Shim proxies bidirectional NDJSON messages with < 2ms forwarding latency (P99)
- [ ] Cedar policy evaluation works identically to HTTP mode
- [ ] Slack approval workflow works identically to HTTP mode
- [ ] Messages exceeding 10MB are rejected with `FramingError`
- [ ] Malformed JSON is logged and skipped (no crash)
- [ ] Server crash is handled: error responses sent, shim exits cleanly
- [ ] No zombie processes after any exit path
- [ ] `--profile development` logs `WOULD_BLOCK` for denied messages and forwards them
- [ ] `--profile development` auto-approves approval workflows
- [ ] All 44 edge cases (EC-STDIO-001 through EC-STDIO-044) have corresponding tests
- [ ] All metrics from NFR-002 are instrumented
- [ ] Integration test: mock agent + mock server + governance, full round-trip
- [ ] Double-wrap detection prevents nesting shim inside shim
- [ ] `--dry-run` prints config diff without modifying files
- [ ] Shim waits for governance `/healthz` before entering proxy loop
- [ ] JSON-RPC batch arrays are rejected with `FramingError::UnsupportedBatch`
- [ ] Shim heartbeat discovers `shutdown: true` and initiates graceful shutdown within one heartbeat interval
- [ ] Shim detects governance service crash (connection refused) and shuts down cleanly (not as error)
- [ ] File lock on `.thoughtgate-lock` survives atomic-save editors (VS Code)
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes (unit + integration)

## 12. References

- [MCP Specification: Transports (stdio)](https://spec.modelcontextprotocol.io/specification/basic/transports/#stdio)
- [MCP Specification: Lifecycle](https://modelcontextprotocol.io/specification/2025-03-26/basic/lifecycle)
- [MCP Specification: Cancellation](https://modelcontextprotocol.info/specification/draft/basic/utilities/cancellation/)
- [mcp-guardian (Rust reference implementation)](https://github.com/eqtylab/mcp-guardian)
- [MCP Rust SDK: TokioChildProcess transport](https://github.com/modelcontextprotocol/rust-sdk)
- [REQ-CORE-003: MCP Transport & Routing](./REQ-CORE-003_MCP_Transport_and_Routing.md)
- [REQ-CFG-001: Configuration Schema](./REQ-CFG-001_Configuration.md)
- [ARCHITECTURE.md](./ARCHITECTURE.md)
