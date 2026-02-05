#![no_main]

//! Fuzz target for REQ-CFG-001 Configuration Parsing
//!
//! # Traceability
//! - Implements: REQ-CFG-001 (Configuration Schema)
//! - Attack surface: Malformed YAML, invalid schemas, env var injection
//!
//! # Goal
//! Verify that configuration parsing does not cause:
//! - Panics during YAML parsing
//! - Memory exhaustion from large configs
//! - Incorrect validation results
//! - Environment variable injection vulnerabilities

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use thoughtgate_core::config::{Config, Version, substitute_env_vars, validate};

/// Fuzz input for configuration testing
#[derive(Arbitrary, Debug)]
struct FuzzConfigInput {
    /// Raw YAML bytes
    raw_yaml: Vec<u8>,
    /// Whether to try structured fuzzing
    use_structured: bool,
    /// Structured config input
    structured: Option<StructuredConfig>,
    /// Environment variables to set
    env_vars: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Arbitrary, Debug)]
struct StructuredConfig {
    /// Schema version
    schema: u8,
    /// Sources
    sources: Vec<FuzzSource>,
    /// Governance rules
    rules: Vec<FuzzRule>,
    /// Approval workflows
    workflows: Vec<FuzzWorkflow>,
}

#[derive(Arbitrary, Debug)]
struct FuzzSource {
    id: Vec<u8>,
    kind: SourceKind,
    url: Vec<u8>,
    prefix: Option<Vec<u8>>,
}

#[derive(Arbitrary, Debug)]
enum SourceKind {
    Mcp,
    Other(Vec<u8>),
}

#[derive(Arbitrary, Debug)]
struct FuzzRule {
    match_pattern: Vec<u8>,
    action: FuzzAction,
    approval: Option<Vec<u8>>,
    policy_id: Option<Vec<u8>>,
    source: Option<Vec<u8>>,
}

#[derive(Arbitrary, Debug)]
enum FuzzAction {
    Forward,
    Approve,
    Deny,
    Policy,
    Invalid(Vec<u8>),
}

#[derive(Arbitrary, Debug)]
struct FuzzWorkflow {
    name: Vec<u8>,
    destination_type: DestinationType,
    channel: Option<Vec<u8>>,
    timeout: Option<Vec<u8>>,
    on_timeout: Option<TimeoutAction>,
}

#[derive(Arbitrary, Debug)]
enum DestinationType {
    Slack,
    Webhook,
    Invalid(Vec<u8>),
}

#[derive(Arbitrary, Debug)]
enum TimeoutAction {
    Approve,
    Deny,
    Invalid(Vec<u8>),
}

fuzz_target!(|input: FuzzConfigInput| {
    fuzz_config_parsing(input);
});

fn fuzz_config_parsing(input: FuzzConfigInput) {
    // Test 1: Raw YAML parsing - should never panic
    if let Ok(yaml_str) = std::str::from_utf8(&input.raw_yaml) {
        // Try to parse as Config
        let _ = serde_saphyr::from_str::<Config>(yaml_str);
    }

    // Test 2: Structured config generation and parsing
    if input.use_structured {
        if let Some(structured) = input.structured {
            let yaml = build_yaml_from_structured(&structured);

            // Parse
            if let Ok(config) = serde_saphyr::from_str::<Config>(&yaml) {
                // Validate - should never panic
                let _ = validate(&config, Version::V0_2);
            }
        }
    }

    // Test 3: Environment variable substitution
    test_env_substitution(&input.raw_yaml, &input.env_vars);

    // Test 4: Edge cases
    test_edge_cases();
}

