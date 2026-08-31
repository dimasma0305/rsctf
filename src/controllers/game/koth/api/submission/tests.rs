use super::evidence::ResolvedInputRow;
use super::*;
use axum::http::HeaderValue;
use hmac::{Hmac, KeyInit, Mac};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

use crate::utils::enums::ParticipationStatus;

mod distinct;
mod max_bounds;

fn row(participation_id: i32, earned: i64) -> ResolvedInputRow {
    ResolvedInputRow {
        participation_id,
        activity_earned: 10,
        activity_possible: 10,
        objective_earned: earned * 2,
        objective_possible: 20,
        objective_count: 2,
        is_crown: participation_id == 1,
    }
}

fn wave(id: &str, rows: Vec<ResolvedInputRow>) -> ResolvedWave {
    ResolvedWave {
        wave_id: id.to_string(),
        ended_at: DateTime::from_timestamp_millis(1).unwrap(),
        rows,
    }
}

#[test]
fn ordinary_windows_are_half_open_and_terminal_cutoff_is_inclusive() {
    let at = |millis| DateTime::from_timestamp_millis(millis).unwrap();
    assert!(!wave_ends_inside_window(
        at(180_000),
        at(120_000),
        at(180_000)
    ));
    assert!(wave_ends_inside_window(
        at(180_000),
        at(120_000),
        at(180_001)
    ));
    assert!(!wave_ends_inside_window(
        at(180_001),
        at(120_000),
        at(180_001)
    ));
}

#[test]
fn snapshot_digest_binds_resolved_identity_and_every_budget() {
    let schema = [7; 32];
    let base = snapshot_hash(
        "context",
        &schema,
        &[wave("wave-1", vec![row(1, 5), row(2, 6)])],
    );
    assert_ne!(
        base,
        snapshot_hash(
            "other",
            &schema,
            &[wave("wave-1", vec![row(1, 5), row(2, 6)])]
        )
    );
    assert_ne!(
        base,
        snapshot_hash(
            "context",
            &[8; 32],
            &[wave("wave-1", vec![row(1, 5), row(2, 6)])]
        )
    );
    assert_ne!(
        base,
        snapshot_hash("context", &schema, &[wave("wave-2", vec![row(1, 5)])])
    );
    assert_ne!(
        base,
        snapshot_hash(
            "context",
            &schema,
            &[wave("wave-1", vec![row(1, 6), row(2, 6)])]
        )
    );
    assert_ne!(
        base,
        snapshot_hash(
            "context",
            &schema,
            &[wave("wave-1", vec![row(2, 6), row(1, 5)])]
        )
    );
}

#[test]
fn resolved_crown_requires_one_unique_completed_leader() {
    let valid = wave("wave-1", vec![row(1, 8), row(2, 7)]);
    assert!(validate_resolved_crowns(&[valid]).is_ok());

    let mut missing = wave("wave-1", vec![row(1, 8), row(2, 7)]);
    missing.rows[0].is_crown = false;
    assert!(validate_resolved_crowns(&[missing]).is_err());

    let mut tie = wave("wave-1", vec![row(1, 8), row(2, 8)]);
    tie.rows[0].is_crown = false;
    assert!(validate_resolved_crowns(&[tie]).is_ok());

    let crowned_tie = wave("wave-1", vec![row(1, 8), row(2, 8)]);
    assert!(validate_resolved_crowns(&[crowned_tie]).is_err());

    let mut weaker = wave("wave-1", vec![row(1, 7), row(2, 8)]);
    weaker.rows[0].is_crown = true;
    weaker.rows[1].is_crown = false;
    assert!(validate_resolved_crowns(&[weaker]).is_err());

    let mut zero = wave("wave-1", vec![row(1, 0)]);
    zero.rows[0].is_crown = false;
    assert!(validate_resolved_crowns(&[zero]).is_ok());
}

