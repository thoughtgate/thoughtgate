#![no_main]

//! Fuzz target for HTTP request parsing and URI extraction
//!
//! # Traceability
//! - Tests: ProxyService::extract_target_uri, is_hop_by_hop_header
//! - Attack surface: Malformed URIs, Host header injection, invalid methods

use arbitrary::Arbitrary;
use http::{Method, Version};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct FuzzRequest {
    method_bytes: Vec<u8>,
    uri_bytes: Vec<u8>,
    headers: Vec<(Vec<u8>, Vec<u8>)>,
}

fuzz_target!(|input: FuzzRequest| {
    // CRITICAL: Use block_in_place for sync fuzzer with async code
    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(async { fuzz_request_handling(input).await }));
});

async fn fuzz_request_handling(input: FuzzRequest) {
    use hyper::Request;
    use thoughtgate_proxy::proxy_service::ProxyService;

    // Parse method
    let method = if let Ok(method_str) = std::str::from_utf8(&input.method_bytes) {
        method_str.parse::<Method>().unwrap_or(Method::GET)
    } else {
        Method::GET
    };

    // Parse URI
    let uri_str = String::from_utf8_lossy(&input.uri_bytes);

    // Build request
    let mut builder = Request::builder().method(method).version(Version::HTTP_11);

    // Add URI (might be invalid)
    if uri_str.starts_with("http://") || uri_str.starts_with("https://") {
        builder = builder.uri(uri_str.as_ref());
    } else {
        builder = builder.uri("/");
        // Add Host header for relative URIs
        builder = builder.header("host", "example.com");
    }

    // Add fuzzy headers (limit to prevent DOS)
    for (name, value) in input.headers.iter().take(50) {
        if let (Ok(n), Ok(v)) = (
            http::header::HeaderName::from_bytes(name),
            http::header::HeaderValue::from_bytes(value),
        ) {
            builder = builder.header(n, v);
        }
    }

    // Build request with empty body for testing URI extraction
    let req = match builder.body(()) {
        Ok(r) => r,
        Err(_) => return,
    };

    // Create service instances for testing
    // Test 1: Forward proxy mode
    if let Ok(service) = ProxyService::new_with_upstream(None) {
        // Test extract_target_uri - should never panic
        let _ = service.extract_target_uri(&req);
    }

    // Test 2: Reverse proxy mode
    if let Ok(service) = ProxyService::new_with_upstream(Some("http://localhost:9999".to_string()))
    {
        // Test extract_target_uri in reverse proxy mode - should never panic
        let _ = service.extract_target_uri(&req);
    }

    // Test 3: Header filtering logic
    use thoughtgate_proxy::proxy_service::is_hop_by_hop_header;
    for (name, _value) in req.headers().iter() {
        let _ = is_hop_by_hop_header(name.as_str());
    }
}
