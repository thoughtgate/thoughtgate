//! Environment variable expansion for MCP server configs.
//!
//! Implements: REQ-CORE-008/F-004

use super::{ConfigError, McpServerEntry};

/// Internal implementation of environment variable expansion.
///
/// When `require_env_prefix` is true, only `${env:VAR}` patterns are expanded;
/// plain `${VAR}` is left unchanged. When false, all `${VAR}` patterns are expanded.
///
/// Both modes support the `:-default` syntax for fallback values.
pub(super) fn expand_vars(input: &str, require_env_prefix: bool) -> Result<String, ConfigError> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_content = String::new();
            let mut found_close = false;
            for inner in chars.by_ref() {
                if inner == '}' {
                    found_close = true;
                    break;
                }
                var_content.push(inner);
            }
            if !found_close {
                // No closing brace — pass through literally.
                result.push('$');
                result.push('{');
                result.push_str(&var_content);
                continue;
            }

            if require_env_prefix {
                if let Some(var_spec) = var_content.strip_prefix("env:") {
                    let value = expand_var_with_default(var_spec)?;
                    result.push_str(&value);
                } else {
                    // Not an env: prefix — pass through unchanged.
                    result.push('$');
                    result.push('{');
                    result.push_str(&var_content);
                    result.push('}');
                }
            } else {
                let value = expand_var_with_default(&var_content)?;
                result.push_str(&value);
            }
        } else {
            result.push(c);
        }
    }

    Ok(result)
}

/// Expand a single variable reference, supporting `VAR:-default` syntax.
///
/// - `VAR` - Returns value of VAR, errors if not set
/// - `VAR:-default` - Returns value of VAR if set, otherwise returns "default"
fn expand_var_with_default(var_spec: &str) -> Result<String, ConfigError> {
    // Check for `:-` default value syntax.
    if let Some((var_name, default_value)) = var_spec.split_once(":-") {
        match std::env::var(var_name) {
            Ok(value) if !value.is_empty() => Ok(value),
            _ => Ok(default_value.to_string()),
        }
    } else {
        std::env::var(var_spec).map_err(|_| ConfigError::UndefinedEnvVar {
            name: var_spec.to_string(),
        })
    }
}

/// Expand environment variables in all string values of a server entry's
/// command and args, using the given expansion function.
pub(super) fn expand_server_entry_env(
    entry: &mut McpServerEntry,
    expander: fn(&str) -> Result<String, ConfigError>,
) -> Result<(), ConfigError> {
    entry.command = expander(&entry.command)?;
    entry.args = entry
        .args
        .iter()
        .map(|a| expander(a))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(())
}
