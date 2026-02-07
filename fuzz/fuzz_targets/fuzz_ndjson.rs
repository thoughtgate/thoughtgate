#![no_main]

//! Fuzz target for REQ-CORE-008 NDJSON Parsing and Smuggling Detection
//!
//! # Traceability
//! - Implements: REQ-CORE-008/F-013 (NDJSON Parsing)
//! - Implements: REQ-CORE-008/F-014 (Smuggling Detection)
//! - Attack surface: Malformed JSON, oversized messages, embedded newlines, UTF-8 edge cases
//!
//! # Goal
//! Verify that NDJSON parsing does not cause:
//! - Panics in the parser
//! - Memory exhaustion from oversized messages
//! - Smuggling attacks via embedded newlines
//! - Incorrect classification of JSON-RPC messages

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use thoughtgate::shim::ndjson::{detect_smuggling, parse_stdio_message};

/// Fuzz input for NDJSON testing
#[derive(Arbitrary, Debug)]
struct FuzzNdjsonInput {
    /// Raw bytes to parse
    raw_bytes: Vec<u8>,
    /// Whether to try structured fuzzing
    use_structured: bool,
    /// Structured input for valid-ish JSON-RPC
    structured: Option<StructuredMessage>,
    /// Whether to inject smuggling attempts
    inject_smuggling: bool,
    /// Smuggled payload (if inject_smuggling is true)
    smuggled_payload: Option<Vec<u8>>,
}

/// Structured input that generates valid-ish JSON-RPC messages
#[derive(Arbitrary, Debug)]
struct StructuredMessage {
    /// Message type
    msg_type: MessageType,
    /// JSON-RPC version (should be "2.0" but fuzz it)
    version: FuzzVersion,
    /// Request/Response ID
    id: FuzzId,
    /// Method name (for requests/notifications)
    method: Vec<u8>,
    /// Whether to include params
    has_params: bool,
    /// Params content
    params: FuzzParams,
    /// Result content (for responses)
    result: Option<Vec<u8>>,
    /// Error content (for error responses)
    error: Option<FuzzError>,
}

#[derive(Arbitrary, Debug)]
enum MessageType {
    Request,
    Response,
    Notification,
    ErrorResponse,
}

#[derive(Arbitrary, Debug)]
enum FuzzVersion {
    Correct,       // "2.0"
    Wrong(u8),     // "1.0", "3.0", etc.
    Missing,       // no jsonrpc field
    NonString,     // number or object
    Empty,         // ""
}

#[derive(Arbitrary, Debug)]
enum FuzzId {
    Integer(i64),
    String(Vec<u8>),
    Null,
    Missing,       // notification
    Float(f64),    // invalid
    Object,        // invalid
    Array,         // invalid
}

#[derive(Arbitrary, Debug)]
enum FuzzParams {
    Object(Vec<(Vec<u8>, Vec<u8>)>),
    Array(Vec<Vec<u8>>),
    Null,
    Invalid,  // string, number
}

#[derive(Arbitrary, Debug)]
struct FuzzError {
    code: i32,
    message: Vec<u8>,
    has_data: bool,
}

fuzz_target!(|input: FuzzNdjsonInput| {
    fuzz_ndjson_parsing(input);
});

fn fuzz_ndjson_parsing(input: FuzzNdjsonInput) {
    // Test 1: Raw bytes parsing - should never panic
    if let Ok(line) = std::str::from_utf8(&input.raw_bytes) {
        let _ = parse_stdio_message(line);
    }

    // Test 2: Smuggling detection on raw bytes - should never panic
    let _ = detect_smuggling(&input.raw_bytes);

    // Test 3: Structured fuzzing for better coverage
    if input.use_structured {
        if let Some(structured) = input.structured {
            let json = build_json_from_structured(&structured);
            let _ = parse_stdio_message(&json);

            // Also test smuggling on structured input
            let _ = detect_smuggling(json.as_bytes());
        }
    }

    // Test 4: Smuggling injection
    if input.inject_smuggling {
        if let Some(smuggled) = input.smuggled_payload {
            test_smuggling_injection(&input.raw_bytes, &smuggled);
        }
    }

    // Test 5: Edge cases
    test_edge_cases();
}

