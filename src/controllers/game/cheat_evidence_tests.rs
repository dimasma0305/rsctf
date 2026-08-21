use chrono::{TimeZone, Utc};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

use super::*;

fn event(kind: SuspicionType, evidence_key: &str) -> EventEvidenceRow {
    EventEvidenceRow {
        event_id: 7,
        game_id: 2,
        participation_id: 3,
        challenge_id: Some(4),
        kind: kind.kind(),
        evidence_key: evidence_key.to_string(),
        score_delta: kind.default_entry().0,
        created_at: Utc.timestamp_millis_opt(1_700_000_000_123).unwrap(),
        team_id: 5,
        team_name: "Review Team".to_string(),
        challenge_title: Some("Review Challenge".to_string()),
    }
}

#[test]
fn assessment_never_calls_context_or_heuristics_direct_proof() {
    assert_eq!(
        assessment_for(SuspicionTier::Hard),
        EvidenceAssessment::DirectEvidence
    );
    assert_eq!(
        assessment_for(SuspicionTier::Strong),
        EvidenceAssessment::StrongIndicator
    );
    assert_eq!(
        assessment_for(SuspicionTier::Behavioral),
        EvidenceAssessment::BehavioralIndicator
    );
    assert_eq!(
        assessment_for(SuspicionTier::Context),
        EvidenceAssessment::ContextOnly
    );

    let hard = base_review(
        &event(SuspicionType::StolenFlag, "submission:9"),
        SuspicionType::StolenFlag,
    );
    assert!(
        !hard.is_direct_proof,
        "proof requires a verified source join"
    );
    assert_eq!(hard.source_status, EvidenceSourceStatus::Unavailable);
}

#[test]
fn evidence_key_parsers_are_strict() {
    assert_eq!(parse_i32_key("submission:42", "submission:"), Some(42));
    assert_eq!(parse_i32_key("submission:-1", "submission:"), Some(-1));
    assert_eq!(parse_i32_key("challenge:x", "challenge:"), None);

    let user = Uuid::new_v4();
    assert_eq!(
        parse_uuid_user_key(
            &format!("fingerprint-churn:user:{user}"),
            "fingerprint-churn:"
        ),
        Some(user)
    );
    assert!(parse_uuid_user_key("fingerprint-churn:user:nope", "fingerprint-churn:").is_none());

    let hash = vec![0xab; 32];
    assert_eq!(
        parse_hash_key(&format!("shared-ip:{}", hex::encode(&hash)), "shared-ip:"),
        Some(hash)
    );
    assert!(parse_hash_key("shared-ip:abcd", "shared-ip:").is_none());
}

#[test]
fn review_wire_format_uses_millis_and_string_classification() {
    let review = base_review(
        &event(SuspicionType::SharedIp, "shared-ip:fixture"),
        SuspicionType::SharedIp,
    );
    let json = serde_json::to_value(review).unwrap();
    assert_eq!(json["assessment"], "contextOnly");
    assert_eq!(json["sourceStatus"], "unavailable");
    assert_eq!(json["observedAt"], 1_700_000_000_123_i64);
    assert_eq!(json["sources"][0]["recordedAt"], 1_700_000_000_123_i64);
    assert_eq!(json["sources"][0]["immutable"], true);
}

