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

#[test]
fn evidence_reconstruction_limits_are_strict() {
    assert_eq!(sources::MAX_PRIOR_IDENTITY_HINTS, 12);
    assert_eq!(sources::MAX_IDENTITY_SAMPLE_ROWS, 200);
    assert_eq!(sources::MAX_PAIR_SAMPLE_ROWS, 12);
    assert_eq!(sources::MAX_WRONG_INTERVAL_ATTEMPTS, 256);
    assert_eq!(sources::MAX_CHALLENGE_SOLVER_ROWS, 256);
    assert_eq!(sources::MAX_SUBMISSION_CONTEXT_ROWS, 64);
    assert_eq!(sources::BURST_SOLVE_ROWS, 3);
}

#[test]
fn pair_reconstruction_applies_the_event_time_to_both_bounded_inputs() {
    let source = include_str!("cheat_evidence_correlations.rs");
    let pair_source = source
        .split_once("async fn add_pair_source")
        .expect("pair evidence source exists")
        .1;
    assert_eq!(
        pair_source
            .matches("AND submission.submit_time_utc <= $")
            .count(),
        2
    );
    assert_eq!(pair_source.matches(".bind(event.created_at)").count(), 2);
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
        INSERT INTO "ContainerAccessEvents"
          (id, game_id, challenge_id, container_owner_participation_id, container_id,
           accessing_user_id, accessing_user_name, accessing_participation_id,
           remote_ip_hash, is_monitor, connected_at_utc)
        SELECT 100 + value, 1, 10, 2, '00000000-0000-0000-0000-000000000100',
               '00000000-0000-0000-0000-000000000002', 'submit-user', 2,
               decode(repeat('07', 32), 'hex'), FALSE,
               '2026-01-01T00:40:00Z'::timestamptz + value * interval '1 second'
          FROM generate_series(1, 65) value;
        INSERT INTO "SuspicionEvaluationOutbox" VALUES
          (1, 1, 1, 2, 10, 33, 'challenge:10', '2026-01-01T00:59:00Z',
           '2026-01-01T00:59:01Z');
        INSERT INTO "SuspicionEvents" VALUES
          (100, 1, 2, 10, 0, 'submission:10', 100, '2026-01-01T01:00:00Z'),
          (101, 1, 2, 10, 33, 'challenge:10', 120, '2026-01-01T00:59:00Z');

        INSERT INTO "GameChallenges" VALUES (13, 1, 'Late Solve');
        INSERT INTO "Submissions"
          (id, game_id, participation_id, challenge_id, user_id, status, submit_time_utc)
        VALUES
          (25, 1, 2, 13, '00000000-0000-0000-0000-000000000002', 1,
           '2026-01-01T01:00:41Z');
        INSERT INTO "FirstSolves" VALUES (2, 13, 25);

        INSERT INTO "GameChallenges" (id, game_id, title)
        SELECT 100 + value, 1, 'Shared ' || value
          FROM generate_series(0, 19) value;
        INSERT INTO "Submissions"
          (id, game_id, participation_id, challenge_id, user_id, status, submit_time_utc)
        SELECT 1000 + value, 1, 1, 100 + value,
               '00000000-0000-0000-0000-000000000001'::uuid, 1,
               '2026-01-01T03:00:00Z'::timestamptz + value * interval '1 minute'
          FROM generate_series(0, 19) value
        UNION ALL
        SELECT 2000 + value, 1, 2, 100 + value,
               '00000000-0000-0000-0000-000000000002'::uuid, 1,
               '2026-01-01T03:00:30Z'::timestamptz + value * interval '1 minute'
          FROM generate_series(0, 19) value;
        INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
        SELECT 1, 100 + value, 1000 + value FROM generate_series(0, 19) value
        UNION ALL
        SELECT 2, 100 + value, 2000 + value FROM generate_series(0, 19) value;

        INSERT INTO "GameChallenges" VALUES (120, 1, 'Future Shared');
        INSERT INTO "Submissions"
          (id, game_id, participation_id, challenge_id, user_id, status, submit_time_utc)
        VALUES
          (1020, 1, 1, 120, '00000000-0000-0000-0000-000000000001', 1,
           '2026-01-01T04:00:00Z'),
          (2020, 1, 2, 120, '00000000-0000-0000-0000-000000000002', 1,
           '2026-01-01T04:00:30Z');
        INSERT INTO "FirstSolves" VALUES (1, 120, 1020), (2, 120, 2020);

        INSERT INTO "Teams" (id, name)
        SELECT 10000 + value, 'Solver ' || value FROM generate_series(1, 257) value;
        INSERT INTO "Participations" (id, game_id, team_id, competitive_admitted_at_utc)
        SELECT 10000 + value, 1, 10000 + value, '2026-01-01T00:00:00Z'
          FROM generate_series(1, 257) value;
        INSERT INTO "Submissions"
          (id, game_id, participation_id, challenge_id, user_id, status, submit_time_utc)
        SELECT 10000 + value, 1, 10000 + value, 10, NULL, 1,
               '2026-01-01T00:31:00Z'::timestamptz + value * interval '1 second'
          FROM generate_series(1, 257) value;
        INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
        SELECT 10000 + value, 10, 10000 + value FROM generate_series(1, 257) value;

        INSERT INTO "Submissions"
          (id, game_id, participation_id, challenge_id, user_id, status, submit_time_utc)
        VALUES (2999, 1, 2, 10,
                '00000000-0000-0000-0000-000000000002', 2,
                '2025-12-31T23:59:00Z');
        INSERT INTO "Submissions"
          (id, game_id, participation_id, challenge_id, user_id, status, submit_time_utc)
        SELECT 5000 + value, 1, 2, 10,
               '00000000-0000-0000-0000-000000000002'::uuid, 2,
               '2026-01-01T01:10:00Z'::timestamptz + value * interval '5 seconds'
          FROM generate_series(0, 299) value
        UNION ALL
        SELECT 3000 + value, 1, 2, 10,
               '00000000-0000-0000-0000-000000000002'::uuid, 2,
               '2026-01-01T02:00:00Z'::timestamptz + value * interval '1 second'
          FROM generate_series(0, 14) value
        UNION ALL
        SELECT 4000 + value, 1, 2, 10,
               '00000000-0000-0000-0000-000000000002'::uuid, 2,
               '2026-01-01T02:01:00Z'::timestamptz + value * interval '1 second'
          FROM generate_series(0, 49) value;
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
    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind, value_hash,
              value_hint, source, observed_at_utc)
           SELECT '00000000-0000-0000-0000-000000000002'::uuid, 2, 1, 2,
                  'Fingerprint', $1, 'fp:' || value, 'Password',
                  '2026-01-01T00:20:00Z'::timestamptz + value * interval '1 millisecond'
             FROM generate_series(1, 205) value
           UNION ALL
           SELECT '00000000-0000-0000-0000-000000000002'::uuid, 2, 1, 2,
                  'Fingerprint', $1, 'fp:late', 'Password',
                  '2026-01-01T00:40:00Z'::timestamptz"#,
    )
    .bind(&identity_hash)
    .execute(&pool)
    .await
    .expect("seed bounded identity history");
    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind, value_hash,
              value_hint, source, observed_at_utc)
           SELECT '00000000-0000-0000-0000-000000000002'::uuid, 2, 1, 2, 'Ip',
                  decode(lpad(to_hex(value), 64, '0'), 'hex'),
                  'ip:' || value, 'Submission',
                  '2026-01-01T00:30:00Z'::timestamptz + value * interval '1 millisecond'
             FROM generate_series(1, 70) value"#,
    )
    .execute(&pool)
    .await
    .expect("seed bounded prior identity hints");

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
        created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 30, 0).unwrap(),
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
    let identity_source = identity_review.sources.last().unwrap();
    assert!(identity_source.facts.iter().any(|fact| {
        fact.label == "Observations through event" && fact.value == "at least 201"
    }));
    assert!(identity_source.facts.iter().any(|fact| {
        fact.label == "Distinct identities in bounded sample" && fact.value == "200"
    }));
    assert!(identity_source
        .facts
        .iter()
        .any(|fact| fact.label == "Bounded sample last observed"
            && fact.value.starts_with("2026-01-01T00:20")));
    assert!(identity_review
        .limitations
        .iter()
        .any(|limitation| limitation.contains("latest 200 observations")));

    let challenge_event = EventEvidenceRow {
        game_id: 1,
        participation_id: 2,
        challenge_id: Some(10),
        evidence_key: "challenge:10".to_string(),
        created_at: Utc.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap(),
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
    let submission_source = challenge_review
        .sources
        .iter()
        .find(|source| source.source_type == "submissionSnapshot")
        .unwrap();
    let prior_hints = submission_source
        .facts
        .iter()
        .find(|fact| fact.label == "Prior masked IP hints")
        .unwrap();
    assert_eq!(prior_hints.value.split(", ").count(), 12);
    assert!(submission_source.facts.iter().any(|fact| {
        fact.label == "Matching access records (lower bound)" && fact.value == "at least 65"
    }));
    assert!(submission_source.facts.iter().any(|fact| {
        fact.label == "Known IP identities in bounded pre-submit sample" && fact.value == "64"
    }));
    assert!(challenge_review
        .limitations
        .iter()
        .any(|limitation| limitation.contains("latest 64 matching records")));
    assert!(challenge_review
        .limitations
        .iter()
        .any(|limitation| limitation.contains("latest 64 observations")));
    let challenge_source = challenge_review.sources.last().unwrap();
    assert!(challenge_source.facts.iter().any(|fact| {
        fact.label == "Canonical solver count (lower bound)" && fact.value == "at least 257"
    }));
    assert!(challenge_review
        .limitations
        .iter()
        .any(|limitation| limitation.contains("earliest 256 canonical solves")));
    assert!(challenge_source
        .facts
        .iter()
        .any(|fact| fact.label == "Wrong attempts before solve" && fact.value == "0"));

    let automated_event = EventEvidenceRow {
        game_id: 1,
        participation_id: 2,
        challenge_id: Some(10),
        evidence_key: "challenge:10".to_string(),
        created_at: Utc.with_ymd_and_hms(2026, 1, 1, 2, 0, 14).unwrap(),
        ..event(SuspicionType::AutomatedPattern, "challenge:10")
    };
    let mut automated_review = base_review(&automated_event, SuspicionType::AutomatedPattern);
    sources::add_challenge_source(
        &pool,
        &automated_event,
        SuspicionType::AutomatedPattern,
        &mut automated_review,
    )
    .await
    .unwrap();
    let automated_source = automated_review.sources.last().unwrap();
    for (label, expected) in [
        ("Wrong attempts through event (lower bound)", "at least 257"),
        ("Wrong attempts in prior 60 seconds (bounded sample)", "15"),
        ("Consecutive sub-2-second intervals (bounded sample)", "14"),
    ] {
        assert!(automated_source
            .facts
            .iter()
            .any(|fact| fact.label == label && fact.value == expected));
    }
    assert!(automated_review.limitations.iter().any(|limitation| {
        limitation.contains("latest 256 wrong attempts")
            && limitation.contains("bounded lower-bound/sample values")
    }));

    let pair_event = EventEvidenceRow {
        game_id: 1,
        participation_id: 2,
        challenge_id: None,
        evidence_key: "pair:1:2".to_string(),
        created_at: Utc.with_ymd_and_hms(2026, 1, 1, 3, 30, 0).unwrap(),
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
    let pair_source = pair_review.sources.last().unwrap();
    assert!(pair_source
        .facts
        .iter()
        .any(|fact| fact.label == "Shared canonical solves"
            && fact.value == "at least 13 in bounded sample"));
    let pair_sample = pair_source
        .facts
        .iter()
        .find(|fact| fact.label == "Solve-gap sample")
        .unwrap();
    assert_eq!(pair_sample.value.split("; ").count(), 12);
    assert!(!pair_sample.value.contains("Future Shared"));
    assert!(pair_review
        .limitations
        .iter()
        .any(|limitation| limitation.contains("latest 12 canonical solves")));

    let burst_event = EventEvidenceRow {
        game_id: 1,
        participation_id: 2,
        challenge_id: None,
        evidence_key: "global".to_string(),
        created_at: Utc.with_ymd_and_hms(2026, 1, 1, 1, 0, 40).unwrap(),
        ..event(SuspicionType::Burst, "global")
    };
    let mut burst_review = base_review(&burst_event, SuspicionType::Burst);
    sources::add_burst_source(&pool, &burst_event, &mut burst_review)
        .await
        .unwrap();
    assert_eq!(burst_review.source_status, EvidenceSourceStatus::Supporting);
    let burst_source = burst_review.sources.last().unwrap();
    let burst_solves = burst_source
        .facts
        .iter()
        .find(|fact| fact.label == "Solves")
        .unwrap();
    assert!(!burst_solves.value.contains("Late Solve"));
    assert_eq!(burst_solves.value.split("; ").count(), 3);

    pool.close().await;
    assert!(schema.starts_with("evidence_review_"));
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}
