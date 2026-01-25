---
sidebar_position: 3
---

# Traffic Routing

ThoughtGate routes all traffic based on governance rules and policy evaluation. Each request is classified and handled according to its action.

## Overview

| Action | Behavior | Latency |
|--------|----------|---------|
| **forward** | Send to upstream immediately | < 2 ms |
| **deny** | Return error immediately | < 1 ms |
| **approve** | Create task, wait for human | Variable |
| **policy** | Evaluate Cedar, then route | < 5 ms |

## Forward Action

**Purpose:** Fast path for trusted, low-risk operations.

### Characteristics

- Minimal processing
- Direct forwarding to upstream
- Responses passed through unchanged

### Typical Use Cases

- `tools/list` — Listing available tools
- Read-only queries
- Idempotent operations
- Internal/trusted tool calls

### Configuration

```yaml
governance:
  defaults:
    action: forward
  rules:
    - match: "get_*"
      action: forward
    - match: "list_*"
      action: forward
```

## Deny Action

**Purpose:** Block dangerous or unauthorized operations.

### Characteristics

- Immediate rejection
- No upstream communication
- Clear error response (`-32003 PolicyDenied`)

### Typical Use Cases

- Administrative tools
- Known attack patterns
- Blocked tool categories

### Configuration

```yaml
governance:
  rules:
    - match: "admin_*"
      action: deny
    - match: "debug_*"
      action: deny
```

## Approve Action

**Purpose:** Human-in-the-loop for sensitive operations.

### Characteristics

- Creates SEP-1686 task
- Posts message to Slack
- Agent receives task ID immediately
- Agent polls for completion
- On approval: forwards to upstream
- On rejection: returns error

### Typical Use Cases

- Destructive operations (delete, drop, remove)
- Financial transactions
- PII modifications
- Privilege escalation

### Configuration

```yaml
governance:
  rules:
    - match: "delete_*"
      action: approve
      approval: ops-team
    - match: "transfer_*"
      action: approve
      approval: finance-team

approval:
  ops-team:
    adapter: slack
    channel: "#ops-approvals"
    timeout: 5m
  finance-team:
    adapter: slack
    channel: "#finance-approvals"
    timeout: 10m
```

### SEP-1686 Task Flow

1. Agent calls `tools/call` with sensitive tool
2. ThoughtGate returns: `{taskId: "tg_abc123", status: "working"}`
3. Slack message posted
4. Agent polls `tasks/get` periodically
5. Human reacts 👍 or 👎
6. Task status changes to `completed` or `rejected`
7. Agent calls `tasks/result` to get the response

## Policy Action

**Purpose:** Complex access control logic using Cedar.

### Characteristics

- Evaluates Cedar policy
- Can check argument values
- Returns forward, approve, or deny
- Slightly higher latency

### Typical Use Cases

- Conditional approvals (e.g., amount > $10,000)
- Role-based access control
- Complex business rules

### Configuration

```yaml
governance:
  rules:
    - match: "transfer_*"
      action: policy
      policy_id: transfer-rules

cedar:
  policy_path: /etc/thoughtgate/policies.cedar
```

```cedar
// Low-value: forward immediately
permit(
    principal,
    action == Action::"Forward",
    resource
) when {
    resource.tool_name == "transfer_funds" &&
    resource.arguments.amount <= 1000
};

// High-value: require approval
permit(
    principal,
    action == Action::"Approve",
    resource
) when {
    resource.tool_name == "transfer_funds" &&
    resource.arguments.amount > 1000
};
```

## Action Selection Logic

```
┌─────────────────────────────────────────────────────────┐
│                  GOVERNANCE EVALUATION                   │
│                                                         │
│  1. Check Gate 1 (Visibility) - tool allowed?           │
│  2. Evaluate rules in order (first match wins)          │
│  3. If action: policy, evaluate Cedar                   │
│  4. If no match, use defaults.action                    │
│                                                         │
│  Results:                                               │
│    forward  → Send to upstream                          │
│    deny     → Return -32003 error                       │
│    approve  → Create task, post to Slack                │
│    policy   → Cedar evaluation → forward|deny|approve   │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

## Performance Implications

| Action | p50 Latency | p99 Latency | Notes |
|--------|-------------|-------------|-------|
| forward | 1-2 ms | 5 ms | Network bound |
| deny | < 1 ms | 2 ms | No upstream call |
| approve (initial) | 3-5 ms | 15 ms | Task + Slack |
| approve (total) | 10s-5m | Timeout | Human bound |
| policy | 2-3 ms | 10 ms | Cedar evaluation |

## Monitoring

Prometheus metrics by action:

```
thoughtgate_requests_total{action="forward"}
thoughtgate_requests_total{action="deny"}
thoughtgate_requests_total{action="approve"}

thoughtgate_request_duration_seconds{action="forward"}
thoughtgate_request_duration_seconds{action="deny"}
thoughtgate_request_duration_seconds{action="approve"}

thoughtgate_approval_total{result="approved|rejected|timeout"}
```

## Choosing the Right Action

### Use Forward When

- Operation is read-only
- Failure is easily recoverable
- No sensitive data involved
- High-frequency operation

### Use Deny When

- Operation should never be allowed
- Clear policy violation
- Known attack pattern

### Use Approve When

- Operation is legitimate but sensitive
- Human judgment required
- Irreversible consequences
- High-value transactions

### Use Policy When

- Need conditional logic
- Different handling based on arguments
- Complex business rules
- Role-based decisions
