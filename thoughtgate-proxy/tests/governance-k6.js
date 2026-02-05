// ThoughtGate k6 Governance Validation Test
//
// Tests the governance 4-gate decision flow in a Kubernetes environment.
// Validates that:
// - Allowed tools (forward action) succeed with 200 response
// - Denied tools (delete_*, *_unsafe) return -32014 error code
//
// Implements: REQ-CORE-003 (4-Gate Decision Flow)
// Implements: REQ-GOV-002 (Governance Rules)
// Implements: REQ-CORE-004 (Error Codes: -32014)
//
// Environment variables:
//   K6_PROXY_HOST: ThoughtGate proxy host (default: 127.0.0.1)
//   K6_PROXY_PORT: ThoughtGate proxy port (default: 4141)
//
// Usage in K8s:
//   kubectl create configmap k6-governance-script --from-file=governance.js=governance-k6.js
//   k6 run /scripts/governance.js

import http from 'k6/http';
import { check, fail } from 'k6';
import { Counter, Trend } from 'k6/metrics';

// Custom metrics for governance validation
const forwardSuccess = new Counter('governance_forward_success');
const forwardFail = new Counter('governance_forward_fail');
const denySuccess = new Counter('governance_deny_success');
const denyFail = new Counter('governance_deny_fail');
const forwardLatency = new Trend('governance_forward_latency_ms');
const denyLatency = new Trend('governance_deny_latency_ms');

// Configurable proxy endpoint
const PROXY_HOST = __ENV.K6_PROXY_HOST || '127.0.0.1';
const PROXY_PORT = __ENV.K6_PROXY_PORT || '4141';
const PROXY_URL = `http://${PROXY_HOST}:${PROXY_PORT}/mcp/v1`;

// Expected error code for governance deny (Gate 2)
const GOVERNANCE_RULE_DENIED = -32014;

export const options = {
  // Single VU for correctness testing (not performance)
  vus: 1,
  // 10 iterations to thoroughly test both paths
  iterations: 10,
  thresholds: {
    // All forward requests should succeed
    'governance_forward_success': ['count>=5'],
    'governance_forward_fail': ['count==0'],
    // All deny requests should return correct error
    'governance_deny_success': ['count>=5'],
    'governance_deny_fail': ['count==0'],
  },
};

// Retry helper for connection issues during startup
function retryRequest(url, payload, maxRetries = 5) {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      const res = http.post(url, payload, {
        headers: { 'Content-Type': 'application/json' },
        timeout: '5s',
      });

      // If connection refused or 503, retry with backoff
      if (res.status === 0 || res.status === 503) {
        console.log(`Attempt ${attempt}/${maxRetries}: Service not ready (status: ${res.status}), retrying...`);
        // Exponential backoff: 1s, 2s, 4s, 8s, 16s
        const sleepMs = Math.pow(2, attempt - 1) * 1000;
        const start = Date.now();
        while (Date.now() - start < sleepMs) {
          // Busy wait (k6 doesn't have sleep that works here)
        }
        continue;
      }

      return res;
    } catch (e) {
      console.log(`Attempt ${attempt}/${maxRetries}: Request failed (${e}), retrying...`);
      const sleepMs = Math.pow(2, attempt - 1) * 1000;
      const start = Date.now();
      while (Date.now() - start < sleepMs) {}
    }
  }

  fail('Max retries exceeded - service not available');
  return null;
}

export default function () {
  const iteration = __ITER;
  const vu = __VU;

  // Alternate between forward and deny tests
  if (iteration % 2 === 0) {
    testForwardAction(iteration, vu);
  } else {
    testDenyAction(iteration, vu);
  }
}

