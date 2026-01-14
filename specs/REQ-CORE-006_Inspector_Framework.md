# REQ-CORE-006: Inspector Framework

| Metadata | Value |
|----------|-------|
| **ID** | `REQ-CORE-006` |
| **Title** | Inspector Framework |
| **Type** | Core Mechanic |
| **Status** | **DEFERRED (v0.3+)** |
| **Priority** | Low (deferred) |
| **Tags** | `#inspection` `#extensibility` `#security` `#pii` `#prompt-injection` `#deferred` |

> ## ⚠️ DEFERRED TO FUTURE VERSION
>
> **This requirement is deferred from v0.2.** The Inspector Framework is designed for Amber Path
> request inspection (PII detection, prompt injection scanning), but v0.2 does not implement
> response inspection or the Amber Path.
>
> **v0.2 Simplification:**
> - All requests pass through without content inspection
> - Policy decisions based on metadata only (tool name, principal, arguments schema)
> - No PII detection or prompt injection scanning
>
> **When to Reintroduce:**
> - When content-based policy decisions are needed
> - When PII detection in arguments is required
> - When prompt injection protection is required
>
> **See:** `architecture.md` Section 7.2 (Out of Scope)

## 1. Context & Decision Rationale

This requirement defines the **Inspector Framework** for ThoughtGate's Amber Path. Inspectors analyze buffered requests and enrich the Cedar policy context with additional signals for decision-making.

**Relationship to RFC-001:**

Per RFC-001 (Traffic Model Architecture), the four-path model determines request handling:

| Path | Trigger | Behavior |
|------|---------|----------|
| **Green** | Policy allows, no inspection | Stream through |
| **Amber** | Policy allows, inspection required | Buffer, run inspectors, forward |
| **Red** | Policy denies | Reject immediately |
| **Approval** | Policy requires approval | Create task, await decision |

Inspectors run during **Amber Path** processing. They:
1. Analyze buffered request content
2. Enrich Cedar policy context with findings
3. Enable sophisticated policy decisions based on content analysis

**Why Inspectors?**

Cedar policies can only evaluate what's in the context. Without inspectors:
- Policy limited to metadata (tool name, principal, headers)
- Cannot detect PII, prompt injection, schema violations
- Cannot make content-aware decisions

With inspectors:
- Content analyzed before policy re-evaluation
- Findings added to context (`pii_detected`, `prompt_guard_score`, etc.)
- Policy can route based on content signals

**Inspector vs Body Modification:**

REQ-CORE-002 defines inspectors that can modify or reject bodies. This requirement defines **context-enriching inspectors** that analyze content and add signals to Cedar context. Both use the same Amber Path buffering but serve different purposes:

| Aspect | REQ-CORE-002 Inspectors | REQ-CORE-006 Inspectors |
|--------|-------------------------|-------------------------|
| Purpose | Modify/reject bodies | Enrich policy context |
| Output | `InspectorDecision` | `PolicyContext` additions |
| Timing | After policy decision | Before policy re-evaluation |
| Examples | PII redaction, schema enforcement | PII detection, prompt guard scoring |

## 2. Dependencies

| Requirement | Relationship | Notes |
|-------------|--------------|-------|
| REQ-CORE-002 | **Extends** | Uses Amber Path buffering infrastructure |
| REQ-POL-001 | **Provides to** | Enriched context for Cedar evaluation |
| REQ-CORE-004 | **Uses** | Error responses for inspector failures |
| REQ-CORE-005 | **Coordinates with** | Graceful shutdown waits for inspectors |

## 3. Intent

The system must:
1. Define a standard `ContextInspector` trait for content analysis
2. Provide an `InspectorRegistry` for managing enabled inspectors
3. Execute inspectors with timeout enforcement (50ms budget)
4. Enrich `PolicyContext` with inspector findings
5. Support feature-gated inspectors (e.g., Prompt Guard via ONNX)
6. Include built-in inspectors for common use cases (PII regex, schema validation)

## 4. Scope

### 4.1 In Scope
- `ContextInspector` trait definition
- `InspectorRegistry` for registration and execution
- Timeout enforcement (50ms default)
- Built-in `RegexPiiInspector`
- Built-in `SchemaValidatorInspector`
- Metrics per inspector (duration, timeouts, findings)
- Feature flag support for optional inspectors
- Configuration via environment variables

