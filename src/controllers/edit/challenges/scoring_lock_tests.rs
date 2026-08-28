use super::challenge_scoring_fields_changed;
use crate::models::data::game_challenge;
use crate::utils::enums::{
    ChallengeBuildStatus, ChallengeCategory, ChallengeReviewStatus, ChallengeType,
    ChallengeVariantMode, NetworkMode, ScoreCurve, SolveReceiptMode,
};

fn challenge() -> game_challenge::Model {
    game_challenge::Model {
        id: 672,
        revision: 1,
        game_id: 19,
        title: "Kuta".into(),
        content: String::new(),
        category: ChallengeCategory::Web,
        challenge_type: ChallengeType::DynamicContainer,
        hints: None,
        is_enabled: true,
        ad_control_revision: 1,
        deadline_utc: None,
        submission_limit: 0,
        accepted_count: 0,
        submission_count: 0,
        container_image: Some("rsctf/19/kuta:latest".into()),
        memory_limit: Some(128),
        storage_limit: None,
        cpu_count: Some(1),
        expose_port: Some(80),
        workload_spec: None,
        file_name: None,
        flag_template: Some("TCP1P{[GUID]}".into()),
        review_status: ChallengeReviewStatus::Active,
        review_note: None,
        submitted_by_user_id: None,
        submitted_at_utc: None,
        reviewed_at_utc: None,
        original_archive_blob_path: Some("challenges/672/source.tar.gz".into()),
        build_context_subdir: Some(".".into()),
        build_status: ChallengeBuildStatus::Failed,
        build_image_digest: None,
        last_build_log: None,
        source_yaml_path: Some("quals/Jeopardy/Web/kuta-rsctf-dynamic/challenge.yml".into()),
        attachment_id: None,
        test_container_id: None,
        enable_traffic_capture: false,
        enable_shared_container: false,
        disable_blood_bonus: true,
        original_score: 1000,
        min_score_rate: 0.01,
        difficulty: 5.0,
        score_curve: ScoreCurve::Standard,
        shared_container_id: None,
        network_mode: Some(NetworkMode::Open),
        variant_mode: ChallengeVariantMode::Disabled,
        variant_generator_image: None,
        variant_generator_digest: None,
        variant_generator_build_context_subdir: None,
        variant_generator_build_status: ChallengeBuildStatus::None,
        variant_generator_last_build_log: None,
        solve_receipt_mode: SolveReceiptMode::Disabled,
        receipt_verifier_identity: None,
        ad_checker_image: None,
        ad_allow_egress: false,
        ad_allow_self_reset: false,
        ad_ssh_requires_flag: false,
        ad_self_hosted: false,
        ad_scoring_weight: 1.0,
    }
}

fn update(json: serde_json::Value) -> super::super::ChallengeUpdateModel {
    serde_json::from_value(json).expect("challenge update must deserialize")
}

#[test]
fn jeopardy_enabled_toggle_remains_an_operational_change_after_scoring_starts() {
    let current = challenge();

    assert!(!challenge_scoring_fields_changed(
        &update(serde_json::json!({ "isEnabled": false })),
        &current,
    ));
}

#[test]
fn actual_jeopardy_scoring_changes_remain_locked() {
    let current = challenge();

    assert!(challenge_scoring_fields_changed(
        &update(serde_json::json!({ "originalScore": 500 })),
        &current,
    ));
    assert!(challenge_scoring_fields_changed(
        &update(serde_json::json!({ "flagTemplate": "TCP1P{[TEAM_HASH]}" })),
        &current,
    ));
}
