//! Bounded persisted checker diagnostics.

const DEFAULT_MAX_BYTES: usize = 4 * 1024;
const MIN_MAX_BYTES: usize = 256;
const ABSOLUTE_MAX_BYTES: usize = 16 * 1024;

fn configured_max_bytes() -> usize {
    bounded_value(
        std::env::var("RSCTF_CHECKER_DIAGNOSTIC_MAX_BYTES")
            .ok()
            .as_deref(),
        MIN_MAX_BYTES,
        ABSOLUTE_MAX_BYTES,
        DEFAULT_MAX_BYTES,
    )
}

fn bounded_value(value: Option<&str>, minimum: usize, maximum: usize, default: usize) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .unwrap_or(default)
}

pub(crate) fn bounded_optional_diagnostic(message: Option<String>) -> Option<String> {
    message.map(bounded_diagnostic)
}

pub(crate) fn bounded_diagnostic(message: String) -> String {
    bounded_diagnostic_with_limit(message, configured_max_bytes())
}

fn bounded_diagnostic_with_limit(mut message: String, max_bytes: usize) -> String {
    if message.len() <= max_bytes {
        return message;
    }

    let original_bytes = message.len();
    let suffix = format!("… [truncated from {original_bytes} bytes]");
    let mut end = max_bytes.saturating_sub(suffix.len());
    while !message.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    message.truncate(end);
    message.push_str(&suffix);
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_configuration_uses_the_bounded_default() {
        assert_eq!(
            bounded_value(Some("999999"), MIN_MAX_BYTES, ABSOLUTE_MAX_BYTES, 1234),
            1234
        );
        assert_eq!(
            bounded_value(Some("512"), MIN_MAX_BYTES, ABSOLUTE_MAX_BYTES, 1234),
            512
        );
    }

    #[test]
    fn diagnostic_truncation_is_utf8_safe_and_self_describing() {
        let original = "é".repeat(DEFAULT_MAX_BYTES);
        let bounded = bounded_diagnostic_with_limit(original.clone(), DEFAULT_MAX_BYTES);

        assert!(bounded.len() <= DEFAULT_MAX_BYTES);
        assert!(bounded.contains(&format!("truncated from {} bytes", original.len())));
        assert!(std::str::from_utf8(bounded.as_bytes()).is_ok());
    }
}