// Test that allowed tools are forwarded successfully
function testForwardAction(iteration, vu) {
  const toolName = iteration % 4 === 0 ? 'get_weather' : 'list_files';

  const payload = JSON.stringify({
    jsonrpc: '2.0',
    id: iteration + 1,
    method: 'tools/call',
    params: {
      name: toolName,
      arguments: { test: true, iteration: iteration }
    }
  });

  console.log(`[VU${vu}][iter${iteration}] Testing FORWARD: ${toolName}`);

  const startTime = Date.now();
  const res = retryRequest(PROXY_URL, payload);
  const duration = Date.now() - startTime;

  if (!res) {
    forwardFail.add(1);
    console.error(`[VU${vu}][iter${iteration}] FORWARD test failed - no response`);
    return;
  }

  forwardLatency.add(duration);

  // Parse response
  let body;
  try {
    body = JSON.parse(res.body);
  } catch (e) {
    forwardFail.add(1);
    console.error(`[VU${vu}][iter${iteration}] FORWARD test failed - invalid JSON: ${res.body}`);
    return;
  }

  // Validate: should have result, no error
  const success = check(body, {
    'forward: has result': (b) => b.result !== undefined && b.result !== null,
    'forward: no error': (b) => b.error === undefined || b.error === null,
    'forward: correct id': (b) => b.id === iteration + 1,
  });

  if (success) {
    forwardSuccess.add(1);
    console.log(`[VU${vu}][iter${iteration}] FORWARD test PASSED: ${toolName} (${duration}ms)`);
  } else {
    forwardFail.add(1);
    console.error(`[VU${vu}][iter${iteration}] FORWARD test FAILED: ${toolName}`);
    console.error(`  Response: ${JSON.stringify(body)}`);
  }
}

// Test that denied tools return -32014 error
function testDenyAction(iteration, vu) {
  // Alternate between delete_* and *_unsafe patterns
  const toolName = iteration % 4 === 1 ? 'delete_user' : 'run_unsafe';

  const payload = JSON.stringify({
    jsonrpc: '2.0',
    id: iteration + 1,
    method: 'tools/call',
    params: {
      name: toolName,
      arguments: { test: true, iteration: iteration }
    }
  });

  console.log(`[VU${vu}][iter${iteration}] Testing DENY: ${toolName}`);

  const startTime = Date.now();
  const res = retryRequest(PROXY_URL, payload);
  const duration = Date.now() - startTime;

  if (!res) {
    denyFail.add(1);
    console.error(`[VU${vu}][iter${iteration}] DENY test failed - no response`);
    return;
  }

  denyLatency.add(duration);

  // Parse response
  let body;
  try {
    body = JSON.parse(res.body);
  } catch (e) {
    denyFail.add(1);
    console.error(`[VU${vu}][iter${iteration}] DENY test failed - invalid JSON: ${res.body}`);
    return;
  }

  // Validate: should have error with code -32014
  const success = check(body, {
    'deny: has error': (b) => b.error !== undefined && b.error !== null,
    'deny: correct error code': (b) => b.error && b.error.code === GOVERNANCE_RULE_DENIED,
    'deny: no result': (b) => b.result === undefined || b.result === null,
    'deny: correct id': (b) => b.id === iteration + 1,
  });

  if (success) {
    denySuccess.add(1);
    console.log(`[VU${vu}][iter${iteration}] DENY test PASSED: ${toolName} returned -32014 (${duration}ms)`);
  } else {
    denyFail.add(1);
    console.error(`[VU${vu}][iter${iteration}] DENY test FAILED: ${toolName}`);
    console.error(`  Expected error code: ${GOVERNANCE_RULE_DENIED}`);
    console.error(`  Response: ${JSON.stringify(body)}`);
    if (body.error) {
      console.error(`  Actual error code: ${body.error.code}`);
    }
  }
}

// Summary handler for nice output
export function handleSummary(data) {
  const forwardOk = data.metrics.governance_forward_success?.values?.count || 0;
  const forwardErr = data.metrics.governance_forward_fail?.values?.count || 0;
  const denyOk = data.metrics.governance_deny_success?.values?.count || 0;
  const denyErr = data.metrics.governance_deny_fail?.values?.count || 0;

  const summary = `
================================================================================
                     ThoughtGate Governance Validation Results
================================================================================

Forward Action Tests (allowed tools → upstream):
  ✓ Passed: ${forwardOk}
  ✗ Failed: ${forwardErr}

Deny Action Tests (blocked tools → -32014 error):
  ✓ Passed: ${denyOk}
  ✗ Failed: ${denyErr}

Overall: ${forwardErr + denyErr === 0 ? '✓ ALL TESTS PASSED' : '✗ SOME TESTS FAILED'}
================================================================================
`;

  console.log(summary);

  return {
    'stdout': summary,
  };
}
