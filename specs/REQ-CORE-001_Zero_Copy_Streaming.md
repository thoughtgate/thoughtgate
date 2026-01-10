# REQ-CORE-001: Zero-Copy Streaming (Green Path)

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-CORE-001` |
| **Title** | Zero-Copy Streaming (Green Path) |
| **Type** | Core Mechanic |
| **Status** | Draft |
| **Priority** | **Critical** |
| **Tags** | `#proxy` `#streaming` `#performance` `#latency` `#zero-copy` |

## 1. Context & Decision Rationale

This requirement implements the **"Green Path"** in ThoughtGate's traffic classification system. The Green Path is the fast lane for trusted, high-volume traffic that doesn't require inspection.

**Traffic Classification:**
| Path | Trigger | Behavior | Requirement |
|------|---------|----------|-------------|
| **Green (This REQ)** | `PolicyDecision::Green` | Zero-copy streaming | REQ-CORE-001 |
| **Amber** | `PolicyDecision::Amber` | Buffered inspection | REQ-CORE-002 |
| **Approval** | `PolicyDecision::Approval` | Human/agent approval | REQ-GOV-001/002/003 |
| **Red** | `PolicyDecision::Red` | Immediate rejection | REQ-CORE-004 |

**When is Green Path Used?**
- LLM token streams (high volume, low latency critical)
- Large file transfers
- Responses from approved tool calls
- Any traffic where Cedar policy permits `StreamRaw` action

**"Zero-Copy" Defined:**
In this context, "zero-copy" means:
- No application-level buffering of body content
- Chunks flow directly from upstream to client
- Only metadata (headers, method, URI) is inspected
- Memory usage is O(1) regardless of payload size

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-POL-001 | **Receives from** | `PolicyDecision::Green` triggers this path |
| REQ-CORE-003 | **Provides to** | Streaming capability for MCP responses |
| REQ-CORE-004 | **Uses** | Error responses for upstream failures |
| REQ-CORE-005 | **Coordinates with** | Shutdown drains active streams |

## 3. Intent

The system must:
1. Forward request and response body chunks immediately upon receipt
2. Minimize Time-To-First-Byte (TTFB) overhead
3. Operate with zero application-level buffering
4. Support HTTP trailers and protocol upgrades
5. Correctly propagate backpressure between client and upstream

## 4. Scope

### 4.1 In Scope
- Zero-copy body forwarding via custom `Body` implementation
- Socket optimization (TCP_NODELAY, keepalive, buffer sizes)
- Concurrency limiting via semaphore
- Backpressure propagation
- HTTP trailer forwarding
- Connection-level stream timeouts
- Client disconnect detection and upstream cancellation

### 4.2 Out of Scope
- Body inspection or modification (REQ-CORE-002)
- Policy evaluation (REQ-POL-001)
- Error response formatting (REQ-CORE-004)
- MCP-specific routing (REQ-CORE-003)
- Protocol upgrades / WebSocket (deferred to v0.2 - MCP uses HTTP+SSE)

## 5. Constraints

### 5.1 Runtime & Dependencies

| Crate | Purpose | Notes |
|-------|---------|-------|
| `tokio` | Async runtime | rt-multi-thread |
| `hyper` | HTTP implementation | v1.x |
| `hyper-util` | Hyper utilities | Connection handling |
| `bytes` | Zero-copy buffers | `Bytes` type |
| `http-body` | Body trait | Frame forwarding |
| `socket2` | Socket configuration | TCP options |

**Forbidden Patterns:**
- `Vec<u8>` accumulation of body chunks
- `String` conversion of body content
- JSON deserialization of body stream
- `clone()` of body chunks (move semantics only)

