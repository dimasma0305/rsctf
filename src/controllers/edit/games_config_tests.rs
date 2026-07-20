use sea_orm::ActiveValue::Set;

use super::{
    apply_ad_creation_settings, apply_clone_challenge_defaults, validate_scoring_transition,
    validate_start_time_transition, GameInfoModel,
};

#[test]
fn game_creation_keeps_every_supplied_ad_timing_field() {
    let model: GameInfoModel = serde_json::from_value(serde_json::json!({
        "adWarmupSeconds": 0,
        "adSnapshotRetentionDays": 9,
        "adTickSeconds": 30,
        "adFlagLifetimeTicks": 5,
        "adResetCooldownMinutes": 7,
        "adAllowSnapshotDownload": false,
        "adGetflagWindowFraction": 0.9,
        "adMinGracePeriodSeconds": 1,
        "adEpochTicks": 2
    }))
    .unwrap();
    let mut active = crate::models::data::game::ActiveModel::default();
    apply_ad_creation_settings(&model, &mut active);

    assert_eq!(active.ad_warmup_seconds, Set(Some(0)));
    assert_eq!(active.ad_snapshot_retention_days, Set(Some(9)));
    assert_eq!(active.ad_tick_seconds, Set(Some(30)));
    assert_eq!(active.ad_flag_lifetime_ticks, Set(Some(5)));
    assert_eq!(active.ad_reset_cooldown_minutes, Set(Some(7)));
    assert_eq!(active.ad_allow_snapshot_download, Set(false));
    assert_eq!(active.ad_getflag_window_fraction, Set(Some(0.9)));
    assert_eq!(active.ad_min_grace_period_seconds, Set(Some(1)));
    assert_eq!(active.ad_epoch_ticks, Set(2));
}

#[test]
fn clone_challenge_defaults_match_non_nullable_schema_defaults() {
    use crate::models::data::game_challenge;
    use crate::utils::enums::{NetworkMode, ScoreCurve};

    let mut active = game_challenge::ActiveModel::default();
    apply_clone_challenge_defaults(&mut active);

    assert_eq!(active.enable_shared_container, Set(false));
    assert_eq!(active.score_curve, Set(ScoreCurve::Standard));
    assert_eq!(active.network_mode, Set(Some(NetworkMode::Open)));
    assert_eq!(active.ad_allow_egress, Set(false));
    assert_eq!(active.ad_allow_self_reset, Set(false));
    assert_eq!(active.ad_ssh_requires_flag, Set(false));
    assert_eq!(active.ad_self_hosted, Set(false));
}

#[test]
fn unstarted_template_scoring_shape_remains_mutable() {
    assert!(validate_scoring_transition(
        8,
        None,
        Some(5),
        Some(120),
        Some(0.5),
        Some(3),
        None,
        4,
        Some(6),
        Some(60),
        Some(0.25),
        Some(1),
    )
    .is_ok());
}

#[test]
fn started_epoch_shape_is_immutable() {
    let validate = |epoch_ticks, lifetime, tick_seconds, window_fraction, grace_seconds| {
        validate_scoring_transition(
            8,
            Some(9),
            Some(5),
            Some(120),
            Some(0.5),
            Some(3),
            Some(9),
            epoch_ticks,
            Some(lifetime),
            Some(tick_seconds),
            Some(window_fraction),
            Some(grace_seconds),
        )
    };
    assert!(validate(4, 5, 120, 0.5, 3).is_err());
    assert!(validate(8, 6, 120, 0.5, 3).is_err());
    assert!(validate(8, 5, 60, 0.5, 3).is_err());
    assert!(validate(8, 5, 120, 0.25, 3).is_err());
    assert!(validate(8, 5, 120, 0.5, 2).is_err());
    assert!(validate(8, 5, 120, 0.5, 3).is_ok());
    assert!(validate_scoring_transition(
        8,
        Some(9),
        Some(5),
        Some(120),
        Some(0.5),
        Some(3),
        None,
        8,
        None,
        Some(120),
        Some(0.5),
        Some(3),
    )
    .is_err());
}

