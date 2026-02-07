//! NDJSON message parsing and smuggling detection for stdio transport.
//!
//! Implements: REQ-CORE-008 §6.5.2 (StdioMessage), F-013 (NDJSON Parsing),
//!             F-014 (Smuggling Detection), F-007b (Batch Rejection)
//!
//! This module provides pure parsing functions — no async I/O, no read loops.
//! The shim's async read loop calls [`parse_stdio_message`] for each NDJSON line
//! and [`detect_smuggling`] on the raw bytes before trimming.

use thoughtgate_core::jsonrpc::{JsonRpcClassifyError, JsonRpcMessageKind, classify_jsonrpc};

use crate::error::FramingError;

/// Maximum NDJSON message size (10 MB).
///
/// Lines exceeding this limit are rejected before JSON parsing to prevent
/// allocation of oversized `serde_json::Value` trees from crafted input.
///
/// Implements: REQ-CORE-008 §5.2
pub const MAX_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

/// A parsed NDJSON line from the stdio transport.
///
/// Contains the classified JSON-RPC message kind plus extracted payload fields
/// needed by the governance evaluator and forwarding logic.
///
/// Implements: REQ-CORE-008 §6.5.2
#[derive(Debug, Clone)]
pub struct StdioMessage {
    /// Classified message kind (Request, Response, or Notification).
    pub kind: JsonRpcMessageKind,
    /// The `params` field, if present (requests and notifications).
    pub params: Option<serde_json::Value>,
    /// The original NDJSON line, preserved for byte-for-byte forwarding.
    pub raw: String,
}

/// Parse a single NDJSON line into a [`StdioMessage`].
///
/// Performs size validation, JSON parsing, batch rejection, and JSON-RPC
/// classification in sequence. The `params` field is extracted from the
/// parsed value to avoid re-parsing downstream.
///
/// # Arguments
///
/// * `line` - A single NDJSON line (may include leading/trailing whitespace).
///
/// # Errors
///
/// Returns [`FramingError`] for:
/// - Oversized messages (`MessageTooLarge`) — checked before JSON parsing
/// - Invalid JSON (`MalformedJson`)
/// - JSON arrays (`UnsupportedBatch`) — MCP rejects batch requests (F-007b)
/// - Missing `jsonrpc` field (`MissingVersion`)
/// - Wrong `jsonrpc` version (`UnsupportedVersion`)
/// - Invalid `id` field type (`MalformedJson`)
/// - Unclassifiable messages (`MalformedJson`)
///
/// Implements: REQ-CORE-008/F-013, F-007b
pub fn parse_stdio_message(line: &str) -> Result<StdioMessage, FramingError> {
    // F-013: Size check on raw byte length BEFORE any JSON parsing.
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(FramingError::MessageTooLarge {
            max_bytes: MAX_MESSAGE_BYTES,
        });
    }

    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(FramingError::MalformedJson {
            reason: "empty message".to_string(),
        });
    }

    // Parse JSON.
    let mut value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| FramingError::MalformedJson {
            reason: e.to_string(),
        })?;

    // F-007b: Reject batch (array) messages.
    if value.is_array() {
        return Err(FramingError::UnsupportedBatch);
    }

    // Classify via transport-agnostic classifier.
    let kind = classify_jsonrpc(&value).map_err(|e| match e {
        JsonRpcClassifyError::InvalidVersion => {
            // Disambiguate: missing vs wrong version.
            match value.get("jsonrpc").and_then(|v| v.as_str()) {
                Some(v) => FramingError::UnsupportedVersion {
                    version: v.to_string(),
                },
                None => FramingError::MissingVersion,
            }
        }
        JsonRpcClassifyError::InvalidId => FramingError::MalformedJson {
            reason: "invalid id field".to_string(),
        },
        JsonRpcClassifyError::Unclassifiable => FramingError::MalformedJson {
            reason: "message has neither id nor method".to_string(),
        },
    })?;

    // Extract params by removing from the mutable value to avoid cloning.
    let params = value.as_object_mut().and_then(|obj| obj.remove("params"));

    Ok(StdioMessage {
        kind,
        params,
        raw: line.to_string(),
    })
}

