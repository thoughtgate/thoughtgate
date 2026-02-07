# ThoughtGate Deep Code Review — Prioritized Recommendations

**Date:** 2026-02-07
**Scope:** Full codebase (~39K LOC source, ~6.9K LOC tests)
**Approach:** 7 parallel focused reviews covering: task state machine, security/secrets, proxy MCP handler, Cedar policy engine, governance evaluator, error handling/safety, CLI shim & config adapter

---

## Summary

| Severity | Count | Status |
|----------|-------|--------|
| **CRITICAL** | 5 | Immediate action required |
| **HIGH** | 13 | Fix before next release |
| **MEDIUM** | 19 | Track and address |
| **LOW** | 14 | Improvement opportunities |

---

## CRITICAL (P0 — Fix Immediately)

### C-1: Deadlock in `TaskStore::create()` — cross-DashMap lock ordering violation

**File:** `thoughtgate-core/src/governance/task.rs:1047-1103`

`create()` holds a shard write lock on `pending_by_principal` (via `DashMap::entry().or_insert_with()`) while calling `self.tasks.insert()`, which acquires a shard lock on `tasks`. Meanwhile, `transition()` holds a shard lock on `tasks` via `get_mut()`, then calls `decrement_pending_counters()` which acquires a read lock on `pending_by_principal`. This is a classic AB/BA lock ordering violation. Because DashMap uses sharding, deadlock is probabilistic — it surfaces under production load but not in unit tests.

**Fix:** Drop the `principal_counter` `RefMut` immediately after the rate-limit check, before accessing any other DashMap.

---

### C-2: `SlackConfig` derives `Debug`, exposing `bot_token` in any debug output

**File:** `thoughtgate-core/src/governance/approval/slack.rs:40-43`

```rust
#[derive(Debug, Clone)]  // <-- Debug will print bot_token in plaintext
pub struct SlackConfig {
    bot_token: String,  // Comment says "NEVER log this value"
```

Any `{:?}` formatting of `SlackConfig` (in logs, error chains, tracing spans) leaks the Slack bot token. Compare with `ApprovalRequest` which correctly provides a manual `Debug` impl with `[REDACTED]`.

**Fix:** Replace `#[derive(Debug)]` with a manual `Debug` impl that redacts `bot_token`.

---

### C-3: Development mode auto-forwards Cedar `Forbid` decisions

**File:** `thoughtgate-core/src/governance/evaluator.rs:498-504`

When `Profile::Development` is active, Cedar `Forbid` decisions are silently overridden to `Forward`. If a deployment misconfigures the profile, all Cedar policies become decorative. While a lifecycle guard exists at startup, the `Profile` can be set independently via config.

**Fix:** Add defense-in-depth: emit a metrics counter when Forbid is overridden. Consider requiring both `THOUGHTGATE_DEV_MODE=true` AND `THOUGHTGATE_ENVIRONMENT=development` in the evaluator itself.

---

### C-4: Embedded default policies permit ALL actions — I/O failure degrades silently

**Files:** `thoughtgate-core/src/policy/defaults.cedar:17-28`, `thoughtgate-core/src/policy/loader.rs:33-52`

The embedded fallback policies are fully permissive (`permit(principal, action, resource)`). If a configured policy file exists but is unreadable (permission error, disk failure), the loader falls through to these defaults. A transient I/O error turns a locked-down production system into a fully open one.

**Fix:** In production profile, return an error if the configured policy file exists but cannot be read. Gate fallback-to-embedded-policies behind `DEV_MODE` or `allow_embedded_policies` flag.

---

### C-5: Principal identity truncation causes task access failures

**Files:** `thoughtgate-proxy/src/mcp_handler.rs:1444`, `thoughtgate-core/src/governance/task.rs:59`

`verify_task_principal` constructs `Principal::new(&p.app_name)` which sets namespace/service_account/roles to defaults. But task creation may use richer identity via `Principal::from_policy(...)`. The derived `PartialEq` compares ALL fields, so tasks are inaccessible to their rightful owners when identity sources differ.

