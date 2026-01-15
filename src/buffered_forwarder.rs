//! Buffered Forwarder for Amber Path traffic.
//!
//! # v0.1 Status: DEFERRED
//!
//! This module implements the "Amber Path" (buffered inspection) which is **deferred**
//! to v0.2+. In v0.1, responses are passed through directly without inspection.
//!
//! The inspection infrastructure is retained for when PII detection or response
//! validation is needed.
//!
//! # When to Activate (v0.2+)
//!
//! - When PII detection/redaction is needed
//! - When schema validation is required
//! - When request/response transformation is needed
//!
//! # Original Design
//!
//! Traffic enters this path when the Governance Engine returns `Decision::Inspect`.
//! The Amber Path prioritizes **Safety** and **Visibility** over raw latency by
//! buffering and inspecting payloads before forwarding.
//!
//! # Key Features
//!
//! - **Safe Buffering**: Size-limited accumulation with `http_body_util::Limited`
//! - **Concurrency Control**: Global semaphore to prevent OOM attacks
//! - **Timeout Protection**: Entire lifecycle wrapped in timeout (Slowloris defense)
//! - **Zero-Copy When Possible**: Uses `Cow<'_, [u8]>` for efficient memory handling
//! - **Inspector Chain**: Executes inspectors in order with short-circuit on rejection
//!
//! # Traceability
//! - Deferred: REQ-CORE-002 (Buffered Termination Strategy)

use std::borrow::Cow;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::{FutureExt, stream};
use http::{HeaderMap, Request, Response};
use http_body::Frame;
use http_body_util::{BodyExt, LengthLimitError, Limited, StreamBody};
use hyper::body::Incoming;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tracing::{debug, error, info, instrument, warn};

use crate::error::{ProxyError, ProxyResult};
use crate::inspector::{Decision, InspectionContext, Inspector};
use crate::metrics::{AmberPathTimer, InspectorTimer, get_amber_metrics};
use crate::proxy_config::ProxyConfig;

/// Helper type alias for bodies that may include trailers.
///
/// Uses `StreamBody` to support both data frames and trailer frames.
type BodyWithTrailers =
    StreamBody<stream::Iter<std::vec::IntoIter<Result<Frame<Bytes>, std::convert::Infallible>>>>;

/// Create a body from bytes and optional trailers.
///
/// This helper ensures trailers are preserved per REQ-CORE-002 F-005.
fn body_with_optional_trailers(data: Bytes, trailers: Option<HeaderMap>) -> BodyWithTrailers {
    let mut frames = vec![Ok(Frame::data(data))];
    if let Some(t) = trailers {
        frames.push(Ok(Frame::trailers(t)));
    }
    StreamBody::new(stream::iter(frames))
}

/// The Buffered Forwarder handles Amber Path traffic.
///
/// This struct manages the buffering, inspection, and forwarding of
/// request/response bodies that require policy validation.
///
/// # Concurrency Model
///
/// The forwarder uses a global semaphore to limit concurrent buffered
/// operations. If the semaphore is exhausted, new requests receive
/// `503 Service Unavailable` immediately.
///
/// # Memory Safety
///
/// All body accumulation is bounded by configurable limits:
/// - Request bodies: `req_buffer_max` (default 2MB)
/// - Response bodies: `resp_buffer_max` (default 10MB)
///
/// # Traceability
/// - Implements: REQ-CORE-002 F-001 (Safe Buffering with Timeout)
/// - Implements: REQ-CORE-002 F-004 (Chain Semantics)
#[derive(Clone)]
pub struct BufferedForwarder {
    /// Shared configuration
    config: ProxyConfig,

    /// Global semaphore for concurrent buffer limit
    semaphore: Arc<Semaphore>,

    /// Registered inspectors (executed in order)
    inspectors: Arc<Vec<Arc<dyn Inspector>>>,
}

