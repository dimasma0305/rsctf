use serde_json::Value as JsonValue;

pub(crate) fn values(hints: Option<&JsonValue>) -> Vec<String> {
    hints
        .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
        .unwrap_or_default()
}

pub(crate) fn retained_release_count(
    old: Option<&JsonValue>,
    new: &[String],
    released_hint_count: i32,
) -> i32 {
    let old = values(old);
    old.iter()
        .zip(new)
        .take(released_hint_count.max(0) as usize)
        .take_while(|(old, new)| old == new)
        .count() as i32
}

pub(crate) fn published(hints: Option<&JsonValue>, released_hint_count: i32) -> Option<JsonValue> {
    let published = values(hints)
        .into_iter()
        .take(released_hint_count.max(0) as usize)
        .collect::<Vec<_>>();
    (!published.is_empty()).then(|| serde_json::json!(published))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appended_hints_stay_draft_without_retracting_the_released_prefix() {
        let old = serde_json::json!(["one", "two"]);
        assert_eq!(
            retained_release_count(Some(&old), &["one".into(), "two".into(), "three".into()], 2),
            2
        );
    }

    #[test]
    fn changing_a_released_hint_retracts_it_and_later_hints() {
        let old = serde_json::json!(["one", "two", "three"]);
        assert_eq!(
            retained_release_count(
                Some(&old),
                &["one".into(), "changed".into(), "three".into()],
                3
            ),
            1
        );
    }

    #[test]
    fn player_projection_contains_only_the_released_prefix() {
        let hints = serde_json::json!(["one", "two", "three"]);
        assert_eq!(
            published(Some(&hints), 2),
            Some(serde_json::json!(["one", "two"]))
        );
        assert_eq!(published(Some(&hints), 0), None);
    }
}
