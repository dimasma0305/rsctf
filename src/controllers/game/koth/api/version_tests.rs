use super::*;
use axum::http::HeaderValue;

#[test]
fn wave_windows_are_contiguous_and_never_precede_official_scoring() {
    let at = |seconds| DateTime::from_timestamp(seconds, 0).unwrap();
    let context = |round_number, round_start, round_end| ActiveObserverContext {
        target_id: 3,
        cycle_id: 41,
        cycle_number: 1,
        reset_attempt: 0,
        reporting_revision: 1,
        container_id: "runtime-a".to_string(),
        round_id: round_number,
        round_number,
        scoring_starts_at: at(130),
        cycle_ends_at: at(310),
        scoring_ends_at: at(290),
        round_starts_at: at(round_start),
        round_ends_at: at(round_end),
        objective_ids: None,
        objective_schema_hash: None,
    };
    let first = context(1, 130, 190).wave_window();
    let second = context(2, 190, 250).wave_window();
    assert_eq!(first, (at(130), at(170)));
    assert_eq!(second, (at(170), at(230)));
    assert_eq!(first.1, second.0);
    let final_round = context(3, 250, 310).wave_window();
    assert_eq!(
        final_round,
        (at(230), at(290) + chrono::Duration::milliseconds(1))
    );
}

#[test]
fn observer_context_wire_contract_includes_cycle_bounds_in_milliseconds() {
    let at = |seconds| DateTime::from_timestamp(seconds, 123_000_000).unwrap();
    let value = serde_json::to_value(KothObserverContextV2Model {
        api_version: "v2",
        context: "a".repeat(64),
        cycle_number: 4,
        reset_attempt: 2,
        round_number: 9,
        cycle_starts_at: at(100),
        cycle_ends_at: at(310),
        scoring_ends_at: at(290),
        wave_window_starts_at: at(170),
        wave_window_ends_at: at(230),
        eligible_token_hashes: vec!["b".repeat(64)],
        objective_ids: vec!["proof-strength".to_string()],
        objective_schema_hash: Some("c".repeat(64)),
        generated_at: at(171),
    })
    .unwrap();
    assert_eq!(value["apiVersion"], "v2");
    assert_eq!(value["cycleStartsAt"], 100_123_i64);
    assert_eq!(value["cycleEndsAt"], 310_123_i64);
    assert_eq!(value["scoringEndsAt"], 290_123_i64);
    assert_eq!(value["waveWindowStartsAt"], 170_123_i64);
    assert_eq!(value["waveWindowEndsAt"], 230_123_i64);
    assert!(value.get("cycle_ends_at").is_none());
    assert_eq!(value.as_object().unwrap().len(), 14);
}

#[test]
fn default_v1_context_preserves_the_strict_legacy_key_set() {
    let at = |seconds| DateTime::from_timestamp(seconds, 123_000_000).unwrap();
    let value = serde_json::to_value(KothObserverContextModel {
        api_version: "v1",
        context: "a".repeat(64),
        cycle_number: 4,
        reset_attempt: 2,
        round_number: 9,
        cycle_ends_at: at(310),
        wave_window_starts_at: at(170),
        wave_window_ends_at: at(230),
        eligible_token_hashes: vec!["b".repeat(64)],
        objective_ids: vec!["proof-strength".to_string()],
        objective_schema_hash: Some("c".repeat(64)),
        generated_at: at(171),
    })
    .unwrap();
    let keys: std::collections::BTreeSet<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from([
            "apiVersion",
            "context",
            "cycleEndsAt",
            "cycleNumber",
            "eligibleTokenHashes",
            "generatedAt",
            "objectiveIds",
            "objectiveSchemaHash",
            "resetAttempt",
            "roundNumber",
            "waveWindowEndsAt",
            "waveWindowStartsAt",
        ])
    );
    assert!(value.get("cycleStartsAt").is_none());
    assert!(value.get("scoringEndsAt").is_none());
}

#[test]
fn context_v2_requires_explicit_bounded_negotiation() {
    let mut headers = HeaderMap::new();
    assert!(!context_v2_requested(&headers).unwrap());
    headers.insert(CONTEXT_API_VERSION_HEADER, HeaderValue::from_static("v2"));
    assert!(context_v2_requested(&headers).unwrap());
    headers.insert(CONTEXT_API_VERSION_HEADER, HeaderValue::from_static("v3"));
    assert!(context_v2_requested(&headers).is_err());

    let response = versioned_context_response(serde_json::json!({"apiVersion":"v2"}));
    assert_eq!(
        response.headers().get(header::VARY).unwrap(),
        CONTEXT_API_VERSION_HEADER
    );
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
}
