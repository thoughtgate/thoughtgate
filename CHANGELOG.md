# Changelog

All notable changes to ThoughtGate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-02-05

### Features

- *(core)* Add transport-agnostic JSON-RPC classification and profile types
- *(governance)* Add evaluate request/response types for stdio shim
- *(metrics)* Add NFR-002 stdio metric instruments
- *(shim)* Add FramingError enum for stdio NDJSON transport
- *(shim)* Add NDJSON parsing and smuggling detection
- *(wrap)* Add ConfigAdapter trait, agent adapters, and env expansion
- *(wrap)* Add ConfigGuard RAII lock and restore
- *(governance)* Add HTTP service for stdio shim communication
- *(shim)* Add StdioError enum and process lifecycle types
- *(shim)* Add bidirectional stdio proxy with governance evaluation
- *(wrap)* Add CLI argument types and config guard skip_restore
- *(wrap)* Add wrap orchestration and agent launch
- *(cli)* Add clap CLI entry point with wrap and shim subcommands
- *(governance)* Add 4-gate evaluator and task status endpoint
- *(thoughtgate)* Wire governance evaluator into CLI wrap and shim
- *(thoughtgate)* Fix REQ-CORE-008 spec compliance gaps (T1+T2 code, T3 spec notes)
- *(telemetry)* Add OTel provider init, shutdown, and noop fallback
- *(telemetry)* Add MCP request span instrumentation
- *(telemetry)* Add Prometheus metrics endpoint with prometheus-client
- *(telemetry)* Add HITL approval span links and trace context storage
- *(telemetry)* Add W3C Trace Context propagation for HTTP transport
- *(telemetry)* Add YAML config, head sampling, and OTLP batch settings
- *(telemetry)* Add Grafana dashboards, Cedar spans, and gate metrics
- *(telemetry)* Add stdio transport trace context propagation
- *(telemetry)* Add decision span, task counters, and gauge wiring
- *(telemetry)* Wire remaining REQ-OBS-002 metrics and spans
- *(test)* Add governance and metrics smoke tests, remove legacy prometheus
- *(wrap)* Add ${VAR:-default} environment variable expansion

### Bug Fixes

- Resolve clippy and compilation errors across workspace
- *(wrap)* Add kill_on_drop to agent process spawn
- *(shim)* Bound read_line to prevent unbounded memory DoS
- *(shim)* Scrub ThoughtGate env vars from server child process
- *(wrap)* Stop deleting lock file on drop to prevent TOCTOU race
- *(shim)* Clamp approval poll interval to minimum 100ms
- Resolve clippy warnings in CI
- *(wrap)* Fail on missing approval adapter in production
- *(wrap)* Add claude-desktop to Linux agent detection
- Resolve clippy errors for amber_path feature tests
- *(wrap)* Acquire config lock before rewriting to prevent TOCTOU race
- *(shim)* Resolve 10 issues found in REQ-CORE-008 audit
- *(telemetry)* Populate cedar outcome in gateway decision span
- *(telemetry)* Add propagate_upstream config, REQ traceability, port validation
- *(telemetry)* Use span links, add baggage propagation, optimize labels
- *(ci)* Add missing integration tests and fix env var inconsistencies
- *(config)* Align env var naming and remove dead THOUGHTGATE_LISTEN
- *(governance)* Document soft limit and add stale shim cleanup
- *(wrap)* Update ClaudeCodeAdapter for new per-project config format
- *(shim)* Gate integration tests on cfg(unix)

### Refactoring

- Restructure into 3-crate Cargo workspace
- *(telemetry)* Consolidate legacy OTel metrics into prometheus-client
- *(core)* Reduce API surface and fix flaky test
- *(fuzz)* Add NDJSON target, remove redundant URI parsing

### Documentation

- Update specs for v0.3 three-crate workspace layout
- Fix stale binary references after workspace restructuring
- *(specs)* Update REQ-CORE-008 stdio transport spec from main
- *(specs)* Add REQ-OBS-002 telemetry spec and update REQ-CFG-001
- *(website)* Add v0.3 CLI wrapper documentation
- Rewrite README for v0.3 CLI wrapper era

### Styling

