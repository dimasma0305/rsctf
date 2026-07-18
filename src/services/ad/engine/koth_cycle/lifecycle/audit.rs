//! Bounded filesystem-diff evidence for crown-cycle audit receipts.

use serde_json::{json, Value};

use crate::services::container::FileChange;
use crate::utils::error::{AppError, AppResult};

const DEFAULT_MAX_ENTRIES: usize = 1_024;
const ABSOLUTE_MAX_ENTRIES: usize = 8_192;
const DEFAULT_MAX_BYTES: usize = 64 * 1024;
const MIN_MAX_BYTES: usize = 4 * 1024;
const ABSOLUTE_MAX_BYTES: usize = 512 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_KIND_BYTES: usize = 64;
const ELLIPSIS: &str = "…";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiffLimits {
    max_entries: usize,
    max_bytes: usize,
}

impl DiffLimits {
    fn from_environment() -> Self {
        Self {
            max_entries: bounded_value(
                std::env::var("RSCTF_KOTH_DIFF_MAX_ENTRIES").ok().as_deref(),
                1,
                ABSOLUTE_MAX_ENTRIES,
                DEFAULT_MAX_ENTRIES,
            ),
            max_bytes: bounded_value(
                std::env::var("RSCTF_KOTH_DIFF_MAX_BYTES").ok().as_deref(),
                MIN_MAX_BYTES,
                ABSOLUTE_MAX_BYTES,
                DEFAULT_MAX_BYTES,
            ),
        }
    }
}

fn bounded_value(value: Option<&str>, minimum: usize, maximum: usize, default: usize) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .unwrap_or(default)
}

pub(super) struct BoundedFilesystemDiff {
    pub(super) value: Value,
    pub(super) summary: Value,
}

/// Retain a deterministic prefix-sized subset of the runtime diff while
/// recording exactly what was omitted. The returned JSON array preserves the
/// existing admin API shape; the additive summary lives in the receipt object.
pub(super) fn bounded_filesystem_diff(
    changes: Vec<FileChange>,
) -> AppResult<BoundedFilesystemDiff> {
    bounded_filesystem_diff_with_limits(changes, DiffLimits::from_environment())
}

fn bounded_filesystem_diff_with_limits(
    changes: Vec<FileChange>,
    limits: DiffLimits,
) -> AppResult<BoundedFilesystemDiff> {
    let observed_entries = changes.len();
    let mut stored = Vec::with_capacity(observed_entries.min(limits.max_entries));
    // Account for the opening and closing JSON array brackets.
    let mut stored_bytes = 2usize;
    let mut truncated_fields = 0usize;

    for mut change in changes {
        truncated_fields += usize::from(truncate_utf8(&mut change.path, MAX_PATH_BYTES));
        truncated_fields += usize::from(truncate_utf8(&mut change.kind, MAX_KIND_BYTES));
        if stored.len() >= limits.max_entries {
            continue;
        }

        let encoded =
            serde_json::to_vec(&change).map_err(|error| AppError::internal(error.to_string()))?;
        let separator_bytes = usize::from(!stored.is_empty());
        let Some(next_size) = stored_bytes
            .checked_add(separator_bytes)
            .and_then(|bytes| bytes.checked_add(encoded.len()))
        else {
            continue;
        };
        if next_size > limits.max_bytes {
            continue;
        }
        stored_bytes = next_size;
        stored.push(change);
    }

    let stored_entries = stored.len();
    let dropped_entries = observed_entries.saturating_sub(stored_entries);
    let value =
        serde_json::to_value(stored).map_err(|error| AppError::internal(error.to_string()))?;
    debug_assert!(serde_json::to_vec(&value).is_ok_and(|json| json.len() == stored_bytes));
    let summary = json!({
        "truncated": dropped_entries > 0 || truncated_fields > 0,
        "observedEntries": observed_entries,
        "storedEntries": stored_entries,
        "droppedEntries": dropped_entries,
        "truncatedFields": truncated_fields,
        "storedBytes": stored_bytes,
        "maxEntries": limits.max_entries,
        "maxBytes": limits.max_bytes,
    });
    Ok(BoundedFilesystemDiff { value, summary })
}

fn truncate_utf8(value: &mut String, max_bytes: usize) -> bool {
    if value.len() <= max_bytes {
        return false;
    }
    let mut end = max_bytes.saturating_sub(ELLIPSIS.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.truncate(end);
    value.push_str(ELLIPSIS);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: impl Into<String>) -> FileChange {
        FileChange {
            path: path.into(),
            kind: "Modified".to_string(),
        }
    }

    #[test]
    fn receipt_diff_obeys_entry_and_serialized_byte_caps() {
        let bounded = bounded_filesystem_diff_with_limits(
            vec![change("/one"), change("/two"), change("/three")],
            DiffLimits {
                max_entries: 2,
                max_bytes: 256,
            },
        )
        .unwrap();

        assert_eq!(bounded.value.as_array().map(Vec::len), Some(2));
        assert!(serde_json::to_vec(&bounded.value).unwrap().len() <= 256);
        assert_eq!(bounded.summary["truncated"], true);
        assert_eq!(bounded.summary["observedEntries"], 3);
        assert_eq!(bounded.summary["storedEntries"], 2);
        assert_eq!(bounded.summary["droppedEntries"], 1);
    }

    #[test]
    fn oversized_unicode_fields_are_truncated_on_a_character_boundary() {
        let bounded = bounded_filesystem_diff_with_limits(
            vec![change(format!("/{}", "é".repeat(MAX_PATH_BYTES)))],
            DiffLimits {
                max_entries: 1,
                max_bytes: 16 * 1024,
            },
        )
        .unwrap();
        let path = bounded.value[0]["path"].as_str().unwrap();

        assert!(path.len() <= MAX_PATH_BYTES);
        assert!(path.ends_with(ELLIPSIS));
        assert_eq!(bounded.summary["truncatedFields"], 1);
        assert_eq!(bounded.summary["truncated"], true);
    }

    #[test]
    fn invalid_or_unsafe_config_values_fall_back_to_safe_defaults() {
        assert_eq!(
            bounded_value(
                Some("999999999"),
                1,
                ABSOLUTE_MAX_ENTRIES,
                DEFAULT_MAX_ENTRIES
            ),
            DEFAULT_MAX_ENTRIES
        );
        assert_eq!(
            bounded_value(Some("2048"), 1, ABSOLUTE_MAX_ENTRIES, DEFAULT_MAX_ENTRIES),
            2_048
        );
    }
}
