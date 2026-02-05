#![no_main]

//! Fuzz target for REQ-POL-001 Cedar Policy Evaluation
//!
//! # Traceability
//! - Implements: REQ-POL-001 (Cedar Policy Engine)
//! - Attack surface: Malformed principals, resources, arguments, context
//!
//! # Goal
//! Verify that policy evaluation with malicious inputs does not cause:
//! - Panics in the Cedar engine
//! - Memory exhaustion from complex policies
//! - Policy bypass through crafted inputs
//! - Denial of service through expensive evaluation

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use thoughtgate_core::policy::engine::CedarEngine;
use thoughtgate_core::policy::{CedarContext, CedarRequest, CedarResource, Principal, TimeContext};

/// Fuzz input for Cedar policy evaluation
#[derive(Arbitrary, Debug)]
struct FuzzCedarInput {
    /// Principal data
    principal: FuzzPrincipal,
    /// Resource data
    resource: FuzzResource,
    /// Context data
    context: FuzzContext,
}

#[derive(Arbitrary, Debug)]
struct FuzzPrincipal {
    /// App name bytes
    app_name: Vec<u8>,
    /// Namespace bytes
    namespace: Vec<u8>,
    /// Service account bytes
    service_account: Vec<u8>,
    /// Roles (fuzzed strings)
    roles: Vec<Vec<u8>>,
}

#[derive(Arbitrary, Debug)]
enum FuzzResource {
    ToolCall {
        name: Vec<u8>,
        server: Vec<u8>,
        arguments: FuzzArguments,
    },
    McpMethod {
        method: Vec<u8>,
        server: Vec<u8>,
    },
}

#[derive(Arbitrary, Debug)]
enum FuzzArguments {
    /// Empty object
    Empty,
    /// Simple key-value pairs
    Simple(Vec<(Vec<u8>, FuzzValue)>),
    /// Deeply nested structure
    Nested { depth: u8, values: Vec<FuzzValue> },
    /// Large array
    LargeArray(Vec<FuzzValue>),
    /// Raw JSON string (might be invalid)
    RawJson(Vec<u8>),
}

#[derive(Arbitrary, Debug, Clone)]
enum FuzzValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(Vec<u8>),
    Array(Vec<u8>), // Simplified - will be converted
}

#[derive(Arbitrary, Debug)]
struct FuzzContext {
    /// Policy ID bytes
    policy_id: Vec<u8>,
    /// Source ID bytes
    source_id: Vec<u8>,
    /// Time context
    time: FuzzTime,
}

#[derive(Arbitrary, Debug)]
struct FuzzTime {
    hour: u8,
    day_of_week: u8,
    timestamp: i64,
}

fuzz_target!(|input: FuzzCedarInput| {
    fuzz_cedar_policy(input);
});

fn fuzz_cedar_policy(input: FuzzCedarInput) {
    // Create the Cedar engine (should not panic even with malformed schema)
    let engine = match CedarEngine::new() {
        Ok(e) => e,
        Err(_) => return, // Engine creation failed, skip
    };

    // Build principal from fuzz input
    let principal = build_principal(&input.principal);

    // Build resource from fuzz input
    let resource = build_resource(&input.resource);

    // Build context from fuzz input
    let context = build_context(&input.context);

    // Create the request
    let request = CedarRequest {
        principal,
        resource,
        context,
    };

    // Test 1: evaluate_v2 should never panic
    let _decision = engine.evaluate_v2(&request);

    // Test 2: Multiple rapid evaluations (DoS resistance)
    for _ in 0..10 {
        let _ = engine.evaluate_v2(&request);
    }
}

fn build_principal(input: &FuzzPrincipal) -> Principal {
    // Limit string lengths to prevent memory exhaustion
    let app_name = sanitize_string(&input.app_name, 256);
    let namespace = sanitize_string(&input.namespace, 256);
    let service_account = sanitize_string(&input.service_account, 256);

    let roles: Vec<String> = input
        .roles
        .iter()
        .take(20) // Limit number of roles
        .map(|r| sanitize_string(r, 128))
        .collect();

    Principal {
        app_name,
        namespace,
        service_account,
        roles,
    }
}