impl BufferedForwarder {
    /// Create a new BufferedForwarder with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Proxy configuration including buffer limits and timeouts
    ///
    /// # Returns
    ///
    /// A new `BufferedForwarder` instance with an empty inspector chain.
    pub fn new(config: ProxyConfig) -> Self {
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_buffers));
        Self {
            config,
            semaphore,
            inspectors: Arc::new(Vec::new()),
        }
    }

    /// Create a BufferedForwarder with pre-registered inspectors.
    ///
    /// # Arguments
    ///
    /// * `config` - Proxy configuration
    /// * `inspectors` - Vector of inspectors to execute on each payload
    pub fn with_inspectors(config: ProxyConfig, inspectors: Vec<Arc<dyn Inspector>>) -> Self {
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_buffers));
        Self {
            config,
            semaphore,
            inspectors: Arc::new(inspectors),
        }
    }

    /// Register an inspector to the chain.
    ///
    /// Inspectors are executed in the order they are registered.
    pub fn add_inspector(&mut self, inspector: Arc<dyn Inspector>) {
        Arc::make_mut(&mut self.inspectors).push(inspector);
    }

    /// Process a request body through the Amber Path.
    ///
    /// This method:
    /// 1. Acquires a semaphore permit (or returns 503)
    /// 2. Buffers the body with size limits (or returns 413)
    /// 3. Runs the inspector chain with timeout (or returns 408)
    /// 4. Returns the (possibly modified) body
    ///
    /// # Arguments
    ///
    /// * `req` - The incoming request with body
    ///
    /// # Returns
    ///
    /// The request with buffered body, ready for forwarding.
    ///
    /// # Errors
    ///
    /// - `BufferSemaphoreExhausted` - Too many concurrent buffered requests
    /// - `PayloadTooLarge` - Request body exceeds `req_buffer_max`
    /// - `BufferTimeout` - Operation exceeded `buffer_timeout`
    /// - `Rejected` - Inspector rejected the payload
    /// - `InspectorPanic` - Inspector panicked during execution
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 F-001 (Safe Buffering with Timeout)
    /// - Implements: REQ-CORE-002 NFR-001 (Observability)
    #[instrument(skip(self, req), fields(path = %req.uri().path()))]
    pub async fn process_request(
        &self,
        req: Request<Incoming>,
    ) -> ProxyResult<Request<BodyWithTrailers>> {
        // 1. Try to acquire semaphore permit FIRST (before starting timer)
        let _permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                warn!("Amber Path semaphore exhausted, rejecting request");
                if let Some(m) = get_amber_metrics() {
                    m.record_error("semaphore");
                }
                return Err(ProxyError::BufferSemaphoreExhausted);
            }
        };

        // Start metrics timer AFTER acquiring permit
        let metrics = get_amber_metrics();
        let timer = metrics.clone().map(AmberPathTimer::new);

        debug!("Acquired Amber Path permit, processing request");

        // Split request into parts and body
        let (mut parts, body) = req.into_parts();

        // 2. Wrap entire operation in timeout
        let result = timeout(self.config.buffer_timeout, async {
            self.buffer_and_inspect_body(body, InspectionContext::Request(&parts), true)
                .await
        })
        .await;

        match result {
            Ok(Ok((buffered_body, trailers))) => {
                // Record success metrics
                if let Some(t) = timer {
                    t.finish_success(buffered_body.len() as u64);
                }

                // 3. Update Content-Length if body was modified
                // (Inspectors may have changed the body size)
                parts.headers.remove(http::header::CONTENT_LENGTH);
                parts
                    .headers
                    .insert(http::header::CONTENT_LENGTH, buffered_body.len().into());

                // 4. Reconstruct request with buffered body and trailers (REQ-CORE-002 F-005)
                let body = body_with_optional_trailers(buffered_body, trailers);
                Ok(Request::from_parts(parts, body))
            }
            Ok(Err(e)) => {
                // Record error type from ProxyError
                if let Some(t) = timer {
                    let error_type = match &e {
                        ProxyError::PayloadTooLarge(_, _) => "limit",
                        ProxyError::Rejected(_, _) => "rejected",
                        ProxyError::InspectorPanic(_) => "panic",
                        ProxyError::InspectorError(_, _) => "error",
                        _ => "error",
                    };
                    t.finish_error(error_type);
                }
                Err(e)
            }
            Err(_) => {
                warn!("Amber Path buffer timeout expired");
                if let Some(t) = timer {
                    t.finish_error("timeout");
                }
                Err(ProxyError::BufferTimeout(
                    "Request buffering timed out".to_string(),
                ))
            }
        }
    }

    /// Process a response body through the Amber Path.
    ///
    /// Similar to `process_request`, but for response bodies.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 F-001 (Safe Buffering with Timeout)
    /// - Implements: REQ-CORE-002 NFR-001 (Observability)
    #[instrument(skip(self, res), fields(status = %res.status()))]
    pub async fn process_response(
        &self,
        res: Response<Incoming>,
    ) -> ProxyResult<Response<BodyWithTrailers>> {
        // 1. Try to acquire semaphore permit FIRST (before starting timer)
        let _permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                warn!("Amber Path semaphore exhausted, rejecting response");
                if let Some(m) = get_amber_metrics() {
                    m.record_error("semaphore");
                }
                return Err(ProxyError::BufferSemaphoreExhausted);
            }
        };

        // Start metrics timer AFTER acquiring permit
        let metrics = get_amber_metrics();
        let timer = metrics.clone().map(AmberPathTimer::new);

        debug!("Acquired Amber Path permit, processing response");

        // Split response into parts and body
        let (mut parts, body) = res.into_parts();

        // Check for compressed response (must reject per REQ-CORE-002 Section 3.3)
        if let Some(encoding) = parts.headers.get(http::header::CONTENT_ENCODING) {
            let encoding_str = encoding.to_str().unwrap_or("unknown");
            if encoding_str != "identity" {
                if let Some(t) = timer {
                    t.finish_error("compressed");
                }
                return Err(ProxyError::CompressedResponse(encoding_str.to_string()));
            }
        }

        // 2. Wrap entire operation in timeout
        let result = timeout(self.config.buffer_timeout, async {
            self.buffer_and_inspect_body(body, InspectionContext::Response(&parts), false)
                .await
        })
        .await;

        match result {
            Ok(Ok((buffered_body, trailers))) => {
                // Record success metrics
                if let Some(t) = timer {
                    t.finish_success(buffered_body.len() as u64);
                }

                // 3. Update Content-Length if body was modified
                // (Inspectors may have changed the body size)
                parts.headers.remove(http::header::CONTENT_LENGTH);
                parts
                    .headers
                    .insert(http::header::CONTENT_LENGTH, buffered_body.len().into());

                // 4. Reconstruct response with buffered body and trailers (REQ-CORE-002 F-005)
                let body = body_with_optional_trailers(buffered_body, trailers);
                Ok(Response::from_parts(parts, body))
            }
            Ok(Err(e)) => {
                // Record error type from ProxyError
                if let Some(t) = timer {
                    let error_type = match &e {
                        ProxyError::PayloadTooLarge(_, _) => "limit",
                        ProxyError::Rejected(_, _) => "rejected",
                        ProxyError::InspectorPanic(_) => "panic",
                        ProxyError::InspectorError(_, _) => "error",
                        _ => "error",
                    };
                    t.finish_error(error_type);
                }
                Err(e)
            }
            Err(_) => {
                warn!("Amber Path buffer timeout expired");
                if let Some(t) = timer {
                    t.finish_error("timeout");
                }
                Err(ProxyError::BufferTimeout(
                    "Response buffering timed out".to_string(),
                ))
            }
        }
    }

    /// Buffer and inspect a body.
    ///
    /// This is the core buffering logic shared between request and response processing.
    ///
    /// # Arguments
    ///
    /// * `body` - The incoming body stream
    /// * `ctx` - Inspection context (request or response parts)
    /// * `is_request` - Whether this is a request body (for size limit selection)
    ///
    /// # Returns
    ///
    /// The buffered (and possibly modified) body bytes and optional trailers.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 F-001 (Safe Buffering)
    /// - Implements: REQ-CORE-002 F-004 (Chain Semantics)
    /// - Implements: REQ-CORE-002 F-005 (Trailer Preservation)
    async fn buffer_and_inspect_body(
        &self,
        body: Incoming,
        ctx: InspectionContext<'_>,
        is_request: bool,
    ) -> ProxyResult<(Bytes, Option<HeaderMap>)> {
        let limit = if is_request {
            self.config.req_buffer_max
        } else {
            self.config.resp_buffer_max
        };

        // 1. Buffer body with size limit (F-001)
        let limited_body = Limited::new(body, limit);
        let collected = limited_body.collect().await.map_err(|e| {
            // Type-safe check for size limit error
            if e.downcast_ref::<LengthLimitError>().is_some() {
                warn!(limit = limit, "Payload exceeded buffer limit");
                // Both args are identical because Limited doesn't expose the actual size
                ProxyError::PayloadTooLarge(limit, limit)
            } else {
                error!(error = %e, "Failed to buffer body");
                ProxyError::Client(e.to_string())
            }
        })?;

        // Preserve trailers per REQ-CORE-002 F-005 (extract before consuming collected)
        let trailers = collected.trailers().cloned();
        let original_bytes = collected.to_bytes();

        // 2. Handle empty body case (F-005)
        // Still run inspectors with empty slice per spec
        if original_bytes.is_empty() {
            debug!("Empty body, running inspectors with empty slice");
            let result = self.run_inspector_chain(&[], ctx).await?;
            let final_bytes = result.unwrap_or_else(|| original_bytes.clone());
            return Ok((final_bytes, trailers));
        }

        // 3. Run inspector chain (F-004)
        let result = self.run_inspector_chain(&original_bytes, ctx).await?;

        // 4. Return original or modified bytes, plus trailers
        Ok((result.unwrap_or(original_bytes), trailers))
    }

    /// Run the inspector chain on a payload.
    ///
    /// # Chain Semantics (F-004)
    ///
    /// - Inspectors are executed in registration order
    /// - If any inspector returns `Reject`, the chain halts immediately
    /// - If Inspector A returns `Modify(Bytes)`, Inspector B receives the new bytes
    /// - If all return `Approve`, `None` is returned (use original buffer)
    ///
    /// # Arguments
    ///
    /// * `body` - The payload bytes to inspect
    /// * `ctx` - Inspection context
    ///
    /// # Returns
    ///
    /// - `Ok(None)` - All inspectors approved (zero-copy path)
    /// - `Ok(Some(bytes))` - At least one inspector modified the payload
    /// - `Err(Rejected)` - An inspector rejected the payload
    /// - `Err(InspectorPanic)` - An inspector panicked
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 F-004 (Chain Semantics)
    /// - Implements: REQ-CORE-002 NFR-001 (Observability)
    async fn run_inspector_chain(
        &self,
        body: &[u8],
        ctx: InspectionContext<'_>,
    ) -> ProxyResult<Option<Bytes>> {
        if self.inspectors.is_empty() {
            return Ok(None);
        }

        // Get metrics for inspector timing
        let metrics = get_amber_metrics();

        // Use Cow for zero-copy when possible (Section 7)
        let mut current_bytes: Cow<'_, [u8]> = Cow::Borrowed(body);
        let mut modified_storage: Option<Bytes> = None;

        for inspector in self.inspectors.iter() {
            let inspector_name = inspector.name();
            debug!(inspector = inspector_name, "Running inspector");

            // Start inspector timer
            let timer = metrics
                .clone()
                .map(|m| InspectorTimer::new(m, inspector_name));

            // Run inspector with panic safety (F-002)
            let decision = self
                .run_inspector_safe(inspector.as_ref(), current_bytes.as_ref(), &ctx)
                .await?;

            // Finish timer
            if let Some(t) = timer {
                t.finish();
            }

            match decision {
                Decision::Approve => {
                    debug!(inspector = inspector_name, "Inspector approved");
                    // Record approval metric
                    if let Some(ref m) = metrics {
                        m.record_inspection("approve");
                    }
                    continue;
                }
                Decision::Modify(new_bytes) => {
                    info!(
                        inspector = inspector_name,
                        old_len = current_bytes.len(),
                        new_len = new_bytes.len(),
                        "Inspector modified payload"
                    );
                    // Record modification metric
                    if let Some(ref m) = metrics {
                        m.record_inspection("modify");
                    }
                    // Store the efficient Bytes handle for final return
                    modified_storage = Some(new_bytes.clone());
                    // Update Cow for next inspector in chain
                    current_bytes = Cow::Owned(new_bytes.to_vec());
                }
                Decision::Reject(status) => {
                    warn!(
                        inspector = inspector_name,
                        status = %status,
                        "Inspector rejected payload"
                    );
                    // Record rejection metric
                    if let Some(ref m) = metrics {
                        m.record_inspection("reject");
                    }
                    return Err(ProxyError::Rejected(inspector_name.to_string(), status));
                }
            }
        }

        // Return the stored Bytes (zero-copy if modified) or None (all approved)
        Ok(modified_storage)
    }

    /// Run a single inspector with panic safety.
    ///
    /// # Panic Handling (F-002)
    ///
    /// If the inspector panics:
    /// - The panic is caught using `FutureExt::catch_unwind`
    /// - An error is logged at `ERROR` level (with inspector name, NOT payload content)
    /// - `InspectorPanic` error is returned
    ///
    /// # Implementation Note
    ///
    /// We use `futures::FutureExt::catch_unwind` with `AssertUnwindSafe` to
    /// catch panics in async code. The payload content is NEVER logged to
    /// prevent accidental data exposure in logs.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 F-002 (Panic Safety)
    /// - Implements: REQ-CORE-002 Section 5.1 EC-007
    async fn run_inspector_safe(
        &self,
        inspector: &dyn Inspector,
        body: &[u8],
        ctx: &InspectionContext<'_>,
    ) -> ProxyResult<Decision> {
        let inspector_name = inspector.name();

        // Wrap the async inspection in catch_unwind
        // Note: AssertUnwindSafe is safe here because:
        // 1. We don't share mutable state with the panic handler
        // 2. The inspector trait requires Send + Sync
        // 3. We immediately handle the result after the await
        let result = AssertUnwindSafe(inspector.inspect(body, ctx.clone_for_inspector()))
            .catch_unwind()
            .await;

        match result {
            Ok(Ok(decision)) => Ok(decision),
            Ok(Err(e)) => {
                error!(
                    inspector = inspector_name,
                    error = %e,
                    "Inspector returned error"
                );
                Err(ProxyError::InspectorError(
                    inspector_name.to_string(),
                    e.to_string(),
                ))
            }
            Err(panic_payload) => {
                // Extract panic message if possible (for logging)
                // CRITICAL: Never log body content here!
                let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };

                error!(
                    inspector = inspector_name,
                    panic_message = %panic_msg,
                    "Inspector panicked (payload content redacted)"
                );

                Err(ProxyError::InspectorPanic(inspector_name.to_string()))
            }
        }
    }

    /// Strip Accept-Encoding header for Amber Path requests.
    ///
    /// This ensures upstream doesn't send compressed responses that
    /// we can't inspect.
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 3.3 (Compression Handling)
    pub fn strip_accept_encoding<B>(req: &mut Request<B>) {
        req.headers_mut().remove(http::header::ACCEPT_ENCODING);
    }

    /// Check if a response is compressed.
    ///
    /// Returns `Some(encoding)` if the response has a Content-Encoding
    /// header that indicates compression (gzip, deflate, br, compress, zstd).
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 3.3 (Compression Handling)
    /// - Implements: REQ-CORE-002 Section 5.1 EC-004
    pub fn is_compressed_response<B>(res: &Response<B>) -> Option<&str> {
        res.headers()
            .get(http::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .filter(|encoding| {
                // "identity" means no encoding
                let enc_lower = encoding.to_lowercase();
                enc_lower != "identity"
                    && (enc_lower.contains("gzip")
                        || enc_lower.contains("deflate")
                        || enc_lower.contains("br")
                        || enc_lower.contains("compress")
                        || enc_lower.contains("zstd"))
            })
    }

    /// Prepare a request for Amber Path processing.
    ///
    /// This strips headers that could cause issues during inspection:
    /// - `Accept-Encoding`: Prevents compressed responses
    ///
    /// # Traceability
    /// - Implements: REQ-CORE-002 Section 3.3 (Compression Handling)
    pub fn prepare_request_for_amber_path<B>(req: &mut Request<B>) {
        // Strip Accept-Encoding to ensure we get uncompressed responses
        Self::strip_accept_encoding(req);

        // Note: We don't strip Transfer-Encoding because chunked encoding
        // is handled transparently by http-body-util::collect()
    }

    /// Returns the current number of available semaphore permits.
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Get the semaphore for testing purposes.
    ///
    /// # Note
    ///
    /// This method is intended for testing only.
    pub fn get_semaphore_for_testing(&self) -> Arc<Semaphore> {
        self.semaphore.clone()
    }

    /// Public method for testing the inspector chain directly.
    ///
    /// This is exposed for unit testing edge cases without needing
    /// to construct a full `hyper::body::Incoming`.
    ///
    /// # Note
    ///
    /// This method is intended for testing only.
    pub async fn run_inspector_chain_public(
        &self,
        body: &[u8],
        ctx: InspectionContext<'_>,
    ) -> ProxyResult<Option<Bytes>> {
        self.run_inspector_chain(body, ctx).await
    }
}

