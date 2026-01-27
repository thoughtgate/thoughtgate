// ThoughtGate k6 Load Test - Fast CI Version
//
// Reduced duration for faster CI feedback. Uses MCP JSON-RPC traffic.
//
// Usage:
//   k6 run tests/benchmark-fast.js

import http from 'k6/http';

export const options = {
  vus: 10,
  duration: '5s', // Reduced from 10s for faster CI tests
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