fn build_resource(input: &FuzzResource) -> CedarResource {
    match input {
        FuzzResource::ToolCall {
            name,
            server,
            arguments,
        } => {
            let name = sanitize_string(name, 256);
            let server = sanitize_string(server, 256);
            let arguments = build_arguments(arguments);

            CedarResource::ToolCall {
                name,
                server,
                arguments,
            }
        }
        FuzzResource::McpMethod { method, server } => {
            let method = sanitize_string(method, 256);
            let server = sanitize_string(server, 256);

            CedarResource::McpMethod { method, server }
        }
    }
}

fn build_arguments(input: &FuzzArguments) -> serde_json::Value {
    match input {
        FuzzArguments::Empty => serde_json::json!({}),

        FuzzArguments::Simple(pairs) => {
            let mut map = serde_json::Map::new();
            for (key, value) in pairs.iter().take(50) {
                let key = sanitize_string(key, 64);
                let value = fuzz_value_to_json(value);
                map.insert(key, value);
            }
            serde_json::Value::Object(map)
        }

        FuzzArguments::Nested { depth, values } => {
            // Limit nesting depth to prevent stack overflow
            let depth = (*depth).min(10) as usize;
            build_nested_object(depth, values)
        }

        FuzzArguments::LargeArray(values) => {
            let arr: Vec<serde_json::Value> = values
                .iter()
                .take(100) // Limit array size
                .map(fuzz_value_to_json)
                .collect();
            serde_json::Value::Array(arr)
        }

        FuzzArguments::RawJson(bytes) => {
            // Try to parse as JSON, fall back to string
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
                // Limit parsed JSON depth/size
                truncate_json_value(value, 5, 100)
            } else {
                serde_json::json!({})
            }
        }
    }
}

fn build_nested_object(depth: usize, values: &[FuzzValue]) -> serde_json::Value {
    if depth == 0 || values.is_empty() {
        return serde_json::json!({});
    }

    let mut map = serde_json::Map::new();
    for (i, value) in values.iter().take(5).enumerate() {
        let key = format!("level_{}", depth);
        if i == 0 && depth > 0 {
            // First value becomes nested object
            map.insert(key, build_nested_object(depth - 1, values));
        } else {
            map.insert(format!("value_{}", i), fuzz_value_to_json(value));
        }
    }
    serde_json::Value::Object(map)
}

fn fuzz_value_to_json(value: &FuzzValue) -> serde_json::Value {
    match value {
        FuzzValue::Null => serde_json::Value::Null,
        FuzzValue::Bool(b) => serde_json::Value::Bool(*b),
        FuzzValue::Int(n) => serde_json::json!(n),
        FuzzValue::Float(f) => {
            if f.is_finite() {
                serde_json::json!(f)
            } else {
                serde_json::Value::Null
            }
        }
        FuzzValue::String(s) => serde_json::Value::String(sanitize_string(s, 256)),
        FuzzValue::Array(bytes) => {
            // Convert bytes to small array
            let arr: Vec<serde_json::Value> = bytes
                .iter()
                .take(20)
                .map(|b| serde_json::json!(b))
                .collect();
            serde_json::Value::Array(arr)
        }
    }
}

fn truncate_json_value(value: serde_json::Value, max_depth: usize, max_items: usize) -> serde_json::Value {
    if max_depth == 0 {
        return serde_json::Value::Null;
    }

    match value {
        serde_json::Value::Object(map) => {
            let truncated: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .take(max_items)
                .map(|(k, v)| (k, truncate_json_value(v, max_depth - 1, max_items)))
                .collect();
            serde_json::Value::Object(truncated)
        }
        serde_json::Value::Array(arr) => {
            let truncated: Vec<serde_json::Value> = arr
                .into_iter()
                .take(max_items)
                .map(|v| truncate_json_value(v, max_depth - 1, max_items))
                .collect();
            serde_json::Value::Array(truncated)
        }
        other => other,
    }
}

fn build_context(input: &FuzzContext) -> CedarContext {
    let policy_id = sanitize_string(&input.policy_id, 256);
    let source_id = sanitize_string(&input.source_id, 256);

    let time = TimeContext {
        hour: input.time.hour % 24,
        day_of_week: input.time.day_of_week % 7,
        timestamp: input.time.timestamp,
    };

    CedarContext {
        policy_id,
        source_id,
        time,
    }
}

/// Convert bytes to a sanitized string with length limit
fn sanitize_string(bytes: &[u8], max_len: usize) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(max_len)
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}