// Helper trait to clone InspectionContext for spawned tasks
impl<'a> InspectionContext<'a> {
    /// Create an owned version of the context for use in spawned tasks.
    ///
    /// Note: This is a workaround for the lifetime issues with async inspection.
    /// In practice, inspectors should be fast enough to not require spawning.
    fn clone_for_inspector(&self) -> InspectionContext<'a> {
        match self {
            InspectionContext::Request(parts) => InspectionContext::Request(parts),
            InspectionContext::Response(parts) => InspectionContext::Response(parts),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::NoOpInspector;
    use async_trait::async_trait;
    use http::StatusCode;

    /// Test inspector that always approves
    struct ApproveInspector;

    #[async_trait]
    impl Inspector for ApproveInspector {
        fn name(&self) -> &'static str {
            "approve"
        }

        async fn inspect(
            &self,
            _body: &[u8],
            _ctx: InspectionContext<'_>,
        ) -> Result<Decision, ProxyError> {
            Ok(Decision::Approve)
        }
    }

    /// Test inspector that modifies by appending
    struct AppendInspector(&'static [u8]);

    #[async_trait]
    impl Inspector for AppendInspector {
        fn name(&self) -> &'static str {
            "append"
        }

        async fn inspect(
            &self,
            body: &[u8],
            _ctx: InspectionContext<'_>,
        ) -> Result<Decision, ProxyError> {
            let mut modified = body.to_vec();
            modified.extend_from_slice(self.0);
            Ok(Decision::Modify(Bytes::from(modified)))
        }
    }

