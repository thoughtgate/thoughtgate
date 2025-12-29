# ThoughtGate

A high-performance, memory-safe sidecar proxy for governing MCP (Model Context Protocol) and A2A (Agent-to-Agent) agentic AI traffic. Built in Rust with zero-copy streaming, full HTTP/1.1 and HTTP/2 support, and production-ready observability.

## Vision

ThoughtGate complements gateway-level projects like [agentgateway](https://github.com/linuxfoundation/agentgateway) (Linux Foundation, Solo.io-donated) by providing **sidecar-level governance** for agentic workloads. While gateways handle ingress/egress at the network boundary, sidecars provide fine-grained control at the application level, enabling:

- **Per-pod policy enforcement** (future: Cedar policy engine)
- **Streaming-aware inspection** (future: struson-based JSON streaming)
- **Low-latency request/response modification** without gateway round-trips
- **SPIFFE-based identity** for mTLS and workload attestation (future)

This project follows the architectural patterns of [Linkerd2-proxy](https://github.com/linkerd/linkerd2-proxy) — minimal footprint, high throughput, and composable Tower middleware.

## Features

### MVP (Current)

- ✅ **Forward HTTP Proxy** - Configure via `HTTP_PROXY`/`HTTPS_PROXY` environment variables
- ✅ **Full HTTP/1.1 Support** - Persistent connections, chunked encoding, hop-by-hop header handling
- ✅ **CONNECT Method Tunneling** - Bidirectional streaming for HTTPS connections (preserves stateful flows)
- ✅ **Zero-Copy Streaming** - Never buffers full bodies (critical for SSE, long-running tasks)
- ✅ **Structured JSON Logging** - Request/response logging with timestamp, method, URI, status, latency
- ✅ **Security-Aware Logging** - Automatically strips sensitive headers (Authorization, Cookies, API keys)
- ✅ **Tower-Based Middleware** - Extensible stack (LoggingLayer now → future: Auth, RateLimit, Policy)

### Future Work

- **HTTP/2 Full Support** - Currently HTTP/2 client support, server-side coming
- **Ambient Redirection** - iptables/nftables-based transparent proxying (no env var config needed)
- **xDS Configuration** - Dynamic configuration via Envoy xDS APIs (like agentgateway)
- **Streaming JSON Inspection** - [struson](https://github.com/qwerty250/st ruson)-based streaming JSON parsing for MCP/A2A protocol inspection
- **Cedar Policy Engine** - Fine-grained authorization policies
- **SPIFFE Integration** - Workload identity and mTLS
- **Metrics & Tracing** - Prometheus metrics, OpenTelemetry tracing
- **Request/Response Modification** - Header injection, body transformation

## Architecture

```
┌─────────────┐
│   Client    │
└──────┬──────┘
       │ HTTP_PROXY=127.0.0.1:4141
       ▼
┌─────────────────────────────────┐
│        ThoughtGate              │
│  ┌───────────────────────────┐ │
│  │  Tower Middleware Stack    │ │
│  │  ┌─────────────────────┐   │ │
│  │  │  LoggingLayer       │   │ │
│  │  └──────────┬──────────┘   │ │
│  │             │               │ │
│  │  ┌──────────▼──────────┐   │ │
│  │  │  ProxyService       │   │ │
│  │  │  - HTTP forwarding  │   │ │
│  │  │  - CONNECT tunnel   │   │ │
│  │  └──────────┬──────────┘   │ │
│  └─────────────┼───────────────┘ │
└────────────────┼─────────────────┘
                 │
                 ▼
         ┌───────────────┐
         │   Upstream    │
         │  (MCP/A2A)    │
         └───────────────┘
```

### Core Components

- **`main.rs`** - Entry point, connection handling, CONNECT detection
- **`proxy_service.rs`** - Core proxy logic (HTTP forwarding, CONNECT tunneling)
- **`logging_layer.rs`** - Tower layer for structured request/response logging
- **`error.rs`** - Error types using `thiserror`

## Build & Run

### Prerequisites

- Rust 1.70+ (edition 2021)
- Cargo

### Build

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release
```

### Run

```bash
# Default: listen on 127.0.0.1:4141
cargo run

# Custom port via CLI
cargo run -- --port 8080

# Custom port via environment variable
PROXY_PORT=8080 cargo run

# Custom bind address and port
cargo run -- --bind 0.0.0.0 --port 4141
```

### Configuration

- **Port**: `--port` flag or `PROXY_PORT` environment variable (default: `4141`)
- **Bind Address**: `--bind` flag (default: `127.0.0.1`)
- **Log Level**: `RUST_LOG` environment variable (default: `info`)

## Usage Examples

### 1. Basic HTTP Proxy

Configure your client to use the proxy:

```bash
export HTTP_PROXY=http://127.0.0.1:4141
export HTTPS_PROXY=http://127.0.0.1:4141

# Test with curl
curl -v http://example.com
```

The proxy will:
1. Receive the request
2. Extract target URI from the request
3. Forward to upstream with proper headers
4. Stream response back to client
5. Log request/response with structured JSON

### 2. HTTPS Tunneling (CONNECT)

For HTTPS connections, the proxy automatically handles CONNECT:

```bash
export HTTPS_PROXY=http://127.0.0.1:4141

# HTTPS request - proxy will tunnel
curl -v https://example.com
```

The proxy will:
1. Receive `CONNECT example.com:443 HTTP/1.1`
2. Connect to upstream `example.com:443`
3. Send `200 Connection Established` to client
4. Bidirectionally copy bytes (zero-copy streaming)
5. Log tunnel establishment and closure

### 3. Streaming SSE (Server-Sent Events)

For SSE streams, the proxy preserves streaming without buffering:

```bash
export HTTP_PROXY=http://127.0.0.1:4141

# SSE endpoint
curl -N http://example.com/sse
```

The proxy streams chunks as they arrive, maintaining low Time-To-First-Byte (TTFB).

### 4. MCP/A2A Protocol Traffic

For agentic AI protocols (MCP, A2A), the proxy handles JSON-RPC and streaming responses:

```bash
export HTTP_PROXY=http://127.0.0.1:4141
export HTTPS_PROXY=http://127.0.0.1:4141

# Your MCP/A2A client will automatically use the proxy
```

## Logging

Logs are emitted as structured JSON to stdout:

```json
{
  "timestamp": "2024-01-15T10:30:45.123Z",
  "level": "INFO",
  "fields": {
    "method": "GET",
    "uri": "http://example.com/api",
    "status": 200,
    "latency_ms": 45,
    "direction": "outbound",
    "body_info": "1024 bytes"
  },
  "target": "thoughtgate",
  "message": "Response sent"
}
```

### Logged Fields

- **Request**: `method`, `uri`, `version`, `headers` (sanitized), `direction: "inbound"`
- **Response**: `method`, `uri`, `status`, `version`, `headers` (sanitized), `body_info`, `latency_ms`, `direction: "outbound"`
- **CONNECT**: `authority`, `target`, `direction: "tunnel"`

### Sensitive Headers (Auto-Redacted)

The following headers are automatically redacted in logs:
- `Authorization`
- `Cookie`
- `Set-Cookie`
- `X-Api-Key`
- `X-Auth-Token`
- `Proxy-Authorization`

## Testing

### Manual Testing

1. **Start the proxy**:
   ```bash
   cargo run -- --port 4141
   ```

2. **Test HTTP**:
   ```bash
   export HTTP_PROXY=http://127.0.0.1:4141
   curl -v http://httpbin.org/get
   ```

3. **Test HTTPS (CONNECT)**:
   ```bash
   export HTTPS_PROXY=http://127.0.0.1:4141
   curl -v https://httpbin.org/get
   ```

4. **Test Streaming**:
   ```bash
   export HTTP_PROXY=http://127.0.0.1:4141
   curl -N http://example.com/sse-endpoint
   ```

### Integration Tests (Future)

Integration tests using `wiremock` to verify:
- Streaming without buffering
- CONNECT tunneling correctness
- Header forwarding
- Error handling

## Performance Considerations

- **Zero-Copy**: Uses `bytes::Bytes` for body chunks, never copies full bodies
- **Backpressure**: Respects client/server backpressure via Tokio streams
- **Connection Pooling**: Hyper client maintains persistent connections
- **Memory Safety**: Rust's ownership system prevents common proxy vulnerabilities

## Dependencies

- **tokio** - Async runtime
- **hyper** (v1) - HTTP/1.1 and HTTP/2 client/server
- **hyper-util** - Hyper utilities and adapters
- **http-body-util** - Body combinators
- **tower** - Middleware stack
- **tracing** + **tracing-subscriber** - Structured logging
- **clap** - CLI argument parsing
- **thiserror** - Error handling

## Contributing

This project follows Rust best practices:
- Use `clippy` with pedantic standards
- Zero-copy where possible (`bytes::Bytes`)
- Proper error handling (`thiserror` for internal, `anyhow` only in `main.rs`/tests)
- Comprehensive comments for complex logic

## License

Apache-2.0 OR MIT

## References

- [Linkerd2-proxy](https://github.com/linkerd/linkerd2-proxy) - Architectural inspiration
- [agentgateway](https://github.com/linuxfoundation/agentgateway) - Gateway-level governance
- [Hyper v1](https://hyper.rs/) - HTTP library
- [Tower](https://github.com/tower-rs/tower) - Middleware stack