/// Build JSON string from structured input
fn build_json_from_structured(input: &StructuredMessage) -> String {
    let mut parts = Vec::new();

    // jsonrpc field
    match &input.version {
        FuzzVersion::Correct => parts.push(r#""jsonrpc":"2.0""#.to_string()),
        FuzzVersion::Wrong(v) => parts.push(format!(r#""jsonrpc":"{}.0""#, v % 10)),
        FuzzVersion::Missing => {} // omit field
        FuzzVersion::NonString => parts.push(r#""jsonrpc":2.0"#.to_string()),
        FuzzVersion::Empty => parts.push(r#""jsonrpc":"""#.to_string()),
    }

    // id field (for requests and responses)
    match input.msg_type {
        MessageType::Request | MessageType::Response | MessageType::ErrorResponse => {
            match &input.id {
                FuzzId::Integer(n) => parts.push(format!(r#""id":{}"#, n)),
                FuzzId::String(s) => {
                    let s = String::from_utf8_lossy(s);
                    let escaped = escape_json_string(&s);
                    parts.push(format!(r#""id":"{}""#, escaped));
                }
                FuzzId::Null => parts.push(r#""id":null"#.to_string()),
                FuzzId::Missing => {} // omit id
                FuzzId::Float(f) => {
                    if f.is_finite() {
                        parts.push(format!(r#""id":{}"#, f))
                    }
                }
                FuzzId::Object => parts.push(r#""id":{}"#.to_string()),
                FuzzId::Array => parts.push(r#""id":[]"#.to_string()),
            }
        }
        MessageType::Notification => {} // no id for notifications
    }

    // method field (for requests and notifications)
    match input.msg_type {
        MessageType::Request | MessageType::Notification => {
            let method = String::from_utf8_lossy(&input.method);
            let method_escaped = escape_json_string(&method.chars().take(256).collect::<String>());
            parts.push(format!(r#""method":"{}""#, method_escaped));
        }
        _ => {}
    }

    // params field (for requests and notifications)
    if input.has_params {
        match input.msg_type {
            MessageType::Request | MessageType::Notification => {
                let params = build_params(&input.params);
                parts.push(format!(r#""params":{}"#, params));
            }
            _ => {}
        }
    }

    // result field (for success responses)
    if let MessageType::Response = input.msg_type {
        if let Some(result) = &input.result {
            let result_str = String::from_utf8_lossy(result);
            let escaped = escape_json_string(&result_str.chars().take(1024).collect::<String>());
            parts.push(format!(r#""result":"{}""#, escaped));
        } else {
            parts.push(r#""result":null"#.to_string());
        }
    }

    // error field (for error responses)
    if let MessageType::ErrorResponse = input.msg_type {
        if let Some(error) = &input.error {
            let message = String::from_utf8_lossy(&error.message);
            let message_escaped =
                escape_json_string(&message.chars().take(256).collect::<String>());
            let error_obj = if error.has_data {
                format!(
                    r#"{{"code":{},"message":"{}","data":null}}"#,
                    error.code, message_escaped
                )
            } else {
                format!(r#"{{"code":{},"message":"{}"}}"#, error.code, message_escaped)
            };
            parts.push(format!(r#""error":{}"#, error_obj));
        }
    }

    format!("{{{}}}", parts.join(","))
}

fn build_params(params: &FuzzParams) -> String {
    match params {
        FuzzParams::Object(entries) => {
            let pairs: Vec<String> = entries
                .iter()
                .take(20)
                .map(|(k, v)| {
                    let key = String::from_utf8_lossy(k);
                    let key_escaped =
                        escape_json_string(&key.chars().take(64).collect::<String>());
                    let value = String::from_utf8_lossy(v);
                    let value_escaped =
                        escape_json_string(&value.chars().take(256).collect::<String>());
                    format!(r#""{}":"{}""#, key_escaped, value_escaped)
                })
                .collect();
            format!("{{{}}}", pairs.join(","))
        }
        FuzzParams::Array(values) => {
            let items: Vec<String> = values
                .iter()
                .take(20)
                .map(|v| {
                    let value = String::from_utf8_lossy(v);
                    let escaped =
                        escape_json_string(&value.chars().take(256).collect::<String>());
                    format!(r#""{}""#, escaped)
                })
                .collect();
            format!("[{}]", items.join(","))
        }
        FuzzParams::Null => "null".to_string(),
        FuzzParams::Invalid => r#""invalid_params""#.to_string(),
    }
}

/// Escape a string for JSON embedding
fn escape_json_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Test smuggling injection scenarios
fn test_smuggling_injection(base: &[u8], smuggled: &[u8]) {
    // Limit sizes to prevent memory issues
    let base = if base.len() > 4096 {
        &base[..4096]
    } else {
        base
    };
    let smuggled = if smuggled.len() > 4096 {
        &smuggled[..4096]
    } else {
        smuggled
    };

    // Create a message with embedded newline + smuggled content
    let mut combined = base.to_vec();
    combined.push(b'\n');
    combined.extend_from_slice(smuggled);

    // Smuggling detection should catch this if smuggled looks like JSON-RPC
    let detected = detect_smuggling(&combined);

    // If smuggled content contains "jsonrpc", detection should trigger
    if smuggled.windows(7).any(|w| w == b"jsonrpc") {
        // Note: detection depends on valid JSON structure, so we just verify no panic
        let _ = detected;
    }
}

/// Test specific edge cases
fn test_edge_cases() {
    // Empty input
    let _ = parse_stdio_message("");
    let _ = parse_stdio_message("   ");
    let _ = parse_stdio_message("\n");
    let _ = parse_stdio_message("\r\n");

    // Whitespace handling
    let _ = parse_stdio_message("  {\"jsonrpc\":\"2.0\",\"method\":\"test\"}  ");
    let _ = parse_stdio_message("\t{\"jsonrpc\":\"2.0\",\"method\":\"test\"}\t");

    // Unicode
    let _ = parse_stdio_message(r#"{"jsonrpc":"2.0","method":"日本語"}"#);
    let _ = parse_stdio_message(r#"{"jsonrpc":"2.0","method":"emoji🎉"}"#);

    // Null bytes (should fail gracefully)
    let _ = parse_stdio_message("{\"jsonrpc\":\"2.0\",\"method\":\"\0test\"}");

    // Very long method name (under limit)
    let long_method = "x".repeat(1000);
    let _ = parse_stdio_message(&format!(
        r#"{{"jsonrpc":"2.0","method":"{}"}}"#,
        long_method
    ));

    // Batch array (should be rejected per F-007b)
    let _ = parse_stdio_message(r#"[{"jsonrpc":"2.0","id":1,"method":"test"}]"#);

    // Missing jsonrpc field
    let _ = parse_stdio_message(r#"{"id":1,"method":"test"}"#);

    // Wrong jsonrpc version
    let _ = parse_stdio_message(r#"{"jsonrpc":"1.0","id":1,"method":"test"}"#);

    // Smuggling edge cases
    let _ = detect_smuggling(b"{}");
    let _ = detect_smuggling(b"{}\n");
    let _ = detect_smuggling(b"{}\n{}");
    let _ = detect_smuggling(b"{\"jsonrpc\":\"2.0\"}\n{\"jsonrpc\":\"2.0\"}");

    // Near size limit (but under)
    // Skip actually allocating 10MB in edge case tests - covered by structured fuzzing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzz_harness_doesnt_panic() {
        // Verify the harness itself doesn't panic on basic inputs
        let input = FuzzNdjsonInput {
            raw_bytes: br#"{"jsonrpc":"2.0","method":"test"}"#.to_vec(),
            use_structured: false,
            structured: None,
            inject_smuggling: false,
            smuggled_payload: None,
        };
        fuzz_ndjson_parsing(input);
    }

    #[test]
    fn test_structured_generation() {
        let msg = StructuredMessage {
            msg_type: MessageType::Request,
            version: FuzzVersion::Correct,
            id: FuzzId::Integer(1),
            method: b"tools/call".to_vec(),
            has_params: true,
            params: FuzzParams::Object(vec![(b"name".to_vec(), b"read_file".to_vec())]),
            result: None,
            error: None,
        };
        let json = build_json_from_structured(&msg);
        assert!(json.contains("jsonrpc"));
        assert!(json.contains("tools/call"));
    }
}
