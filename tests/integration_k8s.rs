use kube::{
    api::{Api, DeleteParams, PostParams, ListParams},
    Client, core::ObjectMeta,
};
use k8s_openapi::api::core::v1::{Pod, Namespace};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::time::{sleep, timeout, Duration};
use tokio::process::Command;
use anyhow::{Context, Result, bail};
use serial_test::serial;

// --- Data Structures for K6 NDJSON output ---
#[derive(Deserialize, Debug)] 
struct K6Metric {
    #[serde(rename = "type")]
    metric_type: String,
    data: K6Data,
}

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
    value: f64 
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
    namespace: String 
}

impl TestContext {
    async fn new() -> Result<Self> {
        let client = Client::try_default().await
            .context("Failed to create K8s client - is kubectl configured?")?;
        
        let suffix: String = rand::Rng::sample_iter(
            rand::thread_rng(), 
            &rand::distributions::Alphanumeric
        )
        .take(6)
        .map(char::from)
        .collect();
        
        let ns_name = format!("thoughtgate-test-{}", suffix.to_lowercase());
        
        let api: Api<Namespace> = Api::all(client.clone());
        api.create(&PostParams::default(), &Namespace {
            metadata: ObjectMeta { 
                name: Some(ns_name.clone()), 
                ..Default::default() 
            }, 
            ..Default::default()
        })
        .await
        .context("Failed to create namespace")?;
        
        println!("🚀 Created Namespace: {}", ns_name);
        Ok(Self { client, namespace: ns_name })
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

    async fn run_test(&self) -> Result<(f64, f64)> {
        // Deploy ConfigMap & Job
        self.kubectl(&["create", "configmap", "k6-script", "--from-file=tests/benchmark.js"])
            .await
            .context("Failed to create k6-script ConfigMap")?;
        
        self.kubectl(&["apply", "-f", "k8s/test-job.yaml"])
            .await
            .context("Failed to apply test-job.yaml")?;

        // Wait for K6 container to complete (max 90s)
        // Note: We check the K6 container status directly since ThoughtGate runs as a sidecar
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &self.namespace);
        let start = tokio::time::Instant::now();
        
        loop {
            if start.elapsed() > Duration::from_secs(90) { 
                bail!("Benchmark timed out after 90s"); 
            }
            
            if let Ok(list) = pods.list(&ListParams::default().labels("job-name=perf-test")).await {
                if let Some(pod) = list.items.first() {
                    if let Some(container_statuses) = &pod.status.as_ref().and_then(|s| s.container_statuses.as_ref()) {
                        // Find K6 container status
                        if let Some(k6_status) = container_statuses.iter().find(|cs| cs.name == "k6") {
                            if let Some(terminated) = &k6_status.state.as_ref().and_then(|s| s.terminated.as_ref()) {
                                if terminated.exit_code == 0 {
                                    println!("✅ K6 container completed successfully");
                                    break;
                                } else {
                                    bail!("K6 container failed with exit code: {}", terminated.exit_code);
                                }
                            }
                        }
                    }
                }
            }
            
            sleep(Duration::from_secs(2)).await;
        }

        // Extract Results with Timeout
        let list = pods.list(&ListParams::default().labels("job-name=perf-test"))
            .await
            .context("Failed to list pods")?;
        
        let pod_name = list.items
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
                .args(&[
                    "exec",
                    "-n", &self.namespace,
                    pod_name,
                    "-c", "file-extractor",
                    "--",
                    "cat",
                    "/results/results.json"
                ])
                .output()
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
        
        println!("K6 output first 500 chars: {}", &results_str[..results_str.len().min(500)]);
        
        // K6 JSON output format: newline-delimited JSON
        // We need to look for the summary metrics at the end
        let mut waiting_values: Vec<f64> = Vec::new();
        let mut duration_values: Vec<f64> = Vec::new();
        
        for line in results_str.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            
            // Try to parse as generic JSON value
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                if json["type"] == "Point" {
                    if let Some(metric_name) = json["metric"].as_str() {
                        if let Some(value) = json["data"]["value"].as_f64() {
                            // K6 outputs duration in milliseconds
                            match metric_name {
                                "http_req_waiting" => waiting_values.push(value),
                                "http_req_duration" => duration_values.push(value),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        
        // Calculate p95 from collected values
        let waiting_p95 = calculate_p95(waiting_values).unwrap_or(0.0);
        let duration_p95 = calculate_p95(duration_values).unwrap_or(0.0);
        
        if waiting_p95 == 0.0 || duration_p95 == 0.0 {
            println!("Waiting values count: {}, Duration values count: {}", waiting_values.len(), duration_values.len());
            bail!("Failed to extract metrics from K6 output");
        }
        
        Ok((waiting_p95, duration_p95))
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
                    eprintln!("⚠️  Failed to create runtime for cleanup of namespace {}: {}", ns, e);
                    return;
                }
            };
            
            rt.block_on(async move {
                match Client::try_default().await {
                    Ok(client) => {
                        let api: Api<Namespace> = Api::all(client);
                        match api.delete(&ns, &DeleteParams {
                            propagation_policy: Some(kube::api::PropagationPolicy::Background), 
                            ..Default::default()
                        }).await {
                            Ok(_) => println!("✅ Successfully initiated cleanup of namespace: {}", ns),
                            Err(e) => eprintln!("⚠️  Failed to delete namespace {}: {}", ns, e),
                        }
                    }
                    Err(e) => {
                        eprintln!("⚠️  Failed to create K8s client for cleanup of namespace {}: {}", ns, e);
                    }
                }
            });
        });
        // Thread is detached - Drop returns immediately without blocking
    }
}

#[tokio::test]
#[serial]
async fn test_performance_baseline() -> Result<()> {
    let ctx = TestContext::new().await?;
    let (ttfb, duration) = ctx.run_test().await?;

    println!("\n--- 📈 Performance Results ---");
    println!("TTFB (p95):     {:.2}ms", ttfb);
    println!("Duration (p95): {:.2}ms", duration);
    println!("-----------------------------\n");

    // Assertions - Mock has 500ms TTFB + 500ms streaming = ~1000ms total
    // Allow generous overhead for Kind/K8s environment
    if ttfb >= 1000.0 { 
        bail!("❌ TTFB Regression: {:.2}ms (expected < 1000ms)", ttfb); 
    }
    if duration >= 2000.0 { 
        bail!("❌ Duration Regression: {:.2}ms (expected < 2000ms)", duration); 
    }

    // Export metrics for CI
    let exports = vec![
        BenchExport { 
            name: "Time To First Byte".into(), 
            unit: "ms".into(), 
            value: ttfb 
        },
        BenchExport { 
            name: "Stream Duration".into(), 
            unit: "ms".into(), 
            value: duration 
        },
    ];
    
    std::fs::write(
        "bench_metrics.json", 
        serde_json::to_string_pretty(&exports)?
    )
    .context("Failed to write bench_metrics.json")?;

    println!("✅ All performance checks passed!");
    Ok(())
}