### 4.2 Out of Scope
- Body modification (REQ-CORE-002)
- Policy evaluation (REQ-POL-001)
- Prompt Guard implementation (v1.0, feature-gated)
- ONNX runtime integration (v1.0)

### 4.3 Future Scope (v1.0+)
- `PromptGuardInspector` using Llama Prompt Guard via ONNX
- Additional ML-based inspectors
- Custom inspector loading via configuration

## 5. Constraints

### 5.1 Runtime & Dependencies

| Crate | Purpose | Notes |
|-------|---------|-------|
| `tokio` | Async runtime | Timeout enforcement |
| `regex` | Pattern matching | PII detection |
| `jsonschema` | Schema validation | Tool argument validation |
| `serde_json` | JSON handling | Argument extraction |
| `metrics` | Observability | Per-inspector metrics |

**Feature-Gated Dependencies (v1.0):**

| Crate | Feature Flag | Purpose |
|-------|--------------|---------|
| `ort` | `prompt-guard` | ONNX Runtime |
| `tokenizers` | `prompt-guard` | Llama tokenizer |

### 5.2 Configuration

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| Inspector timeout | `50ms` | `THOUGHTGATE_INSPECTOR_TIMEOUT_MS` |
| Enable PII inspector | `true` | `THOUGHTGATE_INSPECTOR_PII_ENABLED` |
| Enable schema inspector | `true` | `THOUGHTGATE_INSPECTOR_SCHEMA_ENABLED` |
| PII patterns file | built-in | `THOUGHTGATE_PII_PATTERNS_FILE` |
| Schema directory | none | `THOUGHTGATE_SCHEMA_DIR` |

**v1.0 Configuration (Prompt Guard):**

| Setting | Default | Environment Variable |
|---------|---------|---------------------|
| Enable Prompt Guard | `false` | `THOUGHTGATE_PROMPT_GUARD_ENABLED` |
| Model path | none | `THOUGHTGATE_PROMPT_GUARD_MODEL` |
| Risk threshold | `0.8` | `THOUGHTGATE_PROMPT_GUARD_THRESHOLD` |
| ONNX threads | `2` | `THOUGHTGATE_ONNX_THREADS` |

### 5.3 Latency Budget

**Critical:** Total inspector execution must complete within 50ms.

| Inspector | Target Latency | Notes |
|-----------|----------------|-------|
| Regex PII | < 5ms | Simple pattern matching |
| Schema Validator | < 10ms | JSON Schema validation |
| Prompt Guard (v1.0) | < 40ms | Quantized ONNX model |

If timeout is exceeded:
- Log warning with inspector name
- Continue without that inspector's context
- Increment `thoughtgate_inspector_timeouts_total` metric

## 6. Interface Contract

### 6.1 ContextInspector Trait

```rust
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

/// Inspector that analyzes requests and enriches policy context.
///
/// Context inspectors run during Amber Path processing. They analyze
/// the buffered request and add findings to the Cedar policy context,
/// enabling content-aware policy decisions.
#[async_trait]
pub trait ContextInspector: Send + Sync {
    /// Unique identifier for this inspector.
    ///
    /// Used in metrics, logging, and configuration.
    fn name(&self) -> &'static str;

    /// Analyze request and enrich policy context.
    ///
    /// # Arguments
    /// * `input` - The buffered request data
    /// * `context` - Mutable policy context to enrich
    ///
    /// # Returns
    /// * `Ok(())` - Analysis complete, context may be enriched
    /// * `Err(InspectorError::Fatal(reason))` - Hard block, reject request
    /// * `Err(InspectorError::Skip)` - Inspector not applicable
    async fn inspect(
        &self,
        input: &InspectorInput<'_>,
        context: &mut PolicyContext,
    ) -> Result<(), InspectorError>;

    /// Check if this inspector applies to the given request.
    ///
    /// Called before `inspect()` for fast filtering.
    /// Default implementation returns `true` for all requests.
    fn applies_to(&self, input: &InspectorInput<'_>) -> bool {
        true
    }

    /// Expected latency for this inspector.
    ///
    /// Used for timeout budgeting when multiple inspectors run.
    fn expected_latency(&self) -> Duration {
        Duration::from_millis(10)
    }
}
```

### 6.2 Inspector Input

