//! Canonical byte policies shared by flag authors, generators, graders, and
//! delivery paths.
//!
//! Normal Jeopardy answers must fit the player submission envelope. A&D flags
//! have a separate, fixed grammar and are deliberately not widened when the
//! normal-answer policy changes.

use std::fmt;

pub const NORMAL_FLAG_MAX_BYTES: usize = 127;
pub const AD_FLAG_BYTES: usize = 38;

const GUID_TOKEN: &str = "[GUID]";
const UUID_TOKEN: &str = "[UUID]";
const TEAM_HASH_TOKEN: &str = "[TEAM_HASH]";
const UUID_EXPANDED_BYTES: usize = 36;
const TEAM_HASH_EXPANDED_BYTES: usize = 16;

/// The exact boundary-whitespace alphabet shared with PostgreSQL and the web
/// flag-import parser. This is Unicode White_Space plus U+FEFF, which browsers
/// also treat as trim whitespace. Keeping the set explicit prevents locale or
/// runtime Unicode-table differences from changing the accepted flag grammar.
pub fn is_flag_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{0085}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

pub fn has_boundary_whitespace(value: &str) -> bool {
    value.chars().next().is_some_and(is_flag_whitespace)
        || value.chars().next_back().is_some_and(is_flag_whitespace)
}

/// Empty and whitespace-only template input both select the bounded default
/// generator. Non-empty input is preserved verbatim for canonical validation.
pub fn is_blank(value: &str) -> bool {
    value.chars().all(is_flag_whitespace)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlagPolicyError {
    Empty,
    SurroundingWhitespace,
    TooLong { actual: usize, maximum: usize },
    TemplateTooTrivial,
    InvalidAdGrammar,
}

impl fmt::Display for FlagPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Flag must not be empty"),
            Self::SurroundingWhitespace => {
                formatter.write_str("Flag must not contain surrounding whitespace")
            }
            Self::TooLong { actual, maximum } => write!(
                formatter,
                "Flag is {actual} UTF-8 bytes; the maximum is {maximum} bytes"
            ),
            Self::TemplateTooTrivial => formatter.write_str(
                "Flag template must contain a [GUID], [UUID], or [TEAM_HASH] placeholder",
            ),
            Self::InvalidAdGrammar => formatter.write_str("Flag does not match the A&D grammar"),
        }
    }
}

/// Validate an exact normal answer. Byte length, rather than Unicode scalar or
/// grapheme count, matches the player API and PostgreSQL index boundary.
pub fn validate_normal(value: &str) -> Result<(), FlagPolicyError> {
    if value.is_empty() {
        return Err(FlagPolicyError::Empty);
    }
    if value.len() > NORMAL_FLAG_MAX_BYTES {
        return Err(FlagPolicyError::TooLong {
            actual: value.len(),
            maximum: NORMAL_FLAG_MAX_BYTES,
        });
    }
    if has_boundary_whitespace(value) {
        return Err(FlagPolicyError::SurroundingWhitespace);
    }
    Ok(())
}

fn occurrence_count(value: &str, needle: &str) -> usize {
    value.match_indices(needle).count()
}

/// Return the exact byte length produced by the production replacement
/// grammar without allocating the expanded flag. All occurrences count.
pub fn expanded_template_bytes(template: &str) -> Option<usize> {
    let guid_count = occurrence_count(template, GUID_TOKEN);
    let uuid_count = occurrence_count(template, UUID_TOKEN);
    let team_hash_count = occurrence_count(template, TEAM_HASH_TOKEN);
    template
        .len()
        .checked_add(guid_count.checked_mul(UUID_EXPANDED_BYTES - GUID_TOKEN.len())?)?
        .checked_add(uuid_count.checked_mul(UUID_EXPANDED_BYTES - UUID_TOKEN.len())?)?
        .checked_add(team_hash_count.checked_mul(TEAM_HASH_EXPANDED_BYTES - TEAM_HASH_TOKEN.len())?)
}