#[test]
fn finalized_waves_may_only_gain_an_unchanged_suffix() {
    let first = wave("wave-1", vec![row(1, 8), row(2, 7)]);
    let second = wave("wave-2", vec![row(1, 7), row(2, 8)]);
    assert!(ensure_finalized_waves_are_append_only(
        std::slice::from_ref(&first),
        &[first.clone(), second]
    )
    .is_ok());
    assert!(ensure_finalized_waves_are_append_only(std::slice::from_ref(&first), &[]).is_err());
    let mut changed = first.clone();
    changed.rows[0].objective_earned += 1;
    assert!(ensure_finalized_waves_are_append_only(&[first], &[changed]).is_err());
}

#[test]
fn semantic_request_identity_normalizes_wave_and_team_order_and_binds_signer() {
    let first = parse_and_normalize(
        br#"{
          "context":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          "objectiveIds":["quality"],
          "waves":[
            {"waveId":"wave-b","endedAtUnixMs":20,"teams":[
              {"tokenHash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","activity":{"earned":1,"possible":1},"objectives":[{"earned":1,"possible":1}],"isCrown":false},
              {"tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","activity":{"earned":1,"possible":1},"objectives":[{"earned":1,"possible":1}],"isCrown":true}
            ]},
            {"waveId":"wave-a","endedAtUnixMs":10,"teams":[]}
          ]
        }"#,
    )
    .unwrap();
    let reordered = parse_and_normalize(
        br#"{"waves":[{"teams":[],"endedAtUnixMs":10,"waveId":"wave-a"},{"teams":[{"isCrown":true,"objectives":[{"possible":1,"earned":1}],"activity":{"possible":1,"earned":1},"tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},{"isCrown":false,"objectives":[{"possible":1,"earned":1}],"activity":{"possible":1,"earned":1},"tokenHash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}],"endedAtUnixMs":20,"waveId":"wave-b"}],"objectiveIds":["quality"],"context":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
    )
    .unwrap();
    let body_digest = canonical_input_digest(&first);
    assert_eq!(body_digest, canonical_input_digest(&reordered));
    assert_eq!(observation_weight(MAX_BODY_BYTES, &first), 6);
    assert_ne!(
        observation_request_digest(7, 9, "observer:7:9", &body_digest),
        observation_request_digest(7, 9, "target:7:9:41:2", &body_digest)
    );
}

#[tokio::test]
async fn stale_context_is_the_only_typed_retryable_conflict() {
    let response = observation_error_response(AppError::conflict(STALE_CONTEXT_MESSAGE)).unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    assert_eq!(response.headers()[axum::http::header::RETRY_AFTER], "1");
    let body = axum::body::to_bytes(response.into_body(), 1_024)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["code"], "stale_context");

    let permanent = observation_error_response(AppError::conflict(
        "Leaderboard objective IDs and order are frozen for this challenge",
    ))
    .unwrap_err();
    assert_eq!(permanent.status(), axum::http::StatusCode::CONFLICT);
}