### 5.2 Configuration

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| TCP_NODELAY | `true` | `THOUGHTGATE_TCP_NODELAY` |
| TCP Keepalive | `60s` | `THOUGHTGATE_TCP_KEEPALIVE_SECS` |
| Socket buffer size | `256KB` | `THOUGHTGATE_SOCKET_BUFFER_BYTES` |
| Stream read timeout | `300s` | `THOUGHTGATE_STREAM_READ_TIMEOUT_SECS` |
| Stream write timeout | `300s` | `THOUGHTGATE_STREAM_WRITE_TIMEOUT_SECS` |
| Total stream timeout | `3600s` | `THOUGHTGATE_STREAM_TOTAL_TIMEOUT_SECS` |
| Max concurrent streams | `10000` | `THOUGHTGATE_MAX_CONCURRENT_STREAMS` |

### 5.3 Network Optimization

**Socket Options (CRITICAL):**
Both client and upstream `TcpStream` connections MUST be configured:
- `TCP_NODELAY`: Disable Nagle's algorithm for low latency
- `SO_KEEPALIVE`: Detect dead connections
- `SO_RCVBUF` / `SO_SNDBUF`: Adequate buffer sizes

**Backpressure:**
The implementation MUST correctly propagate backpressure:
- If client reads slowly → proxy pauses reading from upstream
- If upstream sends slowly → proxy waits without timeout (within limits)
- TCP window management handles flow control automatically

### 5.4 Protocol Transparency

- Preserve `Content-Length` and `Transfer-Encoding` exactly
- Do not manually strip hop-by-hop headers (hyper handles this)

## 6. Interfaces

### 6.1 Input

```rust
/// Green Path is triggered when policy returns this decision
pub enum PolicyDecision {
    Green,  // This path
    Amber,
    Hitl { ... },
    Red { ... },
}

/// Input: HTTP request with body stream
pub type IncomingRequest = Request<Incoming>;
```

### 6.2 Output

```rust
/// Output: HTTP response with streaming body
pub type StreamingResponse = Response<ProxyBody<Incoming>>;

/// Or on error
pub type ErrorResponse = Response<Full<Bytes>>;
```

### 6.3 Core Types

```rust
/// Zero-copy body wrapper that forwards frames without buffering
pub struct ProxyBody<B> {
    inner: B,
    metrics: StreamMetrics,
    cancel_token: CancellationToken,
}

impl<B> http_body::Body for ProxyBody<B>
where
    B: http_body::Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        // 1. Check cancellation (client disconnect)
        if self.cancel_token.is_cancelled() {
            return Poll::Ready(None);
        }

        // 2. Poll inner body
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                // 3. Record metrics (inspect ref only, no copy)
                if let Some(data) = frame.data_ref() {
                    self.metrics.record_bytes(data.len());
                } else if frame.is_trailers() {
                    self.metrics.record_trailers();
                }
                // 4. Forward frame (move, not copy)
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => {
                self.metrics.record_error();
                Poll::Ready(Some(Err(e.into())))
            }
            Poll::Ready(None) => {
                self.metrics.record_complete();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
```

### 6.4 Errors

| Condition | HTTP Status | Error Code |
|-----------|-------------|------------|
| Upstream connection refused | 502 | -32000 |
| Upstream timeout | 504 | -32001 |
| Client disconnect | N/A | (connection closed) |
| Concurrency limit exceeded | 503 | -32013 |
| Stream timeout | 504 | -32001 |

## 7. Functional Requirements

### F-001: Zero-Copy Frame Forwarding

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        ZERO-COPY FORWARDING                                     │
│                                                                                 │
│   Upstream                    ProxyBody                         Client          │
│      │                           │                                │             │
│      │  Frame<Bytes>             │                                │             │
│      │ ─────────────────────────▶│                                │             │
│      │                           │  (inspect ref for metrics)     │             │
│      │                           │                                │             │
│      │                           │  Frame<Bytes>                  │             │
│      │                           │ ──────────────────────────────▶│             │
│      │                           │  (move, not copy)              │             │
│      │                           │                                │             │
│                                                                                 │
│   Memory: O(1) - only one frame in flight at a time                            │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

- **F-001.1:** Implement custom `Body` struct wrapping incoming stream
- **F-001.2:** Yield `Frame<Bytes>` directly without cloning
- **F-001.3:** Use move semantics only; no `clone()` of chunks
- **F-001.4:** Record metrics by inspecting frame references

### F-002: Fail-Fast Error Propagation