```rust
/// Input provided to context inspectors.
#[derive(Debug)]
pub struct InspectorInput<'a> {
    /// Tool name being called
    pub tool_name: &'a str,

    /// Tool arguments as JSON
    pub arguments: &'a Value,

    /// Principal making the request
    pub principal: &'a Principal,

    /// Traffic type (MCP, Bridged, HTTP)
    pub traffic_type: TrafficType,

    /// Raw body bytes (for deep inspection)
    pub body: &'a [u8],
}

/// Traffic type classification per RFC-001
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficType {
    /// Native MCP to upstream MCP server
    NativeMcp,
    /// Bridged tool (MCP to HTTP backend)
    BridgedTool,
    /// Direct HTTP (intercepted via HTTP_PROXY)
    DirectHttp,
}
```

### 6.3 Policy Context

```rust
/// Policy context that can be enriched by inspectors.
///
/// Context is passed to Cedar for policy evaluation. Inspectors
/// add fields that policies can reference.
#[derive(Debug, Default)]
pub struct PolicyContext {
    inner: HashMap<String, ContextValue>,
}

/// Values that can be stored in policy context.
#[derive(Debug, Clone)]
pub enum ContextValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    List(Vec<ContextValue>),
}

impl PolicyContext {
    /// Set a context value.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<ContextValue>) {
        self.inner.insert(key.into(), value.into());
    }

    /// Get a context value.
    pub fn get(&self, key: &str) -> Option<&ContextValue> {
        self.inner.get(key)
    }

    /// Check if a key exists.
    pub fn contains(&self, key: &str) -> bool {
        self.inner.contains_key(key)
    }

    /// Convert to Cedar context format.
    pub fn to_cedar_context(&self) -> cedar_policy::Context {
        // Implementation converts HashMap to Cedar's context format
        todo!()
    }
}

// Convenient conversions
impl From<bool> for ContextValue {
    fn from(v: bool) -> Self { ContextValue::Bool(v) }
}

impl From<i64> for ContextValue {
    fn from(v: i64) -> Self { ContextValue::Int(v) }
}

impl From<f64> for ContextValue {
    fn from(v: f64) -> Self { ContextValue::Float(v) }
}

impl From<String> for ContextValue {
    fn from(v: String) -> Self { ContextValue::String(v) }
}

impl From<&str> for ContextValue {
    fn from(v: &str) -> Self { ContextValue::String(v.to_string()) }
}

impl<T: Into<ContextValue>> From<Vec<T>> for ContextValue {
    fn from(v: Vec<T>) -> Self {
        ContextValue::List(v.into_iter().map(Into::into).collect())
    }
}
```

### 6.4 Inspector Errors

```rust
/// Errors that can occur during inspection.
#[derive(Debug, thiserror::Error)]
pub enum InspectorError {
    /// Fatal error - request should be rejected.
    #[error("Inspector fatal: {reason}")]
    Fatal { reason: String },

    /// Skip - inspector not applicable or failed gracefully.
    #[error("Inspector skipped")]
    Skip,

    /// Internal error - log and continue.
    #[error("Inspector internal error: {0}")]
    Internal(#[from] anyhow::Error),

    /// Timeout - inspector took too long.
    #[error("Inspector timeout after {elapsed:?}")]
    Timeout { elapsed: Duration },
}
```

### 6.5 Inspector Registry

