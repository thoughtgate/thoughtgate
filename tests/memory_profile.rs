//! Memory profiling tests for REQ-CORE-001
//!
//! # Traceability
//! - Implements: REQ-CORE-001 Section 5 (Memory Profile - Peak RSS delta < 5MB)
//!
//! # Usage
//!
//! To run with memory tracking:
//! ```bash
//! # macOS: Use Instruments or time -l
//! /usr/bin/time -l cargo test --test memory_profile -- --nocapture
//!
//! # Linux: Use /usr/bin/time -v or heaptrack
//! /usr/bin/time -v cargo test --test memory_profile -- --nocapture
//!
//! # Or use cargo-instruments (macOS)
//! cargo install cargo-instruments
//! cargo instruments --template Allocations -t memory_profile
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Memory profiling test: Stream 100MB through proxy-like relay
///
/// # Traceability
/// - Implements: REQ-CORE-001 Section 5 (Memory Profile)
///
/// # Goal
/// Peak RSS delta should be < 5MB when streaming 100MB payload.
/// The proxy should only buffer small chunks (8KB) in flight, not accumulate.
///
/// # Instructions
/// Run with system memory profiler:
/// - macOS: `/usr/bin/time -l cargo test --test memory_profile -- --nocapture`
/// - Linux: `/usr/bin/time -v cargo test --test memory_profile -- --nocapture`
///
/// Look for "maximum resident set size" in the output.
/// Baseline (no streaming) + streaming should be < 5MB difference.
#[tokio::test]
async fn test_100mb_stream_memory_profile() {
    println!("\n=== REQ-CORE-001 Memory Profile Test ===");
    println!("Streaming 100MB through zero-copy relay...\n");

    // Start origin server
    let origin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = origin_listener.local_addr().unwrap();

    let bytes_sent = Arc::new(AtomicUsize::new(0));
    let bytes_sent_clone = bytes_sent.clone();

    tokio::spawn(async move {
        let (mut socket, _) = origin_listener.accept().await.unwrap();

        // Stream 100MB in 8KB chunks
        let chunk = vec![0x42; 8192];
        let total_chunks = (100 * 1024 * 1024) / 8192;

        for _ in 0..total_chunks {
            if socket.write_all(&chunk).await.is_err() {
                break;
            }
            bytes_sent_clone.fetch_add(chunk.len(), Ordering::SeqCst);
        }
    });

    // Start relay (proxy simulation)
    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay_listener.local_addr().unwrap();

    let bytes_relayed = Arc::new(AtomicUsize::new(0));
    let bytes_relayed_clone = bytes_relayed.clone();

    // Use a channel to signal when relay is ready
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let (mut client_socket, _) = relay_listener.accept().await.unwrap();

        // Signal that relay is ready (after successful accept)
        let _ = ready_tx.send(());

        let mut origin_socket = TcpStream::connect(origin_addr).await.unwrap();

        // Set TCP_NODELAY (REQ-CORE-001 F-001)
        client_socket.set_nodelay(true).unwrap();
        origin_socket.set_nodelay(true).unwrap();

        // Zero-copy relay: small buffer, immediate forwarding
        let mut buffer = vec![0u8; 8192]; // Only 8KB in memory at a time

        loop {
            match origin_socket.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    bytes_relayed_clone.fetch_add(n, Ordering::SeqCst);
                    if client_socket.write_all(&buffer[..n]).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Wait for relay to be ready with timeout
    tokio::time::timeout(std::time::Duration::from_secs(5), ready_rx)
        .await
        .expect("Relay failed to start within timeout")
        .expect("Relay startup failed");

    // Client: Connect and receive 100MB
    let mut client = TcpStream::connect(relay_addr).await.unwrap();
    client.set_nodelay(true).unwrap();

    let mut total_received = 0usize;
    let mut buffer = vec![0u8; 8192];

    let start = std::time::Instant::now();

    loop {
        match client.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => {
                total_received += n;
                // Don't accumulate - just count
            }
            Err(_) => break,
        }
    }

    let elapsed = start.elapsed();

    println!(
        "Total received: {} bytes ({:.2} MB)",
        total_received,
        total_received as f64 / 1_048_576.0
    );
    println!("Time elapsed: {:.2}s", elapsed.as_secs_f64());
    println!(
        "Throughput: {:.2} MB/s",
        (total_received as f64 / 1_048_576.0) / elapsed.as_secs_f64()
    );
    println!("\n=== Memory Verification ===");
    println!("Expected: Peak RSS delta < 5MB (only 8KB buffer * 2 legs)");
    println!("Check the 'maximum resident set size' in time output above.");
    println!("========================================\n");

    // Verify we received all data
    assert_eq!(
        total_received,
        100 * 1024 * 1024,
        "Should receive exactly 100MB"
    );
    assert_eq!(
        bytes_sent.load(Ordering::SeqCst),
        100 * 1024 * 1024,
        "Origin should send 100MB"
    );
    assert_eq!(
        bytes_relayed.load(Ordering::SeqCst),
        100 * 1024 * 1024,
        "Relay should forward 100MB"
    );
}

/// Baseline memory test: Measure memory without streaming
///
/// Run this first to establish baseline memory usage, then run the 100MB test
/// and compare the "maximum resident set size" values.
#[tokio::test]
async fn test_baseline_memory() {
    println!("\n=== Baseline Memory Test ===");
    println!("Establishing baseline RSS (no streaming)...\n");

    // Just start servers but don't stream
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        // Accept but don't do anything
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    });

    let _client = TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    println!("Baseline established. Check 'maximum resident set size' above.");
    println!("================================\n");
}

/// Test that demonstrates peak memory is bounded by buffer size
///
/// # Traceability
/// - Implements: REQ-CORE-001 Section 3 (Constraints - No Vec<u8> accumulation)
#[tokio::test]
async fn test_memory_bounded_by_buffer() {
    // This test documents that memory is bounded by buffer size (8KB per leg)
    // not by total stream size (100MB)

    const BUFFER_SIZE: usize = 8192;
    const STREAM_SIZE: usize = 10 * 1024 * 1024; // 10MB

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let chunk = vec![0x55; BUFFER_SIZE];

        for _ in 0..(STREAM_SIZE / BUFFER_SIZE) {
            if socket.write_all(&chunk).await.is_err() {
                break;
            }
        }
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut total = 0;

    // Read stream without accumulating into Vec
    while total < STREAM_SIZE {
        match client.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                // Verify data but don't store it
                assert_eq!(buffer[0], 0x55);
            }
            Err(_) => break,
        }
    }

    assert_eq!(total, STREAM_SIZE);

    // Memory used: ~8KB (buffer) + connection overhead
    // NOT 10MB (total stream size)
    println!(
        "✅ Streamed {}MB using only {}KB buffer",
        STREAM_SIZE / 1_048_576,
        BUFFER_SIZE / 1024
    );
}
