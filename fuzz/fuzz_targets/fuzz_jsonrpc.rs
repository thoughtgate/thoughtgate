#![no_main]

//! Fuzz target for REQ-CORE-003 JSON-RPC 2.0 Parsing
//!
//! # Traceability
//! - Implements: REQ-CORE-003/F-001 (JSON-RPC Parsing)
//! - Attack surface: Malformed JSON, invalid structure, ID type coercion
//!
//! # Goal
//! Verify that malformed JSON-RPC messages do not cause:
//! - Panics in the parser
//! - Memory leaks or unbounded allocation
//! - Incorrect ID type handling
//! - Security vulnerabilities (injection, DoS)

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use thoughtgate::transport::jsonrpc::{parse_jsonrpc, JsonRpcResponse};

/// Fuzz input for JSON-RPC testing
#[derive(Arbitrary, Debug)]
struct FuzzJsonRpcInput {
    /// Raw bytes to parse as JSON-RPC
    raw_bytes: Vec<u8>,
    /// Whether to try structured fuzzing
    use_structured: bool,
    /// Structured input for valid-ish JSON
    structured: Option<StructuredInput>,
}

/// Structured input that generates valid-ish JSON-RPC
#[derive(Arbitrary, Debug)]
struct StructuredInput {
    /// JSON-RPC version (should be "2.0" but fuzz it)
    jsonrpc_version: Option<FuzzVersion>,
    /// Request ID type
    id_type: IdType,
    /// Method name bytes
    method_bytes: Vec<u8>,
    /// Whether to include params
    has_params: bool,
    /// Params structure
    params_type: ParamsType,
    /// Whether this is a batch request
    is_batch: bool,
    /// Number of requests in batch (if batch)
    batch_size: u8,
}

#[derive(Arbitrary, Debug)]
enum FuzzVersion {
    Correct,      // "2.0"
    Wrong,        // "1.0", "3.0", etc
    Missing,      // no jsonrpc field
    NonString,    // number or object
    Empty,        // ""
}

#[derive(Arbitrary, Debug)]
enum IdType {
    Integer(i64),
    String(Vec<u8>),
    Null,
    Missing,       // notification
    Float(f64),    // invalid but should handle gracefully
    Object,        // invalid
    Array,         // invalid
}

#[derive(Arbitrary, Debug)]
enum ParamsType {
    Object(Vec<(Vec<u8>, ParamValue)>),
    Array(Vec<ParamValue>),
    Null,
    Invalid,  // string, number, etc.
}

#[derive(Arbitrary, Debug)]
enum ParamValue {
    Null,
    Bool(bool),
    Number(i64),
    String(Vec<u8>),
    Nested,  // nested object
}

fuzz_target!(|input: FuzzJsonRpcInput| {
    fuzz_jsonrpc_parsing(input);
});

fn fuzz_jsonrpc_parsing(input: FuzzJsonRpcInput) {
    // Test 1: Raw bytes parsing - should never panic
    let _ = parse_jsonrpc(&input.raw_bytes);

    // Test 2: Structured fuzzing for better coverage
    if input.use_structured {
        if let Some(structured) = input.structured {
            let json = build_json_from_structured(&structured);
            let _ = parse_jsonrpc(json.as_bytes());
        }
    }

    // Test 3: Response serialization - should never panic
    test_response_serialization(&input.raw_bytes);
}

/// Build JSON string from structured input
fn build_json_from_structured(input: &StructuredInput) -> String {
    if input.is_batch {
        // Build batch request
        let count = (input.batch_size % 10).max(1) as usize;
        let requests: Vec<String> = (0..count)
            .map(|i| build_single_request(input, i))
            .collect();
        format!("[{}]", requests.join(","))
    } else {
        build_single_request(input, 0)
    }
}