```rust
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Registry of enabled context inspectors.
pub struct InspectorRegistry {
    inspectors: Vec<Arc<dyn ContextInspector>>,
    timeout: Duration,
}

impl InspectorRegistry {
    /// Create a new registry with the given timeout.
    pub fn new(timeout: Duration) -> Self {
        Self {
            inspectors: Vec::new(),
            timeout,
        }
    }

    /// Register an inspector.
    pub fn register(&mut self, inspector: Arc<dyn ContextInspector>) {
        tracing::info!(
            inspector = inspector.name(),
            "Registered context inspector"
        );
        self.inspectors.push(inspector);
    }

    /// Run all applicable inspectors, enriching context.
    ///
    /// Inspectors run sequentially. If total time exceeds timeout,
    /// remaining inspectors are skipped.
    pub async fn inspect_all(
        &self,
        input: &InspectorInput<'_>,
        context: &mut PolicyContext,
    ) -> Result<(), InspectorError> {
        let deadline = Instant::now() + self.timeout;

        for inspector in &self.inspectors {
            // Check deadline
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                tracing::warn!(
                    inspector = inspector.name(),
                    "Skipping inspector due to timeout budget"
                );
                metrics::counter!(
                    "thoughtgate_inspector_skipped_total",
                    "inspector" => inspector.name(),
                    "reason" => "timeout_budget"
                );
                continue;
            }

            // Check applicability
            if !inspector.applies_to(input) {
                continue;
            }

            // Run with timeout
            let start = Instant::now();
            let result = timeout(remaining, inspector.inspect(input, context)).await;

            let elapsed = start.elapsed();
            metrics::histogram!(
                "thoughtgate_inspector_duration_seconds",
                elapsed.as_secs_f64(),
                "inspector" => inspector.name()
            );

            match result {
                Ok(Ok(())) => {
                    tracing::debug!(
                        inspector = inspector.name(),
                        elapsed_ms = elapsed.as_millis(),
                        "Inspector completed"
                    );
                }
                Ok(Err(InspectorError::Skip)) => {
                    tracing::debug!(
                        inspector = inspector.name(),
                        "Inspector skipped"
                    );
                }
                Ok(Err(InspectorError::Fatal { reason })) => {
                    tracing::warn!(
                        inspector = inspector.name(),
                        reason = %reason,
                        "Inspector fatal error"
                    );
                    metrics::counter!(
                        "thoughtgate_inspector_fatal_total",
                        "inspector" => inspector.name()
                    );
                    return Err(InspectorError::Fatal { reason });
                }
                Ok(Err(InspectorError::Internal(e))) => {
                    tracing::warn!(
                        inspector = inspector.name(),
                        error = %e,
                        "Inspector internal error, continuing"
                    );
                    metrics::counter!(
                        "thoughtgate_inspector_errors_total",
                        "inspector" => inspector.name()
                    );
                }
                Ok(Err(InspectorError::Timeout { .. })) => {
                    // Shouldn't happen with outer timeout, but handle anyway
                    tracing::warn!(
                        inspector = inspector.name(),
                        "Inspector self-reported timeout"
                    );
                }
                Err(_) => {
                    // Tokio timeout elapsed
                    tracing::warn!(
                        inspector = inspector.name(),
                        elapsed_ms = elapsed.as_millis(),
                        "Inspector timeout"
                    );
                    metrics::counter!(
                        "thoughtgate_inspector_timeouts_total",
                        "inspector" => inspector.name()
                    );
                }
            }
        }

        Ok(())
    }
}
```

## 7. Built-in Inspectors

### 7.1 Regex PII Inspector

Detects potential PII using configurable regex patterns.

```rust
use regex::Regex;

/// Detects potential PII using regex patterns.
pub struct RegexPiiInspector {
    patterns: Vec<PiiPattern>,
}

struct PiiPattern {
    name: String,
    regex: Regex,
    severity: PiiSeverity,
}

#[derive(Debug, Clone, Copy)]
pub enum PiiSeverity {
    Low,    // Email, phone
    Medium, // Address, DOB
    High,   // SSN, credit card
}

impl RegexPiiInspector {
    /// Create with default patterns.
    pub fn default_patterns() -> Self {
        Self {
            patterns: vec![
                PiiPattern {
                    name: "ssn".into(),
                    regex: Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap(),
                    severity: PiiSeverity::High,
                },
                PiiPattern {
                    name: "credit_card".into(),
                    regex: Regex::new(r"\b\d{4}[- ]?\d{4}[- ]?\d{4}[- ]?\d{4}\b").unwrap(),
                    severity: PiiSeverity::High,
                },
                PiiPattern {
                    name: "email".into(),
                    regex: Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b").unwrap(),
                    severity: PiiSeverity::Low,
                },
                PiiPattern {
                    name: "phone_us".into(),
                    regex: Regex::new(r"\b\d{3}[- .]?\d{3}[- .]?\d{4}\b").unwrap(),
                    severity: PiiSeverity::Low,
                },
                PiiPattern {
                    name: "ip_address".into(),
                    regex: Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b").unwrap(),
                    severity: PiiSeverity::Low,
                },
            ],
        }
    }

    /// Load patterns from file.
    pub fn from_file(path: &Path) -> Result<Self, anyhow::Error> {
        // Load YAML/JSON pattern definitions
        todo!()
    }
}

#[async_trait]
impl ContextInspector for RegexPiiInspector {
    fn name(&self) -> &'static str {
        "regex_pii"
    }

    async fn inspect(
        &self,
        input: &InspectorInput<'_>,
        context: &mut PolicyContext,
    ) -> Result<(), InspectorError> {
        // Convert arguments to searchable text
        let text = input.arguments.to_string();

        let mut detected: Vec<String> = Vec::new();
        let mut max_severity = None;

        for pattern in &self.patterns {
            if pattern.regex.is_match(&text) {
                detected.push(pattern.name.clone());
                max_severity = Some(match (max_severity, pattern.severity) {
                    (None, s) => s,
                    (Some(PiiSeverity::Low), s) => s,
                    (Some(PiiSeverity::Medium), PiiSeverity::High) => PiiSeverity::High,
                    (Some(s), _) => s,
                });
            }
        }

        // Enrich context
        context.set("pii_detected", !detected.is_empty());
        context.set("pii_types", detected.clone());
        context.set("pii_count", detected.len() as i64);

        if let Some(severity) = max_severity {
            context.set("pii_max_severity", match severity {
                PiiSeverity::Low => "low",
                PiiSeverity::Medium => "medium",
                PiiSeverity::High => "high",
            });
        }

        tracing::debug!(
            pii_detected = !detected.is_empty(),
            pii_types = ?detected,
            "PII inspection complete"
        );

        Ok(())
    }

    fn expected_latency(&self) -> Duration {
        Duration::from_millis(5)
    }
}
```

