# REQ-OBS-002: Telemetry, Tracing & Metrics

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-OBS-002` |
| **Title** | Telemetry, Tracing & Metrics |
| **Type** | Observability |
| **Status** | Draft |
| **Priority** | **High** |
| **Tags** | `#observability` `#opentelemetry` `#tracing` `#metrics` `#prometheus` `#otlp` |

## 1. Context & Decision Rationale

This requirement defines the runtime observability instrumentation for ThoughtGate—how the proxy emits structured telemetry (traces, metrics, logs) that enables debugging, performance analysis, and operational monitoring in production environments.

REQ-OBS-001 covers CI-based benchmark collection. REQ-OBS-003 covers audit logging and compliance. This spec covers everything between: the engineering observability story of "instrument and observe."

### 1.1 Why OpenTelemetry?

OpenTelemetry is the de facto standard for observability instrumentation, adopted by every major APM vendor (Datadog, New Relic, Dynatrace, Honeycomb, Grafana). Competitive analysis (see `MCP_Proxy_Observability_Analysis.md`) shows that only Acuvity Minibridge ships OTel support among MCP security tools—this is a significant gap ThoughtGate fills.

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Instrumentation standard | OpenTelemetry | Vendor-neutral, adopted by all major APM vendors |
| Semantic conventions | OTel GenAI semconv v1.39.0+ | Includes MCP-specific attributes in Development status |
| Export protocol | OTLP (gRPC + HTTP/protobuf) | Native OTel, widest backend support |
| Metrics exposition | Prometheus `/metrics` endpoint | Kubernetes-native, zero-dependency scraping |
| Trace propagation | W3C `traceparent`/`tracestate` | Industry standard, interoperable with all APM vendors |
| Span linking strategy | Span links for async boundaries | Prevents multi-hour parent-child spans distorting latency histograms |

### 1.2 OTel GenAI Semantic Conventions

The OpenTelemetry project released official MCP semantic conventions in v1.39.0, providing standardized attribute names for tool calls, sampling requests, and session management. These conventions remain in **Development** status, meaning breaking changes are possible before stability. ThoughtGate adopts these conventions now—they represent the emerging standard already used by Datadog, New Relic, Dynatrace, and Honeycomb.

**Stability strategy:** ThoughtGate implements a configuration flag `OTEL_SEMCONV_STABILITY_OPT_IN=gen_ai_latest_experimental` to ensure access to the richest telemetry set. When conventions stabilize, ThoughtGate adds the stable attributes alongside experimental ones during a transition period, then deprecates experimental-only attributes.

### 1.3 The Trace-Log-Metric Trinity

The schema is designed so that all three signals are correlated:

- **Metrics**: "Show me the error rate (`mcp.result.is_error=true`) grouped by `mcp.tool.name`."
- **Tracing**: "Find all traces where `cedar.decision=deny` and `gen_ai.system=openai`."
- **Logs**: "Retrieve the full redacted arguments for the `database_query` tool in Trace ID `abc-123`."

Every span includes `trace_id` and `span_id`. Every metric exemplar includes `trace_id`. Every log record includes both. This ensures seamless drill-down from dashboard → trace → log in any observability backend.

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-OBS-001 | **Complements** | CI benchmarks (offline) vs runtime telemetry (online) |
| REQ-OBS-003 | **Feeds** | Trace context (trace_id, span_id) is embedded in audit events |
| REQ-CORE-001 | **Instruments** | Streaming spans, TTFT/TPOT metrics |
| REQ-CORE-003 | **Instruments** | MCP transport spans, JSON-RPC correlation |
| REQ-CORE-007 | **Instruments** | SEP-1686 async task spans |
| REQ-CORE-008 | **Instruments** | Stdio transport trace propagation via `params._meta` |
| REQ-POL-001 | **Instruments** | Cedar evaluation spans, policy decision attributes |
| REQ-GOV-001 | **Instruments** | Task lifecycle spans, state transition events |
| REQ-GOV-002 | **Instruments** | Approval workflow spans with span links |
| REQ-GOV-003 | **Instruments** | Slack/webhook integration spans |
| REQ-CFG-001 | **Configured by** | Telemetry export settings in YAML config |

## 3. Intent

The system must:

1. Emit OpenTelemetry traces for every MCP request passing through the proxy
2. Expose Prometheus-compatible metrics for operational dashboards
3. Propagate W3C trace context across synchronous and asynchronous boundaries
4. Use span links (not parent-child) for async HITL approval workflows
5. Support tiered sampling to manage volume without losing debugging capability
6. Export telemetry via OTLP to any compatible backend
7. Ship with Grafana dashboard templates for immediate operational visibility

## 4. Scope

### 4.1 In Scope

- OTel span schema: MCP spans, Cedar evaluation spans, Gateway decision spans
- Metrics: Prometheus counters, histograms, gauges
- Trace context propagation: HTTP headers and `params._meta` injection
- Span links for async boundaries (HITL approval, A2A dispatch)
- OTLP export configuration (gRPC and HTTP/protobuf)
- Storage backend recommendations (ClickHouse via SigNoz/HyperDX)
- Grafana dashboard templates
- Streaming-specific metrics (TTFT, TPOT via bookends approach)

### 4.2 Out of Scope

- CI benchmark collection (REQ-OBS-001)
- Audit logging, redaction, and compliance events (REQ-OBS-003)
- Alerting/paging rule definitions (deployment-specific)
- Custom backend implementations (ClickHouse schema DDL)
- A2A span schema (deferred to v1.0+ per roadmap; placeholder attributes defined)

### 4.3 Version Alignment

| Capability | Version | Rationale |
|------------|---------|-----------|
| Foundation spans (MCP inbound, Cedar eval, gateway decision) | **v0.2** | Core debugging from day one |
| Prometheus `/metrics` endpoint | **v0.2** | Kubernetes-native monitoring |
| OTLP gRPC/HTTP export | **v0.2** | Backend flexibility from launch |
| Basic sampling (100% errors, configurable success rate) | **v0.2** | Volume management |
| Grafana dashboard templates | **v0.2** | Immediate operational visibility |
| Stdio transport trace propagation via `params._meta` | **v0.3** | Requires CLI wrapper (REQ-CORE-008) |
| Durable context storage for approval workflows | **v0.3** | Requires persistent state (SQLite/Sled) |
| Streaming metrics (TTFT, TPOT) | **v0.4+** | Requires buffered inspection (REQ-CORE-002) |
| A2A span attributes | **v1.0+** | A2A protocol bridge deferred per roadmap |

## 5. Span Schema

### 5.1 MCP Spans