- **F-002.1:** Upstream connection refused → `502 Bad Gateway`
- **F-002.2:** Upstream timeout → `504 Gateway Timeout`
- **F-002.3:** Client disconnect → Immediately cancel upstream via `CancellationToken`
- **F-002.4:** On any error, close both connections and log at `WARN`

### F-003: HTTP Trailer Support

- **F-003.1:** Forward trailers via `Frame::trailers()`
- **F-003.2:** Handle `poll_frame` returning trailer frames after data frames
- **F-003.3:** Preserve trailer headers exactly

### ~~F-004: Protocol Upgrade Handling~~ (Deferred to v0.2)

Protocol upgrades (WebSocket, HTTP/2) are not required for v0.1. MCP uses HTTP + SSE which does not require upgrade handling. Hyper's default behavior is sufficient.

**Rationale:** MCP's Streamable HTTP transport uses standard HTTP POST with SSE responses. WebSocket transport is uncommon in MCP deployments.

### F-004: Connection-Level Timeout (Slow-Read Protection)

**Problem:** Without timeouts, a misbehaving or malicious upstream can hold connections indefinitely (slow-read attack), exhausting proxy resources.

**Solution:** Wrap the entire upstream request/response cycle in `tokio::time::timeout`. This provides protection without per-chunk allocation overhead.

```rust
/// Implements: REQ-CORE-001/F-004
/// 
/// Connection-level timeout protects against slow-read attacks.
/// Unlike per-chunk TimeoutBody wrappers, this approach:
/// - Zero allocation overhead (no Box<dyn Body>)
/// - Catches slow connection establishment
/// - Catches slow header delivery
/// - Catches slow body streaming
pub async fn forward_with_timeout(
    upstream: &UpstreamClient,
    request: Request<Body>,
    timeout_duration: Duration,
) -> Result<Response<Body>, ThoughtGateError> {
    tokio::time::timeout(timeout_duration, upstream.forward(request))
        .await
        .map_err(|_| ThoughtGateError::UpstreamTimeout {
            url: upstream.base_url().to_string(),
            timeout_secs: timeout_duration.as_secs(),
        })?
}
```

- **F-004.1:** Wrap `upstream.forward()` in `tokio::time::timeout()`
- **F-004.2:** Use `THOUGHTGATE_STREAM_TOTAL_TIMEOUT_SECS` (default: 3600s)
- **F-004.3:** On timeout, return `ThoughtGateError::UpstreamTimeout`
- **F-004.4:** Log timeout events at `WARN` level with upstream URL

### F-005: Concurrency Limiting

- **F-006.1:** Use global `tokio::sync::Semaphore` for stream limit
- **F-006.2:** Acquire permit before starting stream
- **F-006.3:** If semaphore exhausted → return `503 Service Unavailable` immediately
- **F-006.4:** Release permit when stream completes (success or error)

## 8. Non-Functional Requirements

### NFR-001: Observability

**Tracing:**
- Emit OTel span `green_path.stream` with attributes:
  - `stream_duration_ms`
  - `bytes_transferred`
  - `error_kind`
  - `timed_out` (boolean)

**Metrics:**
```
green_path_bytes_total{direction="upload|download"}
green_path_streams_active
green_path_streams_total{outcome="success|error|timeout"}
green_path_ttfb_seconds (histogram, buckets: .001, .005, .01, .05, .1)
green_path_chunk_size_bytes (histogram)
green_path_timeouts_total
```

### NFR-002: Performance

| Metric | Target |
|--------|--------|
| TTFB overhead | < 2ms (P99) |
| Memory (1GB stream) | < 5MB peak RSS delta |
| Concurrent streams | 10,000 on standard instance |
| Chunk forwarding | < 100µs per chunk |

### NFR-003: Reliability

- No memory leaks after 10k streams (verify with Valgrind/ASAN)
- Graceful handling of malformed chunks
- Clean shutdown with stream draining

## 9. Verification Plan

### 9.1 Edge Case Matrix