**Cedar Policy Examples:**

```cedar
// Require approval if high-severity PII detected
permit(
    principal,
    action == Action::"tools/call",
    resource
) when {
    context.pii_detected == true &&
    context.pii_max_severity == "high"
} with { approval = "required" };

// Block if SSN detected in certain tools
forbid(
    principal,
    action == Action::"tools/call",
    resource == Tool::"send_email"
) when {
    context.pii_types.contains("ssn")
};
```

### 7.2 Schema Validator Inspector

Validates tool arguments against JSON Schema definitions.

```rust
use jsonschema::JSONSchema;
use std::collections::HashMap;

/// Validates tool arguments against JSON Schema.
pub struct SchemaValidatorInspector {
    schemas: HashMap<String, JSONSchema>,
}

impl SchemaValidatorInspector {
    /// Create empty validator.
    pub fn new() -> Self {
        Self {
            schemas: HashMap::new(),
        }
    }

    /// Load schemas from directory.
    ///
    /// Expects files named `{tool_name}.json` containing JSON Schema.
    pub fn from_directory(path: &Path) -> Result<Self, anyhow::Error> {
        let mut schemas = HashMap::new();

        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map(|e| e == "json").unwrap_or(false) {
                let tool_name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| anyhow::anyhow!("Invalid schema filename"))?;

                let schema_json: serde_json::Value =
                    serde_json::from_reader(std::fs::File::open(&path)?)?;

                let schema = JSONSchema::compile(&schema_json)
                    .map_err(|e| anyhow::anyhow!("Invalid schema {}: {}", tool_name, e))?;

                schemas.insert(tool_name.to_string(), schema);
            }
        }

        Ok(Self { schemas })
    }

    /// Register a schema for a tool.
    pub fn register_schema(&mut self, tool_name: &str, schema: JSONSchema) {
        self.schemas.insert(tool_name.to_string(), schema);
    }
}

#[async_trait]
impl ContextInspector for SchemaValidatorInspector {
    fn name(&self) -> &'static str {
        "schema_validator"
    }

    fn applies_to(&self, input: &InspectorInput<'_>) -> bool {
        self.schemas.contains_key(input.tool_name)
    }

    async fn inspect(
        &self,
        input: &InspectorInput<'_>,
        context: &mut PolicyContext,
    ) -> Result<(), InspectorError> {
        let Some(schema) = self.schemas.get(input.tool_name) else {
            return Err(InspectorError::Skip);
        };

        let result = schema.validate(input.arguments);

        let is_valid = result.is_ok();
        let errors: Vec<String> = result
            .err()
            .map(|iter| iter.map(|e| e.to_string()).collect())
            .unwrap_or_default();

        context.set("schema_valid", is_valid);
        context.set("schema_errors", errors.clone());
        context.set("schema_error_count", errors.len() as i64);

        tracing::debug!(
            tool = input.tool_name,
            valid = is_valid,
            error_count = errors.len(),
            "Schema validation complete"
        );

        Ok(())
    }

    fn expected_latency(&self) -> Duration {
        Duration::from_millis(10)
    }
}
```