MCP spans represent the end-to-end processing of a JSON-RPC request through the proxy. Every MCP request creates exactly one root span (from ThoughtGate's perspective) with child spans for internal processing stages.

**Span name convention:** `{mcp.method}` (e.g., `tools/call`, `resources/read`, `sampling/createMessage`)

**Span kind:** `SERVER` (ThoughtGate receives a request from the MCP client)

#### 5.1.1 MCP Span Attributes (OTel GenAI Semconv Compliant)

| Attribute | Type | Requirement | Description | Example |
|-----------|------|-------------|-------------|---------|
| `mcp.method.name` | string | **Required** | JSON-RPC method name | `tools/call` |
| `mcp.session.id` | string | **Recommended** | MCP session identifier for correlation | `sess-abc123` |
| `mcp.protocol.version` | string | **Recommended** | MCP specification version | `2025-06-18` |
| `mcp.message.type` | string | **Required** | JSON-RPC message discriminator | `request`, `response`, `notification` |
| `mcp.message.id` | string | **Conditional** | JSON-RPC `id` field for request correlation | `42` |
| `gen_ai.operation.name` | string | **Required** | Operation type | `execute_tool`, `chat` |
| `gen_ai.tool.name` | string | **Conditional** | Tool being invoked (for `tools/call`) | `web_search`, `database_query` |
| `gen_ai.tool.call.id` | string | **Conditional** | Unique tool call identifier | `call-456` |
| `mcp.tool.category` | string | **Recommended** | Logical tool grouping from config | `system`, `financial`, `readonly` |
| `mcp.resource.uri` | string | **Conditional** | Resource URI (for `resources/read`) | `file:///config.yaml` |
| `jsonrpc.request.id` | string | **Conditional** | JSON-RPC request correlation ID | `req-789` |
| `mcp.result.is_error` | boolean | **Required** | Whether MCP returned an error (even if HTTP 200) | `true` |
| `mcp.error.code` | int | **Conditional** | JSON-RPC error code if present | `-32601` |
| `error.type` | string | **Conditional** | Error classification | `rate_limit`, `invalid_tool`, `policy_denied` |

#### 5.1.2 Sampling/LLM Span Attributes

For `sampling/createMessage` requests that pass through the proxy, additional GenAI attributes capture LLM interaction metadata:

| Attribute | Type | Requirement | Description | Example |
|-----------|------|-------------|-------------|---------|
| `gen_ai.system` | string | **Required** | LLM provider | `anthropic`, `openai`, `vertex_ai` |
| `gen_ai.request.model` | string | **Required** | Requested model | `claude-3-opus` |
| `gen_ai.response.model` | string | **Recommended** | Actual model used (may differ from requested) | `claude-3-opus-20240229` |
| `gen_ai.usage.input_tokens` | int | **Recommended** | Input token count | `1500` |
| `gen_ai.usage.output_tokens` | int | **Recommended** | Output token count | `350` |
| `gen_ai.temperature` | float | **Optional** | Temperature setting | `0.7` |
| `mcp.sampling.strategy` | string | **Optional** | Sampling strategy if present | `ModelPreferences`, `cost-optimized` |

**Content attribute policy:** All sensitive content attributes (`gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.tool.call.arguments`) are **disabled by default**. They must be explicitly enabled via configuration and are always passed through the redaction pipeline (REQ-OBS-003) before emission. Complex JSON tool arguments should be stored as **span events** or **log bodies** linked to the trace, not as span attributes, to avoid cardinality explosion (most OTel backends truncate attributes at 4KB).

### 5.2 Cedar Evaluation Spans

Cedar evaluation spans are **child spans** of the MCP request span, recording the synchronous policy decision.

**Span name:** `cedar.evaluate`

**Span kind:** `INTERNAL`

| Attribute | Type | Requirement | Description | Example |
|-----------|------|-------------|-------------|---------|
| `cedar.decision` | string | **Required** | Policy outcome | `allow`, `deny` |
| `cedar.policy_id` | string | **Conditional** | Determining policy identifier | `policy::tool-access-v2` |
| `cedar.principal.type` | string | **Required** | Principal entity type | `Agent`, `User` |
| `cedar.principal.id` | string | **Required** | Principal identifier | `agent::researcher` |
| `cedar.action` | string | **Required** | Authorized action | `Action::"invoke_tool"` |
| `cedar.resource.type` | string | **Required** | Resource entity type | `Tool`, `Document` |
| `cedar.resource.id` | string | **Required** | Resource identifier | `tool::web_search` |
| `cedar.evaluation.duration_ms` | double | **Required** | Policy evaluation latency in milliseconds | `0.45` |
| `cedar.policy_set.version` | string | **Recommended** | Policy bundle version/hash | `sha256:a1b2c3...` |
| `cedar.diagnostics.reasons` | string[] | **Optional** | Policy IDs that contributed to decision | `["policy-fin-001"]` |

**Span events on denial:**

When Cedar denies a request, the span records a span event `cedar.denial` with the attribute `cedar.violation.reason` containing a human-readable denial reason (e.g., "Insufficient clearance for PII in prompt"). This aids debugging agent failures without requiring log correlation.

### 5.3 Gateway Decision Spans

Gateway decision spans record ThoughtGate's 4-gate decision flow as a single child span with events for each gate.

**Span name:** `thoughtgate.decision`

**Span kind:** `INTERNAL`

| Attribute | Type | Requirement | Description | Example |
|-----------|------|-------------|-------------|---------|
| `thoughtgate.request_id` | string | **Required** | Internal correlation ID | `tg-req-abc123` |
| `thoughtgate.gate.visibility` | string | **Required** | Gate 1 outcome | `pass`, `block` |
| `thoughtgate.gate.governance` | string | **Required** | Gate 2 outcome (YAML rule) | `forward`, `deny`, `approve`, `policy` |
| `thoughtgate.gate.cedar` | string | **Conditional** | Gate 3 outcome (only if Gate 2 = `policy`) | `allow`, `deny` |
| `thoughtgate.gate.approval` | string | **Conditional** | Gate 4 outcome (only if approval required) | `started`, `pending`, `approved`, `denied`, `timeout` |
| `thoughtgate.governance.rule_id` | string | **Recommended** | Matched YAML governance rule | `rule-financial-tools` |
| `thoughtgate.upstream.target` | string | **Required** | Upstream MCP server identifier | `mcp-server-filesystem` |
| `thoughtgate.upstream.latency_ms` | double | **Recommended** | Upstream call round-trip latency | `245.3` |
| `thoughtgate.policy.evaluated` | boolean | **Required** | Whether any policy was checked | `true` |

### 5.4 Approval Workflow Spans

Approval workflows introduce asynchronous boundaries that may span minutes to hours. These **must not** use parent-child relationships—doing so would create spans lasting hours, distorting latency histograms and potentially exhausting span storage.

**Architecture: Span links, not parent-child.**

```
┌─────────────┐       ┌──────────────┐       ┌──────────────┐
│ MCP Request │       │ Approval     │       │ Execution    │
│ Span        │──────▶│ Dispatch     │       │ Span         │
│ (completes  │ link  │ Span         │ link  │ (starts on   │
│  quickly)   │       │ (completes   │──────▶│  callback)   │
└─────────────┘       │  quickly)    │       └──────────────┘
                      └──────────────┘
```

#### 5.4.1 Approval Dispatch Span

Created when ThoughtGate sends an approval request to Slack/webhook. Completes immediately after dispatch.

**Span name:** `thoughtgate.approval.dispatch`

**Span kind:** `PRODUCER`

| Attribute | Type | Requirement | Description | Example |
|-----------|------|-------------|-------------|---------|
| `thoughtgate.task.id` | string | **Required** | SEP-1686 task identifier | `tg-task-xyz789` |
| `thoughtgate.approval.channel` | string | **Required** | Approval integration type | `slack`, `webhook`, `teams` |
| `thoughtgate.approval.target` | string | **Recommended** | Target channel/endpoint (redacted) | `#security-approvals` |
| `thoughtgate.approval.timeout_s` | int | **Recommended** | Configured approval timeout | `3600` |

**Span link:** Links back to the parent MCP request span's `trace_id` and `span_id`.

#### 5.4.2 Approval Callback Span

Created when the human responds (approve/deny) via Slack or webhook. This is a **new root span** or independent span within the same trace, linked to the dispatch span.

**Span name:** `thoughtgate.approval.callback`

**Span kind:** `CONSUMER`

| Attribute | Type | Requirement | Description | Example |
|-----------|------|-------------|-------------|---------|
| `thoughtgate.task.id` | string | **Required** | SEP-1686 task identifier | `tg-task-xyz789` |
| `thoughtgate.approval.decision` | string | **Required** | Human decision | `approved`, `denied` |
| `thoughtgate.approval.user` | string | **Recommended** | Approving user (pseudonymized per REQ-OBS-003 §B-OBS3-008) | `user:hmac:a1b2...` |
| `thoughtgate.approval.latency_s` | double | **Required** | Wall-clock time from dispatch to callback | `127.4` |

**Span link:** Links to the dispatch span's `trace_id` and `span_id`, preserving causal traceability without creating a multi-hour parent-child relationship.

### 5.5 Streaming-Specific Attributes (v0.4+)

Streaming spans use the **bookends approach**: log the start and end of token streams, measure stream performance via metrics (not per-chunk spans).

| Attribute | Type | Requirement | Description | Example |
|-----------|------|-------------|-------------|---------|
| `gen_ai.streaming.enabled` | boolean | **Required** | Whether response was streamed | `true` |
| `gen_ai.streaming.ttft_ms` | double | **Recommended** | Time to First Token in milliseconds | `180.5` |
| `gen_ai.streaming.tpot_avg_ms` | double | **Recommended** | Average Time Per Output Token | `22.1` |
| `gen_ai.streaming.total_duration_ms` | double | **Recommended** | Total stream duration | `3200` |
| `gen_ai.streaming.chunk_count` | int | **Recommended** | Total number of SSE chunks received | `145` |

**Bookends logging strategy:**
1. **Start event** (`stream.start`): Emitted when the first byte of the upstream response is received. Contains request metadata (tool name, method, principal).
2. **End event** (`stream.complete`): Emitted when the stream closes. Contains aggregated statistics (token count, duration, TTFT, average TPOT, finish reason). The full concatenated response is optionally logged (after redaction) as a log body linked to the trace.

**Why not per-chunk spans?** A single response may consist of 500+ chunks. At 100 QPS, that's 50,000 span events per second—a denial-of-service attack on your own observability infrastructure. The bookends approach captures the performance characteristics operators actually need.

### 5.6 A2A Spans (v1.0+ — Placeholder)

A2A protocol bridge is deferred to v1.0+ per the roadmap. These attribute definitions are reserved for future use and included here for schema completeness.

| Attribute | Type | Requirement | Description | Example |
|-----------|------|-------------|-------------|---------|
| `a2a.task.id` | string | Required | A2A task identifier | `task-xyz-789` |
| `a2a.task.state` | string | Required | Task lifecycle state | `submitted`, `working`, `completed` |
| `a2a.source.agent_id` | string | Required | Initiating agent identifier | `agent-orchestrator` |
| `a2a.target.agent_id` | string | Required | Destination agent identifier | `agent-researcher` |
| `a2a.conversation.id` | string | Recommended | Multi-turn conversation correlation | `conv-abc` |
| `a2a.message.role` | string | Recommended | Message sender role | `user`, `assistant` |
| `a2a.requires_approval` | boolean | Recommended | Human approval required flag | `true` |

## 6. Metrics Definitions

All metrics use the `thoughtgate.` prefix namespace. Metrics are exposed via a Prometheus-compatible `/metrics` endpoint and simultaneously exported via OTLP.

### 6.1 Counters

| ID | Metric Name | Labels | Description |
|----|-------------|--------|-------------|
| MC-001 | `thoughtgate.requests.total` | `method`, `tool_name`, `status` | Total MCP requests processed |
| MC-002 | `thoughtgate.decisions.total` | `gate`, `outcome` | Decision counts per gate (`visibility`, `governance`, `cedar`, `approval`) |
| MC-003 | `thoughtgate.errors.total` | `error_type`, `method` | Error counts by type (`policy_denied`, `upstream_error`, `timeout`, `invalid_request`) |
| MC-004 | `thoughtgate.cedar.evaluations.total` | `decision`, `policy_id` | Cedar policy evaluation counts |
| MC-005 | `thoughtgate.approval.requests.total` | `channel`, `outcome` | Approval requests by channel and outcome |
| MC-006 | `thoughtgate.upstream.requests.total` | `target`, `status_code` | Upstream MCP server call counts |
| MC-007 | `thoughtgate.tasks.created.total` | `task_type` | SEP-1686 tasks created (approval, upstream) |
| MC-008 | `thoughtgate.tasks.completed.total` | `task_type`, `outcome` | SEP-1686 tasks completed |

### 6.2 Histograms

| ID | Metric Name | Labels | Buckets | Description |
|----|-------------|--------|---------|-------------|
| MH-001 | `thoughtgate.request.duration_ms` | `method`, `tool_name` | `1, 2, 5, 10, 25, 50, 100, 250, 500, 1000, 5000` | End-to-end request latency (excludes async approval wait) |
| MH-002 | `thoughtgate.cedar.evaluation.duration_ms` | `decision` | `0.01, 0.05, 0.1, 0.25, 0.5, 1, 5, 10` | Cedar policy evaluation latency |
| MH-003 | `thoughtgate.upstream.duration_ms` | `target` | `1, 5, 10, 25, 50, 100, 250, 500, 1000, 5000, 10000` | Upstream MCP server call latency |
| MH-004 | `thoughtgate.approval.wait_duration_s` | `channel`, `outcome` | `1, 5, 10, 30, 60, 300, 900, 1800, 3600` | Approval wait time in seconds (dispatch to callback) |
| MH-005 | `thoughtgate.request.payload_size_bytes` | `direction`, `method` | `128, 512, 1024, 4096, 16384, 65536, 262144, 1048576` | Request/response payload sizes |

### 6.3 Streaming Metrics (v0.4+)

| ID | Metric Name | Labels | Buckets | Description |
|----|-------------|--------|---------|-------------|
| MS-001 | `thoughtgate.streaming.ttft_ms` | `gen_ai.system`, `gen_ai.request.model` | `10, 25, 50, 100, 250, 500, 1000, 2500, 5000` | Time to First Token histogram |
| MS-002 | `thoughtgate.streaming.tpot_avg_ms` | `gen_ai.system`, `gen_ai.request.model` | `5, 10, 15, 20, 30, 50, 100, 250` | Average Time Per Output Token |
| MS-003 | `thoughtgate.streaming.total_duration_ms` | `gen_ai.system`, `gen_ai.request.model` | `100, 500, 1000, 2500, 5000, 10000, 30000, 60000` | Total stream duration |
| MS-004 | `thoughtgate.streaming.chunk_count` | `gen_ai.system` | `10, 25, 50, 100, 250, 500, 1000` | Chunks per streaming response |

**TTFT calculation:** `timestamp_first_chunk - timestamp_request_sent`

**TPOT calculation:** Average inter-arrival time of chunks 2..N: `(timestamp_last_chunk - timestamp_first_chunk) / (chunk_count - 1)`

**Implementation note:** Metrics are calculated by observing the stream without buffering. The sidecar passes chunks to the client immediately (zero-copy passthrough) while a background Tokio task timestamps chunk arrivals and emits aggregated metrics on stream close.

### 6.4 Gauges

| ID | Metric Name | Labels | Description |
|----|-------------|--------|-------------|
| MG-001 | `thoughtgate.connections.active` | `transport` | Active MCP connections (http, stdio) |
| MG-002 | `thoughtgate.tasks.pending` | `task_type` | Currently pending approval tasks |
| MG-003 | `thoughtgate.cedar.policies.loaded` | — | Number of loaded Cedar policies |
| MG-004 | `thoughtgate.uptime_seconds` | — | Process uptime |
| MG-005 | `thoughtgate.config.reload.timestamp` | — | Unix timestamp of last config reload |
| MG-006 | `thoughtgate.audit.buffer.utilization` | — | Audit writer buffer fullness ratio (0.0–1.0). Alert when approaching 1.0 (see REQ-OBS-003 §B-OBS3-005) |
| MC-009 | `thoughtgate.telemetry.dropped.total` | `signal` | Telemetry items dropped due to full export queue (labels: `spans`, `metrics`, `logs`). **Type: Counter** (monotonically increasing, not a gauge). |

### 6.5 Metric Label Cardinality Management

**Critical concern:** Labels like `tool_name` can have high cardinality (hundreds of distinct tools). Labels like `policy_id` are bounded but variable. Labels derived from request payloads (e.g., arguments) are **never** used as metric labels.

| Label | Expected Cardinality | Mitigation |
|-------|---------------------|------------|
| `method` | ~10 | Fixed set from MCP spec |
| `tool_name` | 10–1,000 | Runtime cap with overflow bucket (see below) |
| `target` | 1–20 | Bounded by configured upstreams |
| `policy_id` | 1–50 | Bounded by Cedar policy set |
| `gen_ai.system` | 3–5 | Fixed small set |
| `gen_ai.request.model` | 5–20 | Monitor; consider aggregation if >50 |

**Runtime cardinality enforcement:** ThoughtGate tracks the distinct values per high-cardinality label at runtime. When a label dimension exceeds the configured cap (default: 200 for `tool_name`), new unseen values are mapped to `__other__`. This prevents unbounded time series growth in multi-tenant or dynamic-tool environments.

```rust
/// Cardinality-limited label resolver
struct CardinalityLimiter {
    known: HashSet<String>,
    max_values: usize,  // Default: 200
}

impl CardinalityLimiter {
    fn resolve(&mut self, value: &str) -> &str {
        if self.known.contains(value) || self.known.len() < self.max_values {
            self.known.insert(value.to_string());
            value
        } else {
            "__other__"
        }
    }
}
```

**Configuration:**
```yaml
telemetry:
  metrics:
    cardinality_limits:
      tool_name: 200      # Max distinct tool_name values before overflow to __other__
      request_model: 50   # Max distinct model names
```

**Monitoring rule:** If `__other__` accounts for >5% of traffic, alert operators to review whether the cap should be raised or whether the label should be moved to span attributes only.

**Hard rule:** If any label dimension would exceed 1,000 unique values even with the cap, that label must be moved to span attributes (traces) only and excluded from metric labels entirely.

## 7. Trace Context Propagation

### 7.1 W3C Trace Context Standard

ThoughtGate propagates trace context using the W3C Trace Context specification via two headers:

- **`traceparent`**: `{version}-{trace-id}-{parent-id}-{trace-flags}` (e.g., `00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01`)
- **`tracestate`**: Vendor-specific key-value pairs (e.g., `thoughtgate=session-xyz`)

### 7.2 HTTP+SSE Transport Propagation

For HTTP-based MCP transports, trace context propagation uses standard HTTP headers:

```
Inbound (MCP Client → ThoughtGate):
  HTTP Header: traceparent: 00-{trace_id}-{parent_span_id}-01
  HTTP Header: tracestate: vendor=value

ThoughtGate processing:
  1. Extract traceparent from HTTP headers
  2. Start child span with extracted context as parent
  3. Process request through 4-gate flow
  4. Inject updated traceparent into upstream HTTP request

Outbound (ThoughtGate → Upstream MCP Server):
  HTTP Header: traceparent: 00-{trace_id}-{new_span_id}-01
  HTTP Header: tracestate: thoughtgate={session_id}

Response (Upstream → ThoughtGate → Client):
  HTTP Header: traceparent: 00-{trace_id}-{response_span_id}-01
```

**Behavioral rules:**
- If inbound request has no `traceparent`, ThoughtGate generates a new trace ID and becomes the trace root.
- If inbound request has a `traceparent`, ThoughtGate creates a child span within that trace.
- `tracestate` is always forwarded and may be augmented with `thoughtgate={session_id}`.

### 7.3 Stdio Transport Propagation (v0.3)

Stdio transport (standard input/output) is used by Claude Desktop, Cursor, VS Code, and Windsurf. It lacks HTTP headers, creating a "black hole" in the trace. ThoughtGate uses the `params._meta` field in JSON-RPC payloads for out-of-band propagation.

#### 7.3.1 Inbound Context Extraction

```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "_meta": {
      "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
      "tracestate": "vendor=thoughtgate"
    },
    "name": "calculator",
    "arguments": { "a": 1, "b": 2 }
  },
  "id": 1
}
```

**Extraction behavior:**
1. Parse JSON-RPC message.
2. Check for `params._meta.traceparent`.
3. If present: extract trace context, create child span.
4. If absent: generate new trace ID, create root span.
5. **Before forwarding to upstream: always strip `_meta.traceparent` and `_meta.tracestate` from the message by default.** Many MCP servers use strict JSON schema validation (e.g., Pydantic with `extra='forbid'`) and will reject requests containing unexpected fields. ThoughtGate captures the trace context on the sidecar side; it must not pollute the upstream payload.

**Configuration to override stripping:**
```yaml
telemetry:
  stdio:
    # Default: false. If true, preserve _meta trace context fields when
    # forwarding to upstream MCP servers. Only enable if you control the
    # upstream server and it accepts _meta fields.
    propagate_upstream: false
```

When `propagate_upstream: true`, `_meta.traceparent` and `_meta.tracestate` are preserved in the forwarded message, enabling end-to-end trace propagation through trace-aware upstream tools.

#### 7.3.2 Response Context Injection

When returning a response to the MCP client over stdio, ThoughtGate injects trace context into the response `_meta` field:

```json
{
  "jsonrpc": "2.0",
  "result": {
    "_meta": {
      "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-{new_span_id}-01"
    },
    "content": [...]
  },
  "id": 1
}
```

This enables clients that understand trace context to continue the trace across multiple tool calls within a single agent session.

### 7.4 Async HITL Boundary Propagation

Human-in-the-loop approval workflows create temporal discontinuities of minutes to hours. Trace context must survive these gaps.

#### 7.4.1 Context Storage at Dispatch

When dispatching an approval request, the full trace context is stored in two locations:

1. **Approval message payload**: Embedded in Slack's `private_metadata` field (or equivalent webhook payload field) so the callback handler can extract it.
2. **Task metadata store**: Persisted alongside the SEP-1686 task record for crash recovery.

```json
{
  "private_metadata": {
    "trace_context": {
      "traceparent": "00-{trace_id}-{dispatch_span_id}-01",
      "tracestate": "thoughtgate=session-xyz",
      "baggage": "mcp.request.id=req-123,thoughtgate.task.id=tg-task-xyz789"
    },
    "task_id": "tg-task-xyz789"
  }
}
```

#### 7.4.2 Context Restoration at Callback

When the approval callback arrives:

1. Extract `trace_context` from the callback payload.
2. Create a **new span** with the stored `trace_id` (maintaining trace correlation).
3. Add a **span link** pointing to the original dispatch span's `span_id`.
4. Do **not** set the dispatch span as the parent (this would create a multi-hour span).

```rust
// Pseudocode: approval callback handler
let stored_ctx = extract_trace_context(&callback.private_metadata);
let link = SpanLink::new(stored_ctx.trace_id, stored_ctx.span_id);

let callback_span = tracer.span_builder("thoughtgate.approval.callback")
    .with_kind(SpanKind::Consumer)
    .with_links(vec![link])
    .start(&tracer);
```

#### 7.4.3 Business Context via Baggage

The W3C `baggage` header carries request-scoped metadata that propagates to all downstream services:

| Baggage Key | Description | Example |
|-------------|-------------|---------|
| `mcp.request.id` | Original MCP request correlation | `req-123` |
| `thoughtgate.task.id` | ThoughtGate task identifier | `tg-task-xyz789` |
| `thoughtgate.tenant` | Multi-tenant deployment tenant ID | `acme-corp` |
| `thoughtgate.priority` | Request priority for sampling decisions | `high` |

## 8. OTLP Export Configuration

### 8.1 Export Pipeline Architecture

```
ThoughtGate Sidecar
    │
    ├── Spans ──────────┐
    ├── Metrics ─────────┤──▶ OTel Collector ──▶ ClickHouse (via SigNoz/HyperDX)
    └── Logs ────────────┘                            │
                                                      ▼
                                                SigNoz/HyperDX UI
                                                     (query)
```

### 8.2 Sidecar Export Configuration

```yaml
# thoughtgate.yaml — telemetry section
telemetry:
  # Enable/disable telemetry globally
  enabled: true

  # OTLP export configuration
  otlp:
    # gRPC endpoint (preferred — binary protocol, lower overhead)
    endpoint: "http://otel-collector:4317"
    # HTTP/protobuf fallback
    # endpoint: "http://otel-collector:4318"
    protocol: grpc  # grpc | http/protobuf

    # TLS configuration (optional)
    tls:
      enabled: false
      ca_cert: ""
      client_cert: ""
      client_key: ""

    # Export headers (e.g., for SaaS backends)
    headers:
      # Authorization: "Bearer ${OTEL_EXPORTER_TOKEN}"

  # Prometheus endpoint
  prometheus:
    enabled: true
    port: 9090
    path: "/metrics"

  # Sampling configuration
  sampling:
    # Default strategy: sample all errors, percentage of successes
    strategy: "tail"  # tail | head | always_on | always_off
    error_sample_rate: 1.0     # 100% of errors — always capture failures
    success_sample_rate: 0.05  # 5% of successes — adjust based on volume
    # Per-method overrides
    overrides:
      "tools/call": 0.10       # 10% of tool calls (higher value operations)
      "resources/list": 0.01   # 1% of list operations (high volume, low value)

  # Batching configuration
  batch:
    max_queue_size: 2048
    max_export_batch_size: 512
    scheduled_delay_ms: 5000
    export_timeout_ms: 30000

  # Resource attributes (attached to all telemetry)
  resource:
    service.name: "thoughtgate"
    service.version: "${THOUGHTGATE_VERSION}"
    deployment.environment: "${DEPLOY_ENV:development}"
    # Kubernetes-injected attributes (via downward API)
    k8s.namespace.name: "${K8S_NAMESPACE:}"
    k8s.pod.name: "${K8S_POD_NAME:}"
    k8s.node.name: "${K8S_NODE_NAME:}"

  # Content capture policy
  content_capture:
    # Master switch — must be explicitly enabled
    enabled: false
    # Capture tool arguments in span events (after redaction)
    capture_tool_arguments: false
    # Capture response content in span events (after redaction)
    capture_response_content: false
    # Maximum content size before truncation (see 8.4)
    max_inline_content_bytes: 102400  # 100KB
```

### 8.3 OTel Collector Pipeline (Recommended)

ThoughtGate recommends deploying an OTel Collector as a sidecar or DaemonSet alongside the proxy. The collector handles batching, attribute processing, and export to the storage backend.

```yaml
# otel-collector-config.yaml (recommended deployment)
receivers:
  otlp:
    protocols:
      grpc:
        endpoint: "0.0.0.0:4317"
      http:
        endpoint: "0.0.0.0:4318"

processors:
  batch:
    timeout: 5s
    send_batch_size: 512
    send_batch_max_size: 1024

  # Add K8s metadata
  k8sattributes:
    extract:
      metadata:
        - k8s.namespace.name
        - k8s.deployment.name
        - k8s.pod.name

  # Secondary redaction check (defense in depth)
  attributes/redact:
    actions:
      - key: gen_ai.tool.call.arguments
        action: hash  # SHA256 hash if present (should already be redacted)

  # Memory limiter to prevent OOM
  memory_limiter:
    check_interval: 1s
    limit_mib: 256
    spike_limit_mib: 64

exporters:
  # Primary: ClickHouse via SigNoz
  otlp/signoz:
    endpoint: "signoz-otel-collector:4317"
    tls:
      insecure: true

  # Fallback: file export for debugging
  file:
    path: /var/log/otel/traces.json
    rotation:
      max_megabytes: 100
      max_backups: 3

service:
  pipelines:
    traces:
      receivers: [otlp]
      processors: [memory_limiter, k8sattributes, attributes/redact, batch]
      exporters: [otlp/signoz]
    metrics:
      receivers: [otlp]
      processors: [memory_limiter, batch]
      exporters: [otlp/signoz]
    logs:
      receivers: [otlp]
      processors: [memory_limiter, batch]
      exporters: [otlp/signoz]
```

### 8.4 Large Payload Handling

LLM prompts and responses can be large (128K tokens = ~500KB+ of text). Storing these inline in span attributes or even span events degrades query performance.

#### v0.4: Inline Truncation (Default)

**Threshold:** Payloads exceeding `max_inline_content_bytes` (default: 100KB) are truncated inline with a marker.

**Flow:**
1. ThoughtGate measures payload size after redaction.
2. If `size > max_inline_content_bytes`:
   - Truncate the content to `max_inline_content_bytes`.
   - Append `...[TRUNCATED original_size_bytes={N}]` marker.
   - Record `thoughtgate.payload.truncated = true` and `thoughtgate.payload.original_size_bytes = {N}` as span attributes.
3. If `size <= max_inline_content_bytes`:
   - Store inline as a span event body (when content capture is enabled).

This approach avoids the I/O complexity and failure modes of external blob storage on the critical path. Most debugging workflows do not require the full 500KB prompt.

**Configuration:** Uses `telemetry.content_capture.enabled` and `telemetry.content_capture.max_inline_content_bytes` from Section 8.2.

#### v1.0+: Blob Offloading (Future)

For enterprise customers requiring full payload retention for forensic analysis, a future release will add optional blob offloading to S3/MinIO/local filesystem. The blob store writes payload to `traces/{trace_id}/{span_id}/payload.json.zstd` (Zstandard compressed) and records a reference attribute on the span: `thoughtgate.payload.blob_ref`. This feature is deferred from v0.4 to reduce delivery risk — inline truncation covers the vast majority of debugging use cases.

### 8.5 Sampling Strategies

| Strategy | Description | Use Case |
|----------|-------------|----------|
| `always_on` | Sample 100% of everything | Development, low-traffic environments |
| `always_off` | Disable tracing entirely | Extreme cost sensitivity (metrics only) |
| `head` | Probabilistic head sampling at ingestion | Simple, predictable costs |
| `tail` | Sample based on outcome after processing | **Recommended**: 100% errors, 5–10% success |

**Tail sampling implementation:**

```rust
// Pseudocode: tail sampling decision
fn should_export(span: &CompletedSpan) -> bool {
    // Always export errors
    if span.status == StatusCode::Error {
        return true;
    }
    // Always export policy denials
    if span.attributes.get("cedar.decision") == Some("deny") {
        return true;
    }
    // Always export approval workflows
    if span.attributes.contains_key("thoughtgate.task.id") {
        return true;
    }
    // Probabilistic sampling for success
    let rate = self.config.overrides
        .get(span.name())
        .unwrap_or(&self.config.success_sample_rate);
    thread_rng().gen::<f64>() < *rate
}
```

**Per-method overrides rationale:**
- `tools/call` at 10%: Higher value operations, worth capturing more frequently.
- `resources/list` / `tools/list` at 1%: High volume discovery operations, low debugging value.
- `sampling/createMessage` at 10%: LLM interactions are expensive and worth monitoring.

## 9. Storage Backend Recommendation

### 9.1 Why ClickHouse

Both observability reports (`Gold_Standard_Observability_Report__Claude_.md` and the Gemini report) independently conclude that **ClickHouse** is the superior storage backend for LLM trace data. Every major AI observability platform (LangSmith, Helicone, SigNoz, HyperDX) has chosen ClickHouse.

| Criteria | ClickHouse | Elasticsearch | Grafana Loki |
|----------|------------|---------------|--------------|
| **High-Cardinality Support** | ★★★★★ Native | ★★ Global ordinals bottleneck | ★ Explicitly unsupported |
| **Large Text Blobs** | ★★★★ LZ4/ZSTD compression | ★★★ Inverted index overhead | ★★ Store in message body |
| **Aggregation Queries** | ★★★★★ 5–100x faster than ES | ★★★ Slow on high cardinality | ★★ Very slow |
| **Storage Efficiency** | ★★★★★ 12–19x better than ES | ★★ High overhead | ★★★ Good with low cardinality |
| **OTel Integration** | ★★★★★ Native via SigNoz | ★★★★ Elastic APM | ★★★ Via Tempo |

### 9.2 Recommended Stack

```
ThoughtGate Proxy → OTel Collector → ClickHouse (traces + metrics + logs)
                                           │
                                           ▼
                                     SigNoz or HyperDX UI
                                     (query + dashboards)
```

**Primary recommendation: SigNoz** — Turnkey OTel-native observability stack with ClickHouse backend, pre-built dashboards for LLM observability, open-source with enterprise support.

**Alternative: HyperDX** — Open-source, ClickHouse-backed, excellent developer experience with session replay. Better for smaller teams wanting a unified logs+traces+metrics view.

**Both are valid for ThoughtGate.** The choice depends on team preference and existing infrastructure. ThoughtGate ships OTLP, which works with either (and any other OTel-compatible backend).

### 9.3 Architecture Note

ThoughtGate does **not** bundle or manage a storage backend. It emits OTLP telemetry; operators choose their backend. The ClickHouse recommendation is guidance, not a dependency. ThoughtGate works with Jaeger, Zipkin, Datadog, Honeycomb, Grafana Tempo, or any OTLP-compatible backend.

## 10. Grafana Dashboard Templates

ThoughtGate ships JSON dashboard templates in `deploy/grafana/` importable into any Grafana instance connected to a Prometheus-compatible data source.

### 10.1 Dashboard: ThoughtGate Overview

**File:** `deploy/grafana/thoughtgate-overview.json`

| Panel | Type | Query | Purpose |
|-------|------|-------|---------|
| Request Rate | Time Series | `rate(thoughtgate.requests.total[5m])` | Traffic volume |
| Error Rate | Time Series | `rate(thoughtgate.errors.total[5m]) / rate(thoughtgate.requests.total[5m])` | Error percentage |
| Latency p50/p95/p99 | Time Series | `histogram_quantile(0.95, thoughtgate.request.duration_ms)` | Request latency distribution |
| Cedar Decisions | Pie Chart | `sum by (decision) (thoughtgate.cedar.evaluations.total)` | Allow/deny ratio |
| Active Connections | Stat | `thoughtgate.connections.active` | Current load |
| Pending Approvals | Stat | `thoughtgate.tasks.pending` | Operational queue depth |
| Top Tools by Volume | Table | `topk(10, sum by (tool_name) (thoughtgate.requests.total))` | Tool usage ranking |
| Upstream Latency by Target | Time Series | `histogram_quantile(0.95, thoughtgate.upstream.duration_ms)` | Per-server performance |

### 10.2 Dashboard: Policy Decisions

**File:** `deploy/grafana/thoughtgate-policies.json`

| Panel | Type | Query | Purpose |
|-------|------|-------|---------|
| Cedar Evaluation Latency | Histogram | `thoughtgate.cedar.evaluation.duration_ms` | Policy engine performance |
| Denials by Policy | Bar Chart | `sum by (policy_id) (thoughtgate.cedar.evaluations.total{decision="deny"})` | Which policies are blocking |
| Denials by Tool | Bar Chart | `sum by (tool_name) (thoughtgate.requests.total{status="denied"})` | Which tools are blocked |
| Approval Wait Times | Histogram | `thoughtgate.approval.wait_duration_s` | Human response latency |
| Approval Outcomes | Pie Chart | `sum by (outcome) (thoughtgate.approval.requests.total)` | Approve/deny/timeout split |
| Gate Flow Funnel | Bar Chart | Gate 1→2→3→4 counts | Request flow through decision pipeline |

### 10.3 Dashboard: Streaming Performance (v0.4+)

**File:** `deploy/grafana/thoughtgate-streaming.json`

| Panel | Type | Query | Purpose |
|-------|------|-------|---------|
| TTFT by Model | Histogram | `thoughtgate.streaming.ttft_ms` grouped by `gen_ai.request.model` | Time to First Token comparison |
| TPOT by Provider | Histogram | `thoughtgate.streaming.tpot_avg_ms` grouped by `gen_ai.system` | Generation speed comparison |
| Stream Duration | Time Series | `histogram_quantile(0.95, thoughtgate.streaming.total_duration_ms)` | Total streaming latency |
| Chunk Count Distribution | Histogram | `thoughtgate.streaming.chunk_count` | Response size distribution |

## 11. Rust Implementation Guidance

### 11.1 Crate Selection

| Crate | Version | Purpose |
|-------|---------|---------|
| `opentelemetry` | 0.27+ | Core OTel API |
| `opentelemetry-otlp` | 0.27+ | OTLP gRPC/HTTP exporter |
| `opentelemetry-prometheus` | 0.27+ | Prometheus metrics bridge |
| `tracing` | 0.1+ | Rust structured logging (integrates with OTel via bridge) |
| `tracing-opentelemetry` | 0.28+ | Bridge `tracing` spans to OTel spans |
| `prometheus-client` | 0.22+ | Prometheus metric types and registry |
| `metrics` | 0.24+ | Alternative: `metrics` facade with Prometheus exporter |

### 11.2 Span Creation Pattern

```rust
use opentelemetry::{trace::{Tracer, SpanKind}, KeyValue};

// MCP request span
let span = tracer.span_builder("tools/call")
    .with_kind(SpanKind::Server)
    .with_attributes(vec![
        KeyValue::new("mcp.method.name", "tools/call"),
        KeyValue::new("mcp.session.id", session_id.clone()),
        KeyValue::new("mcp.message.type", "request"),
        KeyValue::new("gen_ai.operation.name", "execute_tool"),
        KeyValue::new("gen_ai.tool.name", tool_name.clone()),
    ])
    .start(&tracer);

// Cedar evaluation child span
let cedar_span = tracer.span_builder("cedar.evaluate")
    .with_kind(SpanKind::Internal)
    .start(&tracer);
// ... evaluate ...
cedar_span.set_attribute(KeyValue::new("cedar.decision", "allow"));
cedar_span.set_attribute(KeyValue::new("cedar.evaluation.duration_ms", eval_duration));
cedar_span.end();
```

### 11.3 Context Propagation Implementation

```rust
use opentelemetry::propagation::{TextMapPropagator, Injector, Extractor};
use opentelemetry_sdk::propagation::TraceContextPropagator;

let propagator = TraceContextPropagator::new();

// Extract from HTTP headers
let parent_ctx = propagator.extract(&HeaderExtractor(&request.headers));

// Extract from _meta (stdio transport)
fn extract_from_meta(meta: &serde_json::Value) -> Context {
    let mut carrier = HashMap::new();
    if let Some(tp) = meta.get("traceparent").and_then(|v| v.as_str()) {
        carrier.insert("traceparent".to_string(), tp.to_string());
    }
    if let Some(ts) = meta.get("tracestate").and_then(|v| v.as_str()) {
        carrier.insert("tracestate".to_string(), ts.to_string());
    }
    propagator.extract(&carrier)
}

// Inject into upstream HTTP request
propagator.inject_context(&span_ctx, &mut HeaderInjector(&mut upstream_headers));

// Inject into _meta (stdio transport)
fn inject_to_meta(ctx: &Context, meta: &mut serde_json::Map<String, Value>) {
    let mut carrier = HashMap::new();
    propagator.inject_context(ctx, &mut carrier);
    if let Some(tp) = carrier.get("traceparent") {
        meta.insert("traceparent".to_string(), Value::String(tp.clone()));
    }
    if let Some(ts) = carrier.get("tracestate") {
        meta.insert("tracestate".to_string(), Value::String(ts.clone()));
    }
}
```

## 12. Behavioral Specifications

### B-OBS2-001: Telemetry Disabled by Default in Development

When `telemetry.enabled: false` (the default in development), ThoughtGate emits zero network traffic for telemetry. The Prometheus `/metrics` endpoint still functions if `prometheus.enabled: true` (to support local Grafana), but OTLP export is fully disabled.

### B-OBS2-002: Graceful Degradation on Export Failure

If the OTLP endpoint is unreachable:
1. Telemetry data is queued up to `batch.max_queue_size`.
2. When the queue is full, oldest telemetry is dropped (not request processing).
3. A warning metric `thoughtgate.telemetry.dropped.total` is incremented.
4. Request processing is **never** blocked or delayed by telemetry failures.

### B-OBS2-003: Zero Overhead When Disabled

When telemetry is disabled, span creation uses no-op implementations. The overhead must be < 100 nanoseconds per request (cost of a no-op span check). No allocations occur for disabled telemetry.

### B-OBS2-004: Sensitive Attribute Gating

Content attributes (`gen_ai.tool.call.arguments`, `gen_ai.input.messages`, `gen_ai.output.messages`) are:
1. Disabled by default (`capture_tool_arguments: false`).
2. When enabled, always processed through the redaction pipeline (REQ-OBS-003) before attachment to spans.
3. Stored as span events (not span attributes) to avoid 4KB truncation in most backends.

### B-OBS2-005: Span Link Consistency for HITL

All approval workflows that cross an async boundary (Slack callback, webhook callback, task polling) **must** use span links. If the stored trace context is corrupted or missing at callback time:
1. Create a new trace (do not fail the callback).
2. Record `thoughtgate.trace_context.recovered: false` on the new span.
3. Log a warning with the task ID for manual trace correlation.

### B-OBS2-006: Metric Exemplars

All histogram metrics include trace ID exemplars when available. This enables clicking from a Grafana histogram bucket directly into the corresponding trace in the tracing backend.

### B-OBS2-007: request_id in Operational Logs

The `thoughtgate.request_id` value is injected as a structured field into all operational log entries (via `tracing` span fields) for every request. This ensures that three independent log streams — operational logs (`tracing`), audit logs (OCSF/REQ-OBS-003), and OTel spans — can all be correlated via `grep` or SIEM query on the same `request_id`. Without this, correlating a Cedar deny audit event with the corresponding operational error log requires joining on `trace_id`, which not all log aggregators support natively.

```rust
// Every request handler starts with request_id in the tracing span
let span = tracing::info_span!(
    "mcp_request",
    thoughtgate.request_id = %request_id,
    otel.trace_id = %trace_id,
);
```

## 13. Error Handling

### 13.1 Telemetry Pipeline Errors

| Error | Impact | Handling |
|-------|--------|----------|
| OTLP endpoint unreachable | No traces exported | Queue → drop oldest, increment `telemetry.dropped.total` |
| OTLP authentication failure | No traces exported | Log error once, retry with backoff, alert via `telemetry.auth_errors.total` |
| OTel collector overloaded (429) | Backpressure | Exponential backoff, reduce batch size |
| Content truncation triggered | Large payload truncated | Truncate with marker, record original size in span attribute, log at DEBUG level |
| Invalid trace context in inbound request | Malformed `traceparent` | Start new trace, log warning |
| Missing `_meta` in stdio request | No parent context | Start new trace (normal behavior for uninstrumented clients) |
| Span attribute value too large | Backend truncation | Pre-truncate to 4KB; large content stored as span events up to `max_inline_content_bytes`, then truncated with marker |

### 13.2 Prometheus Endpoint Errors

| Error | Impact | Handling |
|-------|--------|----------|
| Port conflict on configured Prometheus port | Metrics unavailable | Fail startup with clear error message |
| Scrape timeout (>10s) | Partial metrics | Optimize metric collection; if registry is too large, reduce cardinality |

## 14. Verification Plan

### 14.1 Test Matrix

| ID | Scenario | Expected Behavior |
|----|----------|-------------------|
| TC-OBS2-001 | MCP `tools/call` request through proxy | Root span created with all required MCP attributes |
| TC-OBS2-002 | Cedar policy evaluation | Child span `cedar.evaluate` with decision, policy_id, duration |
| TC-OBS2-003 | Cedar denial | Span event `cedar.denial` with violation reason |
| TC-OBS2-004 | Request with inbound `traceparent` (HTTP) | Child span created within existing trace |
| TC-OBS2-005 | Request without `traceparent` | New trace ID generated, root span |
| TC-OBS2-006 | Stdio request with `_meta.traceparent` | Trace context extracted from `_meta` (v0.3) |
| TC-OBS2-007 | Stdio request without `_meta` | New trace ID generated (v0.3) |
| TC-OBS2-008 | Approval dispatch to Slack | `PRODUCER` span with link to request span, completes immediately |
| TC-OBS2-009 | Approval callback from Slack | `CONSUMER` span with link to dispatch span, not parent-child |
| TC-OBS2-010 | Approval callback with corrupted trace context | New trace created, `trace_context.recovered: false` |
| TC-OBS2-011 | OTLP endpoint unreachable | Telemetry queued, dropped when full, requests unaffected |
| TC-OBS2-012 | Telemetry disabled | Zero network traffic, no-op spans, < 100ns overhead |
| TC-OBS2-013 | Prometheus `/metrics` scrape | All counters, histograms, gauges present |
| TC-OBS2-014 | Tool names exceeding cardinality cap | Tools beyond the configured limit (default 200) map to `__other__` label; total unique metric series bounded |
| TC-OBS2-015 | Payload > 100KB with content capture enabled | Content truncated inline with `...[TRUNCATED]` marker, `thoughtgate.payload.truncated = true` attribute set |
| TC-OBS2-016 | Payload > 100KB with content capture disabled | No content stored, span attributes record size only |
| TC-OBS2-017 | Content capture enabled | Tool arguments appear as span events after redaction |
| TC-OBS2-018 | Content capture disabled (default) | No content in spans or events |
| TC-OBS2-019 | Tail sampling: error response | Always exported (100%) |
| TC-OBS2-020 | Tail sampling: success response | Exported at configured rate |
| TC-OBS2-021 | Tail sampling: Cedar denial | Always exported (100%) |
| TC-OBS2-022 | Streaming response with TTFT/TPOT | Streaming attributes on span, histogram metrics emitted (v0.4) |
| TC-OBS2-023 | Grafana dashboard import | All panels render with sample data |
| TC-OBS2-024 | Metric exemplars | Histogram buckets contain trace_id exemplars |

### 14.2 Integration Test: End-to-End Trace

Validate that a complete MCP request flow produces a connected trace:

1. Send `tools/call` request with `traceparent` header.
2. Verify span hierarchy: `tools/call` (root) → `thoughtgate.decision` (child) → `cedar.evaluate` (child).
3. Verify all required attributes present on each span.
4. Verify `traceparent` injected into upstream request.
5. Verify response span recorded with upstream latency.

### 14.3 Integration Test: HITL Trace Continuity

Validate that approval workflows maintain trace correlation:

1. Send `tools/call` that triggers approval.
2. Verify `thoughtgate.approval.dispatch` span (PRODUCER) created and completed.
3. Simulate Slack callback with stored trace context.
4. Verify `thoughtgate.approval.callback` span (CONSUMER) created with span link to dispatch.
5. Verify both spans share the same `trace_id` but different `span_id`s.
6. Verify no parent-child relationship between dispatch and callback spans.

## 15. Definition of Done

- [ ] MCP spans emitted for all JSON-RPC methods with required OTel GenAI attributes
- [ ] Cedar evaluation spans with decision, policy_id, duration attributes
- [ ] Gateway decision spans with 4-gate outcome attributes
- [ ] Prometheus `/metrics` endpoint exposing all counters, histograms, and gauges
- [ ] OTLP gRPC and HTTP/protobuf export functional
- [ ] W3C `traceparent`/`tracestate` propagation via HTTP headers
- [ ] Tail sampling: 100% errors and denials, configurable success rate
- [ ] Telemetry export does not block or delay request processing
- [ ] Telemetry disabled = zero network overhead
- [ ] Grafana dashboard templates importable and rendering
- [ ] All test cases (TC-OBS2-001 through TC-OBS2-024) passing
- [ ] Documentation updated in CLAUDE.md

### v0.3 Additions
- [ ] Stdio transport trace propagation via `params._meta` extraction/injection
- [ ] Durable trace context storage for approval workflows (crash recovery)

### v0.4+ Additions
- [ ] Streaming metrics (TTFT, TPOT) via bookends approach
- [ ] Large payload truncation with `...[TRUNCATED]` marker
- [ ] Streaming Grafana dashboard template
