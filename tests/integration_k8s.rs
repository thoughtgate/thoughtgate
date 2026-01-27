//! Kubernetes integration tests for ThoughtGate performance baseline.
//!
//! These tests are marked as `#[ignore]` by default because they require:
//! - A running Kubernetes cluster (e.g., minikube, kind, or cloud provider)
//! - kubectl configured and accessible
//! - k6 installed for load testing
//!
//! To run these tests:
//! ```bash
//! cargo test --test integration_k8s -- --ignored
//! ```

use anyhow::{Context, Result, bail};
use k8s_openapi::api::core::v1::{Namespace, Pod};
use kube::{
    Client,
    api::{Api, DeleteParams, ListParams, PostParams},
    core::ObjectMeta,
};
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{Duration, sleep, timeout};
use uuid::Uuid;

// --- Data Structures for K6 NDJSON output ---
#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct K6Metric {
    #[serde(rename = "type")]
    metric_type: String,
    data: K6Data,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct K6Data {
    #[serde(default)]
    tags: Option<serde_json::Value>,
    #[serde(default)]
    value: Option<f64>,
}

#[derive(Serialize)]
struct BenchExport {
    name: String,
    unit: String,
    value: f64,
}

// --- Helper Functions ---

/// Calculate P95 percentile from a vector of f64 values.
/// Returns None if the input is empty.
/// Filters out NaN values and sorts using total_cmp for panic-free comparison.
fn calculate_p95(mut values: Vec<f64>) -> Option<f64> {
    // Filter out NaN values to ensure safe sorting
    values.retain(|v| !v.is_nan());

    if values.is_empty() {
        return None;
    }

    // Sort using total_cmp (Rust 1.62+) to avoid panics on special float values
    values.sort_by(|a, b| a.total_cmp(b));

    let idx = (values.len() as f64 * 0.95) as usize;
    let bounded_idx = idx.min(values.len() - 1);

    Some(values[bounded_idx])
}

// --- Test Context (RAII) ---
struct TestContext {
    client: Client,
    namespace: String,
}