    /// Test inspector that rejects
    struct RejectInspector(StatusCode);

    #[async_trait]
    impl Inspector for RejectInspector {
        fn name(&self) -> &'static str {
            "reject"
        }

        async fn inspect(
            &self,
            _body: &[u8],
            _ctx: InspectionContext<'_>,
        ) -> Result<Decision, ProxyError> {
            Ok(Decision::Reject(self.0))
        }
    }

    #[test]
    fn test_buffered_forwarder_creation() {
        let config = ProxyConfig::default();
        let forwarder = BufferedForwarder::new(config.clone());

        assert_eq!(forwarder.available_permits(), config.max_concurrent_buffers);
    }

    #[test]
    fn test_buffered_forwarder_with_inspectors() {
        let config = ProxyConfig::default();
        let inspectors: Vec<Arc<dyn Inspector>> =
            vec![Arc::new(NoOpInspector), Arc::new(ApproveInspector)];

        let forwarder = BufferedForwarder::with_inspectors(config, inspectors);
        assert_eq!(forwarder.inspectors.len(), 2);
    }

    #[tokio::test]
    async fn test_inspector_chain_all_approve() {
        let config = ProxyConfig::default();
        let inspectors: Vec<Arc<dyn Inspector>> =
            vec![Arc::new(ApproveInspector), Arc::new(ApproveInspector)];

        let forwarder = BufferedForwarder::with_inspectors(config, inspectors);

        let req = Request::builder().body(()).unwrap();
        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        let result = forwarder.run_inspector_chain(b"test", ctx).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // None = zero-copy path
    }

    #[tokio::test]
    async fn test_inspector_chain_modification() {
        let config = ProxyConfig::default();
        let inspectors: Vec<Arc<dyn Inspector>> = vec![Arc::new(AppendInspector(b"-suffix"))];

        let forwarder = BufferedForwarder::with_inspectors(config, inspectors);

        let req = Request::builder().body(()).unwrap();
        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        let result = forwarder.run_inspector_chain(b"test", ctx).await;
        assert!(result.is_ok());
        let bytes = result.unwrap();
        assert!(bytes.is_some());
        assert_eq!(&bytes.unwrap()[..], b"test-suffix");
    }

    #[tokio::test]
    async fn test_inspector_chain_rejection() {
        let config = ProxyConfig::default();
        let inspectors: Vec<Arc<dyn Inspector>> = vec![
            Arc::new(ApproveInspector),
            Arc::new(RejectInspector(StatusCode::FORBIDDEN)),
            Arc::new(ApproveInspector), // Should not be reached
        ];

        let forwarder = BufferedForwarder::with_inspectors(config, inspectors);

        let req = Request::builder().body(()).unwrap();
        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        let result = forwarder.run_inspector_chain(b"test", ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::Rejected(name, status) => {
                assert_eq!(name, "reject");
                assert_eq!(status, StatusCode::FORBIDDEN);
            }
            _ => panic!("Expected Rejected error"),
        }
    }

    #[tokio::test]
    async fn test_inspector_chain_modification_propagation() {
        // Test that Inspector B receives modified bytes from Inspector A
        let config = ProxyConfig::default();
        let inspectors: Vec<Arc<dyn Inspector>> = vec![
            Arc::new(AppendInspector(b"-A")),
            Arc::new(AppendInspector(b"-B")),
        ];

        let forwarder = BufferedForwarder::with_inspectors(config, inspectors);

        let req = Request::builder().body(()).unwrap();
        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        let result = forwarder.run_inspector_chain(b"start", ctx).await;
        assert!(result.is_ok());
        let bytes = result.unwrap().unwrap();
        assert_eq!(&bytes[..], b"start-A-B");
    }

    #[test]
    fn test_strip_accept_encoding() {
        let mut req = Request::builder()
            .header(http::header::ACCEPT_ENCODING, "gzip, deflate")
            .body(())
            .unwrap();

        assert!(req.headers().contains_key(http::header::ACCEPT_ENCODING));
        BufferedForwarder::strip_accept_encoding(&mut req);
        assert!(!req.headers().contains_key(http::header::ACCEPT_ENCODING));
    }

    #[tokio::test]
    async fn test_empty_body_inspection() {
        let config = ProxyConfig::default();
        let inspectors: Vec<Arc<dyn Inspector>> = vec![Arc::new(ApproveInspector)];

        let forwarder = BufferedForwarder::with_inspectors(config, inspectors);

        let req = Request::builder().body(()).unwrap();
        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        // Run with empty body
        let result = forwarder.run_inspector_chain(b"", ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_trailer_preservation_helper() {
        use http_body_util::BodyExt;

        // Test the helper function that creates bodies with trailers
        let data = Bytes::from("test data");
        let mut trailers = HeaderMap::new();
        trailers.insert("x-trailer-1", "value1".parse().unwrap());
        trailers.insert("x-trailer-2", "value2".parse().unwrap());

        // Create body with trailers
        let body = body_with_optional_trailers(data.clone(), Some(trailers.clone()));

        // Collect and verify (extract trailers before consuming with to_bytes)
        let collected = body.collect().await.unwrap();
        let collected_trailers = collected.trailers().cloned();
        let collected_data = collected.to_bytes();

        assert_eq!(collected_data, data);
        assert!(collected_trailers.is_some());
        let collected_trailers = collected_trailers.unwrap();
        assert_eq!(collected_trailers.get("x-trailer-1").unwrap(), "value1");
        assert_eq!(collected_trailers.get("x-trailer-2").unwrap(), "value2");
    }

    #[tokio::test]
    async fn test_body_without_trailers() {
        use http_body_util::BodyExt;

        // Test the helper function without trailers
        let data = Bytes::from("test data");
        let body = body_with_optional_trailers(data.clone(), None);

        // Collect and verify (extract trailers before consuming with to_bytes)
        let collected = body.collect().await.unwrap();
        let collected_trailers = collected.trailers().cloned();
        let collected_data = collected.to_bytes();

        assert_eq!(collected_data, data);
        assert!(collected_trailers.is_none());
    }

    /// Test inspector that panics
    struct PanickingInspector;

    #[async_trait]
    impl Inspector for PanickingInspector {
        fn name(&self) -> &'static str {
            "panicking"
        }

        async fn inspect(
            &self,
            _body: &[u8],
            _ctx: InspectionContext<'_>,
        ) -> Result<Decision, ProxyError> {
            panic!("Test panic from inspector");
        }
    }

    #[tokio::test]
    async fn test_inspector_panic_safety() {
        let config = ProxyConfig::default();
        let inspectors: Vec<Arc<dyn Inspector>> = vec![
            Arc::new(ApproveInspector),
            Arc::new(PanickingInspector),
            Arc::new(ApproveInspector), // Should not be reached
        ];

        let forwarder = BufferedForwarder::with_inspectors(config, inspectors);

        let req = Request::builder().body(()).unwrap();
        let (parts, _) = req.into_parts();
        let ctx = InspectionContext::Request(&parts);

        let result = forwarder.run_inspector_chain(b"test", ctx).await;

        // Should return InspectorPanic error, not crash
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::InspectorPanic(name) => {
                assert_eq!(name, "panicking");
            }
            other => panic!("Expected InspectorPanic error, got: {:?}", other),
        }
    }

    #[test]
    fn test_is_compressed_response() {
        // gzip
        let res = Response::builder()
            .header(http::header::CONTENT_ENCODING, "gzip")
            .body(())
            .unwrap();
        assert!(BufferedForwarder::is_compressed_response(&res).is_some());

        // deflate
        let res = Response::builder()
            .header(http::header::CONTENT_ENCODING, "deflate")
            .body(())
            .unwrap();
        assert!(BufferedForwarder::is_compressed_response(&res).is_some());

        // br (Brotli)
        let res = Response::builder()
            .header(http::header::CONTENT_ENCODING, "br")
            .body(())
            .unwrap();
        assert!(BufferedForwarder::is_compressed_response(&res).is_some());

        // identity (not compressed)
        let res = Response::builder()
            .header(http::header::CONTENT_ENCODING, "identity")
            .body(())
            .unwrap();
        assert!(BufferedForwarder::is_compressed_response(&res).is_none());

        // No header (not compressed)
        let res = Response::builder().body(()).unwrap();
        assert!(BufferedForwarder::is_compressed_response(&res).is_none());
    }
}
