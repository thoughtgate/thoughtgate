// ThoughtGate k6 Load Test - MCP JSON-RPC Traffic
//
// Tests proxy performance with realistic MCP tool call requests.
//
// Environment variables:
//   K6_PROXY_HOST: ThoughtGate proxy host (default: 127.0.0.1)
//   K6_PROXY_PORT: ThoughtGate proxy port (default: 7467)
//
// Usage:
//   # Start mock MCP server
//   MOCK_MCP_PORT=8888 ./target/release/mock_mcp
//
//   # Start ThoughtGate proxy
//   THOUGHTGATE_UPSTREAM_URL=http://127.0.0.1:8888 \
//   THOUGHTGATE_OUTBOUND_PORT=7467 \
//   ./target/release/thoughtgate
//
//   # Run benchmark
//   k6 run tests/benchmark.js
//
//   # Run with custom port
//   # Run with custom port (if not using default 7467)
//   k6 run -e K6_PROXY_PORT=8080 tests/benchmark.js

import http from 'k6/http';

// Configurable proxy endpoint
const PROXY_HOST = __ENV.K6_PROXY_HOST || '127.0.0.1';
const PROXY_PORT = __ENV.K6_PROXY_PORT || '7467';
const PROXY_URL = `http://${PROXY_HOST}:${PROXY_PORT}/mcp/v1`;

export const options = {
  vus: 10,
  duration: '10s',
};

// MCP JSON-RPC tool call request
const PAYLOAD = JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'tools/call',
  params: {
    name: 'benchmark_tool',
    arguments: { data: 'A'.repeat(1000) },
  },
});

export default function () {
  // Hit the ThoughtGate proxy MCP endpoint
  http.post(PROXY_URL, PAYLOAD, {
    headers: { 'Content-Type': 'application/json' },
    timeout: '5s',
  });
}
