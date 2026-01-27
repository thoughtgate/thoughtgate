// ThoughtGate k6 Load Test - MCP JSON-RPC Traffic
//
// Tests proxy performance with realistic MCP tool call requests.
//
// Usage:
//   # Start mock MCP server
//   MOCK_MCP_PORT=8888 ./target/release/mock_mcp
//
//   # Start ThoughtGate proxy
//   THOUGHTGATE_UPSTREAM_URL=http://127.0.0.1:8888 \
//   THOUGHTGATE_OUTBOUND_PORT=4141 \
//   ./target/release/thoughtgate
//
//   # Run benchmark
//   k6 run tests/benchmark.js

import http from 'k6/http';

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
  http.post('http://127.0.0.1:4141/mcp/v1', PAYLOAD, {
    headers: { 'Content-Type': 'application/json' },
    timeout: '5s',
  });
}