**Fix:** Implement a custom `.is_same_identity()` method comparing only authorization-relevant fields (e.g., `app_name` + `namespace`), or construct the full principal in `verify_task_principal`.

---

## HIGH (P1 — Fix Before Next Release)

### H-1: Counter leak when `Task::new()` fails

**File:** `thoughtgate-core/src/governance/task.rs:1028-1074`

Both `pending_count` and per-principal counters are incremented before `Task::new()`. If `Task::new()` returns `Err` (e.g., `MAX_CANONICAL_JSON_DEPTH` exceeded), counters are never rolled back. Over time this permanently blocks task creation.

**Fix:** Add counter rollback in the error path of `Task::new()`.

---

### H-2: `record_auto_approve_result()` bypasses state machine validation

**File:** `thoughtgate-core/src/governance/task.rs:1282-1305`

This method modifies task result/approval/transitions without checking current status. It hardcodes `from: TaskStatus::Expired, to: TaskStatus::Expired` regardless of actual state.

**Fix:** Add a status guard that rejects calls on non-expired tasks.

---

### H-3: `TaskError` has no JSON-RPC error code mapping

**File:** `thoughtgate-core/src/governance/task.rs:729-805`

The `TaskError` enum defines 8 variants but has no `to_jsonrpc_code()` method and no `From<TaskError> for ThoughtGateError` impl. Most variants collapse into a generic `Internal` error.

**Fix:** Add direct error code mappings (e.g., `NotFound→-32602`, `Expired→-32005`, `RateLimited→-32009`).

---

### H-4: No authorization check on Slack approval reactions

**File:** `thoughtgate-core/src/governance/approval/slack.rs:325-360`

Any Slack user who can view the channel and add a reaction can approve/reject critical operations. No approver allowlist, no RBAC, no four-eyes principle. Bots with channel access can auto-approve everything.

**Fix:** Implement an approver allowlist (by Slack user group or specific user IDs). Reject unauthorized reactions.

---

### H-5: `tool_arguments` sent unredacted to Slack messages

**File:** `thoughtgate-core/src/governance/approval/slack.rs:192-222`

CLAUDE.md says "NEVER log tool_arguments". While `Debug` impl correctly redacts them, `build_approval_blocks` sends full arguments to Slack with no size limit or sensitivity filtering. Could contain API keys, passwords, PII.

**Fix:** Implement argument redaction/allowlist. At minimum, truncate to ~3000 chars. Consider secure viewer link.

---

### H-6: `.expect()` calls in scheduler runtime code

**File:** `thoughtgate-core/src/governance/approval/scheduler.rs:50-64`

Four `.expect()` calls in `quota_from_rate()` which runs during request handling. Violates stated safety rules.

**Fix:** Replace with `const` values for `NonZeroU32::new(1)` and fallback handling for `Quota::with_period`.

---

### H-7: No per-request timeout on MCP handler path

**File:** `thoughtgate-proxy/src/mcp_handler.rs`

The MCP buffered request handler has no per-request timeout. Slow upstream calls, Cedar evaluations, or approval flows can block indefinitely. The connection-level timeout is too coarse for multiplexed HTTP/2.

**Fix:** Wrap `route_request()` or key sub-calls in `tokio::time::timeout()` with configurable per-request timeout.

---

### H-8: Non-atomic multi-ArcSwap policy reload creates inconsistency window

**File:** `thoughtgate-core/src/policy/engine.rs:952-960`

`policies`, `annotations`, and `source` are stored in separate `ArcSwap` instances. Between the first and last `store`, concurrent evaluations see mismatched data (new policies, old annotations), causing wrong approval workflow routing.

**Fix:** Bundle into a single `ArcSwap<PolicyBundle>` for atomic swap.

---

### H-9: No policy count validation on reload

**File:** `thoughtgate-core/src/policy/engine.rs:952-954`

A reload can succeed with zero policies (causing total denial-of-service) or with a truncated file (losing critical permit policies).

**Fix:** Add configurable minimum policy count check. Warn/reject if new set has significantly fewer policies.

---

### H-10: HOSTNAME-based principal identity is spoofable

**File:** `thoughtgate-core/src/policy/principal.rs:67-69`

