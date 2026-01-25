---
sidebar_position: 3
---

# Write Governance Rules

This guide shows you how to write governance rules to control which MCP tool calls require approval.

## YAML Rules (Primary)

Most use cases can be handled with simple YAML rules. Cedar policies are optional for complex logic.

### Basic Structure

```yaml
governance:
  defaults:
    action: forward  # Default action when no rule matches
  rules:
    - match: "pattern"
      action: forward|approve|deny|policy
```

### Actions

| Action | Behavior |
|--------|----------|
| `forward` | Send to upstream immediately |
| `deny` | Return error immediately |
| `approve` | Create task, post to Slack, wait for approval |
| `policy` | Evaluate Cedar policy for complex logic |

### Pattern Matching

Rules use glob patterns:

```yaml
rules:
  # Exact match
  - match: "delete_user"
    action: approve

  # Wildcard suffix
  - match: "delete_*"
    action: approve

  # Wildcard prefix
  - match: "*_dangerous"
    action: deny

  # Single character wildcard
  - match: "tool_?"
    action: forward
```

### Rule Order

Rules are evaluated top-to-bottom. First match wins.

```yaml
rules:
  # Most specific first
  - match: "admin_read_*"
    action: forward
  
  # Then broader patterns
  - match: "admin_*"
    action: deny
  
  # Catch-all (or use defaults)
  - match: "*"
    action: forward
```

## Approval Workflows

Link rules to named approval workflows:

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

## Cedar Policies (Advanced)

For complex access control logic, use Cedar policies:

```yaml
governance:
  rules:
    - match: "transfer_*"
      action: policy
      policy_id: transfer-rules

cedar:
  policy_path: /etc/thoughtgate/policies.cedar
```

### Cedar Policy Syntax

```cedar
// Forward low-value transfers
permit(
    principal,
    action == Action::"Forward",
    resource
) when {
    resource.tool_name == "transfer_funds" &&
    resource.arguments.amount <= 1000
};

// Require approval for high-value transfers
permit(
    principal,
    action == Action::"Approve",
    resource
) when {
    resource.tool_name == "transfer_funds" &&
    resource.arguments.amount > 1000
};
```

### Cedar Actions

| Cedar Action | Behavior |
|--------------|----------|
| `Action::"Forward"` | Send to upstream immediately |
| `Action::"Approve"` | Require human approval |
| `Action::"Deny"` | Reject immediately |

### Resource Attributes

For `tools/call` requests:

| Attribute | Type | Description |
|-----------|------|-------------|
| `resource.tool_name` | String | Name of the tool |
| `resource.arguments` | Object | Tool arguments (JSON) |
| `resource.method` | String | MCP method name |

## Examples

### Read-Only Safe, Write Requires Approval

```yaml
governance:
  defaults:
    action: forward
  rules:
    - match: "get_*"
      action: forward
    - match: "list_*"
      action: forward
    - match: "create_*"
      action: approve
    - match: "update_*"
      action: approve
    - match: "delete_*"
      action: approve
```

### Block Admin, Approve Destructive

```yaml
governance:
  defaults:
    action: forward
  rules:
    - match: "admin_*"
      action: deny
    - match: "delete_*"
      action: approve
    - match: "drop_*"
      action: approve
```

### Different Channels by Severity

```yaml
governance:
  defaults:
    action: forward
  rules:
    - match: "delete_*"
      action: approve
      approval: critical
    - match: "update_*"
      action: approve
      approval: standard

approval:
  critical:
    adapter: slack
    channel: "#critical-ops"
    timeout: 2m
  standard:
    adapter: slack
    channel: "#approvals"
    timeout: 10m
```

## Testing Rules

Validate your configuration:

```bash
# Check YAML syntax
cat thoughtgate.yaml | python -c "import sys, yaml; yaml.safe_load(sys.stdin)"

# Start ThoughtGate and test
export THOUGHTGATE_CONFIG=./thoughtgate.yaml
./thoughtgate &

# Test a forwarded request
curl -X POST http://localhost:7467 \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_user"},"id":1}'

# Test a denied request
curl -X POST http://localhost:7467 \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"admin_delete"},"id":2}'
```

## Next Steps

- See the full [Configuration Reference](/docs/reference/configuration)
- Understand the [4-Gate Architecture](/docs/explanation/architecture)