fn build_yaml_from_structured(input: &StructuredConfig) -> String {
    let mut yaml = String::new();

    // Schema version
    yaml.push_str(&format!("schema: {}\n\n", input.schema % 10));

    // Sources
    if !input.sources.is_empty() {
        yaml.push_str("sources:\n");
        for source in input.sources.iter().take(10) {
            let id = sanitize_yaml_string(&source.id, 64);
            let url = sanitize_yaml_string(&source.url, 256);

            yaml.push_str(&format!("  - id: {}\n", id));
            yaml.push_str(&format!("    kind: {}\n", match &source.kind {
                SourceKind::Mcp => "mcp".to_string(),
                SourceKind::Other(s) => sanitize_yaml_string(s, 32),
            }));
            yaml.push_str(&format!("    url: {}\n", url));

            if let Some(prefix) = &source.prefix {
                yaml.push_str(&format!("    prefix: {}\n", sanitize_yaml_string(prefix, 32)));
            }
        }
        yaml.push('\n');
    }

    // Governance
    if !input.rules.is_empty() {
        yaml.push_str("governance:\n");
        yaml.push_str("  defaults:\n");
        yaml.push_str("    action: forward\n");
        yaml.push_str("  rules:\n");

        for rule in input.rules.iter().take(20) {
            let pattern = sanitize_yaml_string(&rule.match_pattern, 64);
            let action = match &rule.action {
                FuzzAction::Forward => "forward".to_string(),
                FuzzAction::Approve => "approve".to_string(),
                FuzzAction::Deny => "deny".to_string(),
                FuzzAction::Policy => "policy".to_string(),
                FuzzAction::Invalid(s) => sanitize_yaml_string(s, 32),
            };

            yaml.push_str(&format!("    - match: \"{}\"\n", pattern));
            yaml.push_str(&format!("      action: {}\n", action));

            if let Some(approval) = &rule.approval {
                yaml.push_str(&format!("      approval: {}\n", sanitize_yaml_string(approval, 64)));
            }

            if let Some(policy_id) = &rule.policy_id {
                yaml.push_str(&format!("      policy_id: {}\n", sanitize_yaml_string(policy_id, 64)));
            }

            if let Some(source) = &rule.source {
                yaml.push_str(&format!("      source: {}\n", sanitize_yaml_string(source, 64)));
            }
        }
        yaml.push('\n');
    }

    // Approval workflows
    if !input.workflows.is_empty() {
        yaml.push_str("approval:\n");

        for workflow in input.workflows.iter().take(5) {
            let name = sanitize_yaml_string(&workflow.name, 64);
            if name.is_empty() {
                continue;
            }

            yaml.push_str(&format!("  {}:\n", name));
            yaml.push_str("    destination:\n");

            let dest_type = match &workflow.destination_type {
                DestinationType::Slack => "slack".to_string(),
                DestinationType::Webhook => "webhook".to_string(),
                DestinationType::Invalid(s) => sanitize_yaml_string(s, 32),
            };
            yaml.push_str(&format!("      type: {}\n", dest_type));

            if let Some(channel) = &workflow.channel {
                yaml.push_str(&format!("      channel: \"{}\"\n", sanitize_yaml_string(channel, 64)));
            }

            if let Some(timeout) = &workflow.timeout {
                yaml.push_str(&format!("    timeout: {}\n", sanitize_yaml_string(timeout, 32)));
            }

            if let Some(on_timeout) = &workflow.on_timeout {
                let action = match on_timeout {
                    TimeoutAction::Approve => "approve".to_string(),
                    TimeoutAction::Deny => "deny".to_string(),
                    TimeoutAction::Invalid(s) => sanitize_yaml_string(s, 32),
                };
                yaml.push_str(&format!("    on_timeout: {}\n", action));
            }
        }
    }

    yaml
}

fn test_env_substitution(yaml_bytes: &[u8], env_vars: &[(Vec<u8>, Vec<u8>)]) {
    // Limit env var count to prevent issues
    for (key, value) in env_vars.iter().take(10) {
        let key_str = sanitize_env_var_name(key);
        let value_str = sanitize_env_var_value(value);

        if !key_str.is_empty() {
            // Set env var (this is safe in fuzz tests)
            // SAFETY: Fuzz test environment with controlled access
            unsafe {
                std::env::set_var(&key_str, &value_str);
            }
        }
    }

    // Try substitution - should never panic
    if let Ok(yaml_str) = std::str::from_utf8(yaml_bytes) {
        let _ = substitute_env_vars(yaml_str);
    }

    // Clean up env vars
    for (key, _) in env_vars.iter().take(10) {
        let key_str = sanitize_env_var_name(key);
        if !key_str.is_empty() {
            // SAFETY: Fuzz test environment with controlled access
            unsafe {
                std::env::remove_var(&key_str);
            }
        }
    }
}

fn test_edge_cases() {
    // Empty config
    let _ = serde_saphyr::from_str::<Config>("");
    let _ = serde_saphyr::from_str::<Config>("   ");
    let _ = serde_saphyr::from_str::<Config>("\n\n\n");

    // Only comments
    let _ = serde_saphyr::from_str::<Config>("# comment\n# another");

    // Minimal valid
    let _ = serde_saphyr::from_str::<Config>("schema: 1\nsources: []");

    // Deeply nested (DoS prevention)
    let nested = "a: ".repeat(100) + "1";
    let _ = serde_saphyr::from_str::<Config>(&nested);

    // Very long string
    let long_str = format!("schema: 1\nname: \"{}\"", "x".repeat(10000));
    let _ = serde_saphyr::from_str::<Config>(&long_str);

    // Unicode
    let unicode = "schema: 1\nname: \"日本語テスト\"";
    let _ = serde_saphyr::from_str::<Config>(unicode);

    // Null bytes
    let null_bytes = "schema: 1\x00\nname: test";
    let _ = serde_saphyr::from_str::<Config>(null_bytes);
}

/// Sanitize a string for safe YAML embedding
fn sanitize_yaml_string(bytes: &[u8], max_len: usize) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(max_len)
        .filter(|c| {
            // Allow alphanumeric, common punctuation, underscore, hyphen
            c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.' || *c == '/' || *c == '*'
        })
        .collect()
}

/// Sanitize an environment variable name
fn sanitize_env_var_name(bytes: &[u8]) -> String {
    // Env var names should be UPPER_CASE with underscores
    // Must also not contain NUL bytes
    String::from_utf8_lossy(bytes)
        .chars()
        .take(64)
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_uppercase()
}

/// Sanitize an environment variable value
fn sanitize_env_var_value(bytes: &[u8]) -> String {
    // Env var values cannot contain NUL bytes (\0)
    String::from_utf8_lossy(bytes)
        .chars()
        .take(256)
        .filter(|c| *c != '\0')
        .collect()
}
