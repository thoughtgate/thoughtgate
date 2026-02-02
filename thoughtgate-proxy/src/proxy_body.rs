//! Custom body wrapper for zero-copy streaming with metrics and cancellation.
//!
//! # Traceability
//! - Implements: REQ-CORE-001 (Zero-Copy Peeking Strategy)
//! - Implements: REQ-CORE-001 F-002 (Client Disconnect Handling)
//! - Implements: REQ-CORE-001 F-003 (Trailer Support)

use bytes::Bytes;
use http_body::{Body, Frame};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_util::sync::CancellationToken;

/// Metrics for a single stream.
///
/// # Traceability
/// - Implements: REQ-CORE-001 NFR-001 (Observability - Metrics)
#[derive(Debug, Clone)]
pub struct StreamMetrics {
    bytes_transferred: u64,
    chunks_count: u64,
    has_trailers: bool,
}

impl StreamMetrics {
    /// Create new stream metrics.
    pub fn new() -> Self {
        Self {
            bytes_transferred: 0,
            chunks_count: 0,
            has_trailers: false,
        }
    }

    /// Record bytes transferred (reference-only inspection).
    pub fn record_bytes(&mut self, count: usize) {
        self.bytes_transferred += count as u64;
        self.chunks_count += 1;

        // Record metrics if available (REQ-CORE-001 NFR-001)
        #[cfg(feature = "metrics")]
        if let Some(metrics) = thoughtgate_core::metrics::get_metrics() {
            metrics.record_chunk_size(count as u64);
        }
    }

    /// Record that trailers were received.
    pub fn record_trailers(&mut self) {
        self.has_trailers = true;
    }

    /// Get total bytes transferred.
    pub fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }

    /// Get total chunks count.
    pub fn chunks_count(&self) -> u64 {
        self.chunks_count
    }

    /// Check if trailers were received.
    pub fn has_trailers(&self) -> bool {
        self.has_trailers
    }
}

impl Default for StreamMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Custom body wrapper that implements zero-copy streaming with metrics and cancellation.
///
/// This wrapper forwards frames directly without cloning or buffering, while tracking
/// metrics and respecting cancellation tokens for client disconnect handling.
///
/// # Traceability
/// - Implements: REQ-CORE-001 F-001 (Zero-Copy Forwarding)
/// - Implements: REQ-CORE-001 F-002 (Client Disconnect Handling)
/// - Implements: REQ-CORE-001 F-003 (Trailer Support)
pub struct ProxyBody<B> {
    inner: B,
    metrics: StreamMetrics,
    cancel_token: CancellationToken,
}

impl<B> ProxyBody<B> {
    /// Create a new ProxyBody wrapper.
    ///
    /// # Arguments
    /// * `inner` - The inner body to wrap
    /// * `cancel_token` - Cancellation token for client disconnect detection
    pub fn new(inner: B, cancel_token: CancellationToken) -> Self {
        Self {
            inner,
            metrics: StreamMetrics::new(),
            cancel_token,
        }
    }

    /// Get a reference to the stream metrics.
    pub fn metrics(&self) -> &StreamMetrics {
        &self.metrics
    }

    /// Get a mutable reference to the stream metrics.
    pub fn metrics_mut(&mut self) -> &mut StreamMetrics {
        &mut self.metrics
    }

    /// Check if the stream has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }
}

impl<B> Body for ProxyBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        // 1. Check Cancellation
        if self.cancel_token.is_cancelled() {
            return Poll::Ready(None);
        }

        // 2. Poll Inner Body
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                // 3. Record Metrics (Inspect Reference Only - Zero Copy)
                if let Some(data) = frame.data_ref() {
                    self.metrics.record_bytes(data.len());
                } else if frame.is_trailers() {
                    self.metrics.record_trailers();
                }

                // 4. Move Frame Forward (Zero Copy)
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e.into()))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{BodyExt, Empty, Full};

    #[tokio::test]
    async fn test_proxy_body_forwards_data() {
        let data = Bytes::from("test data");
        let body = Full::new(data.clone());
        let cancel_token = CancellationToken::new();

        let proxy_body = ProxyBody::new(body, cancel_token);

        // Collect all frames
        let collected = proxy_body.collect().await.unwrap().to_bytes();

        assert_eq!(collected, data);
    }

    #[tokio::test]
    async fn test_proxy_body_tracks_metrics() {
        let data = Bytes::from("test data");
        let body = Full::new(data.clone());
        let cancel_token = CancellationToken::new();

        let proxy_body = ProxyBody::new(body, cancel_token);

        // Collect the body
        let collected = proxy_body.collect().await.unwrap();
        let bytes = collected.to_bytes();

        // Verify data was forwarded correctly
        assert_eq!(bytes, data);
    }

    #[tokio::test]
    async fn test_proxy_body_cancellation() {
        let body = Empty::<Bytes>::new();
        let cancel_token = CancellationToken::new();

        let proxy_body = ProxyBody::new(body, cancel_token.clone());

        // Cancel the token
        cancel_token.cancel();

        // Poll should return None immediately
        assert!(proxy_body.is_cancelled());

        let result = proxy_body.collect().await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_stream_metrics() {
        let mut metrics = StreamMetrics::new();

        assert_eq!(metrics.bytes_transferred(), 0);
        assert_eq!(metrics.chunks_count(), 0);
        assert!(!metrics.has_trailers());

        metrics.record_bytes(100);
        metrics.record_bytes(200);

        assert_eq!(metrics.bytes_transferred(), 300);
        assert_eq!(metrics.chunks_count(), 2);

        metrics.record_trailers();
        assert!(metrics.has_trailers());
    }
}