- *(proxy)* Fix import ordering in trace context tests

### Testing

- *(wrap)* Add config parsing fixture files
- *(shim)* Add shim proxy integration tests
- *(cli)* Add CLI argument parsing tests
- *(ndjson)* Add EC-STDIO-017 unit test for unclassifiable messages
- *(thoughtgate)* Add stdio transport integration tests
- *(ndjson)* Add property-based parse round-trip tests

### Dependencies

- *(deps)* Remove unused opentelemetry-prometheus and metrics feature
- *(deps)* Update dependencies and fix flaky test
- *(deps)* Remove unused opentelemetry-prometheus from proxy

### Miscellaneous

- *(thoughtgate)* Add proptest dev-dep, fix config_guard doc comments
- Configure cargo-dist for binary distribution

### CI/CD

- Optimize pipeline and add missing CLI tests

## [0.2.2] - 2026-02-02

### Features

- *(logging)* Add correlation ID to every request span

### Bug Fixes

- Address PR review comments
- *(config)* Warn on invalid environment variable values
- *(traffic)* Prevent Content-Type and path bypass in MCP detection
- *(policy)* Harden json_to_cedar_expr against DoS and data corruption
- *(main)* Respect --bind flag for admin server and remove dummy socket
- *(config)* Reject invalid glob patterns at startup
- *(policy)* Add reload retry, eval histogram, and document blocking I/O
- *(transport)* Forward non-tg_ task requests to upstream
- *(transport)* Set RFC 4122 version/variant bits in correlation IDs
- *(transport)* Fail closed when source not found in visibility check
- *(transport)* Enforce response body size limit for upstream responses
- *(transport)* Use HTTP client for health check instead of raw TCP
- *(transport)* Weight semaphore by batch size and return JSON-RPC error
- *(transport)* Strip task metadata instead of rejecting on compat mismatch
- *(governance)* Use atomic per-principal counter to prevent TOCTOU race
- *(governance)* Record result on auto-approved expired tasks
- *(protocol)* Redact sensitive fields in Debug output
- *(proxy)* Use std::sync::Once for rustls crypto provider init
- *(lifecycle)* Treat 4xx health check responses as reachable
- *(proxy)* Make pool_max_idle_per_host configurable
- *(proxy)* Apply TimeoutBody to response streams
- *(transport)* Add aggregate buffer tracking to prevent OOM
- *(transport)* Wrap infer_principal in spawn_blocking
- *(governance,transport)* Fix per-principal counter leak and streaming body limit
- *(policy)* Replace unreachable with fail-closed reject
- *(policy)* Remove std::thread::sleep from reload retry
- *(metrics)* Normalize method labels to prevent cardinality explosion
- *(governance)* Add depth limit to canonical_json to prevent stack overflow
- *(governance)* Add timeout to inspector execution
- *(governance)* Wrap policy re-evaluation in spawn_blocking
- *(governance)* Include method in hash_request to prevent collisions
- *(metrics)* Add tasks/list and tasks/result to normalize_method
- *(governance)* Return error from canonical_json on depth exceeded
- *(transport)* Fail closed in verify_task_principal on identity error
- *(transport)* Return 204 for all-notifications batch per JSON-RPC 2.0
- *(governance)* Clean up pending_by_principal entries when count reaches zero
- *(transport)* Replace .expect() with error propagation in rustls init
- *(governance)* Add validation to PipelineConfig::from_env

### Refactoring

- *(policy)* Replace manual date math with chrono
- *(protocol)* Simplify capability cache flag to AtomicBool
- *(governance)* Store Arc<Task> in TaskEntry for cheap reads

### Documentation

- *(admin)* Fix AdminServerConfig bind_addr default in doc comment

### Testing

- *(bench)* Add JSON-RPC parsing micro-benchmark
- *(transport)* Add read_body_limited tests for response size enforcement

### Miscellaneous

- Bump serde-saphyr to 0.0.17 and release v0.2.2

### Performance