/// Detect JSON-RPC message smuggling in raw NDJSON bytes.
///
/// Smuggling occurs when a single NDJSON "line" contains literal `0x0A` (newline)
/// bytes that embed additional JSON-RPC messages. A well-behaved MCP server or
/// agent never sends literal newlines within a JSON-RPC message.
///
/// The function splits on `0x0A`, skips the first segment (the legitimate message),
/// and checks whether any subsequent non-empty segment parses as JSON containing
/// a `"jsonrpc"` key — indicating a smuggled message.
///
/// # Arguments
///
/// * `raw` - The raw bytes of the NDJSON line, **before** any trimming.
///
/// # Returns
///
/// `true` if a smuggled JSON-RPC message is detected, `false` otherwise.
///
/// Implements: REQ-CORE-008/F-014
pub fn detect_smuggling(raw: &[u8]) -> bool {
    // Split on literal 0x0A bytes.
    let mut segments = raw.split(|&b| b == b'\n');

    // Skip the first segment — it's the legitimate message.
    segments.next();

    // Check remaining segments for smuggled JSON-RPC messages.
    for segment in segments {
        if segment.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(segment) {
            if value.get("jsonrpc").is_some() {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use thoughtgate_core::jsonrpc::JsonRpcId;

    // ─────────────────────────────────────────────────────────────────────
    // parse_stdio_message tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn test_parse_request() {
        let line =
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_file"}}"#;
        let msg = parse_stdio_message(line).unwrap();
        assert_eq!(
            msg.kind,
            JsonRpcMessageKind::Request {
                id: JsonRpcId::Number(1),
                method: "tools/call".to_string(),
            }
        );
        assert!(msg.params.is_some());
        assert_eq!(
            msg.params.unwrap().get("name").unwrap().as_str().unwrap(),
            "read_file"
        );
    }

    #[test]
    fn test_parse_response() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"content":"hello"}}"#;
        let msg = parse_stdio_message(line).unwrap();
        assert_eq!(
            msg.kind,
            JsonRpcMessageKind::Response {
                id: JsonRpcId::Number(1),
            }
        );
    }

    #[test]
    fn test_parse_notification() {
        let line = r#"{"jsonrpc":"2.0","method":"initialized"}"#;
        let msg = parse_stdio_message(line).unwrap();
        assert_eq!(
            msg.kind,
            JsonRpcMessageKind::Notification {
                method: "initialized".to_string(),
            }
        );
        assert!(msg.params.is_none());
    }

    #[test]
    fn test_parse_error_response() {
        let line = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"bad"}}"#;
        let msg = parse_stdio_message(line).unwrap();
        assert_eq!(
            msg.kind,
            JsonRpcMessageKind::Response {
                id: JsonRpcId::Number(1),
            }
        );
    }

    #[test]
    fn test_parse_oversized_message() {
        // Generate a string > 10 MB.
        let big = "x".repeat(MAX_MESSAGE_BYTES + 1);
        let err = parse_stdio_message(&big).unwrap_err();
        assert!(
            matches!(err, FramingError::MessageTooLarge { max_bytes } if max_bytes == MAX_MESSAGE_BYTES)
        );
    }

    #[test]
    fn test_parse_malformed_json() {
        let line = r#"{"truncated"#;
        let err = parse_stdio_message(line).unwrap_err();
        assert!(matches!(err, FramingError::MalformedJson { .. }));
    }

    #[test]
    fn test_parse_missing_jsonrpc() {
        let line = r#"{"id":1,"method":"x"}"#;
        let err = parse_stdio_message(line).unwrap_err();
        assert!(matches!(err, FramingError::MissingVersion));
    }

    #[test]
    fn test_parse_wrong_version() {
        let line = r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#;
        let err = parse_stdio_message(line).unwrap_err();
        assert!(
            matches!(err, FramingError::UnsupportedVersion { ref version } if version == "1.0")
        );
    }

    #[test]
    fn test_parse_batch_array() {
        let line = r#"[{"jsonrpc":"2.0","id":1,"method":"x"}]"#;
        let err = parse_stdio_message(line).unwrap_err();
        assert!(matches!(err, FramingError::UnsupportedBatch));
    }

    #[test]
    fn test_parse_empty_line() {
        let err = parse_stdio_message("").unwrap_err();
        assert!(
            matches!(err, FramingError::MalformedJson { ref reason } if reason == "empty message")
        );

        let err = parse_stdio_message("  \n  ").unwrap_err();
        assert!(
            matches!(err, FramingError::MalformedJson { ref reason } if reason == "empty message")
        );
    }

    /// EC-STDIO-032: Large base64 tool response (~5MB) under limit is accepted.
    #[test]
    fn test_parse_large_valid_message() {
        // ~5 MB base64-like payload in the result field — should be accepted.
        let payload = "A".repeat(5 * 1024 * 1024);
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":{{"data":"{}"}}}}"#,
            payload
        );
        let msg = parse_stdio_message(&line).unwrap();
        assert_eq!(
            msg.kind,
            JsonRpcMessageKind::Response {
                id: JsonRpcId::Number(1),
            }
        );
    }

    #[test]
    fn test_parse_preserves_raw() {
        let line = r#"{"jsonrpc":"2.0","method":"initialized"}"#;
        let msg = parse_stdio_message(line).unwrap();
        assert_eq!(msg.raw, line);
    }

    #[test]
    fn test_parse_whitespace_trimmed() {
        let line = "  {\"jsonrpc\":\"2.0\",\"method\":\"initialized\"}  \n  ";
        let msg = parse_stdio_message(line).unwrap();
        assert_eq!(
            msg.kind,
            JsonRpcMessageKind::Notification {
                method: "initialized".to_string(),
            }
        );
        // raw preserves original (untrimmed) input.
        assert_eq!(msg.raw, line);
    }

    #[test]
    fn test_parse_extracts_params() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"/tmp/x"}}}"#;
        let msg = parse_stdio_message(line).unwrap();
        let params = msg.params.unwrap();
        assert_eq!(params.get("name").unwrap().as_str().unwrap(), "write_file");
    }

    // ─────────────────────────────────────────────────────────────────────
    // detect_smuggling tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn test_smuggling_detected() {
        // Literal 0x0A byte followed by a smuggled JSON-RPC message.
        let mut raw = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#.to_vec();
        raw.push(b'\n');
        raw.extend_from_slice(br#"{"jsonrpc":"2.0","id":2,"method":"evil/call"}"#);
        assert!(detect_smuggling(&raw));
    }

    #[test]
    fn test_smuggling_not_detected_escaped() {
        // Escaped \\n inside a JSON string value — no literal 0x0A byte.
        let raw =
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"data":"line1\\nline2"}}"#;
        assert!(!detect_smuggling(raw));
    }

    #[test]
    fn test_smuggling_not_detected_no_jsonrpc() {
        // Literal newline but the second segment has no `jsonrpc` key.
        let mut raw = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#.to_vec();
        raw.push(b'\n');
        raw.extend_from_slice(br#"{"data":"hello"}"#);
        assert!(!detect_smuggling(&raw));
    }

    #[test]
    fn test_smuggling_not_detected_single_line() {
        let raw = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#;
        assert!(!detect_smuggling(raw));
    }

    #[test]
    fn test_parse_no_id_no_method() {
        // EC-STDIO-017: Valid JSON with jsonrpc:"2.0" but neither id nor method.
        // Should be classified as Unclassifiable → MalformedJson.
        let line = r#"{"jsonrpc":"2.0"}"#;
        let err = parse_stdio_message(line).unwrap_err();
        assert!(
            matches!(err, FramingError::MalformedJson { ref reason } if reason.contains("neither id nor method")),
            "expected MalformedJson for unclassifiable message, got: {err:?}"
        );
    }

    // EC-STDIO-016: Missing jsonrpc field.
    // (Also covered by test_parse_missing_jsonrpc above — explicit tag here.)

    /// EC-STDIO-027: Partial line (no trailing newline) parses correctly.
    #[test]
    fn test_parse_no_trailing_newline_ec027() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#;
        assert!(parse_stdio_message(line).is_ok());
    }

    /// EC-STDIO-030: Protocol version mismatch passes through transparently.
    #[test]
    fn test_version_mismatch_passthrough_ec030() {
        let line =
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"99.0"}}"#;
        assert!(parse_stdio_message(line).is_ok());
    }

    #[test]
    fn test_smuggling_empty_after_newline() {
        // Trailing newline only — no smuggled content.
        let mut raw = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#.to_vec();
        raw.push(b'\n');
        assert!(!detect_smuggling(&raw));
    }
}