Cedar principal ID comes from `HOSTNAME` env var. Outside K8s, this is trivially controllable. Even in K8s, `spec.hostname` can override it. If policies match on principal name, impersonation is possible.

**Fix:** In production, prefer K8s downward API fields. Document that HOSTNAME identity is only trustworthy with proper K8s RBAC.

---

### H-11: Pipeline policy re-evaluation hard-codes `server: "default"`

**File:** `thoughtgate-core/src/governance/pipeline.rs:887-889`

Post-approval Cedar re-evaluation uses `server: "default"` instead of the real server ID. Server-specific policies produce different results, causing false/missed drift detection.

**Fix:** Store `server_id` on the `Task` struct and pass through to `build_policy_request`.

---

### H-12: Non-atomic config file writes risk corruption

**File:** `thoughtgate/src/wrap/config_adapter.rs:325-332`

`std::fs::write()` is not atomic. Crash/SIGKILL mid-write corrupts real user config files (Claude Desktop, VS Code, Cursor, Windsurf, Zed).

**Fix:** Use write-to-temp-then-rename pattern. On Unix, `rename()` is atomic within the same filesystem.

---

### H-13: `ServiceUnavailable` leaks internal implementation details to clients

**File:** `thoughtgate-core/src/error/mod.rs:430`

`safe_details()` for `ServiceUnavailable` returns the full `reason` string, which includes internal component names (Cedar, Slack), architecture details, and library error messages.

**Fix:** Return generic "Service temporarily unavailable" to clients. Log full reason server-side with correlation ID.

---

## MEDIUM (P2 — Track and Address)

| # | Finding | File | Issue |
|---|---------|------|-------|
| M-1 | Atomic counter underflow risk | `task.rs:967` | `fetch_sub` wraps to `usize::MAX` if called on 0 |
| M-2 | `by_principal` index shows inconsistent state | `task.rs:1084-1090` | Task visible in `tasks` DashMap before `by_principal` |
| M-3 | `list_for_principal` unbounded Vec clone | `task.rs:1498-1501` | Full clone of per-principal task list |
| M-4 | Approve-over-reject reaction priority | `slack.rs:331-343` | Single approve overrides any number of rejects |
| M-5 | User display name cache clear amplification | `slack.rs:312-317` | Full cache clear at 1000 entries (not LRU) |
| M-6 | `tool_arguments` in Slack with no size limit | `slack.rs:192-223` | 40K char Slack limit; oversized args waste rate limits |
| M-7 | No bot token format validation | `slack.rs:91-93` | Accepts user tokens (`xoxp-`), invalid strings |
| M-8 | `reactions.get` per-task polling (spec violation) | `slack.rs:580-592` | CLAUDE.md says use `conversations.history` batch |
| M-9 | Race between polling and `execute_on_result` | `engine.rs:680-696` | Tight timing on approval record population |
| M-10 | Aggregate buffer limit TOCTOU race | `mcp_handler.rs:561-576` | Concurrent adds temporarily exceed limit |
| M-11 | `spawn_blocking` for principal inference has no timeout | `mcp_handler.rs:1374+` | 5 call sites without timeout wrapper |
| M-12 | Forward proxy mode allows SSRF | `proxy_service.rs:458-490` | No target host allowlist in forward proxy mode |
| M-13 | `Connection` header fields not processed per RFC 7230 | `proxy_service.rs:532-537` | Dynamic `Connection` header names not removed |
| M-14 | JSON `null` mapped to empty Cedar record | `policy/engine.rs:612-619` | Surprising policy behavior for null arguments |
| M-15 | Float-to-integer epsilon check is fragile | `policy/engine.rs:632-646` | Large floats silently rounded to incorrect integers |
| M-16 | No path traversal protection on policy file path | `policy/loader.rs:30-36` | `THOUGHTGATE_POLICY_FILE` used directly |
| M-17 | No principal authorization in core task handlers | `handlers.rs:76-78` | Authorization only at transport layer, not domain |
| M-18 | `deny_source` lost on wire serialization | `api.rs:87-89` | Shim path loses denial source for error code mapping |
| M-19 | Approval polling blocks all agent-to-server traffic | `shim/proxy.rs:1227-1389` | No pass-through for cancellations during approval wait |

