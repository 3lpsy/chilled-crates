//! Small parsing helpers used while building the configuration.

use std::collections::HashSet;

/// Normalizes a requested log level to a known value, defaulting to `info`.
pub(crate) fn normalize_log_level(level: Option<String>) -> String {
    match level.as_deref().map(str::trim).map(str::to_ascii_lowercase) {
        Some(l) if matches!(l.as_str(), "error" | "warn" | "info" | "debug" | "trace" | "off") => l,
        _ => "info".to_string(),
    }
}

/// Parses a comma/whitespace-separated crate list into a lower-cased set.
pub(crate) fn parse_overrides(list: &str) -> HashSet<String> {
    list.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_normalizes() {
        // Known levels pass through, lower-cased and trimmed.
        assert_eq!(normalize_log_level(Some("debug".into())), "debug");
        assert_eq!(normalize_log_level(Some("  WARN ".into())), "warn");
        assert_eq!(normalize_log_level(Some("Off".into())), "off");
        // Unknown value and absent value both fall back to `info`.
        assert_eq!(normalize_log_level(Some("verbose".into())), "info");
        assert_eq!(normalize_log_level(Some(String::new())), "info");
        assert_eq!(normalize_log_level(None), "info");
    }

    #[test]
    fn overrides_parse_lowercased() {
        let set = parse_overrides("Serde, tokio ,,FOO\nbar");
        assert!(set.contains("serde"));
        assert!(set.contains("tokio"));
        assert!(set.contains("foo"));
        assert!(set.contains("bar"));
        assert_eq!(set.len(), 4);
        assert!(parse_overrides("").is_empty());
    }
}