| Scenario | Expected Behavior | Test ID |
|----------|-------------------|---------|
| Trailers present | Forward chunks → Forward trailers → Close | EC-GRN-001 |
| Client disconnect | Detect EOF, close upstream < 10ms | EC-GRN-002 |
| Upstream RST | Propagate 502 to client immediately | EC-GRN-003 |
| Slow reader (backpressure) | Upstream read pauses until client consumes | EC-GRN-004 |
| No-body response (204) | Forward headers, yield no frames, finish | EC-GRN-005 |
| Large chunk (16MB) | Forward without splitting or buffering | EC-GRN-006 |
| Concurrent stream limit | 10,000 OK; 10,001st gets 503 | EC-GRN-007 |
| Invalid chunk from upstream | Detect error, close connection, log | EC-GRN-008 |
| Connection timeout exceeded | Return UpstreamTimeout error, log at WARN | EC-GRN-009 |
| Slow upstream (near timeout) | Complete successfully if within timeout | EC-GRN-010 |

### 9.2 Assertions

**Unit Tests:**
- `test_proxy_body_no_buffering` — Memory stays flat (O(1)) for 100MB stream
- `test_cancellation_on_client_disconnect` — Upstream cancelled within 10ms
- `test_trailer_forwarding` — Trailers arrive at client
- `test_connection_timeout` — UpstreamTimeout returned when timeout exceeded

**Integration Tests:**
- `test_bidirectional_backpressure` — Pause downstream → upstream pauses
- `test_concurrent_stream_limit` — 503 returned when limit exceeded
- `test_slow_upstream_succeeds` — Completes if within timeout threshold

**Benchmarks:**
- `bench_ttfb_overhead` — P99 < 2ms
- `bench_throughput` — Saturate network before CPU

**Fuzzing:**
- `cargo fuzz run green_path` — No panics on malformed chunks/headers

## 10. Implementation Status

### 10.1 Completed
- [x] `ProxyBody` wrapper implementing `http_body::Body`
- [x] `TCP_NODELAY` and `SO_KEEPALIVE` configured via `socket2`
- [x] Concurrency limit (Semaphore) enforced
- [x] Backpressure verified
- [x] Prometheus metrics and OTel spans

### 10.2 v0.1 Implementation Required

**F-004: Connection-Level Timeout**
- [ ] Wrap `upstream.forward()` in `tokio::time::timeout()`
- [ ] Use `THOUGHTGATE_STREAM_TOTAL_TIMEOUT_SECS` configuration
- [ ] Return `ThoughtGateError::UpstreamTimeout` on timeout
- [ ] Add `green_path_timeouts_total` metric

**Implementation Note:** This is a small change (~10 lines) that wraps the existing forward call. No architectural changes required.

### 10.3 Deferred to v0.2

**Protocol Upgrades (WebSocket/HTTP2)**
- Not required for MCP's HTTP+SSE transport
- Hyper's default handling sufficient for v0.1
- Will implement explicit `hyper::upgrade::on()` in v0.2 if needed

### 10.4 Pending (Non-Blocking for v0.1)
- [ ] Memory leak test (Valgrind/ASAN)
- [ ] Performance benchmarks (TTFB < 2ms verification)

## 11. Anti-Patterns to Avoid

- **❌ Cloning chunks:** `frame.data().clone()` defeats zero-copy
- **❌ Buffering:** `Vec::extend_from_slice()` turns Green into slow Amber
- **❌ String conversion:** `String::from_utf8()` allocates and fails on binary
- **❌ Ignoring backpressure:** Reading upstream without waiting for client
- **❌ Blocking operations:** Any `std::sync` primitives in async path

## 12. Definition of Done

- [x] `ProxyBody` wrapper implemented complying with `http_body::Body`
- [x] `TCP_NODELAY` & `SO_KEEPALIVE` configured via `socket2`
- [x] Concurrency limit (Semaphore) enforced and tested
- [x] Backpressure verified
- [ ] Connection-level timeout implemented (F-004)
- [x] Prometheus metrics and OTel spans
- [ ] Memory leak test passed
- [ ] Performance benchmarks passed (TTFB < 2ms, Memory O(1))