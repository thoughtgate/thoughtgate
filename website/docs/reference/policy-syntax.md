---
sidebar_position: 2
---

# Policy Syntax Reference

ThoughtGate supports two levels of policy configuration: YAML governance rules (primary) and Cedar policies (advanced).

## YAML Governance Rules

The primary way to configure ThoughtGate.

### Structure

```yaml
governance:
  defaults:
    action: forward  # Default when no rule matches
  rules:
    - match: "pattern"
      action: forward|approve|deny|policy
      approval: workflow-name  # For action: approve
      policy_id: policy-name   # For action: policy
```

### Pattern Syntax

Patterns use glob matching:

| Pattern | Matches |
|---------|---------|
| `delete_user` | Exact match |
| `delete_*` | `delete_user`, `delete_file`, etc. |
| `*_dangerous` | `very_dangerous`, `somewhat_dangerous` |
| `tool_?` | `tool_a`, `tool_1` (single char) |
| `[abc]_tool` | `a_tool`, `b_tool`, `c_tool` |

### Actions

| Action | Behavior |
|--------|----------|
| `forward` | Send to upstream immediately |
| `deny` | Return `-32003 PolicyDenied` error |
| `approve` | Create SEP-1686 task, post to Slack |
| `policy` | Evaluate Cedar policy for decision |

## Cedar Policies (Advanced)

For complex access control logic that can't be expressed in YAML.

### When to Use Cedar

- Conditional logic based on argument values
- Complex boolean expressions
- Role-based access control
- Multi-factor decisions

### Structure

```cedar
permit|forbid(
    principal,
    action == Action::"<action>",
    resource
) when {
    <conditions>
};
```

### Cedar Actions

| Action | Result |
|--------|--------|
| `Action::"Forward"` | Send to upstream immediately |
| `Action::"Approve"` | Require human approval |
| `Action::"Deny"` | Return error |

### Resource Attributes

For `tools/call` requests:

| Attribute | Type | Description |
|-----------|------|-------------|
| `resource.tool_name` | String | Name of the tool being called |
| `resource.arguments` | Object | Tool arguments (JSON) |
| `resource.method` | String | MCP method name |

### Conditions

#### String Comparisons

```cedar
when { resource.tool_name == "delete_user" }
when { resource.tool_name != "safe_tool" }
when { resource.tool_name like "admin_*" }
```

#### Numeric Comparisons

```cedar
when { resource.arguments.amount > 10000 }
when { resource.arguments.count >= 5 }
when { resource.arguments.priority < 3 }
```

#### Boolean Logic

```cedar
when {
    resource.tool_name == "transfer" &&
    resource.arguments.amount > 1000
}

when {
    resource.tool_name == "delete" ||
    resource.tool_name == "remove"
}
```

### Evaluation Order

1. All `forbid` policies evaluated first
2. If any forbid matches → Deny
3. All `permit` policies evaluated
4. First matching permit → Use that action
5. No match → Implicit deny

## Examples

### YAML: Basic Governance

```yaml
governance:
  defaults:
    action: forward
  rules:
    # Block admin tools
    - match: "admin_*"
      action: deny
    # Approve destructive ops
    - match: "delete_*"
      action: approve
    - match: "drop_*"
      action: approve
```

### YAML: Multiple Approval Channels

```yaml
governance:
  rules:
    - match: "delete_*"
      action: approve
      approval: ops
    - match: "transfer_*"
      action: approve
      approval: finance

approval:
  ops:
    adapter: slack
    channel: "#ops"
    timeout: 5m
  finance:
    adapter: slack
    channel: "#finance"
    timeout: 10m
```

### Cedar: Value-Based Routing

```cedar
// Low-value transfers: forward
permit(
    principal,
    action == Action::"Forward",
    resource
) when {
    resource.tool_name == "transfer_funds" &&
    resource.arguments.amount <= 1000
};

// High-value transfers: approve
permit(
    principal,
    action == Action::"Approve",
    resource
) when {
    resource.tool_name == "transfer_funds" &&
    resource.arguments.amount > 1000
};
```

### Cedar: Deny with Exceptions

```cedar
// Allow specific safe admin tools
permit(
    principal,
    action == Action::"Forward",
    resource
) when {
    resource.tool_name == "admin_read_config"
};

// Deny all other admin tools
forbid(
    principal,
    action,
    resource
) when {
    resource.tool_name like "admin_*"
};
```

## Validation

### YAML Validation

```bash
# Check YAML syntax
cat thoughtgate.yaml | python -c "import sys, yaml; yaml.safe_load(sys.stdin)"
```

### Cedar Validation

```bash
# Validate Cedar syntax
cedar validate --policies policy.cedar
```

## Hot Reload

ThoughtGate watches configuration files and reloads on changes. No restart required.

```bash
# Modify config file
vim thoughtgate.yaml

# ThoughtGate logs: "Configuration reloaded successfully"
```
