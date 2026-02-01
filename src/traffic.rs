//! Traffic type discrimination for routing requests.
//!
//! # Traceability
//! - Implements: REQ-CORE-003/F-002 (MCP Traffic Detection)
//!
//! # Overview
//!
//! ThoughtGate handles two types of traffic:
//!
//! 1. **MCP Traffic** - JSON-RPC requests to the Model Context Protocol
//!    - Buffered for inspection and policy evaluation
//!    - Routed through CedarEngine for authorization
//!
//! 2. **HTTP Traffic** - All other HTTP requests
//!    - Zero-copy streaming passthrough to upstream
//!    - No inspection or buffering overhead
//!
//! # Detection Strategy
//!
//! MCP traffic is detected using header-only checks (no body parsing):
//! - HTTP method is POST
//! - Content-Type contains "application/json"
//! - Path is "/mcp/v1" or "/" (root path for simple deployments)

use hyper::{Method, Request, header};

/// Type of traffic for routing decisions.
///
/// Implements: REQ-CORE-003/F-002 (Traffic Classification)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficType {
    /// MCP JSON-RPC traffic - buffered for inspection.
    ///
    /// This traffic type triggers:
    /// - Request body buffering
    /// - JSON-RPC parsing
    /// - Cedar policy evaluation
    /// - Task handler routing
    Mcp,

    /// Regular HTTP traffic - zero-copy passthrough.
    ///
    /// This traffic type uses:
    /// - Streaming body forwarding (no buffering)
    /// - Direct upstream connection
    /// - Minimal latency overhead
    Http,
}

/// Discriminate traffic type from HTTP request headers.
///
/// This is a fast, header-only check that determines routing without
/// parsing the request body. Performance is critical here as this
/// runs on every request.
///
/// # Detection Criteria
///
/// Traffic is classified as MCP if ALL of the following are true:
/// 1. HTTP method is POST
/// 2. Content-Type header contains "application/json"
/// 3. Path is "/mcp/v1" or "/"
///
/// Everything else is classified as HTTP for zero-copy passthrough.
///
/// # Arguments
///
/// * `req` - Reference to the incoming HTTP request
///
/// # Returns
///
/// `TrafficType::Mcp` for MCP JSON-RPC traffic, `TrafficType::Http` otherwise.
///
/// # Performance
///
/// This function is O(1) with respect to request body size:
/// - No body reading or parsing
/// - Simple header string comparisons
/// - Short-circuit evaluation on first non-match
///
/// # Example
///
/// ```rust
/// use hyper::{Request, Method};
/// use thoughtgate::traffic::{TrafficType, discriminate_traffic};
///
/// let mcp_request = Request::builder()
///     .method(Method::POST)
///     .uri("/mcp/v1")
///     .header("content-type", "application/json")
///     .body(())
///     .unwrap();
///
/// assert_eq!(discriminate_traffic(&mcp_request), TrafficType::Mcp);
///
/// let http_request = Request::builder()
///     .method(Method::GET)
///     .uri("/api/status")
///     .body(())
///     .unwrap();
///
/// assert_eq!(discriminate_traffic(&http_request), TrafficType::Http);
/// ```
///
/// # Traceability
/// - Implements: REQ-CORE-003/F-002 (Method Routing)
pub fn discriminate_traffic<B>(req: &Request<B>) -> TrafficType {
    // Check 1: Must be POST method
    // MCP JSON-RPC only uses POST
    if req.method() != Method::POST {
        return TrafficType::Http;
    }

    // Check 2: Must have application/json Content-Type
    // This avoids buffering non-JSON POST requests
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Case-insensitive check per RFC 7231 Section 3.1.1.1
    if !content_type
        .to_ascii_lowercase()
        .contains("application/json")
    {
        return TrafficType::Http;
    }

    // Check 3: Path must be MCP endpoint
    // Standard: /mcp/v1
    // Also accept "/" for simple single-upstream deployments
    // Normalize: collapse consecutive slashes, trim trailing slash
    let path = req.uri().path();
    let normalized = normalize_path(path);
    let normalized = normalized.as_str();
    if normalized == "/mcp/v1" || normalized == "/" {
        return TrafficType::Mcp;
    }

    // Default: HTTP passthrough
    TrafficType::Http
}

/// Check if a request is an MCP request.
///
/// Convenience wrapper around `discriminate_traffic`.
///
/// # Example
///
/// ```rust
/// use hyper::{Request, Method};
/// use thoughtgate::traffic::is_mcp_request;
///
/// let req = Request::builder()
///     .method(Method::POST)
///     .uri("/mcp/v1")
///     .header("content-type", "application/json")
///     .body(())
///     .unwrap();
///
/// assert!(is_mcp_request(&req));
/// ```
#[inline]
pub fn is_mcp_request<B>(req: &Request<B>) -> bool {
    discriminate_traffic(req) == TrafficType::Mcp
}

