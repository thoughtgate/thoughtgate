// ThoughtGate k6 Load Test - Fast CI Version
//
// Reduced duration for faster CI feedback. Uses MCP JSON-RPC traffic.
//
// Environment variables:
//   K6_PROXY_HOST: ThoughtGate proxy host (default: 127.0.0.1)
//   K6_PROXY_PORT: ThoughtGate proxy port (default: 7467)
//
// Usage:
//   k6 run thoughtgate-proxy/tests/benchmark-fast.js
//   k6 run -e K6_PROXY_PORT=8080 thoughtgate-proxy/tests/benchmark-fast.js

import http from 'k6/http';

// Configurable proxy endpoint
const PROXY_HOST = __ENV.K6_PROXY_HOST || '127.0.0.1';
const PROXY_PORT = __ENV.K6_PROXY_PORT || '7467';
const PROXY_URL = `http://${PROXY_HOST}:${PROXY_PORT}/mcp/v1`;

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
  http.post(PROXY_URL, PAYLOAD, {
    headers: { 'Content-Type': 'application/json' },
    timeout: '5s',
  });
}
