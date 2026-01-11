window.BENCHMARK_DATA = {
  "lastUpdate": 1768167185434,
  "repoUrl": "https://github.com/olegmukhin/thoughtgate",
  "entries": {
    "Benchmark": [
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "distinct": true,
          "id": "6e84ea0b390d6248f735e1ac4441aaf77c024fec",
          "message": "fix: header redaction security bugs and benchmark CI initialization\n\nSecurity Fixes (Found by Fuzzer):\n- Fix case-sensitive header comparison allowing credential leaks\n  Headers like \"Cookie\", \"COOKIE\", \"Authorization\" were not being\n  redacted due to case-sensitive string matching. HTTP header names\n  are case-insensitive per RFC 7230 Section 3.2.\n\n- Fix memory allocation crash in header sanitization\n  The .to_lowercase() approach allocated a String for every header,\n  causing crashes with malformed input and potential DoS via memory\n  exhaustion.\n\n- Solution: Use zero-allocation .eq_ignore_ascii_case() for header\n  comparison. This is faster, safer, and prevents both bugs.\n\nCI Fix:\n- Add automatic gh-pages branch creation for benchmarks\n  The benchmark-action failed on first run because gh-pages branch\n  didn't exist. Added setup step to create orphan branch with README\n  if needed, allowing benchmark tracking to work from first run.\n\nDiscovered-by: cargo-fuzz (fuzz_header_redaction target)",
          "timestamp": "2026-01-04T10:19:20Z",
          "tree_id": "3289b7e920aee5073d2d85a594c624f210fd3747",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/6e84ea0b390d6248f735e1ac4441aaf77c024fec"
        },
        "date": 1767522118637,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 131606.40457170393,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11398889.061111115,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "distinct": true,
          "id": "ce3c78ca992b2e7e5fd3be8c77958bea1fd23200",
          "message": "Formatting fixes",
          "timestamp": "2026-01-04T10:34:42Z",
          "tree_id": "83cbecd8d145870c3f49bff2d51c3fb5f2fcad8c",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/ce3c78ca992b2e7e5fd3be8c77958bea1fd23200"
        },
        "date": 1767523021118,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 95455.66009773948,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11379650.08333333,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "distinct": true,
          "id": "33bdeaf5a2ef7a6cbc70c01fa561534db59c5056",
          "message": "refactor: streamline header sanitization logic in fuzz test\n\n- Consolidated header name and value creation to reduce redundancy.\n- Enhanced sensitive header detection to ensure proper redaction patterns are checked.\n- Improved assertions for verifying that sensitive headers are not leaked and are correctly redacted.\n\nThis refactor aims to improve code clarity and maintainability while ensuring robust security checks for sensitive headers.",
          "timestamp": "2026-01-04T11:39:37Z",
          "tree_id": "42297936c89bf09d3554d3f0cf1b13bf4793e710",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/33bdeaf5a2ef7a6cbc70c01fa561534db59c5056"
        },
        "date": 1767526911878,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 128714.0174946416,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11366306.38,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "distinct": true,
          "id": "ae76348d58c6a43f2a13766990e6594e2b777eb7",
          "message": "chore: update dependencies and remove unused packages\n\n- Removed obsolete dependencies from Cargo.lock and Cargo.toml, including aws-lc-rs, aws-lc-sys, cmake, dunce, fs_extra, and jobserver.\n- Updated hyper-rustls and rustls configurations to disable default features and include specific features for improved performance and security.\n\nThese changes help streamline the dependency tree and enhance the overall project configuration.",
          "timestamp": "2026-01-04T11:54:38Z",
          "tree_id": "4634c40fb0ca14e25073af9201a98680a546e9a3",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/ae76348d58c6a43f2a13766990e6594e2b777eb7"
        },
        "date": 1767527850883,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 139242.0134312134,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11345661.565555556,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "distinct": true,
          "id": "533021825a8ee950e6773d4f8665aef6a18c524d",
          "message": "fix(fuzz): handle header truncation in redaction test\n\nThe fuzz test was incorrectly failing when sensitive headers were\ntruncated due to MAX_HEADERS_TO_LOG=50 defense-in-depth limit.\n\nBefore: Test asserted ALL sensitive headers must appear as redacted\nAfter: Skip validation for headers that don't appear (truncated)\n\nOnly validate redaction for headers actually present in output.",
          "timestamp": "2026-01-06T10:43:39Z",
          "tree_id": "03e2d26f4ce5c4c68d8506d7a4213b45bd279b60",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/533021825a8ee950e6773d4f8665aef6a18c524d"
        },
        "date": 1767696335959,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 130352.5170348292,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11351371.344444443,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "957055731c178f0f2e68f22d544be13edf7e11cf",
          "message": "Merge pull request #13 from olegmukhin/traffic-termination\n\nfeat: implement zero-copy peeking and buffered termination strategies",
          "timestamp": "2026-01-09T22:13:22Z",
          "tree_id": "35ce71f4f8856a7d07f44fb0d6b804d101786595",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/957055731c178f0f2e68f22d544be13edf7e11cf"
        },
        "date": 1767997036336,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 131415.99600925605,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11448396.460000003,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "8e6ee03e4f2d05b1d3235b29d941e50dffaaba1c",
          "message": "Merge pull request #14 from olegmukhin/refactor\n\nRemoved unused files and added CLAUDE.md",
          "timestamp": "2026-01-10T22:35:41Z",
          "tree_id": "0ea6adbfdfe17154359d6d04782f83426dd37890",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/8e6ee03e4f2d05b1d3235b29d941e50dffaaba1c"
        },
        "date": 1768084699302,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 138279.4339354855,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11375513.813333333,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "distinct": true,
          "id": "03b772f4a63227300d10232f3458c6ac4a4df43f",
          "message": "docs: merge pre-commit checklist",
          "timestamp": "2026-01-10T23:31:53Z",
          "tree_id": "6545ed3c245930dbb4b5724b3418a76e2a07d72c",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/03b772f4a63227300d10232f3458c6ac4a4df43f"
        },
        "date": 1768088399866,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 141104.0656612643,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11354554.626666663,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "0eef9941219be80be9975cb21e2fc975ce520e3d",
          "message": "feat(policy): implement Cedar policy engine (#15)\n\n* feat(policy): implement Cedar policy engine\n\nImplement comprehensive Cedar policy engine with 4-way traffic\nclassification (Green/Amber/Approval/Red paths) based on declarative\npolicies.\n\nKey features:\n- Policy evaluation with action priority (StreamRaw → Inspect → Approve)\n- Post-approval re-evaluation with policy drift detection\n- Policy loading priority: ConfigMap → Environment → Embedded defaults\n- Hot-reload with atomic swap (arc-swap) for zero-downtime updates\n- K8s identity inference from ServiceAccount mounts\n- Schema validation for all policies\n- Development mode override for local testing\n\nTechnical implementation:\n- Uses cedar-policy crate for sub-millisecond policy evaluation\n- Lock-free hot-reload using arc_swap::ArcSwap\n- Best-effort JWT parsing for ServiceAccount name extraction\n- Comprehensive error types (ParseError, SchemaValidation, IdentityError)\n- 31 unit tests covering all 16 edge cases (EC-POL-001 to EC-POL-016)\n\nFiles:\n- src/policy/mod.rs: Core types and public API\n- src/policy/engine.rs: Cedar engine with evaluate() and reload()\n- src/policy/loader.rs: Policy loading with fallback chain\n- src/policy/principal.rs: K8s identity inference\n- src/policy/schema.cedarschema: Cedar schema definition\n- src/policy/defaults.cedar: Embedded permissive policies for dev\n\nImplements: REQ-POL-001\nRefs: specs/REQ-POL-001_Cedar_Policy_Engine.md\n\n* chore(policy): update deps and add serial test annotations\n\nUpdate policy engine dependencies to latest stable versions:\n- cedar-policy: 4.2 → 4.8.0 (actual: 4.8.2)\n- arc-swap: 1.7 → 1.8.0\n\nAdd serial_test annotations to prevent race conditions in tests\nthat mutate global environment variables. Tests with #[serial]\nexecute sequentially, while pure-read tests remain parallel.\n\nFiles updated:\n- src/policy/engine.rs: 9 tests annotated with #[serial]\n- src/policy/loader.rs: 5 tests annotated with #[serial]\n- src/policy/principal.rs: 5 tests annotated with #[serial]\n\nVerified:\n- All 31 policy tests passing without --test-threads=1\n- cargo clippy clean (no warnings)\n- cargo build successful\n- No API changes required in existing code\n\n* fix(policy): add entity attributes, strict dev mode, and last_reload tracking\n\nFix critical issues in Cedar policy engine:\n\n1. Add principal entity attributes to Cedar evaluation\n   - Build entities with namespace, service_account, and roles\n   - Enables policies to match on principal.namespace and role hierarchy\n   - Previously used Entities::empty() causing silent policy failures\n\n2. Fix dev mode check to require explicit \"true\" value\n   - Changed from .is_ok() to .as_deref() == Ok(\"true\")\n   - Prevents accidental dev mode activation from THOUGHTGATE_DEV_MODE=false\n   - Security fix: operators can now safely disable dev mode\n\n3. Track last_reload timestamp in PolicyStats\n   - Add arc_swap::ArcSwap<Option<SystemTime>> to Stats struct\n   - Update timestamp on successful reload\n   - Improves observability for policy hot-reload operations\n\n4. Add serial test annotations for env-dependent tests\n   - Mark test_engine_creation, test_evaluate_with_default_policies,\n     test_stats with #[serial] to prevent race conditions\n\nTesting:\n- Added test_dev_mode_requires_true to verify strict checking\n- All 32 policy tests passing\n- cargo clippy clean\n\nFixes issues that would cause:\n- Policies using principal attributes to silently fail\n- Unintended dev mode activation (security vulnerability)\n- Missing observability data for policy reloads\n\n* Revert Cedar entity building to fix CI test failures\n\nReverts the build_entities() implementation that was causing 6 policy\nengine tests to fail in CI. The entity-building code had several issues:\n\n1. Role entities were created without required \"name\" attribute\n2. Resource entities were missing from entity store\n3. Cedar schema validation became stricter with entities present\n\nRoot cause: When entities are provided to Cedar's authorizer, it\nvalidates them against the schema. The incomplete entity construction\ncaused validation failures that manifested as policy denials.\n\nFor v0.1, we use Entities::empty() which works correctly with:\n- Entity UID-based policies (principal == ThoughtGate::App::\"name\")\n- All default embedded policies\n- All test policies\n\nThis does NOT support:\n- Attribute-based policies (principal.namespace == \"prod\")\n- Role hierarchy checks (principal in ThoughtGate::Role::\"admin\")\n\nFull entity store support will be added in a future version when\nneeded for production RBAC policies.\n\nFixes:\n- test_ec_pol_001_streamraw_permitted\n- test_ec_pol_002_inspect_only\n- test_ec_pol_006_post_approval_denied\n- test_ec_pol_010_invalid_syntax\n- test_ec_pol_011_schema_violation\n- test_ec_pol_012_reload_updates_stats (indirectly)\n\n* style(policy): remove extra blank line\n\n* fix(test): use BTreeMap for deterministic snapshot ordering\n\nChanged HashMap to BTreeMap in snapshot tests to ensure consistent\nkey ordering across test runs. This fixes CI failures where HashMap\niteration order is non-deterministic.\n\nFixes test_integrity_snapshot failures in CI.\n\n* docs(policy): add requirement traceability to public methods\n\nAdded doc comments with requirement links to policy_source() and\nstats() methods per coding guidelines. All public functions must\nlink to their implementing requirement.\n\n- policy_source(): Links to REQ-POL-001/F-003 (Policy Loading)\n- stats(): Links to REQ-POL-001/F-005 (Hot-Reload)",
          "timestamp": "2026-01-11T01:39:39Z",
          "tree_id": "2c29a0515d15a0f9eb475dd144a7ea18df068d76",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/0eef9941219be80be9975cb21e2fc975ce520e3d"
        },
        "date": 1768095900242,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 137666.42281840704,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11360106.245555554,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "ec4bb39ba81cbf7e35e7f9bfa52bd642df3721e4",
          "message": "feat(transport): implement REQ-CORE-003 MCP Transport & Routing (#16)\n\n* feat(transport): implement REQ-CORE-003 MCP Transport & Routing\n\nAdd MCP transport layer for JSON-RPC 2.0 message handling:\n\n- jsonrpc.rs: JSON-RPC 2.0 types (JsonRpcId, JsonRpcRequest/Response,\n  McpRequest) with custom serde to preserve ID types, SEP-1686 task\n  metadata extraction, batch request support\n- router.rs: Method routing (tools/* → Policy, tasks/* → TaskHandler,\n  resources/*, prompts/* → Policy, unknown → PassThrough)\n- upstream.rs: Connection-pooled reqwest client with timeout handling\n  and error classification\n- server.rs: Axum HTTP server with POST /mcp/v1 endpoint, semaphore-\n  based concurrency limiting, proper notification handling\n\nImplements: REQ-CORE-003\nRefs: specs/REQ-CORE-003_MCP_Transport_and_Routing.md\n\n* docs(transport): clarify JsonRpcId::Null usage and header forwarding\n\n- Document that JsonRpcId::Null is for error response serialization when\n  request ID is unknown, not for deserializing \"id\": null (which becomes\n  Option::None via serde's default behavior)\n- Add note explaining header forwarding (F-004.2) is intentionally not\n  implemented due to security concerns with forwarding Authorization\n- Add SAFETY comments to env var tests explaining single-threaded context\n\n* fix(transport): address review feedback for robustness and safety\n\nServer fixes:\n- Return proper JSON-RPC error instead of empty array on batch\n  serialization failure\n\nUpstream client fixes:\n- Add requirement traceability to with_base_url and UpstreamForwarder\n- Validate env var parsing with proper error messages instead of\n  silent fallback to defaults\n- Validate base_url is non-empty and parseable in UpstreamClient::new()\n- Handle 204 No Content responses for notifications\n- Use consistent -32002 error code for all upstream errors\n- Reject empty batch requests before forwarding to upstream\n\nTest improvements:\n- Add #[serial] attribute to env var tests\n- Add RAII EnvVarGuard for proper env var restoration\n- Add tests for invalid URL, empty URL, and invalid timeout values\n\nCI fix:\n- Add fallback from cargo-binstall to cargo install for cargo-audit\n- Add version verification after installation\n\n* fix(ci): force reinstall cargo-audit to handle stale cache\n\n* fix(jsonrpc): preserve explicit null ID per JSON-RPC 2.0 spec\n\nPer the JSON-RPC 2.0 specification, \"id\": null is a valid (though\nunusual) request that should have its null ID echoed back in responses.\nThis is distinct from a missing id field, which indicates a notification.\n\nPreviously, serde's Option<T> handling converted both missing fields\nand explicit null to None, treating \"id\": null as a notification.\n\nChanges:\n- Add MaybeNull<T> wrapper to distinguish Absent vs Null vs Present\n- Add custom deserialize_optional_id that preserves explicit null as\n  Some(JsonRpcId::Null)\n- Update tests to verify null ID is preserved and is NOT a notification\n- Add test_missing_id_is_notification to verify notifications work\n\nImplements: REQ-CORE-003/F-001.4 (Preserve ID type)\n\n* fix(transport): address additional review feedback\n\njsonrpc.rs:\n- Add MAX_TTL_MS constant (24 hours) and clamp TTL values to prevent\n  issues with extremely large Duration values\n\nrouter.rs:\n- Add requirement traceability to TaskMethod::as_str() and McpRouter::new()\n\nserver.rs:\n- Document why batch processing is sequential (F-007.5 batch approval\n  requires evaluating all requests together, not parallel)\n\nNote: env var test isolation (serial_test + EnvVarGuard) was already\nimplemented in previous commit.\n\n* fix(docker): resolve E0761 duplicate module error\n\n- Update Dockerfile to create src/error/mod.rs instead of src/error.rs\n  to match actual project structure and prevent Rust compiler error\n- Add requirement traceability to parse_single_request function\n- Update uuid dependency from 1.11 to 1.19\n\nFixes: CI build container image failure\n\n* fix(transport): add traceability markers and HTTP-layer body limit\n\n- Add requirement traceability to JsonRpcResponse::success/error,\n  McpRequest::is_notification/is_task_augmented/to_jsonrpc_request\n- Add requirement traceability to McpServer::new/with_upstream/router/run\n- Apply DefaultBodyLimit layer in router() to reject oversized requests\n  before buffering (prevents memory exhaustion from large payloads)\n- Update test_body_size_limit to verify HTTP-layer rejection (413)\n\nImplements: REQ-CORE-003/§6.2, F-001.3, F-003, F-004, §5.2\n\n* fix(transport): handle batch 204, add TTL test, document security\n\n- Handle 204 No Content in forward_batch for notification-only batches\n- Document that synthetic notification responses are for internal use only\n- Add test_ttl_clamped_to_max to verify 24-hour TTL limit enforcement\n- Document TLS 1.2+ enforcement by rustls in module docs\n\nImplements: REQ-CORE-003/F-007, F-003\n\n* fix(jsonrpc): serialize response id as null per JSON-RPC 2.0 spec\n\nPer JSON-RPC 2.0 spec section 5, the response \"id\" field is REQUIRED\nand MUST be null if the request id could not be determined (e.g., for\nparse errors). Previously, None was skipped during serialization.\n\n- Remove skip_serializing_if from JsonRpcResponse.id field\n- None now serializes as \"id\": null (required by spec)\n- Add documentation explaining id serialization semantics\n- Add test verifying null id serialization for parse errors\n\nImplements: REQ-CORE-003/§6.2",
          "timestamp": "2026-01-11T15:56:35Z",
          "tree_id": "ceb4a0c77988b171b5792e22604bc261fc156be9",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/ec4bb39ba81cbf7e35e7f9bfa52bd642df3721e4"
        },
        "date": 1768147322450,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 136045.6110411227,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11343962.458888888,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "b7d83e8386e03dc7dff563db4bdf31835b0071ad",
          "message": "refactor: simplify v0.1 to 3-way policy model (Forward/Approve/Reject) (#17)\n\n* refactor: simplify v0.1 to 3-way policy model (Forward/Approve/Reject)\n\nReplace 4-way traffic classification (Green/Amber/Approval/Red) with\nsimplified 3-way policy actions for v0.1:\n\n- Forward: Send request to upstream immediately\n- Approve: Block until Slack approval, then forward\n- Reject: Return JSON-RPC error immediately\n\nKey changes:\n\nSpecs:\n- Mark REQ-CORE-001 (Green Path) and REQ-CORE-002 (Amber Path) as DEFERRED\n- Update REQ-POL-001 Cedar schema to use Forward/Approve actions only\n- Update REQ-CORE-003 routing to use PolicyAction enum\n- Simplify architecture.md diagrams and data flows\n\nCode:\n- Add src/policy.rs with PolicyAction enum\n- Mark proxy_service.rs (Green Path streaming) as deferred\n- Mark buffered_forwarder.rs (Amber Path inspection) as deferred\n- Mark inspector.rs framework as deferred\n- Update error types and config with v0.1 status notes\n- Update lib.rs module docs with simplified traffic model\n\nThe Green/Amber path infrastructure is retained for v0.2+ when response\ninspection or LLM streaming is needed. All responses pass through\ndirectly in v0.1 without inspection.\n\n* docs(spec): update REQ-POL-001 implementation reference to v0.1 model\n\nUpdate the Cedar Engine Implementation section to match the actual v0.1\nimplementation:\n- Return PolicyAction (Forward/Approve/Reject) instead of PolicyDecision\n- Check Forward → Approve actions instead of StreamRaw/Inspect/Approve\n- Remove stale action_to_decision() helper showing 4-way logic\n\nThis fixes a documentation inconsistency where the spec defined v0.1\nactions but the implementation reference showed the old 4-way model.\n\nImplements: REQ-POL-001",
          "timestamp": "2026-01-11T16:56:58Z",
          "tree_id": "9915f5367a2fff6efb779ce955a2c6ef135a2d48",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/b7d83e8386e03dc7dff563db4bdf31835b0071ad"
        },
        "date": 1768150752938,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 131134.31803748145,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11357475.02,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "218646e2722dcc9885608e0d1c758562adb79c66",
          "message": "feat(governance): add task lifecycle management for approval workflows (#18)\n\n* feat(governance): add task lifecycle management for approval workflows\n\nImplement REQ-GOV-001 task lifecycle for ThoughtGate's v0.1 blocking\nmode. Provides internal state tracking for approval workflows without\nexposing SEP-1686 task API (deferred to v0.2+).\n\nFeatures:\n- Task struct with all fields from spec §6.1 (id, status, requests,\n  principal, timing, approval records, results, failure info)\n- TaskStatus state machine (Working, InputRequired, Executing,\n  Completed, Failed, Rejected, Cancelled, Expired)\n- TaskStore with DashMap for lock-free concurrent access\n- Optimistic locking for state transitions (F-007)\n- Rate limiting per principal and global capacity limits (F-009)\n- TTL enforcement and expiration cleanup (F-008)\n- 27 unit tests covering all edge cases (EC-TASK-001 to EC-TASK-016)\n\nImplements: REQ-GOV-001\nRefs: specs/REQ-GOV-001_Task_Lifecycle.md\n\n* fix(governance): address task lifecycle code review feedback\n\n- Add rust-version = \"1.87\" to Cargo.toml for MSRV enforcement\n- Align uuid features between deps and dev-deps (add serde feature)\n- Add missing REQ-GOV-001 doc comments to public functions\n- Remove misleading TaskId::as_str() method (Display impl suffices)\n- Add Working -> Expired transition to state machine for TTL enforcement\n- Fix cancel() to return InvalidTransition for Executing state\n- Fix wait_for_terminal race condition with proper notify/check loop\n- Fix TTL conversion to clamp to 30 days instead of zero on overflow\n- Combine duplicate to_sep1686() match arms\n- Document non-atomic capacity check limitation for v0.1\n\nRefs: REQ-GOV-001\n\n* docs(governance): clarify request hash excludes mcp_request_id\n\nDocument that the hash_request function intentionally excludes the\nmcp_request_id field because the hash is for semantic content integrity\nverification, not request instance identification. The request ID is\ntransport-layer metadata for JSON-RPC correlation.\n\nRefs: REQ-GOV-001/F-002.3",
          "timestamp": "2026-01-11T17:56:33Z",
          "tree_id": "236bba44a9494208275ea16171bc8e23fd341da0",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/218646e2722dcc9885608e0d1c758562adb79c66"
        },
        "date": 1768154532720,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 142087.69565956524,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11386984.62777778,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "90d3f55f650449921a0395522d4b6bcbfc784e7a",
          "message": "feat(governance): add Slack approval integration (#19)\n\n* feat(governance): add Slack approval integration\n\nImplement polling-based approval architecture for human-in-the-loop\nworkflows. Sidecars post approval requests to Slack and poll for\nreaction-based decisions (👍 = approve, 👎 = reject).\n\nComponents:\n- ApprovalAdapter trait abstracting external approval systems\n- SlackAdapter with Block Kit messages and reactions.get polling\n- PollingScheduler with BTreeMap priority queue\n- Token bucket rate limiter (1 req/sec for Slack API)\n- Exponential backoff: 5s → 10s → 20s → 30s max\n\nKey design decisions:\n- Polling model (not callbacks) since sidecars aren't addressable\n- DashMap for concurrent task reference storage\n- User display name caching to reduce API calls\n- Graceful shutdown drains pending approvals\n\nImplements: REQ-GOV-003\n\n* fix(governance): address review feedback for approval integration\n\n- Fix BTreeMap key collision when multiple tasks share same poll time\n  by using composite key (Instant, TaskId) instead of just Instant.\n  Added Ord/PartialOrd derives to TaskId.\n\n- Use configurable initial_poll_interval in SlackAdapter instead of\n  hardcoded 5 second value. Reads THOUGHTGATE_APPROVAL_POLL_INTERVAL_SECS\n  environment variable.\n\n- Make user display name lookup best-effort in poll_for_decision.\n  Falls back to user_id if Slack API call fails, preventing decision\n  loss due to transient network errors.\n\n- Add test_concurrent_submit_no_data_loss to verify multiple tasks\n  with same poll time are all preserved in the queue.",
          "timestamp": "2026-01-11T18:36:53Z",
          "tree_id": "ba72a8e497fb6ac5402ffc2ca554ce2e3e70f831",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/90d3f55f650449921a0395522d4b6bcbfc784e7a"
        },
        "date": 1768156765842,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 135829.08751747853,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11366349.33,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "ab7beb66a2e6e89020aff53a01f2f5e5d7f7d901",
          "message": "feat(governance): implement REQ-GOV-002 Approval Execution Pipeline (#20)\n\n* feat(governance): implement REQ-GOV-002 Approval Execution Pipeline\n\nAdd multi-phase execution pipeline for approval-required requests:\n\n- Pre-Approval Amber: Transform/validate request through inspector chain\n  before showing to human reviewer\n- Post-Approval Amber: Re-validate after approval with transform drift\n  detection (strict/permissive modes)\n- Policy re-evaluation: Check policy with ApprovalGrant context to detect\n  policy drift between approval and execution\n- Upstream forwarding: Send approved requests to MCP server with timeout\n\nKey types:\n- ExecutionPipeline trait with pre_approval_amber() and execute_approved()\n- ApprovalPipeline implementation with configurable inspector chain\n- PipelineResult with detailed FailureStage attribution\n- PipelineConfig with env var support\n\nEdge cases covered:\n- EC-PIP-001 to EC-PIP-014: All pipeline failure scenarios including\n  approval expiry, policy drift, transform drift, upstream errors\n\nNFR-001 (metrics) and NFR-002 (benchmarks) deferred as non-blocking.\n\nImplements: REQ-GOV-002\nRefs: specs/REQ-GOV-002_Execution_Pipeline.md\n\n* fix(governance): address code review findings for REQ-GOV-002\n\n- Unify hash_request: Export from task.rs and import in pipeline.rs to\n  ensure consistent hashing between task creation and execution\n- Add InvalidTaskState failure stage: Better describes task state\n  validation failures during execution (was using PreHitlInspection)\n- Remove unused test locals: Clean up test_approval_expired\n- Remove duplicate hash_request: Eliminates algorithm mismatch that\n  would cause false transform drift detection\n\nRefs: REQ-GOV-002",
          "timestamp": "2026-01-11T21:30:29Z",
          "tree_id": "893a78b6c120fb9bb37623c44b4487cc8957580e",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/ab7beb66a2e6e89020aff53a01f2f5e5d7f7d901"
        },
        "date": 1768167184590,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 131558.16262931126,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11356819.873333333,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}