**Cedar Policy Examples:**

```cedar
// Block requests with invalid schemas
forbid(
    principal,
    action == Action::"tools/call",
    resource
) when {
    context.schema_valid == false
};

// Require approval for valid but unusual argument patterns
permit(
    principal,
    action == Action::"tools/call",
    resource == Tool::"bulk_delete"
) when {
    context.schema_valid == true &&
    context has "argument_count" &&
    context.argument_count > 100
} with { approval = "required" };
```

## 8. v1.0 Feature: Prompt Guard Inspector

**Note:** This section documents the planned v1.0 implementation. It is not in scope for v0.1.

### 8.1 Overview

Prompt Guard uses Meta's Llama Prompt Guard model to detect prompt injection attempts. The model runs locally via ONNX Runtime for low-latency inference.

### 8.2 Feature Flag

```toml
# Cargo.toml
[features]
default = []
prompt-guard = ["ort", "tokenizers"]

[dependencies]
ort = { version = "2.0", optional = true }
tokenizers = { version = "0.15", optional = true }
```

### 8.3 Implementation

```rust
#[cfg(feature = "prompt-guard")]
pub struct PromptGuardInspector {
    session: ort::Session,
    tokenizer: tokenizers::Tokenizer,
    threshold: f32,
    max_length: usize,
}

#[cfg(feature = "prompt-guard")]
impl PromptGuardInspector {
    pub fn new(model_path: &Path, threshold: f32) -> Result<Self, anyhow::Error> {
        let session = ort::Session::builder()?
            .with_optimization_level(ort::GraphOptimizationLevel::Level3)?
            .with_intra_threads(2)?
            .commit_from_file(model_path)?;

        // Load tokenizer (bundled or from HuggingFace)
        let tokenizer = tokenizers::Tokenizer::from_pretrained(
            "meta-llama/Prompt-Guard-86M",
            None,
        )?;

        Ok(Self {
            session,
            tokenizer,
            threshold,
            max_length: 512,
        })
    }
}

#[cfg(feature = "prompt-guard")]
#[async_trait]
impl ContextInspector for PromptGuardInspector {
    fn name(&self) -> &'static str {
        "prompt_guard"
    }

    async fn inspect(
        &self,
        input: &InspectorInput<'_>,
        context: &mut PolicyContext,
    ) -> Result<(), InspectorError> {
        // Extract text from arguments
        let text = extract_promptable_text(input.arguments);
        if text.is_empty() {
            context.set("prompt_guard_score", 0.0);
            context.set("prompt_guard_flagged", false);
            return Ok(());
        }

        // Tokenize
        let encoding = self.tokenizer
            .encode(text.as_str(), true)
            .map_err(|e| InspectorError::Internal(e.into()))?;

        let input_ids: Vec<i64> = encoding
            .get_ids()
            .iter()
            .take(self.max_length)
            .map(|&id| id as i64)
            .collect();

        let attention_mask: Vec<i64> = vec![1i64; input_ids.len()];

        // Run inference
        let outputs = self.session.run(ort::inputs![
            "input_ids" => ndarray::Array::from_shape_vec(
                (1, input_ids.len()),
                input_ids
            )?,
            "attention_mask" => ndarray::Array::from_shape_vec(
                (1, attention_mask.len()),
                attention_mask
            )?
        ]?)?;

        // Extract scores [benign, injection]
        let logits: ndarray::ArrayView2<f32> = outputs[0].try_extract_tensor()?;
        let injection_score = softmax(logits[[0, 0]], logits[[0, 1]]);

        // Enrich context
        context.set("prompt_guard_score", injection_score as f64);
        context.set("prompt_guard_flagged", injection_score > self.threshold);

        tracing::debug!(
            score = injection_score,
            flagged = injection_score > self.threshold,
            threshold = self.threshold,
            "Prompt Guard inspection complete"
        );

        Ok(())
    }

    fn expected_latency(&self) -> Duration {
        Duration::from_millis(40)
    }
}

#[cfg(feature = "prompt-guard")]
fn extract_promptable_text(args: &Value) -> String {
    // Extract string fields that might contain prompts
    let mut texts = Vec::new();

    fn extract_strings(value: &Value, texts: &mut Vec<String>) {
        match value {
            Value::String(s) => texts.push(s.clone()),
            Value::Array(arr) => arr.iter().for_each(|v| extract_strings(v, texts)),
            Value::Object(map) => map.values().for_each(|v| extract_strings(v, texts)),
            _ => {}
        }
    }

    extract_strings(args, &mut texts);
    texts.join(" ")
}

#[cfg(feature = "prompt-guard")]
fn softmax(a: f32, b: f32) -> f32 {
    let max = a.max(b);
    let exp_a = (a - max).exp();
    let exp_b = (b - max).exp();
    exp_b / (exp_a + exp_b)
}
```