impl TestContext {
    async fn new() -> Result<Self> {
        let client = Client::try_default()
            .await
            .context("Failed to create K8s client - is kubectl configured?")?;

        // Use UUID for guaranteed uniqueness across test runs
        let uuid = Uuid::new_v4();
        let ns_name = format!("thoughtgate-test-{}", uuid);

        let api: Api<Namespace> = Api::all(client.clone());
        api.create(
            &PostParams::default(),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(ns_name.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .context("Failed to create namespace")?;

        println!("🚀 Created Namespace: {}", ns_name);
        Ok(Self {
            client,
            namespace: ns_name,
        })
    }

    // Helper: Run kubectl with 30s timeout
    async fn kubectl(&self, args: &[&str]) -> Result<()> {
        let mut full_args = vec!["-n", &self.namespace];
        full_args.extend(args);

        let child = Command::new("kubectl")
            .args(&full_args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn kubectl")?;

        let output = timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .context("Kubectl command timed out after 30s")?
            .context("Failed to wait for kubectl")?;

        if !output.status.success() {
            bail!(
                "Kubectl command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    /// Dump container logs on failure for debugging
    async fn dump_logs_on_failure(&self) {
        eprintln!("\n🚨 === FAILURE DETECTED - DUMPING LOGS ===\n");

        // Find the pod name
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &self.namespace);
        let pod_name = match pods
            .list(&ListParams::default().labels("job-name=perf-test"))
            .await
        {
            Ok(list) => {
                if let Some(pod) = list.items.first() {
                    pod.metadata
                        .name
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string())
                } else {
                    eprintln!("⚠️  No pods found for perf-test job");
                    return;
                }
            }
            Err(e) => {
                eprintln!("⚠️  Failed to list pods: {}", e);
                return;
            }
        };

        // Dump ThoughtGate logs
        eprintln!("📋 ThoughtGate Container Logs:");
        eprintln!("================================");
        match Command::new("kubectl")
            .args([
                "logs",
                "-n",
                &self.namespace,
                &pod_name,
                "-c",
                "thoughtgate",
            ])
            .output()
            .await
        {
            Ok(output) => {
                eprintln!("{}", String::from_utf8_lossy(&output.stdout));
                if !output.stderr.is_empty() {
                    eprintln!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => eprintln!("⚠️  Failed to get thoughtgate logs: {}", e),
        }

        // Dump K6 logs
        eprintln!("\n📋 K6 Container Logs:");
        eprintln!("================================");
        match Command::new("kubectl")
            .args(["logs", "-n", &self.namespace, &pod_name, "-c", "k6"])
            .output()
            .await
        {
            Ok(output) => {
                eprintln!("{}", String::from_utf8_lossy(&output.stdout));
                if !output.stderr.is_empty() {
                    eprintln!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => eprintln!("⚠️  Failed to get k6 logs: {}", e),
        }

        // Try to get mock-mcp logs (from the separate mock-mcp-pod)
        eprintln!("\n📋 Mock MCP Container Logs:");
        eprintln!("================================");
        match Command::new("kubectl")
            .args([
                "logs",
                "-n",
                &self.namespace,
                "mock-mcp-pod",
                "-c",
                "server",
            ])
            .output()
            .await
        {
            Ok(output) => {
                eprintln!("{}", String::from_utf8_lossy(&output.stdout));
                if !output.stderr.is_empty() {
                    eprintln!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => eprintln!("⚠️  Failed to get mock-mcp logs: {}", e),
        }

        eprintln!("\n🚨 === END OF LOG DUMP ===\n");
    }

    async fn run_test(&self) -> Result<(f64, f64, f64)> {
        // Deploy ConfigMap & Job
        self.kubectl(&[
            "create",
            "configmap",
            "k6-script",
            "--from-file=tests/benchmark.js",
        ])
        .await
        .context("Failed to create k6-script ConfigMap")?;

        self.kubectl(&["apply", "-f", "k8s/test-job.yaml"])
            .await
            .context("Failed to apply test-job.yaml")?;

        // Wait for K6 container to complete (max 300s to allow for image pulls in slow environments)
        // Note: We check the K6 container status directly since ThoughtGate runs as a sidecar
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &self.namespace);
        let start = tokio::time::Instant::now();
        let timeout_duration = Duration::from_secs(300);

        println!("⏳ Waiting for test pod to become ready...");

        // Smart polling: Wait for all containers to be ready before checking K6 completion
        let _pod_name = loop {
            if start.elapsed() > timeout_duration {
                // Dump logs on timeout for debugging
                self.dump_logs_on_failure().await;
                bail!("Test setup timed out after 300s waiting for pods to be ready");
            }

            if let Ok(list) = pods
                .list(&ListParams::default().labels("job-name=perf-test"))
                .await
                && let Some(pod) = list.items.first()
            {
                let pod_name_ref = pod.metadata.name.as_ref();

                if let Some(status) = &pod.status {
                    // Check if pod phase is Running
                    if let Some(phase) = &status.phase
                        && phase == "Running"
                    {
                        // Check if all containers are ready
                        if let Some(container_statuses) = &status.container_statuses {
                            let all_ready = container_statuses.iter().all(|cs| cs.ready);
                            if all_ready {
                                println!(
                                    "✅ All containers ready in pod: {}",
                                    pod_name_ref.unwrap()
                                );
                                break pod_name_ref.unwrap().clone();
                            } else {
                                println!(
                                    "⏳ Waiting for containers to be ready... (elapsed: {:?})",
                                    start.elapsed()
                                );
                            }
                        }
                    }
                }
            }

            sleep(Duration::from_millis(500)).await;
        };

        println!("⏳ Waiting for K6 container to complete...");

        // Now wait for K6 to complete
        loop {
            if start.elapsed() > timeout_duration {
                // Dump logs on timeout for debugging
                self.dump_logs_on_failure().await;
                bail!("Benchmark timed out after 300s");
            }

            if let Ok(list) = pods
                .list(&ListParams::default().labels("job-name=perf-test"))
                .await
                && let Some(pod) = list.items.first()
                && let Some(container_statuses) = &pod
                    .status
                    .as_ref()
                    .and_then(|s| s.container_statuses.as_ref())
            {
                // Find K6 container status
                if let Some(k6_status) = container_statuses.iter().find(|cs| cs.name == "k6")
                    && let Some(terminated) =
                        &k6_status.state.as_ref().and_then(|s| s.terminated.as_ref())
                {
                    if terminated.exit_code == 0 {
                        println!("✅ K6 container completed successfully");
                        break;
                    } else {
                        // Dump logs on failure
                        self.dump_logs_on_failure().await;
                        bail!(
                            "K6 container failed with exit code: {}",
                            terminated.exit_code
                        );
                    }
                }
            }

            sleep(Duration::from_millis(500)).await;
        }

        // Extract Results with Timeout
        let list = pods
            .list(&ListParams::default().labels("job-name=perf-test"))
            .await
            .context("Failed to list pods")?;

        let pod_name = list
            .items
            .first()
            .context("No pods found for job")?
            .metadata
            .name
            .as_ref()
            .context("Pod has no name")?;

        println!("📊 Extracting results from pod: {}", pod_name);

        // Use file-extractor busybox container to cat the results
        let output = timeout(
            Duration::from_secs(10),
            Command::new("kubectl")
                .args([
                    "exec",
                    "-n",
                    &self.namespace,
                    pod_name,
                    "-c",
                    "file-extractor",
                    "--",
                    "cat",
                    "/results/results.json",
                ])
                .output(),
        )
        .await
        .context("Timeout extracting results from pod")?
        .context("Failed to execute kubectl exec")?;

        if !output.status.success() {
            bail!(
                "Failed to cat results: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Parse K6 NDJSON output
        let results_str = String::from_utf8_lossy(&output.stdout);

        println!(
            "K6 output first 500 chars: {}",
            &results_str[..results_str.len().min(500)]
        );

        // K6 JSON output format: newline-delimited JSON
        // We need to look for the summary metrics at the end
        let mut waiting_values: Vec<f64> = Vec::new();
        let mut duration_values: Vec<f64> = Vec::new();
        let mut request_count: u64 = 0;

        for line in results_str.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Try to parse as generic JSON value
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line)
                && json["type"] == "Point"
                && let Some(metric_name) = json["metric"].as_str()
                && let Some(value) = json["data"]["value"].as_f64()
            {
                // K6 outputs duration in milliseconds
                match metric_name {
                    "http_req_waiting" => waiting_values.push(value),
                    "http_req_duration" => duration_values.push(value),
                    "http_reqs" => request_count += 1,
                    _ => {}
                }
            }
        }

        // Calculate percentiles from collected values
        let waiting_count = waiting_values.len();
        let duration_count = duration_values.len();

        let waiting_p95 = calculate_p95(waiting_values).unwrap_or(0.0);
        let duration_p95 = calculate_p95(duration_values).unwrap_or(0.0);

        if waiting_p95 == 0.0 || duration_p95 == 0.0 {
            println!(
                "Waiting values count: {}, Duration values count: {}",
                waiting_count, duration_count
            );
            bail!("Failed to extract metrics from K6 output");
        }

        // Calculate throughput (requests per second)
        // K6 benchmark runs for 10 seconds (see tests/benchmark.js)
        const K6_DURATION_SECS: f64 = 10.0;
        let throughput_rps = request_count as f64 / K6_DURATION_SECS;

        println!(
            "K6 stats: {} requests in {}s = {:.0} RPS",
            request_count, K6_DURATION_SECS, throughput_rps
        );

        Ok((waiting_p95, duration_p95, throughput_rps))
    }
}

// RAII Cleanup (Background)
impl Drop for TestContext {
    fn drop(&mut self) {
        let ns = self.namespace.clone();
        println!("🧹 Cleaning up namespace: {}", ns);

        // Spawn detached background thread for async cleanup
        // Do not join() to avoid blocking Drop and prevent panics
        std::thread::spawn(move || {
            // Handle runtime creation errors gracefully
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!(
                        "⚠️  Failed to create runtime for cleanup of namespace {}: {}",
                        ns, e
                    );
                    return;
                }
            };

            rt.block_on(async move {
                match Client::try_default().await {
                    Ok(client) => {
                        let api: Api<Namespace> = Api::all(client);
                        match api
                            .delete(
                                &ns,
                                &DeleteParams {
                                    propagation_policy: Some(
                                        kube::api::PropagationPolicy::Background,
                                    ),
                                    ..Default::default()
                                },
                            )
                            .await
                        {
                            Ok(_) => {
                                println!("✅ Successfully initiated cleanup of namespace: {}", ns)
                            }
                            Err(e) => eprintln!("⚠️  Failed to delete namespace {}: {}", ns, e),
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "⚠️  Failed to create K8s client for cleanup of namespace {}: {}",
                            ns, e
                        );
                    }
                }
            });
        });
        // Thread is detached - Drop returns immediately without blocking
    }
}

#[tokio::test]
#[serial]
#[ignore = "Requires Kubernetes cluster - run with: cargo test -- --ignored"]
async fn test_performance_baseline() -> Result<()> {
    let ctx = TestContext::new().await?;
    let (ttfb, duration, throughput) = ctx.run_test().await?;

    println!("\n--- 📈 Performance Results ---");
    println!("TTFB (p95):     {:.2}ms", ttfb);
    println!("Latency (p95):  {:.2}ms", duration);
    println!("Throughput:     {:.0} RPS", throughput);
    println!("-----------------------------\n");

    // Assertions - Mock has 500ms TTFB + 500ms streaming = ~1000ms total
    // Allow generous overhead for Kind/K8s environment
    if ttfb >= 1000.0 {
        bail!("❌ TTFB Regression: {:.2}ms (expected < 1000ms)", ttfb);
    }
    if duration >= 2000.0 {
        bail!(
            "❌ Latency Regression: {:.2}ms (expected < 2000ms)",
            duration
        );
    }

    // Export metrics for CI (REQ-OBS-001 compliant naming)
    // Note: These are K8s/Kind environment metrics, not bare-metal targets
    let exports = vec![
        BenchExport {
            name: "ttfb/p95".into(),
            unit: "ms".into(),
            value: ttfb,
        },
        BenchExport {
            name: "latency/p95".into(),
            unit: "ms".into(),
            value: duration,
        },
        BenchExport {
            name: "throughput/rps".into(),
            unit: "req/s".into(),
            value: throughput,
        },
    ];

    std::fs::write(
        "bench_metrics.json",
        serde_json::to_string_pretty(&exports)?,
    )
    .context("Failed to write bench_metrics.json")?;

    println!("✅ All performance checks passed!");
    Ok(())
}