#[test]
fn privacy_helpers_never_render_full_identity_hashes() {
    let value = vec![0xab; 32];
    let hint = hash_hint(Some(&value));
    assert_eq!(hint, "abababababab…");
    assert!(!hint.contains(&hex::encode(value)));
    assert_eq!(hash_hint(None), "not captured");
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn source_review_resolves_direct_submission_identity_and_pair_ledgers() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("evidence_review_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect isolated schema");

    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ NOT NULL,
          end_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
          competitive_admitted_at_utc TIMESTAMPTZ NULL
        );
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, title TEXT NOT NULL
        );
        CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, user_name TEXT NOT NULL);
        CREATE TABLE "UserParticipations" (
          user_id UUID NOT NULL, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL, PRIMARY KEY (user_id, game_id)
        );
        CREATE TABLE "SuspicionEvents" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, participation_id INTEGER NOT NULL,
          challenge_id INTEGER NULL, kind SMALLINT NOT NULL, evidence_key TEXT NOT NULL,
          score_delta INTEGER NULL, created_at TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "Submissions" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, participation_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL, user_id UUID NULL, status SMALLINT NOT NULL,
          submit_time_utc TIMESTAMPTZ NOT NULL, submit_remote_ip_hash BYTEA NULL,
          container_id UUID NULL, container_last_operation_at_submit TIMESTAMPTZ NULL,
          container_was_loaded_at_submit BOOLEAN NULL, first_open_at_submit TIMESTAMPTZ NULL,
          first_download_at_submit TIMESTAMPTZ NULL,
          first_container_start_at_submit TIMESTAMPTZ NULL
        );
        CREATE TABLE "FirstSolves" (
          participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
          submission_id INTEGER NOT NULL
        );
        CREATE TABLE "CheatInfo" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, submission_id INTEGER NOT NULL,
          submit_participation_id INTEGER NOT NULL, source_participation_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL, evidence_key TEXT NOT NULL,
          observed_at_utc TIMESTAMPTZ NOT NULL, evidence_payload JSONB NOT NULL,
          evidence_version SMALLINT NOT NULL
        );
        CREATE TABLE "ContainerAccessEvents" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
          container_owner_participation_id INTEGER NOT NULL, container_id UUID NOT NULL,
          accessing_user_id UUID NULL, accessing_user_name TEXT NULL,
          accessing_participation_id INTEGER NULL, remote_ip_hash BYTEA NULL,
          is_monitor BOOLEAN NULL, connected_at_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "SuspicionEvaluationOutbox" (
          id BIGINT PRIMARY KEY, source_id INTEGER NOT NULL, game_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL, challenge_id INTEGER NULL, rule_kind SMALLINT NULL,
          evidence_key TEXT NOT NULL, observed_at_utc TIMESTAMPTZ NOT NULL,
          completed_at_utc TIMESTAMPTZ NULL
        );
        CREATE TABLE "IdentityObservations" (
          id BIGSERIAL PRIMARY KEY, user_id UUID NOT NULL, team_id INTEGER NULL,
          game_id INTEGER NULL, participation_id INTEGER NULL, kind TEXT NOT NULL,
          value_hash BYTEA NOT NULL, subnet_group_hash BYTEA NULL, value_hint TEXT NOT NULL,
          source TEXT NOT NULL, observed_at_utc TIMESTAMPTZ NOT NULL
        );

        INSERT INTO "Games" VALUES (1, '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z');
        INSERT INTO "Teams" VALUES (1, 'Owner'), (2, 'Submitter'), (3, 'Peer');
        INSERT INTO "Participations" VALUES
          (1, 1, 1, '2026-01-01T00:00:00Z'),
          (2, 1, 2, '2026-01-01T00:00:00Z'),
          (3, 1, 3, '2026-01-01T00:00:00Z');
        INSERT INTO "GameChallenges" VALUES
          (10, 1, 'Crypto'), (11, 1, 'Web'), (12, 1, 'Pwn');
        INSERT INTO "AspNetUsers" VALUES
          ('00000000-0000-0000-0000-000000000001', 'owner-user'),
          ('00000000-0000-0000-0000-000000000002', 'submit-user'),
          ('00000000-0000-0000-0000-000000000003', 'peer-user');
        INSERT INTO "UserParticipations" VALUES
          ('00000000-0000-0000-0000-000000000001', 1, 1, 1),
          ('00000000-0000-0000-0000-000000000002', 1, 2, 2),
          ('00000000-0000-0000-0000-000000000003', 1, 3, 3);

        INSERT INTO "Submissions"
          (id, game_id, participation_id, challenge_id, user_id, status,
           submit_time_utc, submit_remote_ip_hash, container_id,
           container_last_operation_at_submit, container_was_loaded_at_submit,
           first_open_at_submit, first_download_at_submit,
           first_container_start_at_submit)
        VALUES
          (10, 1, 2, 10, '00000000-0000-0000-0000-000000000002', 3,
           '2026-01-01T01:00:00Z', decode(repeat('07', 32), 'hex'), NULL,
           NULL, NULL, NULL, NULL, NULL),
          (20, 1, 1, 10, '00000000-0000-0000-0000-000000000001', 1,
           '2026-01-01T00:30:00Z', decode(repeat('08', 32), 'hex'), NULL,
           NULL, NULL, NULL, NULL, NULL),
          (21, 1, 2, 10, '00000000-0000-0000-0000-000000000002', 1,
           '2026-01-01T01:00:00Z', decode(repeat('07', 32), 'hex'),
           '00000000-0000-0000-0000-000000000100', '2026-01-01T00:20:00Z', FALSE,
           '2026-01-01T00:45:00Z', NULL, '2026-01-01T00:10:00Z'),
          (22, 1, 2, 11, '00000000-0000-0000-0000-000000000002', 1,
           '2026-01-01T01:00:20Z', NULL, NULL, NULL, NULL, NULL, NULL, NULL),
          (23, 1, 2, 12, '00000000-0000-0000-0000-000000000002', 1,
           '2026-01-01T01:00:40Z', NULL, NULL, NULL, NULL, NULL, NULL, NULL),
          (24, 1, 3, 10, '00000000-0000-0000-0000-000000000003', 1,
           '2026-01-01T01:05:00Z', NULL, NULL, NULL, NULL, NULL, NULL, NULL);
        INSERT INTO "FirstSolves" VALUES
          (1, 10, 20), (2, 10, 21), (2, 11, 22), (2, 12, 23), (3, 10, 24);
        INSERT INTO "CheatInfo" VALUES
          (1, 1, 10, 2, 1, 10, 'submission:10', '2026-01-01T01:00:00Z',
           '{"sourceTeamName":"Owner","submitTeamName":"Submitter","submitUserName":"submit-user","challengeTitle":"Crypto"}', 1);
        INSERT INTO "ContainerAccessEvents" VALUES
          (1, 1, 10, 1, '00000000-0000-0000-0000-000000000100',
           '00000000-0000-0000-0000-000000000002', 'submit-user', 2,
           decode(repeat('07', 32), 'hex'), FALSE, '2026-01-01T00:59:00Z'),
          (2, 1, 10, 2, '00000000-0000-0000-0000-000000000100',
           '00000000-0000-0000-0000-000000000002', 'submit-user', 2,
           decode(repeat('07', 32), 'hex'), FALSE, '2026-01-01T00:59:30Z');
        INSERT INTO "SuspicionEvaluationOutbox" VALUES
          (1, 1, 1, 2, 10, 33, 'challenge:10', '2026-01-01T00:59:00Z',
           '2026-01-01T00:59:01Z');
        INSERT INTO "SuspicionEvents" VALUES
          (100, 1, 2, 10, 0, 'submission:10', 100, '2026-01-01T01:00:00Z'),
          (101, 1, 2, 10, 33, 'challenge:10', 120, '2026-01-01T00:59:00Z');
        "#,
    )
    .execute(&pool)
    .await
    .expect("seed evidence review fixture");

    let identity_hash = vec![0xabu8; 32];
    for (user, team, participation, hint) in [
        ("00000000-0000-0000-0000-000000000001", 1, 1, "fp:a1"),
        ("00000000-0000-0000-0000-000000000002", 2, 2, "fp:a1"),
    ] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind, value_hash,
                  value_hint, source, observed_at_utc)
               VALUES ($1, $2, 1, $3, 'Fingerprint', $4, $5, 'Password',
                       '2026-01-01T00:10:00Z')"#,
        )
        .bind(Uuid::parse_str(user).unwrap())
        .bind(team)
        .bind(participation)
        .bind(&identity_hash)
        .bind(hint)
        .execute(&pool)
        .await
        .unwrap();
    }

    let stolen = load_event(&pool, 1, 100).await.unwrap();
    let mut stolen_review = base_review(&stolen, SuspicionType::StolenFlag);
    add_stolen_flag_source(&pool, &stolen, &mut stolen_review)
        .await
        .unwrap();
    assert!(stolen_review.is_direct_proof);
    assert_eq!(stolen_review.source_status, EvidenceSourceStatus::Verified);
    assert!(stolen_review.sources[1]
        .facts
        .iter()
        .any(|fact| fact.label == "Flag owner" && fact.value.contains("Owner")));

    let cross = load_event(&pool, 1, 101).await.unwrap();
    let mut cross_review = base_review(&cross, SuspicionType::CrossTeamContainerAccess);
    sources::add_cross_team_access_source(&pool, &cross, &mut cross_review)
        .await
        .unwrap();
    assert!(cross_review.is_direct_proof);
    assert_eq!(cross_review.source_status, EvidenceSourceStatus::Verified);

    let identity_event = event(
        SuspicionType::SharedFingerprint,
        &format!("shared-fingerprint:{}", hex::encode(&identity_hash)),
    );
    let identity_event = EventEvidenceRow {
        game_id: 1,
        participation_id: 2,
        challenge_id: None,
        ..identity_event
    };
    let mut identity_review = base_review(&identity_event, SuspicionType::SharedFingerprint);
    sources::add_identity_source(
        &pool,
        &identity_event,
        SuspicionType::SharedFingerprint,
        &mut identity_review,
    )
    .await
    .unwrap();
    assert_eq!(
        identity_review.source_status,
        EvidenceSourceStatus::Supporting
    );
    assert!(!identity_review.is_direct_proof);

    let challenge_event = EventEvidenceRow {
        game_id: 1,
        participation_id: 2,
        challenge_id: Some(10),
        evidence_key: "challenge:10".to_string(),
        ..event(SuspicionType::AdaptiveFastSolve, "challenge:10")
    };
    let mut challenge_review = base_review(&challenge_event, SuspicionType::AdaptiveFastSolve);
    sources::add_submission_source(&pool, &challenge_event, &mut challenge_review)
        .await
        .unwrap();
    sources::add_challenge_source(
        &pool,
        &challenge_event,
        SuspicionType::AdaptiveFastSolve,
        &mut challenge_review,
    )
    .await
    .unwrap();
    assert!(challenge_review.sources.len() >= 3);

    let pair_event = EventEvidenceRow {
        game_id: 1,
        participation_id: 2,
        challenge_id: None,
        evidence_key: "pair:1:2".to_string(),
        ..event(SuspicionType::SequenceSimilarity, "pair:1:2")
    };
    let mut pair_review = base_review(&pair_event, SuspicionType::SequenceSimilarity);
    sources::add_pair_source(
        &pool,
        &pair_event,
        SuspicionType::SequenceSimilarity,
        &mut pair_review,
    )
    .await
    .unwrap();
    assert_eq!(pair_review.source_status, EvidenceSourceStatus::Supporting);

    let burst_event = EventEvidenceRow {
        game_id: 1,
        participation_id: 2,
        challenge_id: None,
        evidence_key: "global".to_string(),
        ..event(SuspicionType::Burst, "global")
    };
    let mut burst_review = base_review(&burst_event, SuspicionType::Burst);
    sources::add_burst_source(&pool, &burst_event, &mut burst_review)
        .await
        .unwrap();
    assert_eq!(burst_review.source_status, EvidenceSourceStatus::Supporting);

    pool.close().await;
    assert!(schema.starts_with("evidence_review_"));
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}