### 8.4 Cedar Policy Examples

```cedar
// Require approval for flagged prompts
permit(
    principal,
    action == Action::"tools/call",
    resource
) when {
    context.prompt_guard_flagged == true
} with { approval = "required" };

// Block very high-risk prompts
forbid(
    principal,
    action == Action::"tools/call",
    resource
) when {
    context.prompt_guard_score > 0.95
};

// Allow low-risk prompts through Green path
permit(
    principal,
    action == Action::"tools/call",
    resource
) when {
    context.prompt_guard_score < 0.3
};
```

## 9. Integration with Amber Path

### 9.1 Flow

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                    AMBER PATH WITH INSPECTORS                                   │
│                                                                                 │
│   Request                                                                       │
│       │                                                                         │
│       ▼                                                                         │
│   ┌─────────────────┐                                                          │
│   │  Initial Cedar  │──▶ Green? ──▶ Stream (REQ-CORE-001)                      │
│   │   Evaluation    │──▶ Red? ──▶ Reject (REQ-CORE-004)                        │
│   │                 │──▶ Approval? ──▶ Task (REQ-GOV-001)                      │
│   │                 │──▶ Amber? ──▶ Continue below                             │
│   └─────────────────┘                                                          │
│           │                                                                     │
│           ▼ (Amber Path)                                                       │
│   ┌─────────────────┐                                                          │
│   │  Buffer Body    │  (REQ-CORE-002)                                          │
│   └────────┬────────┘                                                          │
│            │                                                                    │
│            ▼                                                                    │
│   ┌─────────────────┐                                                          │
│   │   Run Context   │  (REQ-CORE-006 - This Requirement)                       │
│   │   Inspectors    │                                                          │
│   │                 │──▶ PII Inspector ──▶ context.pii_detected                │
│   │                 │──▶ Schema Inspector ──▶ context.schema_valid             │
│   │                 │──▶ Prompt Guard (v1.0) ──▶ context.prompt_guard_score    │
│   └────────┬────────┘                                                          │
│            │                                                                    │
│            ▼                                                                    │
│   ┌─────────────────┐                                                          │
│   │  Re-evaluate    │  Cedar with enriched context                             │
│   │  Cedar Policy   │──▶ May change path based on inspection results           │
│   └────────┬────────┘                                                          │
│            │                                                                    │
│            ▼                                                                    │
│   ┌─────────────────┐                                                          │
│   │  Body Inspectors│  (REQ-CORE-002)                                          │
│   │  (modify/reject)│──▶ PII redaction, schema enforcement                     │
│   └────────┬────────┘                                                          │
│            │                                                                    │
│            ▼                                                                    │
│   Forward to upstream                                                           │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 9.2 Two-Phase Inspection

| Phase | Requirement | Purpose | Output |
|-------|-------------|---------|--------|
| **Context Enrichment** | REQ-CORE-006 | Add signals to Cedar context | `PolicyContext` |
| **Body Modification** | REQ-CORE-002 | Modify or reject body | `InspectorDecision` |

Both phases run during Amber Path, but serve different purposes.

## 10. Edge Cases

### 10.1 Edge Case Matrix

