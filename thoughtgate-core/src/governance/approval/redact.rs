//! Configurable field redaction for Slack approval messages.
//!
//! Replaces matching field values with `"[REDACTED]"` before sending
//! tool_arguments to Slack, preventing sensitive data exposure.
//!
//! Supports:
//! - Top-level fields: `"password"` matches `{"password": "..."}`.
//! - Dot-notation: `"credentials.api_key"` matches
//!   `{"credentials": {"api_key": "..."}}`.

use serde_json::Value;

/// Redact fields in a JSON value based on the configured field paths.
///
/// Returns a new `Value` with matching field values replaced by `"[REDACTED]"`.
/// If `fields` is empty, returns the value unchanged (no allocation).
pub fn redact_fields(value: &Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        return value.clone();
    }

    let mut result = value.clone();
    for field in fields {
        let parts: Vec<&str> = field.split('.').collect();
        redact_path(&mut result, &parts);
    }
    result
}

/// Recursively walk the JSON to redact a specific dotted path.
fn redact_path(value: &mut Value, path: &[&str]) {
    match path {
        [] => {}
        [key] => {
            // Terminal: redact this key if it exists
            if let Value::Object(map) = value {
                if map.contains_key(*key) {
                    map.insert((*key).to_string(), Value::String("[REDACTED]".to_string()));
                }
            }
        }
        [key, rest @ ..] => {
            // Intermediate: descend into this key
            if let Value::Object(map) = value {
                if let Some(child) = map.get_mut(*key) {
                    redact_path(child, rest);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_redact_top_level_field() {
        let value = json!({"password": "secret", "user": "admin"});
        let result = redact_fields(&value, &["password".to_string()]);
        assert_eq!(result["password"], "[REDACTED]");
        assert_eq!(result["user"], "admin");
    }

    #[test]
    fn test_redact_nested_field() {
        let value = json!({"credentials": {"api_key": "sk-123", "region": "us-east-1"}});
        let result = redact_fields(&value, &["credentials.api_key".to_string()]);
        assert_eq!(result["credentials"]["api_key"], "[REDACTED]");
        assert_eq!(result["credentials"]["region"], "us-east-1");
    }

    #[test]
    fn test_redact_missing_field_is_noop() {
        let value = json!({"user": "admin"});
        let result = redact_fields(&value, &["password".to_string()]);
        assert_eq!(result, value);
    }

    #[test]
    fn test_redact_empty_config_is_noop() {
        let value = json!({"password": "secret"});
        let result = redact_fields(&value, &[]);
        assert_eq!(result, value);
    }

    #[test]
    fn test_redact_multiple_fields() {
        let value = json!({"password": "secret", "api_key": "sk-123", "name": "test"});
        let result = redact_fields(&value, &["password".to_string(), "api_key".to_string()]);
        assert_eq!(result["password"], "[REDACTED]");
        assert_eq!(result["api_key"], "[REDACTED]");
        assert_eq!(result["name"], "test");
    }

    #[test]
    fn test_redact_deeply_nested() {
        let value = json!({"a": {"b": {"c": "deep_secret"}}});
        let result = redact_fields(&value, &["a.b.c".to_string()]);
        assert_eq!(result["a"]["b"]["c"], "[REDACTED]");
    }

    #[test]
    fn test_redact_missing_intermediate_is_noop() {
        let value = json!({"a": {"x": "value"}});
        let result = redact_fields(&value, &["a.b.c".to_string()]);
        assert_eq!(result, value);
    }
}