/// Validate a dynamic-container template against the worst-case production
/// expansion. Blank templates are represented as `None` by callers and use the
/// bounded default generator instead.
pub fn validate_dynamic_template(template: &str) -> Result<(), FlagPolicyError> {
    if template.is_empty() {
        return Err(FlagPolicyError::Empty);
    }
    // Expansion never shrinks a supported token, so this cheap byte check
    // rejects attacker-sized templates before the placeholder scans below.
    if template.len() > NORMAL_FLAG_MAX_BYTES {
        return Err(FlagPolicyError::TooLong {
            actual: template.len(),
            maximum: NORMAL_FLAG_MAX_BYTES,
        });
    }
    if has_boundary_whitespace(template) {
        return Err(FlagPolicyError::SurroundingWhitespace);
    }
    if !(template.contains(GUID_TOKEN)
        || template.contains(UUID_TOKEN)
        || template.contains(TEAM_HASH_TOKEN))
    {
        return Err(FlagPolicyError::TemplateTooTrivial);
    }
    let actual = expanded_template_bytes(template).unwrap_or(usize::MAX);
    if actual > NORMAL_FLAG_MAX_BYTES {
        return Err(FlagPolicyError::TooLong {
            actual,
            maximum: NORMAL_FLAG_MAX_BYTES,
        });
    }
    Ok(())
}

/// A&D round flags are exactly `flag{` + 32 URL-safe alphanumeric bytes + `}`.
pub fn validate_ad(value: &str) -> Result<(), FlagPolicyError> {
    const PREFIX: &[u8] = b"flag{";
    const PAYLOAD_BYTES: usize = 32;
    let bytes = value.as_bytes();
    let valid = bytes.len() == AD_FLAG_BYTES
        && bytes.starts_with(PREFIX)
        && bytes.ends_with(b"}")
        && bytes[PREFIX.len()..PREFIX.len() + PAYLOAD_BYTES]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(FlagPolicyError::InvalidAdGrammar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_policy_uses_utf8_bytes_and_the_submission_ceiling() {
        assert_eq!(validate_normal(&"a".repeat(127)), Ok(()));
        assert!(matches!(
            validate_normal(&"a".repeat(128)),
            Err(FlagPolicyError::TooLong { actual: 128, .. })
        ));
        assert_eq!("界".len(), 3);
        assert_eq!(validate_normal(&"界".repeat(42)), Ok(()));
        assert!(matches!(
            validate_normal(&"界".repeat(43)),
            Err(FlagPolicyError::TooLong { actual: 129, .. })
        ));
        assert_eq!(
            validate_normal(" flag{answer}"),
            Err(FlagPolicyError::SurroundingWhitespace)
        );
        for whitespace in ['\u{0085}', '\u{00A0}', '\u{2003}', '\u{202F}', '\u{FEFF}'] {
            assert_eq!(
                validate_normal(&format!("{whitespace}flag{{answer}}")),
                Err(FlagPolicyError::SurroundingWhitespace)
            );
            assert_eq!(
                validate_normal(&format!("flag{{answer}}{whitespace}")),
                Err(FlagPolicyError::SurroundingWhitespace)
            );
        }
        assert!(is_blank("\u{00A0}\u{2003}\u{FEFF}"));
        assert!(!is_blank("\u{00A0}x\u{2003}"));
    }

    #[test]
    fn template_policy_counts_every_production_placeholder() {
        let template = "flag{[GUID]-[UUID]-[TEAM_HASH]-[GUID]}";
        assert_eq!(expanded_template_bytes(template), Some(133));
        assert!(matches!(
            validate_dynamic_template(template),
            Err(FlagPolicyError::TooLong { actual: 133, .. })
        ));
        assert_eq!(
            validate_dynamic_template("flag{[UUID]-[TEAM_HASH]}"),
            Ok(())
        );
        assert_eq!(
            validate_dynamic_template("flag{same-for-every-team}"),
            Err(FlagPolicyError::TemplateTooTrivial)
        );
        assert_eq!(
            validate_dynamic_template("flag{[UUID]}\u{2003}"),
            Err(FlagPolicyError::SurroundingWhitespace)
        );
    }

    #[test]
    fn ad_policy_keeps_the_fixed_round_grammar() {
        assert_eq!(
            validate_ad("flag{ABCDEFGHIJKLMNOPQRSTUVWXYZabcd_-}"),
            Ok(())
        );
        assert_eq!(
            validate_ad("flag{ABCDEFGHIJKLMNOPQRSTUVWXYZabcd+/}"),
            Err(FlagPolicyError::InvalidAdGrammar)
        );
    }
}
