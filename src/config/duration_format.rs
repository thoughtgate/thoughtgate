//! Duration parsing for configuration files.
//!
//! Implements: REQ-CFG-001 Section 5.4 (Duration Format)
//!
//! Supports two formats:
//! - `humantime`: `10m`, `1h 30m`, `2d`
//! - ISO 8601: `PT10M`, `PT1H30M`, `P2D`

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::time::Duration;

/// Parse a duration string.
///
/// Tries humantime first, then ISO 8601.
///
/// # Traceability
/// - Implements: REQ-CFG-001 Section 5.4 (Duration Format)
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    // Try humantime first (10m, 1h 30m, etc.)
    if let Ok(d) = humantime::parse_duration(s) {
        return Ok(d);
    }

    // Fall back to ISO 8601 (PT10M, PT1H30M, etc.)
    if let Ok(d) = iso8601_duration::Duration::parse(s) {
        if let Some(std_duration) = d.to_std() {
            return Ok(std_duration);
        }
    }

    Err(format!(
        "invalid duration '{}': expected humantime (10m) or ISO 8601 (PT10M)",
        s
    ))
}

/// Deserialize a duration from a string.
#[allow(dead_code)]
pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    parse_duration(&s).map_err(serde::de::Error::custom)
}

/// Deserialize an optional duration from a string.
pub fn deserialize_option<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        Some(s) => parse_duration(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

/// Serialize a duration to a humantime string.
#[allow(dead_code)]
pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let s = humantime::format_duration(*duration).to_string();
    s.serialize(serializer)
}

/// Serialize an optional duration to a humantime string.
#[allow(dead_code)]
pub fn serialize_option<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match duration {
        Some(d) => {
            let s = humantime::format_duration(*d).to_string();
            serializer.serialize_some(&s)
        }
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_humantime_seconds() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn test_parse_humantime_minutes() {
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
    }

    #[test]
    fn test_parse_humantime_hours() {
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
    }

    #[test]
    fn test_parse_humantime_compound() {
        assert_eq!(parse_duration("1h 30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(
            parse_duration("2h 30m 15s").unwrap(),
            Duration::from_secs(9015)
        );
    }

    #[test]
    fn test_parse_iso8601_minutes() {
        assert_eq!(parse_duration("PT10M").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("PT30M").unwrap(), Duration::from_secs(1800));
    }

    #[test]
    fn test_parse_iso8601_hours() {
        assert_eq!(parse_duration("PT1H").unwrap(), Duration::from_secs(3600));
        assert_eq!(
            parse_duration("PT1H30M").unwrap(),
            Duration::from_secs(5400)
        );
    }

    #[test]
    fn test_parse_invalid() {
        assert!(parse_duration("invalid").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn test_serde_roundtrip() {
        #[derive(Debug, Deserialize, Serialize, PartialEq)]
        struct TestStruct {
            #[serde(deserialize_with = "deserialize", serialize_with = "serialize")]
            duration: Duration,
        }

        let yaml = "duration: 10m\n";
        let parsed: TestStruct = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.duration, Duration::from_secs(600));

        // Note: serialized form may differ (humantime format)
        let serialized = serde_yaml::to_string(&parsed).unwrap();
        let reparsed: TestStruct = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn test_serde_option_some() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct TestStruct {
            #[serde(default, deserialize_with = "deserialize_option")]
            duration: Option<Duration>,
        }

        let yaml = "duration: 5m\n";
        let parsed: TestStruct = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.duration, Some(Duration::from_secs(300)));
    }

    #[test]
    fn test_serde_option_none() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct TestStruct {
            #[serde(default, deserialize_with = "deserialize_option")]
            duration: Option<Duration>,
        }

        let yaml = "{}\n";
        let parsed: TestStruct = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.duration, None);
    }
}
