use super::*;
use crate::services::cache::{Cache, InMemoryCache};

#[tokio::test]
async fn cache_lookup_accepts_json_and_evicts_corrupt_entries() {
    let cache = InMemoryCache::new();
    cache
        .set(
            "valid-board",
            br#"{"teams":[]}"#,
            Some(AD_SCOREBOARD_STALE_TTL),
        )
        .await;
    cache
        .set("corrupt-board", b"not-json", Some(AD_SCOREBOARD_STALE_TTL))
        .await;

    assert_eq!(
        cached_scoreboard_bundle(&cache, "valid-board")
            .await
            .as_deref(),
        Some(br#"{"teams":[]}"#.as_slice())
    );
    assert!(cached_scoreboard_bundle(&cache, "corrupt-board")
        .await
        .is_none());
    assert!(cache.get("corrupt-board").await.is_none());
}

#[tokio::test]
async fn hard_invalidation_removes_fresh_and_stale_for_both_views() {
    let cache = InMemoryCache::new();
    let game_id = 246_810;
    let live = scoreboard_cache_key(game_id, true);
    let frozen = scoreboard_cache_key(game_id, false);
    let keys = [
        live.clone(),
        stale_scoreboard_key(&live),
        frozen.clone(),
        stale_scoreboard_key(&frozen),
    ];
    for key in &keys {
        cache.set(key, br#"{"teams":[]}"#, None).await;
    }
    cache.set("unrelated-board", b"keep", None).await;

    hard_invalidate_ad_scoreboard_cache(&cache, game_id).await;

    for key in &keys {
        assert!(cache.get(key).await.is_none(), "{key} survived");
    }
    assert_eq!(
        cache.get("unrelated-board").await.as_deref(),
        Some(b"keep".as_slice())
    );
}

#[test]
fn cloneable_fill_result_preserves_not_found() {
    let result = ScoreboardFillResult::NotFound("Game not found".to_owned());
    let error = completed_scoreboard_bundle(result.clone()).unwrap_err();
    assert_eq!(error.status(), axum::http::StatusCode::NOT_FOUND);
    assert!(matches!(result, ScoreboardFillResult::NotFound(_)));
    assert!(matches!(
        ScoreboardFillResult::default(),
        ScoreboardFillResult::Failed
    ));
}

#[test]
fn ad_state_serializes_snapshotted_epoch_config_in_camel_case() {
    let value = serde_json::to_value(AdStateModel {
        current_round: 13,
        epoch_ticks: 8,
        start_round: Some(5),
        flags_ready: true,
        flag_delivery_failures: 0,
        round_started_at: None,
        round_ends_at: None,
        services: Vec::new(),
    })
    .expect("A&D state serializes");

    assert_eq!(value["currentRound"], 13);
    assert_eq!(value["epochTicks"], 8);
    assert_eq!(value["startRound"], 5);
}

#[test]
fn revision_fence_distinguishes_current_changed_and_missing_rows() {
    assert_eq!(
        revision_disposition("101", Some("101")),
        RevisionDisposition::Current
    );
    assert_eq!(
        revision_disposition("101", Some("102")),
        RevisionDisposition::Changed
    );
    assert_eq!(
        revision_disposition("101", None),
        RevisionDisposition::Missing
    );
}

#[test]
fn stale_keys_are_isolated_by_view() {
    let live = "_AdScoreBoard_987654321";
    let frozen = "_AdScoreBoardFrozen_987654321";
    assert_eq!(stale_scoreboard_key(live), format!("{live}:stale"));
    assert_ne!(stale_scoreboard_key(live), stale_scoreboard_key(frozen));
}

#[test]
fn atomic_reservation_coalesces_and_collision_only_defers() {
    let first = "_AdScoreBoard_123456789";
    let shard = scoreboard_refresh_shard(first);
    let collision = (0..10_000)
        .map(|id| format!("_AdScoreBoardFrozen_{id}"))
        .find(|key| key != first && scoreboard_refresh_shard(key) == shard)
        .expect("256 shards guarantee a collision in this search range");

    let reservation = reserve_scoreboard_refresh(first).expect("first refresh is admitted");
    assert!(reserve_scoreboard_refresh(first).is_none());
    assert!(reserve_scoreboard_refresh(&collision).is_none());
    drop(reservation);
    assert!(reserve_scoreboard_refresh(&collision).is_some());
}
