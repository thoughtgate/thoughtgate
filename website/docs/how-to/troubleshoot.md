---
sidebar_position: 8
---

# Troubleshoot Common Issues

## CLI Wrapper Issues

### Config not restored after crash

**Symptom:** Your agent's MCP config still points to ThoughtGate shims after a crash.

**Check:**

```bash
# Look for the backup file
ls ~/.claude.json.thoughtgate-backup
```

**Fix:**
- Manually copy the backup over the config: `cp ~/.claude.json.thoughtgate-backup ~/.claude.json`
- ThoughtGate registers a panic hook and signal handlers to restore on exit, but an `OOM kill` or `SIGKILL` bypasses these
- Use `--no-restore` if you intentionally want to keep the rewritten config

### Lock file conflicts

**Symptom:** ThoughtGate exits with "Config already locked" error.

**Check:**

```bash
# Look for stale lock files
ls ~/.claude.json.thoughtgate-lock
```

**Fix:**
- Ensure no other ThoughtGate instance is running
- Remove the stale lock file: `rm ~/.claude.json.thoughtgate-lock`
- Lock files are advisory — they're safe to remove when no ThoughtGate process is active

### Agent not detected

**Symptom:** ThoughtGate exits with "Cannot detect agent type" error.

**Fix:**
- Use `--agent-type` to specify explicitly: `thoughtgate wrap --agent-type cursor -- /path/to/my-cursor`
- Auto-detection matches the command basename against known patterns (`claude`, `cursor`, `code`, etc.)

### Double-wrap detection

**Symptom:** ThoughtGate exits with "Config already managed by ThoughtGate" error.

**Fix:**
- This means the agent config already has ThoughtGate shim commands. Either:
  - Run the agent directly without `thoughtgate wrap`
  - Restore the original config from the `.thoughtgate-backup` file

### Shim connection failures

**Symptom:** Shim processes log "governance service unreachable" errors.

**Fix:**
- The governance service may not have started yet — shims use exponential backoff to retry
- Check for port conflicts: use `--governance-port` to specify a fixed port
- Check ThoughtGate's stderr for governance service startup errors

### WOULD_BLOCK but nothing actually blocked

**Symptom:** You see `WOULD_BLOCK` in logs but all requests go through.

**Fix:**
- This is expected in **development mode** (`--profile development`). Switch to production to enforce blocking:
  ```bash
  thoughtgate wrap --profile production -- claude-code
  ```

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
echo $THOUGHTGATE_SLACK_BOT_TOKEN | head -c 10

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

**Fix (HTTP sidecar):**
```bash
# Verify THOUGHTGATE_CONFIG is set
echo $THOUGHTGATE_CONFIG
ls -la $THOUGHTGATE_CONFIG
```

**Fix (CLI wrapper):**
```bash
# Default: thoughtgate.yaml in working directory
ls -la thoughtgate.yaml

# Or specify explicitly
thoughtgate wrap --thoughtgate-config /path/to/config.yaml -- claude-code
```

### All requests denied

**Symptom:** Every request returns "denied" or policy error.

**Check:**

```bash
# Validate YAML syntax
python -c "import sys, yaml; yaml.safe_load(open('thoughtgate.yaml'))"

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
- In-memory state is lost on pod restart

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
