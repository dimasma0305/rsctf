use std::collections::HashSet;

use serde_json::Value as JsonValue;

/// Match `ChallengeUpdateModel.IsHintUpdated` and RSCTF's set-hash comparison:
/// order is irrelevant, while count or distinct membership changes trigger a
/// new-hint notification.
pub(super) fn updated(old: Option<&JsonValue>, new: &[String]) -> bool {
    let old_values = old
        .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
        .unwrap_or_default();
    let old_set = old_values
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let new_set = new.iter().map(String::as_str).collect::<HashSet<_>>();
    old_values.len() != new.len() || old_set != new_set
}
