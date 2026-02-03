//! Property-based tests for NDJSON parsing round-trip invariants.
//!
//! Uses `proptest` to generate arbitrary valid JSON-RPC 2.0 messages and verify
//! that `parse_stdio_message` preserves the raw line and parses successfully.
//!
//! Implements verification plan: REQ-CORE-008 §9

use proptest::prelude::*;
use thoughtgate::shim::ndjson::parse_stdio_message;

// ─────────────────────────────────────────────────────────────────────────────
// Strategies
// ─────────────────────────────────────────────────────────────────────────────

/// Generate a JSON-RPC 2.0 numeric or string id.
fn arb_jsonrpc_id() -> impl Strategy<Value = String> {
    prop_oneof![
        // Numeric id.
        (1i64..=100_000).prop_map(|n| n.to_string()),
        // String id.
        "[a-zA-Z0-9_-]{1,32}".prop_map(|s| format!("\"{}\"", s)),
    ]
}

/// Generate a valid JSON-RPC method name (alphanumeric + slashes).
fn arb_method() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_/]{0,30}"
}

/// Generate a valid JSON-RPC 2.0 request.
fn arb_jsonrpc_request() -> impl Strategy<Value = String> {
    (arb_jsonrpc_id(), arb_method()).prop_map(|(id, method)| {
        format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"{}","params":{{}}}}"#,
            id, method
        )
    })
}

/// Generate a valid JSON-RPC 2.0 response.
fn arb_jsonrpc_response() -> impl Strategy<Value = String> {
    arb_jsonrpc_id().prop_map(|id| format!(r#"{{"jsonrpc":"2.0","id":{},"result":"ok"}}"#, id))
}

/// Generate a valid JSON-RPC 2.0 notification.
fn arb_jsonrpc_notification() -> impl Strategy<Value = String> {
    arb_method().prop_map(|method| format!(r#"{{"jsonrpc":"2.0","method":"{}"}}"#, method))
}

/// Generate any valid JSON-RPC 2.0 message.
fn arb_jsonrpc_message() -> impl Strategy<Value = String> {
    prop_oneof![
        arb_jsonrpc_request(),
        arb_jsonrpc_response(),
        arb_jsonrpc_notification(),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// Properties
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    #[test]
    fn parse_preserves_raw(msg in arb_jsonrpc_message()) {
        let result = parse_stdio_message(&msg);
        prop_assert!(result.is_ok(), "valid JSON-RPC should parse: {:?}", result.err());
        let parsed = result.unwrap();
        prop_assert_eq!(&parsed.raw, &msg, "raw should be preserved exactly");
    }

    #[test]
    fn parsed_raw_is_valid_json(msg in arb_jsonrpc_message()) {
        let result = parse_stdio_message(&msg).unwrap();
        let reparsed: Result<serde_json::Value, _> = serde_json::from_str(&result.raw);
        prop_assert!(reparsed.is_ok(), "raw should be valid JSON");
    }
}