#[test]
fn koth_only_boundary_leaves_ad_epoch_mutable_but_locks_shared_timing() {
    let validate = |epoch_ticks, tick_seconds, window_fraction, grace_seconds| {
        validate_scoring_transition(
            8,
            None,
            Some(5),
            Some(120),
            Some(window_fraction),
            Some(grace_seconds),
            Some(9),
            epoch_ticks,
            Some(5),
            Some(tick_seconds),
            Some(0.5),
            Some(3),
        )
    };
    assert!(validate(4, 120, 0.5, 3).is_ok());
    assert!(validate(8, 60, 0.5, 3).is_err());
    assert!(validate(8, 120, 0.25, 3).is_err());
    assert!(validate(8, 120, 0.5, 1).is_err());
    assert!(validate(8, 120, 0.5, 3).is_ok());
}

#[test]
fn ad_only_game_does_not_require_a_koth_compatible_epoch() {
    assert!(validate_scoring_transition(
        4,
        Some(9),
        Some(5),
        Some(120),
        Some(0.5),
        Some(3),
        None,
        4,
        Some(5),
        Some(120),
        Some(0.5),
        Some(3),
    )
    .is_ok());
}

#[test]
fn game_info_rejects_out_of_range_epoch_timing() {
    let start = chrono::Utc::now().timestamp_millis();
    for mut value in [
        serde_json::json!({ "adEpochTicks": 0 }),
        serde_json::json!({ "adEpochTicks": 65 }),
        serde_json::json!({ "adTickSeconds": 29 }),
        serde_json::json!({ "adFlagLifetimeTicks": 0 }),
        serde_json::json!({ "adGetflagWindowFraction": 0.01 }),
        serde_json::json!({ "adMinGracePeriodSeconds": 0 }),
        serde_json::json!({ "adResetCooldownMinutes": -1 }),
        serde_json::json!({ "adSnapshotRetentionDays": 0 }),
        serde_json::json!({ "teamMemberCountLimit": -1 }),
        serde_json::json!({ "containerCountLimit": -1 }),
        serde_json::json!({ "kothEpochTicks": 1 }),
        serde_json::json!({ "kothCycleTicks": 0 }),
        serde_json::json!({ "kothChampionCooldownTicks": 64 }),
        serde_json::json!({ "kothClaimConfirmationTicks": 0 }),
    ] {
        value["start"] = serde_json::json!(start);
        value["end"] = serde_json::json!(start + 60_000);
        let model: GameInfoModel = serde_json::from_value(value).unwrap();
        assert!(model.validate().is_err());
    }

    let mut model: GameInfoModel = serde_json::from_value(serde_json::json!({
        "start": start,
        "end": start + 60_000,
        "adEpochTicks": 64,
        "adTickSeconds": 600,
        "adFlagLifetimeTicks": 50,
        "adGetflagWindowFraction": 0.9,
        "adMinGracePeriodSeconds": 60
    }))
    .unwrap();
    assert!(model.validate().is_ok());
    model.ad_getflag_window_fraction = Some(f64::NAN);
    assert!(model.validate().is_err());
}

#[test]
fn game_window_requires_positive_duration_and_live_start_is_immutable() {
    let start = chrono::Utc::now();
    let mut model: GameInfoModel = serde_json::from_value(serde_json::json!({
        "start": start.timestamp_millis(),
        "end": start.timestamp_millis()
    }))
    .unwrap();
    assert!(model.validate().is_err());
    model.end_time_utc = start + chrono::Duration::seconds(1);
    assert!(model.validate().is_ok());

    assert!(validate_start_time_transition(start, start, true).is_ok());
    assert!(
        validate_start_time_transition(start, start + chrono::Duration::seconds(1), true).is_err()
    );
    assert!(
        validate_start_time_transition(start, start + chrono::Duration::seconds(1), false).is_ok()
    );
}
