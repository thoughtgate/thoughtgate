#![no_main]

//! Fuzz target for URI extraction and parsing logic
//!
//! # Traceability
//! - Tests: ProxyService::extract_target_uri
//! - Attack surface: Malformed URLs, Host header injection, special characters

use libfuzzer_sys::fuzz_target;
use arbitrary::Arbitrary;
use thoughtgate::proxy_service::ProxyService;
use hyper::{Request, Method};

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    uri_bytes: Vec<u8>,
    host_header: Option<Vec<u8>>,
    has_scheme: bool,
}

fuzz_target!(|input: FuzzInput| {
    fuzz_uri_extraction(input);
});

fn fuzz_uri_extraction(input: FuzzInput) {
    // Create ProxyService with no upstream (forward proxy mode)
    let service = match ProxyService::new_with_upstream(None) {
        Ok(s) => s,
        Err(_) => return,  // TLS init failure, skip
    };

    // Try to construct a URI from fuzz input
    let uri_str = String::from_utf8_lossy(&input.uri_bytes);
    
    // Build request with fuzzy URI
    let mut builder = Request::builder()
        .method(Method::GET);
    
    // Add scheme if specified
    if input.has_scheme {
        builder = builder.uri(uri_str.as_ref());
    } else {
        // Relative URI - need Host header
        builder = builder.uri("/path");
    }
    
    // Add fuzzy Host header if provided
    if let Some(host_bytes) = input.host_header {
        if let Ok(host_str) = std::str::from_utf8(&host_bytes) {
            builder = builder.header("host", host_str);
        }
    }
    
    // Try to build request
    let req = match builder.body(()) {
        Ok(r) => r,
        Err(_) => return,  // Invalid request, skip
    };
    
    // The critical test: extract_target_uri should never panic
    let _ = service.extract_target_uri(&req);
    
    // Success if we got here without panic
}

