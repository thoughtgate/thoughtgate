---
sidebar_position: 4
---

# Security Model

ThoughtGate is a security boundary between AI agents and the tools they can access. Understanding its trust model is essential for secure deployment.

## Trust Boundaries

```
┌─────────────────────────────────────────────────────────────────────┐
│                        UNTRUSTED ZONE                                │
│                                                                     │
│  ┌─────────────┐                                                    │
│  │  AI Agent   │  • May be compromised by prompt injection          │
│  │             │  • May misinterpret instructions                   │
│  │             │  • May have bugs in reasoning                      │
│  └──────┬──────┘                                                    │
│         │                                                           │
└─────────┼───────────────────────────────────────────────────────────┘
          │
          ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      TRUST BOUNDARY                                  │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │                    THOUGHTGATE                               │   │
│  │                                                              │   │
│  │  • Enforces policies regardless of agent intent              │   │
│  │  • Cannot be instructed by agent to bypass policies          │   │
│  │  • Maintains audit trail                                     │   │
│  │  • Requires human approval for sensitive operations          │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
          │
          ▼
┌─────────────────────────────────────────────────────────────────────┐
│                        TRUSTED ZONE                                  │
│                                                                     │
│  ┌─────────────┐                                                    │
│  │  MCP Server │  • Executes tool calls                             │
│  │  (Tools)    │  • Trusts ThoughtGate's decisions                  │
│  └─────────────┘                                                    │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

## Threat Model

### Threats Mitigated

| Threat | Mitigation |
|--------|------------|
| Prompt injection | Policy enforcement, human approval |
| Agent hallucination | Same as above |
| Unauthorized actions | Governance rules deny |
| Runaway automation | Rate limiting, approval requirements |

### Threats NOT Mitigated

| Threat | Why | Recommendation |
|--------|-----|----------------|
| Compromised upstream | ThoughtGate trusts upstream responses | Secure your MCP servers |
| Malicious policies | Policies are trusted configuration | Protect config files |
| Insider threat | Approvers can approve anything | Multi-approver (planned) |
| Network eavesdropping | No built-in TLS | Use service mesh |

## Security Properties

### 1. Fail-Safe Defaults

If ThoughtGate cannot evaluate a request, it **denies** rather than allows:

- Policy parse error → Deny
- Policy evaluation error → Deny
- Unknown action → Deny
- No matching rule → Uses `defaults.action`

### 2. Policy Immutability

Policies cannot be modified by:
- AI agent requests
- Upstream responses
- Slack messages

Policies only change when:
- The config file on disk changes
- ThoughtGate restarts with a new file

### 3. Approval Independence

Approval decisions are made by humans through Slack, completely independent of:
- The AI agent making the request
- The content of the request
- Previous approval decisions

### 4. Audit Trail

All decisions are logged:
- Request details (method, tool name)
- Policy evaluation result
- Action taken
- Approval outcome (if applicable)
- Timing information

## Deployment Security

### Network Security

```yaml
# Recommended: Only expose admin port internally
spec:
  containers:
    - name: thoughtgate
      ports:
        - containerPort: 7467  # Proxy: localhost only
          name: proxy
        - containerPort: 7469  # Admin: internal network
          name: admin
```

### Secret Management

```yaml
# Use Kubernetes secrets for Slack token
env:
  - name: SLACK_BOT_TOKEN
    valueFrom:
      secretKeyRef:
        name: thoughtgate-secrets
        key: slack-token
```

### Config Protection

```yaml
# Mount config as read-only
volumeMounts:
  - name: config
    mountPath: /etc/thoughtgate
    readOnly: true
```

## Slack Security

### Bot Token Scopes

Minimize permissions:

| Scope | Required | Purpose |
|-------|----------|---------|
| `chat:write` | Yes | Post approval messages |
| `reactions:read` | Yes | Detect approval reactions |
| `channels:history` | Yes | Poll for reactions |
| `users:read` | Optional | Resolve display names |

### Channel Security

- Use a **private channel** for approvals
- Only invite trusted approvers
- Consider separate channels for different sensitivity levels

### Approval Authentication

Currently, any user who can react to messages can approve. Planned improvements:

- Approver allowlists
- Multi-approver requirements
- Time-based access windows

## Hardening Checklist

- [ ] Run as non-root user
- [ ] Use read-only root filesystem
- [ ] Drop all capabilities
- [ ] Set resource limits
- [ ] Use private Slack channel
- [ ] Rotate Slack token regularly
- [ ] Monitor approval patterns
- [ ] Alert on config load failures
- [ ] Enable structured logging
- [ ] Use TLS for all network traffic

## Example Secure Deployment

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: agent-secure
spec:
  securityContext:
    runAsNonRoot: true
    runAsUser: 1000
    fsGroup: 1000
  containers:
    - name: thoughtgate
      image: ghcr.io/thoughtgate/thoughtgate:v0.2.0
      securityContext:
        readOnlyRootFilesystem: true
        allowPrivilegeEscalation: false
        capabilities:
          drop:
            - ALL
      resources:
        limits:
          memory: "100Mi"
          cpu: "200m"
      env:
        - name: THOUGHTGATE_CONFIG
          value: "/etc/thoughtgate/config.yaml"
        - name: SLACK_BOT_TOKEN
          valueFrom:
            secretKeyRef:
              name: thoughtgate-secrets
              key: slack-token
      volumeMounts:
        - name: config
          mountPath: /etc/thoughtgate
          readOnly: true
  volumes:
    - name: config
      configMap:
        name: thoughtgate-config
```
