---
sidebar_position: 4
---

# Deploy to Kubernetes

ThoughtGate is designed to run as a sidecar container alongside your AI agent pods.

## Sidecar Deployment

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: my-agent
  labels:
    app: my-agent  # Used for principal identity
spec:
  containers:
    # Your AI agent
    - name: agent
      image: my-agent:latest
      env:
        - name: MCP_SERVER_URL
          value: "http://localhost:7467"  # Points to ThoughtGate

    # ThoughtGate sidecar
    - name: thoughtgate
      image: ghcr.io/thoughtgate/thoughtgate:v0.2.2
      ports:
        - containerPort: 7467
          name: proxy
        - containerPort: 7469
          name: admin
      env:
        - name: THOUGHTGATE_CONFIG
          value: "/etc/thoughtgate/config.yaml"
        - name: THOUGHTGATE_SLACK_BOT_TOKEN
          valueFrom:
            secretKeyRef:
              name: thoughtgate-secrets
              key: slack-token
      volumeMounts:
        - name: config
          mountPath: /etc/thoughtgate
          readOnly: true
      livenessProbe:
        httpGet:
          path: /health
          port: admin
        initialDelaySeconds: 5
        periodSeconds: 10
      readinessProbe:
        httpGet:
          path: /ready
          port: admin
        initialDelaySeconds: 5
        periodSeconds: 5
      resources:
        requests:
          memory: "20Mi"
          cpu: "50m"
        limits:
          memory: "100Mi"
          cpu: "200m"
  volumes:
    - name: config
      configMap:
        name: thoughtgate-config
```

## Create the Secret

```bash
kubectl create secret generic thoughtgate-secrets \
  --from-literal=slack-token=xoxb-your-token
```

## ConfigMap for Configuration

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: thoughtgate-config
data:
  config.yaml: |
    schema: 1

    sources:
      - id: upstream
        kind: mcp
        url: http://mcp-server:3000

    governance:
      defaults:
        action: forward
      rules:
        - match: "delete_*"
          action: approve
        - match: "drop_*"
          action: approve
        - match: "admin_*"
          action: deny

    approval:
      default:
        adapter: slack
        channel: "#approvals"
        timeout: 5m
```

## Port Model

ThoughtGate uses an Envoy-inspired 3-port architecture:

| Port | Name | Purpose |
|------|------|---------|
| 7467 | Outbound | Client requests → upstream (main proxy) |
| 7468 | Inbound | Reserved for webhooks (v0.3+) |
| 7469 | Admin | Health checks, metrics |

## Health Checks

ThoughtGate exposes health endpoints on the admin port:

| Endpoint | Purpose | Use Case |
|----------|---------|----------|
| `/health` | Liveness | Process is running |
| `/ready` | Readiness | Ready to accept traffic |
| `/metrics` | Prometheus | Observability |

## Resource Recommendations

| Workload | Memory Request | Memory Limit | CPU Request | CPU Limit |
|----------|----------------|--------------|-------------|-----------|
| Light | 20Mi | 50Mi | 25m | 100m |
| Medium | 50Mi | 100Mi | 50m | 200m |
| Heavy | 100Mi | 200Mi | 100m | 500m |

## Monitoring

Expose Prometheus metrics:

```yaml
metadata:
  annotations:
    prometheus.io/scrape: "true"
    prometheus.io/port: "7469"
    prometheus.io/path: "/metrics"
```

## Zero-Config Identity

ThoughtGate automatically infers agent identity from Kubernetes pod labels using the [Downward API](https://kubernetes.io/docs/concepts/workloads/pods/downward-api/). No API keys or external identity providers required.

### How It Works

1. **Pod labels** (like `app: my-agent`) are exposed to ThoughtGate via a downwardAPI volume
2. ThoughtGate reads labels at startup and uses them as the **principal** in Cedar policy evaluation
3. Policies can then grant different permissions to different agents

### Configuration

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: my-agent
  labels:
    app: my-agent           # Used as principal name
    team: data-engineering  # Can be used in policies
spec:
  containers:
    - name: thoughtgate
      image: ghcr.io/thoughtgate/thoughtgate:v0.2.2
      env:
        - name: THOUGHTGATE_CONFIG
          value: "/etc/thoughtgate/config.yaml"
        - name: POD_NAME
          valueFrom:
            fieldRef:
              fieldPath: metadata.name
        - name: POD_NAMESPACE
          valueFrom:
            fieldRef:
              fieldPath: metadata.namespace
      volumeMounts:
        - name: config
          mountPath: /etc/thoughtgate
        - name: podinfo
          mountPath: /etc/podinfo
          readOnly: true
  volumes:
    - name: podinfo
      downwardAPI:
        items:
          - path: "labels"
            fieldRef:
              fieldPath: metadata.labels
```

### Using Identity in Cedar Policies

The principal is constructed from pod labels:

```cedar
// Allow data-engineering team to use transfer tools
permit(
    principal == Agent::"data-engineering/my-agent",
    action == Action::"Forward",
    resource
) when {
    resource.tool_name like "transfer_*"
};

// Require approval for all other agents
permit(
    principal,
    action == Action::"Approve",
    resource
) when {
    resource.tool_name like "transfer_*"
};
```

### Benefits

- **No secrets to manage** — Identity comes from K8s metadata
- **Tamper-proof** — Pods can't change their own labels
- **Audit-friendly** — Identity tied to K8s RBAC
- **Multi-tenant** — Different policies per agent/team

## Next Steps

- Review the [Configuration Reference](/docs/reference/configuration)
- Learn about the [4-Gate architecture](/docs/explanation/architecture)