| ID | Scenario | Expected Behavior | Test |
|----|----------|-------------------|------|
| EC-001 | Inspector timeout | Skip inspector, log warning, continue | `test_inspector_timeout` |
| EC-002 | Inspector panic | Catch panic, log error, continue | `test_inspector_panic` |
| EC-003 | Inspector fatal error | Reject request with error | `test_inspector_fatal` |
| EC-004 | No inspectors registered | Context unchanged, continue | `test_no_inspectors` |
| EC-005 | Empty arguments | Inspectors receive empty input | `test_empty_arguments` |
| EC-006 | Very large arguments | Inspectors handle gracefully | `test_large_arguments` |
| EC-007 | Invalid JSON in arguments | Inspectors handle gracefully | `test_invalid_json` |
| EC-008 | Multiple PII matches | All matches reported | `test_multiple_pii` |
| EC-009 | Schema not found | Skip validation, log | `test_missing_schema` |
| EC-010 | Timeout budget exhausted | Skip remaining inspectors | `test_budget_exhausted` |

### 10.2 Error Handling

```rust
// Panic safety for inspectors
impl InspectorRegistry {
    async fn run_inspector_safely(
        &self,
        inspector: &Arc<dyn ContextInspector>,
        input: &InspectorInput<'_>,
        context: &mut PolicyContext,
    ) -> Result<(), InspectorError> {
        let result = std::panic::AssertUnwindSafe(
            inspector.inspect(input, context)
        )
        .catch_unwind()
        .await;

        match result {
            Ok(inner) => inner,
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "Unknown panic".to_string());

                tracing::error!(
                    inspector = inspector.name(),
                    panic = %msg,
                    "Inspector panicked"
                );

                metrics::counter!(
                    "thoughtgate_inspector_panics_total",
                    "inspector" => inspector.name()
                );

                Err(InspectorError::Internal(anyhow::anyhow!("Inspector panic: {}", msg)))
            }
        }
    }
}
```

## 11. Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `thoughtgate_inspector_duration_seconds` | Histogram | `inspector` | Time spent in each inspector |
| `thoughtgate_inspector_timeouts_total` | Counter | `inspector` | Inspector timeout count |
| `thoughtgate_inspector_errors_total` | Counter | `inspector` | Inspector internal errors |
| `thoughtgate_inspector_fatal_total` | Counter | `inspector` | Inspector fatal errors (request rejected) |
| `thoughtgate_inspector_panics_total` | Counter | `inspector` | Inspector panics caught |
| `thoughtgate_inspector_skipped_total` | Counter | `inspector`, `reason` | Inspectors skipped |
| `thoughtgate_pii_detected_total` | Counter | `pii_type` | PII detections by type |
| `thoughtgate_schema_validation_total` | Counter | `valid` | Schema validation results |
| `thoughtgate_prompt_guard_score` | Histogram | | Prompt Guard score distribution |

## 12. Definition of Done

### 12.1 Functional Completeness

- [ ] `ContextInspector` trait implemented
- [ ] `InspectorRegistry` implemented with timeout enforcement
- [ ] `PolicyContext` implemented with Cedar conversion
- [ ] `RegexPiiInspector` implemented with default patterns
- [ ] `SchemaValidatorInspector` implemented
- [ ] Integration with Amber Path (REQ-CORE-002)
- [ ] Re-evaluation of Cedar policy after context enrichment

### 12.2 Non-Functional Requirements

- [ ] Total inspector execution < 50ms (p99)
- [ ] Panic safety for all inspectors
- [ ] Graceful timeout handling
- [ ] All metrics implemented

### 12.3 Testing

- [ ] Unit tests for each inspector
- [ ] Unit tests for registry timeout handling
- [ ] Integration test with Amber Path
- [ ] Edge case tests per matrix
- [ ] Benchmark tests for latency

### 12.4 Documentation

- [ ] Rustdoc for all public types
- [ ] Configuration reference
- [ ] Cedar policy examples
- [ ] Inspector development guide

## 13. RFC Alignment

This requirement aligns with RFC-001 as follows:

| RFC-001 Section | Alignment |
|-----------------|-----------|
| §4.4 Path Constraints | Inspectors require Amber Path (buffering) |
| §8.2 Policy Examples | Cedar policies reference inspector context |
| §10.2 Prompt Guard Integration | Feature-gated PromptGuardInspector |
| §11.1 Per-Path Metrics | Inspector metrics included |

### 13.1 Terminology Alignment

Per RFC-001, the following terminology is used:

| Term | Definition |
|------|------------|
| **Approval** | Human or agent approval (formerly "HITL") |
| **Approval Path** | Path requiring approval before execution |
| **Context Inspector** | Inspector that enriches policy context |
| **Body Inspector** | Inspector that modifies/rejects body (REQ-CORE-002) |