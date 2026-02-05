---
sidebar_position: 5
---

# Error Codes Reference

ThoughtGate returns JSON-RPC 2.0 compliant errors. Every error includes structured `data` with a correlation ID for debugging.

## Standard JSON-RPC Errors

| Code | Name | Description |
|------|------|-------------|
| `-32700` | Parse Error | Invalid JSON |
| `-32600` | Invalid Request | Not a valid JSON-RPC request |
| `-32601` | Method Not Found | Unknown method |
| `-32602` | Invalid Params | Invalid method parameters |
| `-32603` | Internal Error | Internal server error |

## ThoughtGate-Specific Errors

### Upstream Errors (-32000 to -32002)

| Code | Name | Description |
|------|------|-------------|
| `-32000` | Upstream Connection Failed | Cannot connect to upstream MCP server |
| `-32001` | Upstream Timeout | Upstream request timed out |
| `-32002` | Upstream Error | Upstream returned an error |

### Gate 3: Cedar Policy (-32003)

| Code | Name | Description |
|------|------|-------------|
| `-32003` | Policy Denied | Cedar policy denied the request |

### Task Errors (-32005, -32006, -32020)

| Code | Name | Description |
|------|------|-------------|
| `-32005` | Task Expired | Task exceeded its TTL |
| `-32006` | Task Cancelled | Task was cancelled |
| `-32020` | Task Result Not Ready | Task result is still pending |

### Gate 4: Approval Errors (-32007, -32008, -32017)

| Code | Name | Description |
|------|------|-------------|
| `-32007` | Approval Rejected | Human reviewer rejected the request |
| `-32008` | Approval Timeout | Approval window expired without a decision |
| `-32017` | Workflow Not Found | Approval workflow name not found in config |

### Rate Limiting (-32009)

| Code | Name | Description |
|------|------|-------------|
| `-32009` | Rate Limited | Too many requests |

### Pipeline Errors (-32010 to -32012)

| Code | Name | Description |
|------|------|-------------|
| `-32010` | Inspection Failed | An inspector rejected the request (v0.3+) |
| `-32011` | Policy Drift | Policy changed between approval and execution (v0.3+) |
| `-32012` | Transform Drift | Request context changed during approval (v0.3+) |

### Service Errors (-32013)

| Code | Name | Description |
|------|------|-------------|
| `-32013` | Service Unavailable | ThoughtGate not ready to serve traffic |

### Gate 2: Governance Rule (-32014)

| Code | Name | Description |
|------|------|-------------|
| `-32014` | Governance Rule Denied | YAML governance rule matched with `action: deny` |

### Gate 1: Visibility (-32015)

| Code | Name | Description |
|------|------|-------------|
| `-32015` | Tool Not Exposed | Tool is hidden by the source's exposure configuration |

### Configuration (-32016)

| Code | Name | Description |
|------|------|-------------|
| `-32016` | Configuration Error | Configuration is invalid or cannot be loaded |

## Error by Gate

Each gate in the 4-gate pipeline produces specific errors:

| Gate | Error Codes | Description |
|------|-------------|-------------|
| **Gate 1: Visibility** | `-32015` | Tool not in the allowlist or in the blocklist |
| **Gate 2: Governance** | `-32014` | YAML rule matched with `action: deny` |
| **Gate 3: Cedar Policy** | `-32003` | Cedar policy evaluation returned deny |
| **Gate 4: Approval** | `-32007`, `-32008`, `-32017` | Approval rejected, timed out, or workflow missing |

## Error Response Format

All errors include a structured `data` field:

```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": -32003,
    "message": "Policy denied access to tool 'admin_console'",
    "data": {
      "correlation_id": "550e8400-e29b-41d4-a716-446655440000",
      "gate": "policy",
      "tool": "admin_console",
      "details": null,
      "error_type": "policy_denied",
      "retry_after": null
    }
  },
  "id": 1
}
```

### `data` Fields

| Field | Type | Description |
|-------|------|-------------|
| `correlation_id` | string | Unique ID for log correlation. Always present. |
| `gate` | string \| null | Which gate rejected: `"visibility"`, `"governance"`, `"policy"`, `"approval"`. Null for non-gate errors. |
| `tool` | string \| null | Tool name that was denied, if applicable. |
| `details` | string \| null | Safe details for the client. Sensitive information is never exposed. |
| `error_type` | string | Machine-readable error classification (e.g., `"policy_denied"`, `"upstream_timeout"`). |
| `retry_after` | number \| null | Seconds to wait before retrying (only set for `-32009 RateLimited`). |

:::info Security
Gate errors (`-32003`, `-32014`, `-32015`) intentionally omit `details` to avoid exposing policy internals, governance rule patterns, or visibility configuration to the agent.
:::

## Handling Errors

### Retry-able Errors

These errors may succeed on retry:

- `-32001` Upstream Timeout — upstream may recover
- `-32005` Task Expired — resubmit the request
- `-32008` Approval Timeout — resubmit for a new approval
- `-32009` Rate Limited — retry after the `retry_after` delay
- `-32013` Service Unavailable — ThoughtGate is starting up
- `-32020` Task Result Not Ready — continue polling

### Non-Retry-able Errors

These errors will not succeed on retry without changes:

- `-32003` Policy Denied — policy must change
- `-32007` Approval Rejected — human decided to reject
- `-32014` Governance Rule Denied — rule must change
- `-32015` Tool Not Exposed — exposure config must change
- `-32016` Configuration Error — fix the config file
- `-32017` Workflow Not Found — add the missing workflow to config

## Example Error Handling

```python
import json

def handle_response(response):
    data = json.loads(response)

    if "error" in data:
        error = data["error"]
        code = error["code"]
        message = error["message"]
        error_data = error.get("data", {})

        # Log correlation ID for debugging
        print(f"Correlation: {error_data.get('correlation_id', 'unknown')}")

        if code in (-32003, -32014, -32015):
            print(f"Access denied (gate: {error_data.get('gate')}): {message}")
        elif code == -32007:
            print(f"Approval rejected: {message}")
        elif code == -32008:
            print(f"Approval timed out: {message}")
        elif code == -32009:
            retry_after = error_data.get("retry_after", 60)
            print(f"Rate limited, retry after {retry_after}s")
        elif code in (-32001, -32013):
            print(f"Temporary error, retrying: {message}")
        else:
            print(f"Error {code}: {message}")
    else:
        return data["result"]
```
