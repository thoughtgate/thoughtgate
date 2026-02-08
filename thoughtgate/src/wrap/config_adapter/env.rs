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

#[cfg(test)]
mod tests {
    use super::*;

    // ── expand_vars (dollar-brace mode) ───────────────────────────────

    #[test]
    fn test_expand_vars_no_vars() {
        assert_eq!(expand_vars("hello world", false).unwrap(), "hello world");
    }

    #[test]
    fn test_expand_vars_known_var() {
        // SAFETY: test-only; no other threads depend on this var.
        unsafe { std::env::set_var("TG_TEST_EV1", "expanded") };
        assert_eq!(expand_vars("${TG_TEST_EV1}", false).unwrap(), "expanded");
        unsafe { std::env::remove_var("TG_TEST_EV1") };
    }

    #[test]
    fn test_expand_vars_with_default_unset() {
        unsafe { std::env::remove_var("TG_TEST_EV2_MISSING") };
        assert_eq!(
            expand_vars("${TG_TEST_EV2_MISSING:-fallback}", false).unwrap(),
            "fallback"
        );
    }

    #[test]
    fn test_expand_vars_with_default_set() {
        unsafe { std::env::set_var("TG_TEST_EV3", "real") };
        assert_eq!(
            expand_vars("${TG_TEST_EV3:-fallback}", false).unwrap(),
            "real"
        );
        unsafe { std::env::remove_var("TG_TEST_EV3") };
    }

    #[test]
    fn test_expand_vars_undefined_no_default() {
        unsafe { std::env::remove_var("TG_TEST_EV4_MISSING") };
        let result = expand_vars("${TG_TEST_EV4_MISSING}", false);
        assert!(result.is_err());
    }

    #[test]
    fn test_expand_vars_mixed_text() {
        unsafe { std::env::set_var("TG_TEST_EV5", "world") };
        assert_eq!(
            expand_vars("hello ${TG_TEST_EV5}!", false).unwrap(),
            "hello world!"
        );
        unsafe { std::env::remove_var("TG_TEST_EV5") };
    }

    #[test]
    fn test_expand_vars_unclosed_brace_passthrough() {
        assert_eq!(expand_vars("${unclosed", false).unwrap(), "${unclosed");
    }

    // ── expand_vars (env-colon mode) ──────────────────────────────────

    #[test]
    fn test_expand_env_colon_mode() {
        unsafe { std::env::set_var("TG_TEST_EV6", "envval") };
        assert_eq!(expand_vars("${env:TG_TEST_EV6}", true).unwrap(), "envval");
        unsafe { std::env::remove_var("TG_TEST_EV6") };
    }

    #[test]
    fn test_expand_env_colon_ignores_plain_var() {
        assert_eq!(expand_vars("${PLAIN_VAR}", true).unwrap(), "${PLAIN_VAR}");
    }

    // ── expand_server_entry_env ───────────────────────────────────────

    #[test]
    fn test_expand_server_entry_env_command_and_args() {
        unsafe { std::env::set_var("TG_TEST_EV7", "/usr/bin/node") };
        let mut entry = McpServerEntry {
            id: "test".to_string(),
            command: "${TG_TEST_EV7}".to_string(),
            args: vec!["--flag".to_string(), "${TG_TEST_EV7}".to_string()],
            env: None,
            enabled: true,
        };
        let expander: fn(&str) -> Result<String, ConfigError> = |s| expand_vars(s, false);
        expand_server_entry_env(&mut entry, expander).unwrap();
        assert_eq!(entry.command, "/usr/bin/node");
        assert_eq!(entry.args, vec!["--flag", "/usr/bin/node"]);
        unsafe { std::env::remove_var("TG_TEST_EV7") };
    }
}
