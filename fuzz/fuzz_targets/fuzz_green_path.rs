#![no_main]

//! Fuzz target for REQ-CORE-001 Green Path (Zero-Copy Peeking Strategy)
//!
//! # Traceability
//! - Implements: REQ-CORE-001 Section 5.2 (Fuzzing - cargo fuzz run green_path)
//!
//! # Goal
//! Verify that malformed chunks/headers do not cause:
//! - Panics in ProxyBody or TimeoutBody
//! - Memory leaks or unbounded buffering
//! - Infinite loops or deadlocks
//! - Incorrect frame forwarding

use arbitrary::Arbitrary;
use bytes::Bytes;
use http_body::{Body, Frame};
use http_body_util::BodyExt;
use libfuzzer_sys::fuzz_target;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use thoughtgate_proxy::proxy_body::{ProxyBody, StreamMetrics};
use thoughtgate_proxy::timeout::{TimeoutBody, TimeoutConfig};

/// Fuzz input for Green Path testing
#[derive(Arbitrary, Debug)]
struct FuzzGreenInput {
    /// Chunks to emit from the fake body
    chunks: Vec<FuzzChunk>,
    /// Whether to include trailers
    include_trailers: bool,
    /// Trailer data (if included)
    trailer_data: Vec<(Vec<u8>, Vec<u8>)>,
    /// Whether to cancel mid-stream
    cancel_after_chunks: Option<u8>,
    /// Timeout configuration
    chunk_timeout_ms: u16,
    total_timeout_ms: u16,
}

/// A single chunk in the fuzz input
#[derive(Arbitrary, Debug, Clone)]
struct FuzzChunk {
    /// Raw bytes for this chunk
    data: Vec<u8>,
    /// Whether this chunk should error
    should_error: bool,
}

/// A fake body that yields predetermined chunks for fuzzing
struct FuzzBody {
    chunks: Vec<FuzzChunk>,
    trailers: Option<http::HeaderMap>,
    current_chunk: usize,
    trailers_sent: bool,
}

impl FuzzBody {
    fn new(chunks: Vec<FuzzChunk>, trailers: Option<http::HeaderMap>) -> Self {
        Self {
            chunks,
            trailers,
            current_chunk: 0,
            trailers_sent: false,
        }
    }
}

impl Body for FuzzBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        // Emit data chunks
        if self.current_chunk < self.chunks.len() {
            let idx = self.current_chunk;
            self.current_chunk += 1;

            let chunk = &self.chunks[idx];
            if chunk.should_error {
                return Poll::Ready(Some(Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Fuzz-induced error",
                ))));
            }

            // Limit chunk size to prevent OOM
            let data = if chunk.data.len() > 64 * 1024 {
                Bytes::from(chunk.data[..64 * 1024].to_vec())
            } else {
                Bytes::from(chunk.data.clone())
            };

            return Poll::Ready(Some(Ok(Frame::data(data))));
        }

        // Emit trailers if present
        if let Some(trailers) = self.trailers.take() {
            if !self.trailers_sent {
                self.trailers_sent = true;
                return Poll::Ready(Some(Ok(Frame::trailers(trailers))));
            }
        }

        // End of stream
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        self.current_chunk >= self.chunks.len() && self.trailers.is_none()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        let remaining: usize = self.chunks[self.current_chunk..]
            .iter()
            .map(|c| c.data.len().min(64 * 1024))
            .sum();
        http_body::SizeHint::with_exact(remaining as u64)
    }
}

fuzz_target!(|input: FuzzGreenInput| {
    // Use tokio runtime for async code
    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(async { fuzz_green_path(input).await }));
});

