//! Bounded operator diagnostics for quarantined durable worker generations.

const MAX_QUARANTINE_MESSAGE_BYTES: usize = 1_024;

pub(super) fn bounded_quarantine_message(value: &str) -> String {
    let mut message = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    message = message.trim().to_owned();
    if message.is_empty() {
        message = "invalid durable workload definition".to_owned();
    }
    if message.len() > MAX_QUARANTINE_MESSAGE_BYTES {
        let boundary = message
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= MAX_QUARANTINE_MESSAGE_BYTES)
            .last()
            .unwrap_or(0);
        message.truncate(boundary);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_is_nonempty_sanitized_and_utf8_byte_bounded() {
        assert_eq!(
            bounded_quarantine_message("\n\t"),
            "invalid durable workload definition"
        );
        let message = bounded_quarantine_message(&format!("bad\n{}", "🦀".repeat(400)));
        assert!(!message.chars().any(char::is_control));
        assert!(message.len() <= MAX_QUARANTINE_MESSAGE_BYTES);
        assert!(!message.is_empty());
    }
}