fn signed_headers(
    secret: &str,
    timestamp: &str,
    game_id: i32,
    challenge_id: i32,
    body: &[u8],
) -> HeaderMap {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(game_id.to_string().as_bytes());
    mac.update(b".");
    mac.update(challenge_id.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let mut headers = HeaderMap::new();
    headers.insert(
        super::super::TIMESTAMP_HEADER,
        HeaderValue::from_str(timestamp).unwrap(),
    );
    headers.insert(
        super::super::SIGNATURE_HEADER,
        HeaderValue::from_str(&format!(
            "{}{}",
            super::super::SIGNATURE_PREFIX,
            hex::encode(mac.finalize().into_bytes())
        ))
        .unwrap(),
    );
    headers
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_semantic_duplicates_join_one_durable_response_and_expired_leases_reclaim() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_koth_retry_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "KothApiObservationOperations" (
             challenge_id INTEGER NOT NULL,
             game_id INTEGER NOT NULL,
             request_digest BYTEA NOT NULL,
             signer_scope TEXT NOT NULL,
             body_digest BYTEA NOT NULL,
             context_hash CHAR(64) NOT NULL,
             lease_token UUID NOT NULL,
             lease_expires_at TIMESTAMPTZ NOT NULL,
             response JSONB NULL,
             created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
             completed_at TIMESTAMPTZ NULL,
             expires_at TIMESTAMPTZ NOT NULL
               DEFAULT (clock_timestamp() + interval '10 minutes'),
             PRIMARY KEY (challenge_id, request_digest)
           );"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let body_digest = [7_u8; 32];
    let request_digest = observation_request_digest(7, 9, "observer:7:9", &body_digest);
    let first = reserve_observation(
        &pool,
        7,
        9,
        "observer:7:9",
        &"a".repeat(64),
        body_digest,
        request_digest,
    )
    .await
    .unwrap();
    let ObservationReservationResult::Owner(owner) = first else {
        panic!("first request must own the operation");
    };
    let contender_pool = pool.clone();
    let contender = tokio::spawn(async move {
        reserve_observation(
            &contender_pool,
            7,
            9,
            "observer:7:9",
            &"a".repeat(64),
            body_digest,
            request_digest,
        )
        .await
        .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let accepted_at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let response = KothObservationAcceptedModel {
        accepted: true,
        cycle_number: 4,
        reset_attempt: 2,
        round_number: 5,
        submitted_waves: 1,
        submitted_teams: 2,
        recognized_teams: 2,
        accepted_at,
    };
    sqlx::query(
        r#"UPDATE "KothApiObservationOperations"
              SET response = $4, completed_at = clock_timestamp()
            WHERE challenge_id = $1 AND request_digest = $2 AND lease_token = $3"#,
    )
    .bind(9_i32)
    .bind(request_digest.as_slice())
    .bind(owner.lease_token)
    .bind(serde_json::to_value(&response).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let ObservationReservationResult::Completed(joined) = contender.await.unwrap() else {
        panic!("concurrent duplicate must join the completed result");
    };
    assert_eq!(joined.accepted_at, accepted_at);
    assert_eq!(joined.recognized_teams, 2);

    let reclaim_digest = [8_u8; 32];
    let stale = reserve_observation(
        &pool,
        7,
        9,
        "observer:7:9",
        &"b".repeat(64),
        [9_u8; 32],
        reclaim_digest,
    )
    .await
    .unwrap();
    let ObservationReservationResult::Owner(stale) = stale else {
        panic!("new request must own its operation");
    };
    sqlx::query(
        r#"UPDATE "KothApiObservationOperations"
              SET lease_expires_at = clock_timestamp() - interval '1 second'
            WHERE challenge_id = 9 AND request_digest = $1"#,
    )
    .bind(reclaim_digest.as_slice())
    .execute(&pool)
    .await
    .unwrap();
    let reclaimed = reserve_observation(
        &pool,
        7,
        9,
        "observer:7:9",
        &"b".repeat(64),
        [9_u8; 32],
        reclaim_digest,
    )
    .await
    .unwrap();
    let ObservationReservationResult::Owner(reclaimed) = reclaimed else {
        panic!("expired lease must be reclaimable");
    };
    assert_ne!(reclaimed.lease_token, stale.lease_token);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn signed_snapshot_is_tick_bound_normalized_replay_safe_and_hash_only() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ,
              end_time_utc TIMESTAMPTZ
            );
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER, "Type" SMALLINT
            );
            CREATE TEMP TABLE "KothOfficialConfigs" (
              game_id INTEGER PRIMARY KEY, roster_snapshot JSONB,
              hills_snapshot JSONB, scoring_start_round INTEGER
            );
            CREATE TEMP TABLE "KothTargets" (
              id INTEGER PRIMARY KEY, game_id INTEGER,
              challenge_id INTEGER, container_id TEXT
            );
            CREATE TEMP TABLE "KothCrownCycles" (
              id BIGINT PRIMARY KEY, game_id INTEGER, challenge_id INTEGER,
              cycle_number INTEGER, reset_attempt INTEGER,
              replacement_container_id TEXT, planned_start_round INTEGER,
              planned_end_round INTEGER, phase TEXT
            );
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER, number INTEGER,
              start_time_utc TIMESTAMPTZ, end_time_utc TIMESTAMPTZ,
              finalized BOOLEAN
            );
            CREATE TEMP TABLE "KothApiObservers" (
              challenge_id INTEGER PRIMARY KEY, game_id INTEGER,
              hmac_secret TEXT, last_used_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothApiObserverRevisions" (
              challenge_id INTEGER PRIMARY KEY, game_id INTEGER,
              revision BIGINT
            );
            CREATE TEMP TABLE "KothTargetReporters" (
              cycle_id BIGINT PRIMARY KEY, game_id INTEGER,
              challenge_id INTEGER, reset_attempt INTEGER,
              hmac_secret TEXT, issued_at TIMESTAMPTZ,
              expires_at TIMESTAMPTZ, last_used_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothApiArenaSchemes" (
              challenge_id INTEGER PRIMARY KEY, game_id INTEGER,
              objective_count SMALLINT, objective_ids TEXT[],
              objective_schema_hash BYTEA, frozen_at TIMESTAMPTZ
                DEFAULT clock_timestamp()
            );
            CREATE TEMP TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER, team_id INTEGER,
              status SMALLINT
            );
            CREATE TEMP TABLE "Teams" (
              id INTEGER PRIMARY KEY, captain_id INTEGER,
              deletion_pending BOOLEAN
            );
            CREATE TEMP TABLE "TeamMembers" (
              team_id INTEGER, user_id INTEGER
            );
            CREATE TEMP TABLE "AspNetUsers" (
              id INTEGER PRIMARY KEY, role SMALLINT
            );
            CREATE TEMP TABLE "KothApiTeamTokens" (
              game_id INTEGER, challenge_id INTEGER,
              participation_id INTEGER, token TEXT,
              revocation_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TEMP TABLE "KothApiSnapshots" (
              target_id INTEGER PRIMARY KEY, game_id INTEGER,
              challenge_id INTEGER, cycle_id BIGINT, reset_attempt INTEGER,
              container_id TEXT, ad_round_id INTEGER, context_hash CHAR(64),
              snapshot_hash BYTEA, objective_schema_hash BYTEA,
              request_timestamp_ms BIGINT,
              accepted_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothApiSnapshotScores" (
              target_id INTEGER, wave_id TEXT, participation_id INTEGER,
              activity_earned BIGINT, activity_possible BIGINT,
              objective_earned BIGINT, objective_possible BIGINT,
              objective_count SMALLINT, is_crown BOOLEAN
            );
            CREATE TEMP TABLE "KothApiSnapshotWaves" (
              target_id INTEGER, wave_id TEXT, ended_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothApiRequestReplays" (
              request_hash BYTEA PRIMARY KEY, challenge_id INTEGER,
              expires_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothApiObservationOperations" (
              challenge_id INTEGER, game_id INTEGER,
              request_digest BYTEA, signer_scope TEXT, body_digest BYTEA,
              context_hash CHAR(64), lease_token UUID,
              lease_expires_at TIMESTAMPTZ, response JSONB,
              created_at TIMESTAMPTZ DEFAULT clock_timestamp(),
              completed_at TIMESTAMPTZ, expires_at TIMESTAMPTZ
                DEFAULT (clock_timestamp() + interval '10 minutes'),
              PRIMARY KEY (challenge_id, request_digest)
            );
            "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"INSERT INTO "Games" VALUES
                 (7, clock_timestamp() - interval '4 minutes 13 seconds',
                     clock_timestamp() + interval '1 hour');
               INSERT INTO "KothOfficialConfigs" VALUES
                 (7, '[11,12]', '[{"challengeId":9,"claimSource":"Api"}]', 3);
               INSERT INTO "KothTargets" VALUES (3, 7, 9, 'runtime-a');
               INSERT INTO "KothCrownCycles" VALUES
                 (41, 7, 9, 4, 2, 'runtime-a', 1, 3, 'Active');
               INSERT INTO "AdRounds" VALUES
                 (47, 7, 1, clock_timestamp() - interval '220 seconds',
                  clock_timestamp() - interval '180 seconds', TRUE),
                 (48, 7, 2, clock_timestamp() - interval '180 seconds',
                  clock_timestamp() - interval '120 seconds', TRUE),
                 (49, 7, 3, clock_timestamp() - interval '120 seconds',
                  clock_timestamp() - interval '60 seconds', TRUE),
                 (50, 7, 4, clock_timestamp() - interval '60 seconds',
                  clock_timestamp() - interval '10 seconds', TRUE),
                 (51, 7, 5, clock_timestamp() - interval '10 seconds',
                  clock_timestamp() + interval '1 minute', FALSE);
               INSERT INTO "KothApiObservers" VALUES
                 (9, 7, 'observer-secret', NULL);
               INSERT INTO "KothApiObserverRevisions" VALUES (9, 7, 1);
               INSERT INTO "KothTargetReporters" VALUES
                 (41, 7, 9, 2, 'target-reporter-secret', clock_timestamp(),
                  clock_timestamp() + interval '1 hour', NULL);
               INSERT INTO "AspNetUsers" VALUES (101, 1), (102, 1);
               INSERT INTO "Teams" VALUES (21, 101, FALSE), (22, 102, FALSE);
               INSERT INTO "KothApiTeamTokens"
                 (game_id, challenge_id, participation_id, token) VALUES
                 (7, 9, 11, 'current-token-a'),
                 (7, 9, 12, 'current-token-b');"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "GameChallenges" VALUES ($1, 7, $2)"#)
        .bind(9_i32)
        .bind(ChallengeType::KingOfTheHill as i16)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" VALUES
                 (11, 7, 21, $1), (12, 7, 22, $1)"#,
    )
    .bind(ParticipationStatus::Accepted as i16)
    .execute(&pool)
    .await
    .unwrap();

    let active_context = load_active_context(&pool, 7, 9).await.unwrap().unwrap();
    let official_scoring_start: chrono::DateTime<Utc> = sqlx::query_scalar(
        r#"SELECT start_time_utc FROM "AdRounds"
            WHERE game_id = 7 AND number = 3"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let event_start: chrono::DateTime<Utc> =
        sqlx::query_scalar(r#"SELECT start_time_utc FROM "Games" WHERE id = 7"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(active_context.round_number, 5);
    assert_eq!(active_context.scoring_starts_at, official_scoring_start);
    assert!(active_context.scoring_starts_at > event_start);
    // Leaderboard arenas are persistent. The compatibility field carries
    // the event cutoff, not the Boot2Root crown-cycle boundary.
    assert!(
        active_context.cycle_ends_at - active_context.round_ends_at > chrono::Duration::minutes(50)
    );
    let context = active_context.opaque_context(
        7,
        9,
        &["current-token-a".to_string(), "current-token-b".to_string()],
    );
    let valid_hash = hex::encode(Sha256::digest(b"current-token-a"));
    let unknown_hash = hex::encode(Sha256::digest(b"stale-token"));
    let ended_at = Utc::now().timestamp_millis();
    let body = serde_json::to_vec(&serde_json::json!({
        "context": context,
        "objectiveIds": ["quality", "throughput"],
        "waves": [{
            "waveId": "heat-17",
            "endedAtUnixMs": ended_at,
            "teams": [
                {
                    "tokenHash": valid_hash,
                    "activity": {"earned": 1, "possible": 1},
                    "objectives": [
                        {"earned": 1, "possible": 10},
                        {"earned": 900, "possible": 1000}
                    ],
                    "isCrown": true
                },
                {
                    "tokenHash": unknown_hash,
                    "activity": {"earned": 1, "possible": 1},
                    "objectives": [
                        {"earned": 1, "possible": 1},
                        {"earned": 1, "possible": 1}
                    ],
                    "isCrown": false
                }
            ]
        }]
    }))
    .unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("current-token-a"));
    let timestamp = Utc::now().timestamp_millis().to_string();
    let invalid_headers = signed_headers("not-a-current-referee-secret", &timestamp, 7, 9, &body);
    let invalid = accept_observation(&pool, 7, 9, &invalid_headers, &body)
        .await
        .unwrap_err();
    assert_eq!(invalid.status(), axum::http::StatusCode::UNAUTHORIZED);
    let reserved_after_invalid_hmac: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "KothApiObservationOperations""#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reserved_after_invalid_hmac, 0);
    // The managed target credential is sufficient for the first schema-free
    // snapshot; no external reporter participates in the scoring path.
    let headers = signed_headers("target-reporter-secret", &timestamp, 7, 9, &body);
    let accepted = accept_observation(&pool, 7, 9, &headers, &body)
        .await
        .unwrap();
    assert_eq!(
        (
            accepted.submitted_waves,
            accepted.submitted_teams,
            accepted.recognized_teams
        ),
        (1, 2, 1)
    );
    // The first accepted objective scheme becomes part of the next opaque
    // context, so every reporter must refetch before deduplicating it.
    let reporter_context = load_active_context(&pool, 7, 9)
        .await
        .unwrap()
        .unwrap()
        .opaque_context(
            7,
            9,
            &["current-token-a".to_string(), "current-token-b".to_string()],
        );
    let mut reporter_value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    reporter_value["context"] = serde_json::json!(reporter_context);
    let reporter_body = serde_json::to_vec(&reporter_value).unwrap();
    let reporter_timestamp = (timestamp.parse::<i64>().unwrap() + 1).to_string();
    let reporter_headers = signed_headers(
        "target-reporter-secret",
        &reporter_timestamp,
        7,
        9,
        &reporter_body,
    );
    accept_observation(&pool, 7, 9, &reporter_headers, &reporter_body)
        .await
        .unwrap();
    let reporter_was_used: bool = sqlx::query_scalar(
        r#"SELECT last_used_at IS NOT NULL FROM "KothTargetReporters"
                WHERE cycle_id = 41"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(reporter_was_used);

    sqlx::query(r#"UPDATE "KothCrownCycles" SET reset_attempt = 3 WHERE id = 41"#)
        .execute(&pool)
        .await
        .unwrap();
    let stale_reporter_timestamp = (timestamp.parse::<i64>().unwrap() + 2).to_string();
    let stale_reporter_headers = signed_headers(
        "target-reporter-secret",
        &stale_reporter_timestamp,
        7,
        9,
        &body,
    );
    let stale_reporter = accept_observation(&pool, 7, 9, &stale_reporter_headers, &body)
        .await
        .unwrap_err();
    assert_eq!(
        stale_reporter.status(),
        axum::http::StatusCode::UNAUTHORIZED
    );
    sqlx::query(r#"UPDATE "KothCrownCycles" SET reset_attempt = 2 WHERE id = 41"#)
        .execute(&pool)
        .await
        .unwrap();
    let frozen_scheme: (i16, Vec<String>, Vec<u8>) = sqlx::query_as(
        r#"SELECT objective_count, objective_ids, objective_schema_hash
                 FROM "KothApiArenaSchemes""#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(frozen_scheme.0, 2);
    assert_eq!(frozen_scheme.1, ["quality", "throughput"]);
    assert_eq!(
        frozen_scheme.2,
        crate::controllers::game::koth::api_contract::objective_schema_hash(&frozen_scheme.1)
    );
    let staged: (String, i32, i64, i64, i16, bool) = sqlx::query_as(
        r#"SELECT wave_id, participation_id, objective_earned, objective_possible,
                      objective_count, is_crown
                 FROM "KothApiSnapshotScores""#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        staged,
        ("heat-17".to_string(), 11, 1_000_000, 2_000_000, 2, true)
    );

    let snapshot_before_retry: DateTime<Utc> =
        sqlx::query_scalar(r#"SELECT accepted_at FROM "KothApiSnapshots" WHERE target_id = 3"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    let replay = accept_observation(&pool, 7, 9, &headers, &body)
        .await
        .unwrap();
    assert_eq!(replay.accepted_at, accepted.accepted_at);
    let retry_timestamp = (timestamp.parse::<i64>().unwrap() + 10).to_string();
    let retry_headers = signed_headers("target-reporter-secret", &retry_timestamp, 7, 9, &body);
    let lost_response_retry = accept_observation(&pool, 7, 9, &retry_headers, &body)
        .await
        .unwrap();
    assert_eq!(lost_response_retry.accepted_at, accepted.accepted_at);
    let snapshot_after_retry: DateTime<Utc> =
        sqlx::query_scalar(r#"SELECT accepted_at FROM "KothApiSnapshots" WHERE target_id = 3"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(snapshot_after_retry, snapshot_before_retry);

    let rewritten_body = serde_json::to_vec(&serde_json::json!({
        "context": context,
        "objectiveIds": ["quality", "throughput"],
        "waves": [{
            "waveId": "heat-17",
            "endedAtUnixMs": ended_at,
            "teams": [{
                "tokenHash": valid_hash,
                "activity": {"earned": 1, "possible": 1},
                "objectives": [
                    {"earned": 2, "possible": 10},
                    {"earned": 900, "possible": 1000}
                ],
                "isCrown": true
            }]
        }]
    }))
    .unwrap();
    let rewritten_timestamp = (timestamp.parse::<i64>().unwrap() + 1).to_string();
    let rewritten_headers = signed_headers(
        "observer-secret",
        &rewritten_timestamp,
        7,
        9,
        &rewritten_body,
    );
    let rewritten = accept_observation(&pool, 7, 9, &rewritten_headers, &rewritten_body)
        .await
        .unwrap_err();
    assert_eq!(rewritten.status(), axum::http::StatusCode::CONFLICT);

    // Rotating one player's event capability changes the opaque context.
    // The old reporter fence is rejected, but the new context cannot rewrite
    // another player's finalized evidence in this scoring round.
    sqlx::query(
        r#"UPDATE "KothApiTeamTokens"
                  SET token = 'rotated-token-b'
                WHERE game_id = 7 AND challenge_id = 9
                  AND participation_id = 12"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let stale_context_timestamp = (timestamp.parse::<i64>().unwrap() + 2).to_string();
    let stale_context_headers =
        signed_headers("observer-secret", &stale_context_timestamp, 7, 9, &body);
    let stale_context = accept_observation(&pool, 7, 9, &stale_context_headers, &body)
        .await
        .unwrap_err();
    assert_eq!(stale_context.status(), axum::http::StatusCode::CONFLICT);
    let rotated_context = load_active_context(&pool, 7, 9)
        .await
        .unwrap()
        .unwrap()
        .opaque_context(
            7,
            9,
            &["current-token-a".to_string(), "rotated-token-b".to_string()],
        );
    let rotated_body = serde_json::to_vec(&serde_json::json!({
        "context": rotated_context,
        "objectiveIds": ["quality", "throughput"],
        "waves": [{
            "waveId": "heat-17",
            "endedAtUnixMs": ended_at,
            "teams": [{
                "tokenHash": valid_hash,
                "activity": {"earned": 1, "possible": 1},
                "objectives": [
                    {"earned": 1, "possible": 10},
                    {"earned": 900, "possible": 1000}
                ],
                "isCrown": true
            }]
        }]
    }))
    .unwrap();
    let rotated_timestamp = (timestamp.parse::<i64>().unwrap() + 3).to_string();
    let mut tampered_rotated: serde_json::Value = serde_json::from_slice(&rotated_body).unwrap();
    tampered_rotated["waves"][0]["teams"][0]["objectives"][0]["earned"] = serde_json::json!(2);
    let tampered_rotated = serde_json::to_vec(&tampered_rotated).unwrap();
    let tampered_headers = signed_headers(
        "observer-secret",
        &rotated_timestamp,
        7,
        9,
        &tampered_rotated,
    );
    let tampered = accept_observation(&pool, 7, 9, &tampered_headers, &tampered_rotated)
        .await
        .unwrap_err();
    assert_eq!(tampered.status(), axum::http::StatusCode::CONFLICT);

    let rotated_timestamp = (timestamp.parse::<i64>().unwrap() + 4).to_string();
    let rotated_headers =
        signed_headers("observer-secret", &rotated_timestamp, 7, 9, &rotated_body);
    assert!(
        accept_observation(&pool, 7, 9, &rotated_headers, &rotated_body)
            .await
            .is_ok()
    );

    let future_body = serde_json::to_vec(&serde_json::json!({
        "context": context,
        "objectiveIds": ["quality", "throughput"],
        "waves": [{
            "waveId": "heat-from-the-future",
            "endedAtUnixMs": Utc::now().timestamp_millis() + 30_000,
            "teams": [{
                "tokenHash": valid_hash,
                "activity": {"earned": 1, "possible": 1},
                "objectives": [
                    {"earned": 1, "possible": 1},
                    {"earned": 1, "possible": 1}
                ],
                "isCrown": true
            }]
        }]
    }))
    .unwrap();
    let future_timestamp = Utc::now().timestamp_millis().to_string();
    let future_headers = signed_headers("observer-secret", &future_timestamp, 7, 9, &future_body);
    let future = accept_observation(&pool, 7, 9, &future_headers, &future_body)
        .await
        .unwrap_err();
    assert_eq!(future.status(), axum::http::StatusCode::CONFLICT);

    // The frozen scoring dimensions belong to the challenge, not the
    // credential. Rotating a compatibility credential cannot change them.
    sqlx::raw_sql(
        r#"DELETE FROM "KothApiObservers";
               INSERT INTO "KothApiObservers" VALUES
                 (9, 7, 'observer-secret-rotated', NULL);
               UPDATE "KothApiObserverRevisions"
                  SET revision = revision + 1
                WHERE game_id = 7 AND challenge_id = 9;"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let current_context = load_active_context(&pool, 7, 9)
        .await
        .unwrap()
        .unwrap()
        .opaque_context(
            7,
            9,
            &["current-token-a".to_string(), "rotated-token-b".to_string()],
        );
    assert_ne!(current_context, rotated_context);
    let changed_scheme_body = serde_json::to_vec(&serde_json::json!({
        "context": current_context,
        "objectiveIds": ["quality"],
        "waves": [{
            "waveId":"heat-17",
            "endedAtUnixMs":ended_at,
            "teams": [{
                "tokenHash": valid_hash,
                "activity": {"earned": 1, "possible": 1},
                "objectives": [{"earned": 1, "possible": 1}],
                "isCrown":true
            }]
        }]
    }))
    .unwrap();
    let changed_timestamp = (timestamp.parse::<i64>().unwrap() + 1).to_string();
    let changed_headers = signed_headers(
        "observer-secret-rotated",
        &changed_timestamp,
        7,
        9,
        &changed_scheme_body,
    );
    let changed = accept_observation(&pool, 7, 9, &changed_headers, &changed_scheme_body)
        .await
        .unwrap_err();
    assert_eq!(changed.status(), axum::http::StatusCode::CONFLICT);

    let older_timestamp = (timestamp.parse::<i64>().unwrap() - 1).to_string();
    let older_headers = signed_headers("observer-secret-rotated", &older_timestamp, 7, 9, &body);
    let older = accept_observation(&pool, 7, 9, &older_headers, &body)
        .await
        .unwrap_err();
    assert_eq!(older.status(), axum::http::StatusCode::CONFLICT);

    sqlx::raw_sql(
        r#"UPDATE "AdRounds" SET finalized = TRUE WHERE id = 51;
           INSERT INTO "AdRounds" VALUES
             (52, 7, 6, clock_timestamp() - interval '5 seconds',
              clock_timestamp() + interval '1 minute', FALSE);"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let later_context = load_active_context(&pool, 7, 9).await.unwrap().unwrap();
    assert_eq!(later_context.round_number, 6);
    assert_eq!(later_context.scoring_starts_at, official_scoring_start);

    sqlx::query(
        r#"UPDATE "Games"
              SET start_time_utc = clock_timestamp() + interval '1 minute'
            WHERE id = 7"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(load_active_context(&pool, 7, 9).await.unwrap().is_none());
}