- *(transport)* Cache upstream URL and use Cow for JSON-RPC version (T-005, T-003)
- *(config)* Pre-compile glob patterns at config load time (S-004)
- *(policy)* Cache Cedar entity type names with LazyLock (P-002 pt1)
- *(policy)* Merge role loops and pre-allocate context maps (P-002 pt2, P-003)
- *(jsonrpc)* Replace Uuid::new_v4() with counter-based correlation IDs (T-002)
- *(server)* Replace deserialize-filter-serialize with Value-tree mutation (S-003)
- *(transport)* Wrap McpRequest params in Arc<Value>
- *(governance)* Return Arc<Task> from TaskStore methods
- *(protocol)* Use serde Visitor for Sep1686Status deserialization
- *(transport)* Parallelize batch request processing
- *(logging)* Use non-blocking stdout writer

## [0.2.1] - 2026-02-01

### Features

- *(metrics)* Add governance pipeline observability metrics
- *(policy)* Implement JWT parsing for K8s ServiceAccount tokens
- *(tracing)* Add instrument spans to key public functions (Y-003)
- *(lifecycle)* Add production environment safety validation
- *(upstream)* Add retry for transport-level connection failures
- *(metrics)* Add request-level MCP metrics (Y-006)

### Bug Fixes

- *(governance)* Bypass status check for auto-approved expired tasks
- *(governance)* Remove empty entries from by_principal index on cleanup
- *(transport)* Add configurable max batch size to prevent DoS
- *(error)* Replace .unwrap() with safe fallback in proxy error responses
- *(transport)* Add TCP keepalive and differentiate upstream HTTP error codes
- *(governance)* Enforce scheduler capacity limit and redact rule patterns
- *(docker)* Run as non-root user and add container vulnerability scanning
- *(governance)* Use Acquire/Release ordering for admission control atomics
- *(governance)* Use canonical JSON serialization for request hashing
- *(governance)* Share single CedarEngine between handler and pipeline
- *(governance)* Add RAII guard for executing set and pipeline timeout
- *(governance)* Use compare_exchange for atomic global admission control
- *(transport)* Fail closed on list method serialization error
- *(transport)* Verify caller principal on task get/result/cancel APIs
- *(governance)* Redact tool_arguments in ApprovalRequest Debug impl
- *(governance)* Bound Slack user cache to prevent unbounded growth
- *(lifecycle)* Replace process::exit with error returns in startup
- *(governance)* Store scheduler JoinHandle for crash detection
- *(transport)* Add per-request timeout for proxy connections
- *(transport)* Enforce body size limit at stream level before collection
- *(transport)* Fail closed when upstream list response cannot be parsed
- *(protocol)* Replace std::sync::RwLock with atomic in CapabilityCache
- *(transport)* Classify upstream HTTP errors by status code
- *(transport)* Return empty JSON array for all-notification batches
- *(governance)* Replace std::thread::sleep in tests and improve principal error
- *(protocol)* Replace misleading last_initialize() with has_initialized()
- *(transport)* Map upstream HTTP 404 to UpstreamError not MethodNotFound
- *(governance)* Handle fractional rate limits in governor quota
- *(governance)* Preserve K8s identity through principal conversion
- *(config)* Improve LazyLock regex panic message and add guard test
- *(pipeline)* Use approved request in permissive drift mode
- *(pipeline)* Log warning for malformed upstream response (Y-007)
- *(policy)* Track loaded policy file paths in policy_info (Y-008)

### Refactoring

- *(transport)* Extract shared request routing logic
- *(transport)* Unify JsonRpcId across transport and governance modules
- *(governance)* Replace custom rate limiter with governor crate
- *(transport)* Replace custom LoggingLayer with tower-http trace

### Documentation

- *(transport)* Document batch concurrency and scheduler rate-limit design
- *(config)* Document deferred config hot-reload capability
- *(policy)* Document spawn_blocking trade-off for Cedar evaluation
- *(pipeline)* Document Forward acceptance in post-approval re-eval

### Testing

- *(e2e)* Add full-flow integration test for approval pipeline

### Dependencies

- *(deps)* Replace once_cell/nanoid with std/uuid, pin serde-saphyr

### Miscellaneous

- Prepare v0.2.1 release (version bump + docs update)

### Performance

- *(jsonrpc)* Eliminate double deserialization for single requests (T-001)

## [0.2.0] - 2026-01-28

### Features