fn build_single_request(input: &StructuredInput, _index: usize) -> String {
    let mut parts = Vec::new();

    // jsonrpc field
    match &input.jsonrpc_version {
        Some(FuzzVersion::Correct) => parts.push(r#""jsonrpc":"2.0""#.to_string()),
        Some(FuzzVersion::Wrong) => parts.push(r#""jsonrpc":"1.0""#.to_string()),
        Some(FuzzVersion::Missing) => {} // omit field
        Some(FuzzVersion::NonString) => parts.push(r#""jsonrpc":2.0"#.to_string()),
        Some(FuzzVersion::Empty) => parts.push(r#""jsonrpc":"""#.to_string()),
        None => parts.push(r#""jsonrpc":"2.0""#.to_string()),
    }

    // id field
    match &input.id_type {
        IdType::Integer(n) => parts.push(format!(r#""id":{}"#, n)),
        IdType::String(s) => {
            let s = String::from_utf8_lossy(s);
            // Escape for JSON
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
            parts.push(format!(r#""id":"{}""#, escaped));
        }
        IdType::Null => parts.push(r#""id":null"#.to_string()),
        IdType::Missing => {} // notification - omit id
        IdType::Float(f) => parts.push(format!(r#""id":{}"#, f)),
        IdType::Object => parts.push(r#""id":{}"#.to_string()),
        IdType::Array => parts.push(r#""id":[]"#.to_string()),
    }

    // method field
    let method = String::from_utf8_lossy(&input.method_bytes);
    let method_escaped = method
        .chars()
        .take(256) // limit method name length
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    parts.push(format!(r#""method":"{}""#, method_escaped));

    // params field
    if input.has_params {
        let params = build_params(&input.params_type);
        parts.push(format!(r#""params":{}"#, params));
    }

    format!("{{{}}}", parts.join(","))
}

fn build_params(params_type: &ParamsType) -> String {
    match params_type {
        ParamsType::Object(entries) => {
            let pairs: Vec<String> = entries
                .iter()
                .take(20) // limit entries
                .map(|(k, v)| {
                    let key = String::from_utf8_lossy(k);
                    let key_escaped = key
                        .chars()
                        .take(64)
                        .collect::<String>()
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"");
                    format!(r#""{}": {}"#, key_escaped, build_param_value(v))
                })
                .collect();
            format!("{{{}}}", pairs.join(","))
        }
        ParamsType::Array(values) => {
            let items: Vec<String> = values
                .iter()
                .take(20)
                .map(build_param_value)
                .collect();
            format!("[{}]", items.join(","))
        }
        ParamsType::Null => "null".to_string(),
        ParamsType::Invalid => r#""invalid_params""#.to_string(),
    }
}

fn build_param_value(value: &ParamValue) -> String {
    match value {
        ParamValue::Null => "null".to_string(),
        ParamValue::Bool(b) => b.to_string(),
        ParamValue::Number(n) => n.to_string(),
        ParamValue::String(s) => {
            let s = String::from_utf8_lossy(s);
            let escaped = s
                .chars()
                .take(256)
                .collect::<String>()
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            format!(r#""{}""#, escaped)
        }
        ParamValue::Nested => r#"{"nested": true}"#.to_string(),
    }
}

/// Test response serialization doesn't panic
fn test_response_serialization(input: &[u8]) {
    // Try to parse and create a response
    if let Ok(parsed) = parse_jsonrpc(input) {
        match parsed {
            thoughtgate::transport::jsonrpc::ParsedRequests::Single(req) => {
                // Create success response
                let success = JsonRpcResponse::success(
                    req.id.clone(),
                    serde_json::json!({"result": "ok"}),
                );
                let _ = serde_json::to_string(&success);

                // Create error response
                let error = JsonRpcResponse::error(
                    req.id.clone(),
                    thoughtgate::error::jsonrpc::JsonRpcError {
                        code: -32600,
                        message: "Test error".to_string(),
                        data: None,
                    },
                );
                let _ = serde_json::to_string(&error);
            }
            thoughtgate::transport::jsonrpc::ParsedRequests::Batch(requests) => {
                for item in requests.iter().take(10) {
                    match item {
                        thoughtgate::transport::jsonrpc::BatchItem::Valid(req) => {
                            let response = JsonRpcResponse::success(
                                req.id.clone(),
                                serde_json::json!(null),
                            );
                            let _ = serde_json::to_string(&response);
                        }
                        thoughtgate::transport::jsonrpc::BatchItem::Invalid { id, error: _ } => {
                            let response = JsonRpcResponse::error(
                                id.clone(),
                                thoughtgate::error::jsonrpc::JsonRpcError {
                                    code: -32600,
                                    message: "Invalid request".to_string(),
                                    data: None,
                                },
                            );
                            let _ = serde_json::to_string(&response);
                        }
                    }
                }
            }
        }
    }
}