---

## LOW (P3 — Improvement Opportunities)

| # | Finding | File | Issue |
|---|---------|------|-------|
| L-1 | Missing `Executing→Expired` transition | `task.rs:353-369` | Hung upstream calls can never expire |
| L-2 | `unwrap_or_default()` loses info in canonical JSON | `task.rs:685,702` | Silent empty string on serialization failure |
| L-3 | Env var parse failures silently use defaults | `slack.rs:95-110` | Misconfigured values ignored without warning |
| L-4 | Root path `/` classified as MCP | `traffic.rs:135` | Non-MCP JSON APIs on `/` are misclassified |
| L-5 | Path normalization misses encoded slashes | `traffic.rs:185-212` | `/%6dcp/v1` bypasses MCP detection |
| L-6 | HTTP 500 vs 200 inconsistency | `mcp_handler.rs:2156` | Serialization failure returns 500, not JSON-RPC error |
| L-7 | `to_lowercase()` allocation in hop-by-hop check | `proxy_service.rs:534` | Hot-path allocation; use `eq_ignore_ascii_case` |
| L-8 | Repeated principal inference (3x per request) | `mcp_handler.rs` | Same filesystem read spawned as 3 blocking tasks |
| L-9 | Invalid timestamps default silently | `policy/types.rs:167-171` | `(0, 0)` fallback without logging |
| L-10 | `.expect()` in LazyLock statics | `policy/engine.rs:43-66` | 5 instances; defensible but violates stated rules |
| L-11 | Missing error codes in CLAUDE.md vs implementation | `error/mod.rs` | 5 codes documented but unimplemented; 1 undocumented |
| L-12 | Unfulfillable tasks when no scheduler configured | `evaluator.rs:670-675` | Agent polls forever for approval that can never come |
| L-13 | PendingRequests HashMap grows unboundedly | `shim/proxy.rs:441-445` | Stale entries not reaped until shutdown |
| L-14 | Non-unique dry-run temp directory | `agent_launch.rs:169` | Concurrent `--dry-run` invocations interfere |

---

## Positive Findings (Things Done Well)

1. **Default-deny is correct** — Both `CedarDecision::default()` and `PolicyAction::default()` return deny/reject
2. **Fail-closed on Cedar error** — Policy evaluation errors result in deny, never permit
3. **4-gate ordering is strict** — No gate can be skipped for governable requests
4. **BufferGuard RAII** — Correct resource cleanup even on panics/early returns
5. **No `.unwrap()` in runtime code** — All occurrences are in tests/benchmarks
6. **No `std::sync::Mutex` in async code** — Correctly uses `tokio::sync::Mutex` everywhere (one justified exception in panic hook)
7. **Robust config backup** — Byte-for-byte verification, double-wrap detection, panic hook restore
8. **Zombie prevention** — Child processes properly reaped via `child.wait()`
9. **NDJSON security** — No buffer overflows, size limits enforced before parsing, smuggling detection
10. **Error sanitization** — `safe_details()` consistently redacts internal information
11. **Shutdown sequence** — Graceful cascade: stdin close → SIGTERM → SIGKILL with proper timeouts

---

## Recommended Review Order

1. **C-1 (Deadlock)** — Highest risk, probabilistic failure under production load
2. **C-2 (Token leak)** — Simple fix, high security impact
3. **C-4 (Permit-all fallback)** — Simple guard, prevents silent security degradation
4. **H-12 (Config write atomicity)** — Affects end-user config files
5. **C-5 (Principal truncation)** — Breaks task ownership, affects core workflow
6. **H-4 (Slack approver auth)** — No authorization on who can approve
7. **H-7 (Per-request timeout)** — Resource exhaustion risk under slow upstream
8. **H-8 (Atomic policy reload)** — Inconsistency window during reload
9. **H-1 (Counter leak)** — Progressive resource exhaustion
10. **C-3 (Dev mode bypass)** — Needs defense-in-depth, not just startup guard
