---
sidebar_position: 5
---

# Troubleshoot Common Issues

## Connection Issues

### "Connection refused" to upstream

**Symptom:** Requests fail with upstream connection errors.

**Check:**

```bash
# Is the upstream reachable?
curl http://your-upstream:3000/health

# Check ThoughtGate logs
docker logs thoughtgate 2>&1 | grep -i upstream
```

**Fix:**
- Verify the `url` in your config's `sources` section is correct
- In Kubernetes, ensure the service DNS is resolvable
- Check network policies aren't blocking traffic

### Requests timeout

**Symptom:** Requests hang and eventually timeout.

**Check:**

```bash
# Check upstream latency
time curl http://your-upstream:3000/health
```

**Fix:**
- Check upstream server performance
- For approval requests, check Slack connectivity

## Slack Integration

### Approval messages not appearing

**Symptom:** Requests return task IDs but no Slack message appears.

**Check:**

```bash
# Verify token is set
echo $SLACK_BOT_TOKEN | head -c 10

# Check ThoughtGate logs for Slack errors
docker logs thoughtgate 2>&1 | grep -i slack
```

**Fix:**
- Verify the bot token has required scopes: `chat:write`, `reactions:read`, `channels:history`, `users:read`
- Ensure the bot is invited to the channel: `/invite @YourBot`
- Check the channel name includes `#`

### Reactions not detected

**Symptom:** Messages appear but 👍/👎 reactions are ignored.

**Check:**
- Verify `reactions:read` scope is granted
- Check the bot can read channel history (`channels:history`)

**Fix:**
- Re-add the bot to the channel
- Verify the reaction is on the correct message (not a thread reply)

## Configuration Issues

### "Config file not found"

**Symptom:** ThoughtGate fails to start with config error.

**Check:**

```bash
# Verify THOUGHTGATE_CONFIG is set
echo $THOUGHTGATE_CONFIG

# Verify file exists
ls -la $THOUGHTGATE_CONFIG
```

**Fix:**
- Set the `THOUGHTGATE_CONFIG` environment variable
- Ensure the file path is absolute or relative to working directory

### All requests denied

**Symptom:** Every request returns "denied" or policy error.

**Check:**

```bash
# Validate YAML syntax
cat $THOUGHTGATE_CONFIG | python -c "import sys, yaml; yaml.safe_load(sys.stdin)"

# Check ThoughtGate logs
docker logs thoughtgate 2>&1 | grep -i policy
```

**Fix:**
- Ensure you have `governance.defaults.action: forward` if you want default allow
- Check for YAML syntax errors
- Verify glob patterns in rules match your tool names

## Performance Issues

### High latency

**Symptom:** Proxy adds significant latency.

**Check:**

```bash
# Measure overhead
time curl -X POST http://localhost:7467 -d '{"jsonrpc":"2.0","method":"tools/list","id":1}'
```

**Fix:**
- Check if upstream is slow
- Ensure adequate CPU resources
- Review if complex Cedar policies are enabled

### Memory growing

**Symptom:** Memory usage increases over time.

**Check:**

```bash
# Monitor memory
docker stats thoughtgate
```

**Fix:**
- Check for stale pending tasks (they timeout automatically)
- Report if consistently growing (may be a bug)

## Health Check Failures

### Liveness probe failing

**Symptom:** Kubernetes keeps restarting the pod.

**Check:**

```bash
curl http://localhost:7469/health
```

**Fix:**
- Increase `initialDelaySeconds`
- Check if the process is actually crashing (logs)

### Readiness probe failing

**Symptom:** Pod not receiving traffic.

**Check:**

```bash
curl http://localhost:7469/ready
```

**Fix:**
- Verify upstream is reachable
- Check config file is loaded correctly

## SEP-1686 Task Issues

### Task not found

**Symptom:** `tasks/get` returns "task not found" error.

**Check:**
- Verify the task ID format starts with `tg_`
- Check if the task has expired

**Fix:**
- Tasks expire after the configured timeout (default 5 minutes)
- In-memory state is lost on pod restart (v0.2 limitation)

### Task stuck in "working" state

**Symptom:** Task never transitions to completed/failed.

**Check:**
- Verify Slack message was posted
- Check if reactions are being detected

**Fix:**
- Ensure `channels:history` scope is granted
- Check Slack polling is working (see logs)

## Getting Help

If you're still stuck:

1. Check the [GitHub Issues](https://github.com/thoughtgate/thoughtgate/issues)
2. Open a new issue with:
   - ThoughtGate version
   - Configuration (redact secrets)
   - Relevant logs
   - Steps to reproduce