- Implement zero-copy peeking and buffered termination strategies
- *(policy)* Implement Cedar policy engine 
- *(transport)* Implement REQ-CORE-003 MCP Transport & Routing 
- *(governance)* Add task lifecycle management for approval workflows 
- *(governance)* Add Slack approval integration 
- *(governance)* Implement REQ-GOV-002 Approval Execution Pipeline 
- *(lifecycle)* Implement REQ-CORE-005 Operational Lifecycle 
- *(obs)* Implement REQ-OBS-001 performance metrics with Bencher.dev 
- *(lifecycle,protocol)* Implement REQ-CORE-005 and REQ-CORE-007 
- *(policy)* Implement REQ-POL-001 Cedar Policy Engine v0.2
- *(transport)* Integrate TaskHandler and CedarEngine into server routing
- *(governance)* Add ApprovalEngine for v0.2 approval workflow
- *(governance)* Add wiremock integration tests for Slack API
- *(config)* Implement REQ-CFG-001 configuration schema 
- Implement v0.2 3-port Envoy-style architecture
- *(governance)* Wire 4-gate request routing model
- *(transport)* Implement SEP-1686 task metadata protocol compliance
- *(protocol)* Implement MCP capability announcements (REQ-CORE-007)
- *(docs)* Add Docusaurus documentation site with GitHub Pages CI
- *(docs)* Add PR preview deployments
- *(proxy)* Wire MCP governance into ProxyService
- *(bench)* Overhaul benchmarks for accuracy and MCP traffic

### Bug Fixes

- Header redaction security bugs and benchmark CI initialization
- Defense-in-depth hardening for header sanitization
- *(fuzz)* Handle header truncation in redaction test
- Prometheus registry fix and upgrades
- Bench errors and review comments
- *(lifecycle)* Ensure drain_timeout always less than shutdown_timeout
- *(governance)* Add missing workflow/policy_id fields to error types
- *(governance)* Remove panic in ApprovalEngine::new
- *(error)* Add TaskResultNotReady error variant and doc tags
- *(proxy)* Improve port reservation and add traffic tests
- *(proxy)* Correct comment and add MCP request handling tests
- *(ci)* Use admin port for health checks in v0.2 3-port model
- *(transport)* Address protocol compliance and gate coverage issues
- *(governance)* Extend 4-gate model to resources and prompts
- *(governance)* Correct integrity verification chain of custody
- *(protocol)* Align with MCP Tasks Specification (2025-11-25)
- *(protocol)* Align tasks/list and list methods with MCP spec
- *(protocol)* Align tasks/cancel error codes with MCP spec
- *(fuzz)* Update fuzz targets for API changes
- *(slack)* Rename env vars to match THOUGHTGATE_ prefix convention
- *(fuzz)* Add missing TimeoutAction argument to fuzz_task_lifecycle
- *(fuzz)* Sanitize env var values to prevent NUL byte panic
- *(tests)* Suppress dead_code warnings in test helpers
- *(ci)* Use statistical testing for benchmark regression detection
- *(ci)* Replace mock_llm with mock_mcp and optimize Dockerfile

### Refactoring

- Streamline header sanitization logic in fuzz test
- Simplify v0.1 to 3-way policy model (Forward/Approve/Reject) 
- Simplify Infallible error handling with absurd pattern
- *(governance)* Rename task_store() to shared_store() for clarity
- *(bench)* Use default ports and improve benchmark reliability

### Documentation

- Add conventional commits guidelines to CLAUDE.md
- Merge conventional commits guidelines
- Add pre-commit checklist with cargo fmt requirement
- Merge pre-commit checklist
- Update documentation for v0.2 release
- *(bench)* Align metric names between benchmarks and REQ-OBS-001

### Testing

- *(governance)* Add integration test suite for 4-gate decision flow

### Dependencies

- *(deps)* Update dependencies for v0.2 release

### Miscellaneous

- Update dependencies and remove unused packages
- *(specs)* Update architecture and requirements for v0.2 
- Bump version to v0.2.0

### CI/CD

- *(bench)* Allow benchmark job to fail without blocking PR
- *(fuzz)* Improve fuzzing coverage and remove from PR CI
- Optimize pipeline with path filtering and concurrency


