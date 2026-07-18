use super::{validate_scoring_transition, GameInfoModel};

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
    for value in [
        serde_json::json!({ "adEpochTicks": 0 }),
        serde_json::json!({ "adEpochTicks": 65 }),
        serde_json::json!({ "adTickSeconds": 29 }),
        serde_json::json!({ "adFlagLifetimeTicks": 0 }),
        serde_json::json!({ "adGetflagWindowFraction": 0.01 }),
        serde_json::json!({ "adMinGracePeriodSeconds": 0 }),
        serde_json::json!({ "kothEpochTicks": 1 }),
        serde_json::json!({ "kothCycleTicks": 0 }),
        serde_json::json!({ "kothChampionCooldownTicks": 64 }),
        serde_json::json!({ "kothClaimConfirmationTicks": 0 }),
    ] {
        let model: GameInfoModel = serde_json::from_value(value).unwrap();
        assert!(model.validate().is_err());
    }

    let mut model: GameInfoModel = serde_json::from_value(serde_json::json!({
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
