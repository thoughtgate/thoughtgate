//! Cardinality limiter for metric labels.
//!
//! Implements: REQ-OBS-002 §6.5 (Cardinality Management)

use std::collections::HashSet;

use parking_lot::Mutex;

/// Limits cardinality of metric labels to prevent unbounded time series growth.
///
/// When distinct values exceed `max_values`, new values are mapped to `"__other__"`.
///
/// Implements: REQ-OBS-002 §6.5
pub struct CardinalityLimiter {
    known: Mutex<HashSet<String>>,
    max_values: usize,
}

impl CardinalityLimiter {
    /// Create a new limiter with the specified maximum distinct values.
    pub fn new(max_values: usize) -> Self {
        Self {
            known: Mutex::new(HashSet::new()),
            max_values,
        }
    }

    /// Resolve a label value, returning "__other__" if cardinality exceeded.
    ///
    /// If the value is already known, returns it unchanged.
    /// If the value is new and we're under the limit, registers and returns it.
    /// If we're at the limit and the value is unknown, returns "__other__".
    pub fn resolve<'a>(&self, value: &'a str) -> &'a str {
        let mut known = self.known.lock();
        if known.contains(value) {
            value
        } else if known.len() < self.max_values {
            known.insert(value.to_string());
            value
        } else {
            "__other__"
        }
    }

    /// Returns the current count of distinct values.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.known.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limiter_allows_up_to_max() {
        let limiter = CardinalityLimiter::new(3);
        assert_eq!(limiter.resolve("a"), "a");
        assert_eq!(limiter.resolve("b"), "b");
        assert_eq!(limiter.resolve("c"), "c");
        assert_eq!(limiter.count(), 3);
    }

    #[test]
    fn test_limiter_returns_other_after_max() {
        let limiter = CardinalityLimiter::new(2);
        assert_eq!(limiter.resolve("a"), "a");
        assert_eq!(limiter.resolve("b"), "b");
        assert_eq!(limiter.resolve("c"), "__other__");
        assert_eq!(limiter.resolve("d"), "__other__");
        assert_eq!(limiter.count(), 2);
    }

    #[test]
    fn test_limiter_remembers_known() {
        let limiter = CardinalityLimiter::new(2);
        assert_eq!(limiter.resolve("a"), "a");
        assert_eq!(limiter.resolve("b"), "b");
        assert_eq!(limiter.resolve("a"), "a"); // Still returns "a"
    }
}
