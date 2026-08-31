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
        scoring_paused: true,
        scoring_paused_at: None,
        services: Vec::new(),
    })
    .expect("A&D state serializes");

    assert_eq!(value["currentRound"], 13);
    assert_eq!(value["epochTicks"], 8);
    assert_eq!(value["startRound"], 5);
    assert_eq!(value["scoringPaused"], true);
    assert!(value.get("scoringPausedAt").is_some());
}

#[test]
fn ad_state_wire_distinguishes_managed_and_connected_byoc_services() {
    let reset_at = chrono::DateTime::from_timestamp_millis(1_777_000_000_123).unwrap();
    let service = |id, self_hosted, host: &str, port, status: Option<&str>| {
        let delivery_state = service_delivery_state(self_hosted, host, port, status);
        AdTeamServiceStateModel {
            ad_team_service_id: id,
            challenge_id: id + 10,
            challenge_title: format!("service-{id}"),
            container_ip: Some(host.to_string()),
            container_port: Some(port),
            current_flag: None,
            last_check_status: status.map(str::to_owned),
            last_reset_at: Some(reset_at),
            can_reset: !self_hosted,
            reset_cooldown_seconds_remaining: None,
            snapshot_available: !self_hosted,
            self_hosted: Some(self_hosted),
            delivery_state,
        }
    };
    let value = serde_json::to_value(AdStateModel {
        current_round: 13,
        epoch_ticks: 8,
        start_round: Some(1),
        flags_ready: true,
        flag_delivery_failures: 0,
        round_started_at: None,
        round_ends_at: None,
        scoring_paused: false,
        scoring_paused_at: None,
        services: vec![
            service(1, false, "10.13.0.6", 8080, Some("Ok")),
            service(2, true, "10.13.0.7", 31337, Some("Ok")),
            service(3, true, "10.13.0.8", 31338, None),
            service(4, true, "10.13.0.9", 31339, Some("Offline")),
        ],
    })
    .unwrap();

    let managed = &value["services"][0];
    assert_eq!(managed["selfHosted"], false);
    assert_eq!(managed["deliveryState"], "Managed");
    assert_eq!(managed["canReset"], true);
    assert_eq!(managed["snapshotAvailable"], true);
    let byoc = &value["services"][1];
    assert_eq!(byoc["selfHosted"], true);
    assert_eq!(byoc["deliveryState"], "ByocHealthy");
    assert_eq!(byoc["containerIp"], "10.13.0.7");
    assert_eq!(byoc["containerPort"], 31337);
    assert_eq!(byoc["lastCheckStatus"], "Ok");
    assert_eq!(byoc["lastResetAt"], 1_777_000_000_123_i64);
    assert_eq!(byoc["canReset"], false);
    assert_eq!(byoc["snapshotAvailable"], false);
    let connecting = &value["services"][2];
    assert_eq!(connecting["selfHosted"], true);
    assert_eq!(connecting["deliveryState"], "ByocConnecting");
    assert_eq!(connecting["containerIp"], "10.13.0.8");
    assert_eq!(connecting["containerPort"], 31338);
    assert!(connecting["lastCheckStatus"].is_null());
    let stale = &value["services"][3];
    assert_eq!(stale["selfHosted"], true);
    assert_eq!(stale["deliveryState"], "ByocStale");
    assert_eq!(stale["containerIp"], "10.13.0.9");
    assert_eq!(stale["containerPort"], 31339);
    assert_eq!(stale["lastCheckStatus"], "Offline");
}

#[test]
fn revision_fence_distinguishes_current_changed_and_missing_rows() {
    let expected = crate::services::ad::scoring::AdScoreboardRevision {
        revision: "101".to_string(),
        immutable_final: false,
    };
    let current = expected.clone();
    let changed = crate::services::ad::scoring::AdScoreboardRevision {
        revision: "102".to_string(),
        immutable_final: false,
    };
    let finalized = crate::services::ad::scoring::AdScoreboardRevision {
        revision: "101".to_string(),
        immutable_final: true,
    };
    assert_eq!(
        revision_disposition(&expected, Some(&current)),
        RevisionDisposition::Current
    );
    assert_eq!(
        revision_disposition(&expected, Some(&changed)),
        RevisionDisposition::Changed
    );
    assert_eq!(
        revision_disposition(&expected, Some(&finalized)),
        RevisionDisposition::Changed
    );
    assert_eq!(
        revision_disposition(&expected, None),
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
fn archived_state_strips_operational_endpoints_and_flags() {
    assert_eq!(live_operational_value(false, "10.0.0.7"), Some("10.0.0.7"));
    assert_eq!(live_operational_value(false, 31337), Some(31337));
    assert_eq!(
        live_operational_value(false, Some("flag")),
        Some(Some("flag"))
    );

    assert_eq!(live_operational_value(true, "10.0.0.7"), None);
    assert_eq!(live_operational_value(true, 31337), None);
    assert_eq!(live_operational_value(true, Some("flag")), None);
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
