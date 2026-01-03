//! TTFB (Time-To-First-Byte) Benchmark for REQ-CORE-001
//!
//! # Traceability
//! - Implements: REQ-CORE-001 Section 5 (Benchmark - P95 TTFB < 10ms)

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;

/// Benchmark Time-To-First-Byte for direct connection (baseline)
///
/// # Traceability
/// - Implements: REQ-CORE-001 Section 5 (Benchmark baseline measurement)
async fn measure_direct_ttfb() -> Duration {
    // Start echo server
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = vec![0u8; 1024];

        loop {
            match socket.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    socket.write_all(&buffer[..n]).await.unwrap();
                }
                Err(_) => break,
            }
        }
    });

    // Connect and measure TTFB
    let mut client = TcpStream::connect(addr).await.unwrap();
    let test_data = b"TTFB_TEST";

    let start = std::time::Instant::now();
    client.write_all(test_data).await.unwrap();

    let mut buffer = vec![0u8; test_data.len()];
    client.read_exact(&mut buffer).await.unwrap();

    start.elapsed()
}

/// Benchmark TTFB through proxy (simulated)
///
/// # Traceability
/// - Implements: REQ-CORE-001 Section 5 (Benchmark proxied measurement)
///
/// Note: This simulates proxy overhead by adding a relay hop
async fn measure_proxied_ttfb() -> Duration {
    // Start origin server
    let origin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = origin_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut socket, _) = origin_listener.accept().await.unwrap();
        let mut buffer = vec![0u8; 1024];

        loop {
            match socket.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    socket.write_all(&buffer[..n]).await.unwrap();
                }
                Err(_) => break,
            }
        }
    });

    // Start proxy relay
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut client_socket, _) = proxy_listener.accept().await.unwrap();
        let mut origin_socket = TcpStream::connect(origin_addr).await.unwrap();

        // Set TCP_NODELAY (REQ-CORE-001 F-001)
        client_socket.set_nodelay(true).unwrap();
        origin_socket.set_nodelay(true).unwrap();

        // Bidirectional relay (zero-copy simulation)
        let (mut client_read, mut client_write) = client_socket.split();
        let (mut origin_read, mut origin_write) = origin_socket.split();

        let client_to_origin = async move {
            let mut buffer = vec![0u8; 8192];
            loop {
                match client_read.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if origin_write.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        };

        let origin_to_client = async move {
            let mut buffer = vec![0u8; 8192];
            loop {
                match origin_read.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if client_write.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        };

        tokio::select! {
            _ = client_to_origin => {},
            _ = origin_to_client => {},
        }
    });

    // Give proxy time to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Connect through proxy and measure TTFB
    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    client.set_nodelay(true).unwrap();

    let test_data = b"TTFB_TEST";

    let start = std::time::Instant::now();
    client.write_all(test_data).await.unwrap();

    let mut buffer = vec![0u8; test_data.len()];
    client.read_exact(&mut buffer).await.unwrap();

    start.elapsed()
}

fn bench_ttfb(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("ttfb");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    // Benchmark direct connection (baseline)
    group.bench_function(BenchmarkId::new("direct", "baseline"), |b| {
        b.to_async(&rt)
            .iter(|| async { measure_direct_ttfb().await });
    });

    // Benchmark proxied connection
    group.bench_function(BenchmarkId::new("proxied", "with_relay"), |b| {
        b.to_async(&rt)
            .iter(|| async { measure_proxied_ttfb().await });
    });

    group.finish();
}

criterion_group!(benches, bench_ttfb);
criterion_main!(benches);
