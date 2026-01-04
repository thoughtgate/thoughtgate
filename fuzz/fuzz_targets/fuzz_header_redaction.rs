#![no_main]

//! Fuzz target for header redaction logic
//!
//! # Traceability
//! - Tests: logging_layer::sanitize_headers
//! - Attack surface: Malformed UTF-8, CRLF injection, long headers

use libfuzzer_sys::fuzz_target;
use arbitrary::Arbitrary;
use http::HeaderMap;
use thoughtgate::logging_layer;

#[derive(Arbitrary, Debug)]
struct FuzzHeaders {
    headers: Vec<(Vec<u8>, Vec<u8>)>,  // (name, value) pairs
}

fuzz_target!(|input: FuzzHeaders| {
    fuzz_header_sanitization(input);
});

fn fuzz_header_sanitization(input: FuzzHeaders) {
    let mut header_map = HeaderMap::new();
    
    // Try to insert each fuzzy header
    for (name_bytes, value_bytes) in input.headers.iter().take(100) {  // Limit to 100 headers
        // Attempt to create header name and value
        if let Ok(name) = http::header::HeaderName::from_bytes(name_bytes) {
            // Attempt to create header value (can be non-UTF8)
            if let Ok(value) = http::header::HeaderValue::from_bytes(value_bytes) {
                header_map.insert(name, value);
            }
        }
    }
    
    // Test 1: sanitize_headers should never panic
    let sanitized = logging_layer::sanitize_headers(&header_map);
    
    // Test 2: Debug formatting should never panic (even with invalid UTF-8)
    let sanitized_str = format!("{:?}", sanitized);
    
    // Test 3: Sensitive header detection
    // Verify that sensitive headers are properly redacted
    for sensitive in logging_layer::SENSITIVE_HEADERS {
        if let Some(value) = header_map.get(*sensitive) {
            // Only verify redaction for valid UTF-8 values that are non-empty
            if let Ok(val_str) = value.to_str() {
                if !val_str.is_empty() && val_str != "[REDACTED]" {
                    // Check that the specific pattern "header_name": "actual_value" doesn't appear
                    // Need to account for various formatting (with/without spaces)
                    let leak_patterns = [
                        format!("\"{}\": \"{}\"", sensitive, val_str),
                        format!("\"{}\":\"{}\"", sensitive, val_str),
                        format!("{}\"{}\"", sensitive, val_str),
                    ];
                    
                    let is_leaked = leak_patterns.iter().any(|pattern| sanitized_str.contains(pattern));
                    
                    // Also verify "[REDACTED]" appears for this header
                    let redaction_patterns = [
                        format!("\"{}\": \"[REDACTED]\"", sensitive),
                        format!("\"{}\":\"[REDACTED]\"", sensitive),
                        format!("\"{}\": \"<binary:", sensitive),
                    ];
                    
                    let is_redacted = redaction_patterns.iter().any(|pattern| sanitized_str.contains(pattern));
                    
                    assert!(
                        !is_leaked && is_redacted,
                        "Sensitive header '{}' with value '{}' not properly redacted.\nExpected redaction pattern, found: {}",
                        sensitive,
                        if val_str.len() > 50 { &val_str[..50] } else { val_str },
                        sanitized_str
                    );
                }
            }
        }
    }
}

