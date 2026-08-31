use std::str::FromStr;

use chrono::{Duration, Utc};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::super::super::{
    ActiveObserverContext, KothObserverContextModel, OBSERVER_CONTEXT_MAX_BYTES,
};
use super::super::*;
use crate::controllers::game::koth::api_contract::MAX_TEAM_ENTRIES;
use crate::utils::enums::{ParticipationStatus, Role};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn maximum_body_and_roster_remain_bounded_before_snapshot_work() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_koth_max_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();

    sqlx::raw_sql(
        r#"CREATE TABLE "AspNetUsers" (
             id UUID PRIMARY KEY, role SMALLINT NOT NULL
           );
           CREATE TABLE "Teams" (
             id INTEGER PRIMARY KEY, captain_id UUID NOT NULL,
             deletion_pending BOOLEAN NOT NULL
           );
           CREATE TABLE "TeamMembers" (
             team_id INTEGER NOT NULL, user_id UUID NOT NULL
           );
           CREATE TABLE "Participations" (
             id INTEGER PRIMARY KEY, status SMALLINT NOT NULL,
             game_id INTEGER NOT NULL, team_id INTEGER NOT NULL
           );
           CREATE TABLE "KothOfficialConfigs" (
             game_id INTEGER PRIMARY KEY, roster_snapshot JSONB NOT NULL
           );
           CREATE TABLE "KothApiTeamTokens" (
             game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
             participation_id INTEGER NOT NULL, token TEXT NOT NULL
           );
           CREATE TABLE "KothApiObservationOperations" (
             challenge_id INTEGER NOT NULL, game_id INTEGER NOT NULL,
             request_digest BYTEA NOT NULL, signer_scope TEXT NOT NULL,
             body_digest BYTEA NOT NULL, context_hash CHAR(64) NOT NULL,
             lease_token UUID NOT NULL, lease_expires_at TIMESTAMPTZ NOT NULL,
             response JSONB NULL,
             created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
             completed_at TIMESTAMPTZ NULL,
             expires_at TIMESTAMPTZ NOT NULL
               DEFAULT (clock_timestamp() + interval '10 minutes'),
             PRIMARY KEY (challenge_id, request_digest)
           );

           INSERT INTO "AspNetUsers" VALUES
             ('00000000-0000-4000-8000-000000000251', 1);
           INSERT INTO "Teams"
           SELECT value, '00000000-0000-4000-8000-000000000251', FALSE
             FROM generate_series(1, 2000) value;
           INSERT INTO "KothOfficialConfigs"
           SELECT 7, jsonb_agg(value ORDER BY value)
             FROM generate_series(1, 2000) value;
           INSERT INTO "KothApiTeamTokens"
           SELECT 7, 9, value, 'koth_max_' || value::text
             FROM generate_series(1, 2000) value;"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations"
           SELECT value, $1, 7, value FROM generate_series(1, 2000) value"#,
    )
    .bind(ParticipationStatus::Accepted as i16)
    .execute(&pool)
    .await
    .unwrap();
    let accounts_with_wrong_role: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "AspNetUsers" WHERE role = $1"#)
            .bind(Role::Banned as i16)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(accounts_with_wrong_role, 0);

    let eligible_capabilities = super::super::super::load_eligible_capabilities(&pool, 7, 9)
        .await
        .unwrap();
    let eligible_tokens: Vec<_> = eligible_capabilities
        .into_iter()
        .map(|capability| capability.token)
        .collect();
    assert_eq!(eligible_tokens.len(), MAX_TEAM_ENTRIES);
    assert_eq!(eligible_tokens.first().unwrap(), "koth_max_1");
    assert_eq!(eligible_tokens.last().unwrap(), "koth_max_2000");

    let now = Utc::now();
    let active = ActiveObserverContext {
        target_id: 3,
        cycle_id: 41,
        cycle_number: 4,
        reset_attempt: 1,
        reporting_revision: 2,
        container_id: "runtime-max".into(),
        round_id: 17,
        round_number: 17,
        scoring_starts_at: now - Duration::hours(1),
        cycle_ends_at: now + Duration::hours(1),
        scoring_ends_at: now + Duration::minutes(59),
        round_starts_at: now - Duration::minutes(1),
        round_ends_at: now + Duration::minutes(1),
        objective_ids: None,
        objective_schema_hash: None,
    };
    let context = active.opaque_context(7, 9, &eligible_tokens);
    let (wave_window_starts_at, wave_window_ends_at) = active.wave_window();
    let context_body = serde_json::to_vec(&KothObserverContextModel {
        api_version: "v1",
        context: context.clone(),
        cycle_number: active.cycle_number,
        reset_attempt: active.reset_attempt,
        round_number: active.round_number,
        cycle_ends_at: active.cycle_ends_at,
        wave_window_starts_at,
        wave_window_ends_at,
        eligible_token_hashes: eligible_tokens
            .iter()
            .map(|token| crate::services::ad::koth_api_capability::token_hash_hex(token))
            .collect(),
        objective_ids: Vec::new(),
        objective_schema_hash: None,
        generated_at: now,
    })
    .unwrap();
    assert!(context_body.len() <= OBSERVER_CONTEXT_MAX_BYTES);

    let teams = (1..=MAX_TEAM_ENTRIES)
        .map(|index| {
            serde_json::json!({
                "tokenHash": format!("{index:064x}"),
                "activity": {"earned": 1, "possible": 1},
                "objectives": [{"earned": 1, "possible": 1}],
                "isCrown": index == 1,
            })
        })
        .collect::<Vec<_>>();
    let mut body = serde_json::to_vec(&serde_json::json!({
        "context": context,
        "objectiveIds": ["quality"],
        "waves": [{
            "waveId": "maximum-roster",
            "endedAtUnixMs": now.timestamp_millis(),
            "teams": teams,
        }],
    }))
    .unwrap();
    assert!(body.len() <= MAX_BODY_BYTES);
    body.resize(MAX_BODY_BYTES, b' ');
    let input = parse_and_normalize(&body).unwrap();
    assert_eq!(
        input
            .waves
            .iter()
            .map(|wave| wave.teams.len())
            .sum::<usize>(),
        MAX_TEAM_ENTRIES
    );
    let weight = observation_weight(body.len(), &input);
    assert!(weight <= OBSERVATION_SIGNER_WEIGHT);
    let admission = WeightedAdmission::new(OBSERVATION_GLOBAL_WEIGHT);
    let _permit = admission
        .try_acquire(
            "observation:observer:7:9".into(),
            weight,
            OBSERVATION_SIGNER_WEIGHT,
        )
        .expect("one maximum request must fit the configured weighted admission bound");

    let body_digest = canonical_input_digest(&input);
    let request_digest = observation_request_digest(7, 9, "observer:7:9", &body_digest);
    let reserved = reserve_observation(
        &pool,
        7,
        9,
        "observer:7:9",
        &input.context,
        body_digest,
        request_digest,
    )
    .await
    .unwrap();
    let ObservationReservationResult::Owner(reservation) = reserved else {
        panic!("maximum request must reserve exactly one durable operation");
    };
    release_observation(&pool, 9, &reservation).await;

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
