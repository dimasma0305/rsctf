use super::{
    blood_notice_type, blood_recognition_eligible, normal_flag_submit_type_allowed, ChallengeType,
    FlagSubmitModel, GamePermission, NoticeType, Uuid, FINALIZE_SUBMISSION_SQL,
    LOAD_GRADING_POLICY_SQL,
};
use chrono::{Duration, Utc};

#[test]
fn challenge_policy_read_does_not_hold_the_hot_row() {
    assert!(
        !LOAD_GRADING_POLICY_SQL.contains("FOR UPDATE"),
        "authoritative policy reads must rely on the late optimistic fence"
    );
}

#[test]
fn finalization_fences_every_authoritative_challenge_input() {
    for predicate in [
        "AND game_id = $3",
        "AND is_enabled",
        "AND review_status = $4",
        "AND submission_limit = $5",
        "AND deadline_utc IS NOT DISTINCT FROM $6",
        "AND disable_blood_bonus = $7",
        "AND \"Type\" = $8",
    ] {
        assert!(
            FINALIZE_SUBMISSION_SQL.contains(predicate),
            "missing optimistic grading fence predicate: {predicate}"
        );
    }
}

#[test]
fn submit_wire_accepts_legacy_omission_and_preserves_a_supplied_attempt_id() {
    let attempt = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
    let parsed: FlagSubmitModel = serde_json::from_value(serde_json::json!({
        "flag": "flag{ok}",
        "attemptId": attempt.to_string(),
    }))
    .unwrap();
    assert_eq!(parsed.attempt_id, attempt);

    let legacy: FlagSubmitModel = serde_json::from_value(serde_json::json!({
        "flag": "flag{legacy-client}"
    }))
    .unwrap();
    assert!(!legacy.attempt_id.is_nil());

    let second_legacy: FlagSubmitModel = serde_json::from_value(serde_json::json!({
        "flag": "flag{second-legacy-client}"
    }))
    .unwrap();
    assert_ne!(legacy.attempt_id, second_legacy.attempt_id);

    let explicit_nil: FlagSubmitModel = serde_json::from_value(serde_json::json!({
        "flag": "flag{invalid}",
        "attemptId": Uuid::nil(),
    }))
    .unwrap();
    assert!(explicit_nil.attempt_id.is_nil());
}

#[test]
fn every_submission_side_effect_is_ordered_behind_the_attempt_reservation() {
    let source = include_str!("submit.rs");
    let reserve = source.find("match reserve_attempt(").unwrap();
    let after_reserve = &source[reserve..];
    let receipt = reserve
        + after_reserve
            .find("validate_receipt_for_submission(")
            .unwrap();
    let submission = reserve + after_reserve.find("INSERT INTO \"Submissions\"").unwrap();
    let first_solve = reserve
        + after_reserve
            .find("claim_first_solve(&mut transaction")
            .unwrap();
    let finalization = reserve + after_reserve.find("let counter_update =").unwrap();
    let completion = finalization + source[finalization..].find("complete_attempt(").unwrap();
    let commit = completion
        + source[completion..]
            .find("transaction\n        .commit()")
            .unwrap();
    assert!(reserve < receipt);
    assert!(reserve < submission);
    assert!(reserve < first_solve);
    assert!(reserve < finalization);
    assert!(finalization < completion && completion < commit);
}

#[test]
fn disabling_bonus_points_does_not_disable_blood_recognition() {
    let start = Utc::now() - Duration::minutes(5);
    let end = Utc::now() + Duration::minutes(5);
    let permissions = GamePermission(GamePermission::GET_BLOOD | GamePermission::GET_SCORE);
    assert!(blood_recognition_eligible(
        Utc::now(),
        start,
        end,
        None,
        permissions,
    ));
    assert_eq!(
        blood_notice_type(true, true, 0),
        Some(NoticeType::FirstBlood)
    );
    assert_eq!(
        blood_notice_type(true, true, 1),
        Some(NoticeType::SecondBlood)
    );
    assert_eq!(
        blood_notice_type(true, true, 2),
        Some(NoticeType::ThirdBlood)
    );
    assert_eq!(blood_notice_type(true, true, 3), None);
}

#[test]
fn live_engine_types_cannot_enter_jeopardy_scoring() {
    let end = Utc::now() + Duration::hours(1);
    let live = end - Duration::minutes(30);
    for challenge_type in [
        ChallengeType::StaticAttachment,
        ChallengeType::StaticContainer,
        ChallengeType::DynamicAttachment,
        ChallengeType::DynamicContainer,
    ] {
        assert!(normal_flag_submit_type_allowed(
            challenge_type as i16,
            false,
            live,
            end
        ));
    }
    for challenge_type in [ChallengeType::AttackDefense, ChallengeType::KingOfTheHill] {
        assert!(!normal_flag_submit_type_allowed(
            challenge_type as i16,
            false,
            live,
            end
        ));
        assert!(!normal_flag_submit_type_allowed(
            challenge_type as i16,
            true,
            live,
            end
        ));
    }
}

#[test]
fn post_game_practice_keeps_the_normal_container_fallback() {
    let end = Utc::now();
    let after_end = end + Duration::seconds(1);
    for challenge_type in [ChallengeType::AttackDefense, ChallengeType::KingOfTheHill] {
        assert!(!normal_flag_submit_type_allowed(
            challenge_type as i16,
            false,
            after_end,
            end
        ));
        assert!(normal_flag_submit_type_allowed(
            challenge_type as i16,
            true,
            after_end,
            end
        ));
    }
    assert!(!normal_flag_submit_type_allowed(
        i16::MAX,
        true,
        after_end,
        end
    ));
}