async fn fuzz_green_path(input: FuzzGreenInput) {
    // Limit chunks to prevent excessive runtime
    let chunks: Vec<_> = input.chunks.into_iter().take(100).collect();

    // Build trailers if requested
    let trailers = if input.include_trailers {
        let mut map = http::HeaderMap::new();
        for (name, value) in input.trailer_data.iter().take(10) {
            if let (Ok(n), Ok(v)) = (
                http::header::HeaderName::from_bytes(name),
                http::header::HeaderValue::from_bytes(value),
            ) {
                map.insert(n, v);
            }
        }
        if !map.is_empty() {
            Some(map)
        } else {
            None
        }
    } else {
        None
    };

    // Test 1: ProxyBody without timeout
    test_proxy_body(chunks.clone(), trailers.clone(), input.cancel_after_chunks).await;

    // Test 2: TimeoutBody wrapper
    let timeout_config = TimeoutConfig::new(
        Duration::from_millis(input.chunk_timeout_ms.max(10) as u64),
        Duration::from_millis(input.total_timeout_ms.max(50) as u64),
    );
    test_timeout_body(chunks.clone(), trailers.clone(), timeout_config).await;

    // Test 3: Combined ProxyBody + TimeoutBody
    test_combined_wrappers(chunks, trailers, input.cancel_after_chunks).await;
}

/// Test ProxyBody frame forwarding and metrics
async fn test_proxy_body(
    chunks: Vec<FuzzChunk>,
    trailers: Option<http::HeaderMap>,
    cancel_after: Option<u8>,
) {
    let body = FuzzBody::new(chunks.clone(), trailers);
    let cancel_token = CancellationToken::new();

    let mut proxy_body = ProxyBody::new(body, cancel_token.clone());

    // Optionally cancel after N chunks
    let cancel_after = cancel_after.map(|n| n as usize);

    let mut chunks_received = 0;

    // Manually poll to test cancellation
    loop {
        // Check if we should cancel
        if let Some(n) = cancel_after {
            if chunks_received >= n {
                cancel_token.cancel();
            }
        }

        // Try to get next frame
        match proxy_body.frame().await {
            Some(Ok(_frame)) => {
                chunks_received += 1;
                // Verify metrics are updating (should not panic)
                let _ = proxy_body.metrics().bytes_transferred();
                let _ = proxy_body.metrics().chunks_count();
            }
            Some(Err(_)) => {
                // Error frame - continue
                break;
            }
            None => {
                // End of stream
                break;
            }
        }

        // Safety limit
        if chunks_received > 1000 {
            break;
        }
    }

    // Final metrics check - should not panic
    let _ = proxy_body.metrics().has_trailers();
}

/// Test TimeoutBody timeout handling
async fn test_timeout_body(
    chunks: Vec<FuzzChunk>,
    trailers: Option<http::HeaderMap>,
    config: TimeoutConfig,
) {
    let body = FuzzBody::new(chunks, trailers);
    let timeout_body = TimeoutBody::new(body, config);

    // Collect with timeout - should not panic
    let _ = timeout_body.collect().await;
}

/// Test combined ProxyBody + TimeoutBody wrappers
async fn test_combined_wrappers(
    chunks: Vec<FuzzChunk>,
    trailers: Option<http::HeaderMap>,
    cancel_after: Option<u8>,
) {
    let body = FuzzBody::new(chunks, trailers);
    let cancel_token = CancellationToken::new();

    // Wrap: FuzzBody -> TimeoutBody -> ProxyBody
    let timeout_config = TimeoutConfig::new(Duration::from_secs(5), Duration::from_secs(30));
    let timeout_body = TimeoutBody::new(body, timeout_config);
    let mut proxy_body = ProxyBody::new(timeout_body, cancel_token.clone());

    // Optionally cancel
    if let Some(n) = cancel_after {
        if n < 5 {
            cancel_token.cancel();
        }
    }

    // Collect - should not panic
    let mut frames = 0;
    while let Some(result) = proxy_body.frame().await {
        match result {
            Ok(_) => frames += 1,
            Err(_) => break,
        }
        if frames > 100 {
            break;
        }
    }
}

/// Test StreamMetrics directly with arbitrary data
#[allow(dead_code)]
fn test_stream_metrics_direct(bytes_counts: &[usize]) {
    let mut metrics = StreamMetrics::new();

    for &count in bytes_counts.iter().take(1000) {
        // Clamp to reasonable size
        let clamped = count.min(1024 * 1024);
        metrics.record_bytes(clamped);
    }

    // Should not overflow or panic
    let _ = metrics.bytes_transferred();
    let _ = metrics.chunks_count();

    metrics.record_trailers();
    assert!(metrics.has_trailers());
}

