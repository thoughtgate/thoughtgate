import http from 'k6/http';
import { check, sleep } from 'k6';
import { Counter } from 'k6/metrics';

// Custom metrics for governance testing
const integrityFailures = new Counter('integrity_failures');
const integrityChecks = new Counter('integrity_checks');

export const options = {
  vus: 1,         // Single VU for correctness testing (not performance)
  iterations: 5,  // 5 verification runs (reduced for faster CI)
  thresholds: {
    'integrity_failures': ['count==0'],  // MUST be zero for test to pass
    'integrity_checks': ['count>=5'],    // At least 5 checks must run
  },
};

// Retry helper for initial connection attempts
function retryRequest(url, payload, maxRetries = 5) {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      const res = http.post(url, payload, {
        headers: { 'Content-Type': 'application/json' },
        timeout: '5s',
      });
      
      // If we get connection refused or 503, retry
      if (res.status === 0 || res.status === 503) {
        console.log(`Attempt ${attempt}/${maxRetries}: Service not ready (status: ${res.status}), retrying...`);
        sleep(Math.pow(2, attempt - 1)); // Exponential backoff: 1s, 2s, 4s, 8s, 16s
        continue;
      }
      
      return res;
    } catch (e) {
      console.log(`Attempt ${attempt}/${maxRetries}: Request failed (${e}), retrying...`);
      sleep(Math.pow(2, attempt - 1));
    }
  }
  
  throw new Error('Max retries exceeded - service not available');
}

export default function () {
  // Generate unique test ID for this request
  const testId = `${Date.now()}-${__VU}-${__ITER}`;
  
  // Create complex payload with nested structure to test data integrity
  const payload = JSON.stringify({
    test_id: testId,
    model: 'gpt-4',
    messages: [
      {
        role: 'system',
        content: 'You are a helpful assistant.',
      },
      {
        role: 'user',
        content: 'Test message with special chars: "quotes", \'apostrophes\', and unicode: 🚀 emoji',
      },
    ],
    temperature: 0.7,
    max_tokens: 100,
    metadata: {
      nested: {
        deeply: {
          value: 'test-data',
          array: [1, 2, 3, 'four', null, true, false],
        },
      },
    },
  });

  console.log(`[${testId}] Sending request through proxy...`);

  // 1. Send request through the proxy
  const proxyRes = retryRequest('http://127.0.0.1:4141/v1/chat/completions', payload);
  
  check(proxyRes, {
    'proxy accepted request': (r) => r.status === 200 || r.status === 0, // 0 means streaming started
  });

  // 2. Allow time for async processing
  sleep(0.5);

  // 3. Query the spy endpoint to retrieve captured requests
  console.log(`[${testId}] Querying spy endpoint for captured requests...`);
  const historyRes = http.get('http://mock-llm:8080/_admin/history', {
    timeout: '5s',
  });

  if (!check(historyRes, {
    'spy endpoint reachable': (r) => r.status === 200,
    'spy returned JSON': (r) => r.headers['Content-Type'] && r.headers['Content-Type'].includes('application/json'),
  })) {
    console.error(`[${testId}] Failed to reach spy endpoint or invalid response`);
    integrityFailures.add(1);
    return;
  }

  // 4. Parse captured requests
  let capturedRequests;
  try {
    capturedRequests = JSON.parse(historyRes.body);
  } catch (e) {
    console.error(`[${testId}] Failed to parse spy response: ${e}`);
    integrityFailures.add(1);
    return;
  }

  if (!Array.isArray(capturedRequests)) {
    console.error(`[${testId}] Spy response is not an array`);
    integrityFailures.add(1);
    return;
  }

  console.log(`[${testId}] Spy captured ${capturedRequests.length} total requests`);

  // 5. Find our request by test_id
  const originalPayload = JSON.parse(payload);
  const matchingRequest = capturedRequests.find(req => req.test_id === testId);

  if (!matchingRequest) {
    console.error(`[${testId}] Request not found in captured history!`);
    integrityFailures.add(1);
    return;
  }

  console.log(`[${testId}] Found matching request in history`);

  // 6. Deep comparison: Verify data integrity
  integrityChecks.add(1);
  
  const integrityCheck = check(matchingRequest, {
    'test_id matches': (req) => req.test_id === originalPayload.test_id,
    'model matches': (req) => req.model === originalPayload.model,
    'messages count matches': (req) => req.messages && req.messages.length === originalPayload.messages.length,
    'first message role matches': (req) => req.messages && req.messages[0].role === originalPayload.messages[0].role,
    'first message content matches': (req) => req.messages && req.messages[0].content === originalPayload.messages[0].content,
    'second message role matches': (req) => req.messages && req.messages[1].role === originalPayload.messages[1].role,
    'second message content matches': (req) => req.messages && req.messages[1].content === originalPayload.messages[1].content,
    'temperature matches': (req) => req.temperature === originalPayload.temperature,
    'max_tokens matches': (req) => req.max_tokens === originalPayload.max_tokens,
    'nested metadata preserved': (req) => req.metadata && req.metadata.nested && req.metadata.nested.deeply && req.metadata.nested.deeply.value === 'test-data',
    'array in metadata preserved': (req) => req.metadata && req.metadata.nested && req.metadata.nested.deeply && Array.isArray(req.metadata.nested.deeply.array) && req.metadata.nested.deeply.array.length === 7,
  });

  if (!integrityCheck) {
    console.error(`[${testId}] ❌ INTEGRITY CHECK FAILED - Data was corrupted during proxying!`);
    console.error(`Original: ${JSON.stringify(originalPayload, null, 2)}`);
    console.error(`Captured: ${JSON.stringify(matchingRequest, null, 2)}`);
    integrityFailures.add(1);
  } else {
    console.log(`[${testId}] ✅ Integrity check passed - data forwarded without corruption`);
  }
}