/// Normalize a URL path by collapsing consecutive slashes and trimming trailing slash.
///
/// Prevents path confusion bypasses where `//mcp/v1` or `/mcp/v1/` would evade
/// exact-match detection.
fn normalize_path(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut prev_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                result.push('/');
            }
            prev_slash = true;
        } else {
            result.push(ch);
            prev_slash = false;
        }
    }
    // Trim trailing slash (but keep "/" as-is)
    if result.len() > 1 && result.ends_with('/') {
        result.pop();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::Request;

    /// Test: POST /mcp/v1 with application/json → MCP
    #[test]
    fn test_mcp_standard_endpoint() {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v1")
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Mcp);
        assert!(is_mcp_request(&req));
    }

    /// Test: POST / with application/json → MCP (root fallback)
    #[test]
    fn test_mcp_root_endpoint() {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/")
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Mcp);
    }

    /// Test: POST /mcp/v1 with charset → MCP (Content-Type with params)
    #[test]
    fn test_mcp_content_type_with_charset() {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v1")
            .header("content-type", "application/json; charset=utf-8")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Mcp);
    }

    /// Test: GET /mcp/v1 → HTTP (wrong method)
    #[test]
    fn test_http_wrong_method() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/mcp/v1")
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
        assert!(!is_mcp_request(&req));
    }

    /// Test: POST /mcp/v1 without Content-Type → HTTP
    #[test]
    fn test_http_missing_content_type() {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v1")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
    }

    /// Test: POST /mcp/v1 with wrong Content-Type → HTTP
    #[test]
    fn test_http_wrong_content_type() {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v1")
            .header("content-type", "text/plain")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
    }

    /// Test: POST /other/path with application/json → HTTP (wrong path)
    #[test]
    fn test_http_wrong_path() {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/chat")
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
    }

    /// Test: POST /mcp/v2 → HTTP (different version)
    #[test]
    fn test_http_different_mcp_version() {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v2")
            .header("content-type", "application/json")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
    }

    /// Test: Various HTTP methods → HTTP
    #[test]
    fn test_http_various_methods() {
        for method in [
            Method::GET,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
            Method::HEAD,
        ] {
            let req = Request::builder()
                .method(method.clone())
                .uri("/mcp/v1")
                .header("content-type", "application/json")
                .body(())
                .unwrap();

            assert_eq!(
                discriminate_traffic(&req),
                TrafficType::Http,
                "Expected HTTP for method {}",
                method
            );
        }
    }

    /// Test: POST with form data → HTTP
    #[test]
    fn test_http_form_data() {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v1")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
    }

    /// Test: POST with multipart → HTTP
    #[test]
    fn test_http_multipart() {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/mcp/v1")
            .header("content-type", "multipart/form-data; boundary=something")
            .body(())
            .unwrap();

        assert_eq!(discriminate_traffic(&req), TrafficType::Http);
    }

    /// Test: Content-Type case insensitivity (RFC 7231)
    #[test]
    fn test_mcp_content_type_case_insensitive() {
        for ct in [
            "Application/JSON",
            "APPLICATION/JSON",
            "application/JSON",
            "Application/Json; charset=utf-8",
        ] {
            let req = Request::builder()
                .method(Method::POST)
                .uri("/mcp/v1")
                .header("content-type", ct)
                .body(())
                .unwrap();

            assert_eq!(
                discriminate_traffic(&req),
                TrafficType::Mcp,
                "Expected MCP for Content-Type: {}",
                ct
            );
        }
    }

    /// Test: Path normalization (trailing slash, double slashes)
    #[test]
    fn test_mcp_path_normalization() {
        for path in ["/mcp/v1/", "//mcp/v1", "/mcp//v1", "//mcp//v1//"] {
            let req = Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("content-type", "application/json")
                .body(())
                .unwrap();

            assert_eq!(
                discriminate_traffic(&req),
                TrafficType::Mcp,
                "Expected MCP for path: {}",
                path
            );
        }
    }

    /// Test: normalize_path helper
    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("/mcp/v1"), "/mcp/v1");
        assert_eq!(normalize_path("/mcp/v1/"), "/mcp/v1");
        assert_eq!(normalize_path("//mcp/v1"), "/mcp/v1");
        assert_eq!(normalize_path("/mcp//v1"), "/mcp/v1");
        assert_eq!(normalize_path("//mcp//v1//"), "/mcp/v1");
    }

    /// Test: TrafficType Debug and Clone
    #[test]
    fn test_traffic_type_traits() {
        let mcp = TrafficType::Mcp;
        let http = TrafficType::Http;

        // Debug
        assert_eq!(format!("{:?}", mcp), "Mcp");
        assert_eq!(format!("{:?}", http), "Http");

        // Clone
        let mcp_clone = mcp;
        assert_eq!(mcp, mcp_clone);

        // Eq
        assert_ne!(mcp, http);
    }
}
