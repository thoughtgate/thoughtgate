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
        // Attempt to create header name
        if let Ok(name_str) = std::str::from_utf8(name_bytes) {
            if let Ok(name) = http::header::HeaderName::from_bytes(name_bytes) {
                // Attempt to create header value (can be non-UTF8)
                if let Ok(value) = http::header::HeaderValue::from_bytes(value_bytes) {
                    header_map.insert(name, value);
                }
            }
        }
    }
    
    // Test 1: sanitize_headers should never panic
    let sanitized = logging_layer::sanitize_headers(&header_map);
    
    // Test 2: Debug formatting should never panic (even with invalid UTF-8)
    let _ = format!("{:?}", sanitized);
    
    // Test 3: Sensitive header detection
    for sensitive in logging_layer::SENSITIVE_HEADERS {
        if let Some(value) = header_map.get(*sensitive) {
            let sanitized_str = format!("{:?}", sanitized);
            // Verify sensitive values are redacted
            if let Ok(val_str) = value.to_str() {
                assert!(!sanitized_str.contains(val_str) || val_str == "[REDACTED]",
                        "Sensitive header '{}' not redacted", sensitive);
            }
        }
    }
}

