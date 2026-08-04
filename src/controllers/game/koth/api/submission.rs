use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, QueryBuilder};
use std::collections::HashMap;

use crate::app_state::SharedState;
use crate::controllers::game::koth::api_contract::{
    flatten, parse_and_normalize, NormalizedInputRow, MAX_BODY_BYTES,
};
use crate::utils::enums::{ChallengeType, ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

use super::{
    load_active_context, parse_signature, parse_timestamp, verify_signature,
    KothObservationAcceptedModel, INSERT_REPLAY_SQL,
};

#[derive(Clone, Debug, sqlx::FromRow)]
struct ResolvedInputRow {
    participation_id: i32,
    activity_earned: i64,
    activity_possible: i64,
    objective_earned: i64,
    objective_possible: i64,
    objective_count: i16,
}

#[derive(sqlx::FromRow)]
struct CurrentCapabilityRow {
    participation_id: i32,
    token: String,
}

fn snapshot_hash(
    context: &str,
    objective_schema_hash: &[u8; 32],
    rows: &[ResolvedInputRow],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(context.as_bytes());
    digest.update(objective_schema_hash);
    digest.update((rows.len() as u64).to_be_bytes());
    for row in rows {
        digest.update(row.participation_id.to_be_bytes());
        digest.update(row.activity_earned.to_be_bytes());
        digest.update(row.activity_possible.to_be_bytes());
        digest.update(row.objective_earned.to_be_bytes());
        digest.update(row.objective_possible.to_be_bytes());
        digest.update(row.objective_count.to_be_bytes());
    }
    digest.finalize().into()
}

async fn resolve_current_capabilities(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    rows: Vec<NormalizedInputRow>,
) -> AppResult<Vec<ResolvedInputRow>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut submitted: HashMap<_, _> = rows.into_iter().map(|row| (row.token_hash, row)).collect();
    let capabilities = sqlx::query_as::<_, CurrentCapabilityRow>(
        r#"SELECT capability.participation_id, capability.token
             FROM "KothApiTeamTokens" capability
             JOIN "Participations" participation
               ON participation.id = capability.participation_id
              AND participation.game_id = $1
              AND participation.status = $3
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "KothOfficialConfigs" config ON config.game_id = $1
             JOIN LATERAL jsonb_array_elements(config.roster_snapshot) roster(item)
               ON participation.id = CASE jsonb_typeof(roster.item)
                    WHEN 'number' THEN (roster.item #>> '{}')::integer
                    WHEN 'object' THEN
                      NULLIF(roster.item->>'participationId', '')::integer
                    ELSE NULL
                  END
            WHERE capability.game_id = $1
              AND capability.challenge_id = $2
              AND NOT team.deletion_pending
              AND NOT EXISTS (
                    SELECT 1
                      FROM (
                          SELECT team.captain_id AS user_id
                          UNION
                          SELECT member.user_id
                            FROM "TeamMembers" member
                           WHERE member.team_id = team.id
                      ) roster_member
                      LEFT JOIN "AspNetUsers" account
                        ON account.id = roster_member.user_id
                     WHERE account.id IS NULL OR account.role = $4
              )
            ORDER BY capability.participation_id
            FOR SHARE OF capability"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut resolved = Vec::with_capacity(submitted.len().min(capabilities.len()));
    for capability in capabilities {
        let token_hash = crate::services::ad::koth_api_capability::token_hash(&capability.token);
        if let Some(row) = submitted.remove(&token_hash) {
            resolved.push(ResolvedInputRow {
                participation_id: capability.participation_id,
                activity_earned: row.activity_earned,
                activity_possible: row.activity_possible,
                objective_earned: row.objective_earned,
                objective_possible: row.objective_possible,
                objective_count: row.objective_count,
            });
        }
    }
    Ok(resolved)
}

async fn replace_snapshot_rows(
    connection: &mut sqlx::PgConnection,
    target_id: i32,
    rows: &[ResolvedInputRow],
) -> AppResult<()> {
    sqlx::query(r#"DELETE FROM "KothApiSnapshotScores" WHERE target_id = $1"#)
        .bind(target_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut query = QueryBuilder::<Postgres>::new(
        r#"INSERT INTO "KothApiSnapshotScores"
           (target_id, participation_id, activity_earned, activity_possible,
            objective_earned, objective_possible, objective_count) "#,
    );
    query.push_values(rows, |mut values, row| {
        values
            .push_bind(target_id)
            .push_bind(row.participation_id)
            .push_bind(row.activity_earned)
            .push_bind(row.activity_possible)
            .push_bind(row.objective_earned)
            .push_bind(row.objective_possible)
            .push_bind(row.objective_count);
    });
    query
        .build()
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn record_replay(
    connection: &mut sqlx::PgConnection,
    challenge_id: i32,
    signature: &[u8; 32],
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "KothApiRequestReplays"
            WHERE request_hash IN (
              SELECT request_hash FROM "KothApiRequestReplays"
               WHERE expires_at < clock_timestamp()
               ORDER BY expires_at
               LIMIT 128
            )"#,
    )
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let request_hash: [u8; 32] = Sha256::digest(signature).into();
    let inserted = sqlx::query(INSERT_REPLAY_SQL)
        .bind(request_hash.as_slice())
        .bind(challenge_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
    if inserted != 1 {
        return Err(AppError::conflict(
            "Leaderboard referee request was already accepted",
        ));
    }
    Ok(())
}

/// Store normalized current-tick input only. The checker still brackets it
/// around an independent functional probe before any durable score is written.
pub async fn submit_observation(
    State(st): State<SharedState>,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<RequestResponse<KothObservationAcceptedModel>> {
    Ok(RequestResponse::ok(
        accept_observation(st.pg(), game_id, challenge_id, &headers, &body).await?,
    ))
}

async fn accept_observation(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
    headers: &HeaderMap,
    body: &[u8],
) -> AppResult<KothObservationAcceptedModel> {
    if body.len() > MAX_BODY_BYTES {
        return Err(AppError::payload_too_large(
            "Leaderboard snapshot body must be at most 512 KiB",
        ));
    }
    let now = Utc::now();
    let (timestamp, timestamp_raw) = parse_timestamp(headers, now.timestamp_millis())?;
    let signature = parse_signature(headers)?;
    let secret: Option<String> = sqlx::query_scalar(
        r#"SELECT observer.hmac_secret
             FROM "KothApiObservers" observer
             JOIN "GameChallenges" challenge
               ON challenge.game_id = observer.game_id
              AND challenge.id = observer.challenge_id
              AND challenge."Type" = $3
            WHERE observer.game_id = $1 AND observer.challenge_id = $2
            "#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let secret = secret.ok_or(AppError::Unauthorized)?;
    verify_signature(
        &secret,
        timestamp_raw,
        game_id,
        challenge_id,
        body,
        &signature,
    )?;
    let input = parse_and_normalize(body)?;
    let submitted_teams = input.teams.len();
    let objective_ids = input.objective_ids.clone();
    let objective_count = objective_ids.len() as i16;
    let objective_schema_hash = input.objective_schema_hash();
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let locked_observer: Option<String> = sqlx::query_scalar(
        r#"SELECT observer.hmac_secret
             FROM "KothApiObservers" observer
             JOIN "GameChallenges" challenge
               ON challenge.game_id = observer.game_id
              AND challenge.id = observer.challenge_id
              AND challenge."Type" = $3
            WHERE observer.game_id = $1 AND observer.challenge_id = $2
            FOR UPDATE OF observer"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(locked_secret) = locked_observer else {
        return Err(AppError::Unauthorized);
    };
    if locked_secret != secret {
        return Err(AppError::Unauthorized);
    }
    let context = load_active_context(&mut *transaction, game_id, challenge_id)
        .await?
        .ok_or_else(|| AppError::conflict("Leaderboard KotH context is not active"))?;
    if input.context != context.opaque_context(game_id, challenge_id) {
        return Err(AppError::conflict(
            "Leaderboard KotH context changed; fetch context and retry",
        ));
    }
    let frozen_scheme = sqlx::query_as::<_, (Vec<String>, Vec<u8>)>(
        r#"SELECT objective_ids, objective_schema_hash
             FROM "KothApiArenaSchemes"
            WHERE game_id = $1 AND challenge_id = $2
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some((stored_ids, stored_hash)) = frozen_scheme {
        if stored_ids != objective_ids || stored_hash.as_slice() != objective_schema_hash {
            return Err(AppError::conflict(
                "Leaderboard objective IDs and order are frozen for this challenge",
            ));
        }
    } else {
        sqlx::query(
            r#"INSERT INTO "KothApiArenaSchemes"
                 (challenge_id, game_id, objective_count, objective_ids,
                  objective_schema_hash)
               VALUES ($2, $1, $3, $4, $5)
               ON CONFLICT (challenge_id) DO NOTHING"#,
        )
        .bind(game_id)
        .bind(challenge_id)
        .bind(objective_count)
        .bind(&objective_ids)
        .bind(objective_schema_hash.as_slice())
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let stored = sqlx::query_as::<_, (Vec<String>, Vec<u8>)>(
            r#"SELECT objective_ids, objective_schema_hash
                 FROM "KothApiArenaSchemes"
                WHERE game_id = $1 AND challenge_id = $2
                FOR UPDATE"#,
        )
        .bind(game_id)
        .bind(challenge_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if stored.0 != objective_ids || stored.1.as_slice() != objective_schema_hash {
            return Err(AppError::conflict(
                "Leaderboard objective IDs and order are frozen for this challenge",
            ));
        }
    }
    let rows = resolve_current_capabilities(
        &mut transaction,
        game_id,
        challenge_id,
        input
            .teams
            .into_iter()
            .map(|team| flatten(team, objective_count as usize))
            .collect(),
    )
    .await?;
    let digest = snapshot_hash(&input.context, &objective_schema_hash, &rows);
    record_replay(&mut transaction, challenge_id, &signature).await?;

    let accepted_at = sqlx::query_scalar::<_, DateTime<Utc>>(
        r#"INSERT INTO "KothApiSnapshots"
             (target_id, game_id, challenge_id, cycle_id, reset_attempt,
              container_id, ad_round_id, context_hash, snapshot_hash,
              objective_schema_hash, request_timestamp_ms, accepted_at)
           SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,clock_timestamp()
            WHERE clock_timestamp() < $12
           ON CONFLICT (target_id) DO UPDATE SET
             game_id = EXCLUDED.game_id,
             challenge_id = EXCLUDED.challenge_id,
             cycle_id = EXCLUDED.cycle_id,
             reset_attempt = EXCLUDED.reset_attempt,
             container_id = EXCLUDED.container_id,
             ad_round_id = EXCLUDED.ad_round_id,
             context_hash = EXCLUDED.context_hash,
             snapshot_hash = EXCLUDED.snapshot_hash,
             objective_schema_hash = EXCLUDED.objective_schema_hash,
             request_timestamp_ms = EXCLUDED.request_timestamp_ms,
             accepted_at = EXCLUDED.accepted_at
           WHERE "KothApiSnapshots".ad_round_id <> EXCLUDED.ad_round_id
              OR "KothApiSnapshots".cycle_id <> EXCLUDED.cycle_id
              OR "KothApiSnapshots".reset_attempt <> EXCLUDED.reset_attempt
              OR "KothApiSnapshots".container_id <> EXCLUDED.container_id
              OR "KothApiSnapshots".request_timestamp_ms
                   < EXCLUDED.request_timestamp_ms
           RETURNING accepted_at"#,
    )
    .bind(context.target_id)
    .bind(game_id)
    .bind(challenge_id)
    .bind(context.cycle_id)
    .bind(context.reset_attempt)
    .bind(&context.container_id)
    .bind(context.round_id)
    .bind(&input.context)
    .bind(digest.as_slice())
    .bind(objective_schema_hash.as_slice())
    .bind(timestamp)
    .bind(context.round_ends_at)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| {
        AppError::conflict("KotH arena snapshot is late or older than the accepted snapshot")
    })?;
    replace_snapshot_rows(&mut transaction, context.target_id, &rows).await?;
    sqlx::query(
        r#"UPDATE "KothApiObservers"
              SET last_used_at = clock_timestamp()
            WHERE game_id = $1 AND challenge_id = $2
              AND (last_used_at IS NULL
                   OR last_used_at < clock_timestamp() - interval '30 seconds')"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(KothObservationAcceptedModel {
        accepted: true,
        cycle_number: context.cycle_number,
        reset_attempt: context.reset_attempt,
        round_number: context.round_number,
        submitted_teams,
        recognized_teams: rows.len(),
        accepted_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use hmac::{Hmac, KeyInit, Mac};
    use sqlx::postgres::PgPoolOptions;

    fn row(participation_id: i32, earned: i64) -> ResolvedInputRow {
        ResolvedInputRow {
            participation_id,
            activity_earned: earned,
            activity_possible: 10,
            objective_earned: earned * 2,
            objective_possible: 20,
            objective_count: 2,
        }
    }

    #[test]
    fn snapshot_digest_binds_resolved_identity_and_every_budget() {
        let schema = [7; 32];
        let base = snapshot_hash("context", &schema, &[row(1, 5), row(2, 6)]);
        assert_ne!(
            base,
            snapshot_hash("other", &schema, &[row(1, 5), row(2, 6)])
        );
        assert_ne!(
            base,
            snapshot_hash("context", &[8; 32], &[row(1, 5), row(2, 6)])
        );
        assert_ne!(base, snapshot_hash("context", &schema, &[row(1, 5)]));
        assert_ne!(
            base,
            snapshot_hash("context", &schema, &[row(1, 6), row(2, 6)])
        );
        assert_ne!(
            base,
            snapshot_hash("context", &schema, &[row(2, 6), row(1, 5)])
        );
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
              hills_snapshot JSONB
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
              participation_id INTEGER, token TEXT
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
              target_id INTEGER, participation_id INTEGER,
              activity_earned BIGINT, activity_possible BIGINT,
              objective_earned BIGINT, objective_possible BIGINT,
              objective_count SMALLINT
            );
            CREATE TEMP TABLE "KothApiRequestReplays" (
              request_hash BYTEA PRIMARY KEY, challenge_id INTEGER,
              expires_at TIMESTAMPTZ
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(
            r#"INSERT INTO "Games" VALUES
                 (7, clock_timestamp() - interval '1 minute',
                     clock_timestamp() + interval '1 hour');
               INSERT INTO "KothOfficialConfigs" VALUES
                 (7, '[11,12]', '[{"challengeId":9,"claimSource":"Api"}]');
               INSERT INTO "KothTargets" VALUES (3, 7, 9, 'runtime-a');
               INSERT INTO "KothCrownCycles" VALUES
                 (41, 7, 9, 4, 2, 'runtime-a', 1, 3, 'Active');
               INSERT INTO "AdRounds" VALUES
                 (51, 7, 2, clock_timestamp() - interval '10 seconds',
                  clock_timestamp() + interval '1 minute', FALSE);
               INSERT INTO "KothApiObservers" VALUES
                 (9, 7, 'observer-secret', NULL);
               INSERT INTO "AspNetUsers" VALUES (101, 1), (102, 1);
               INSERT INTO "Teams" VALUES (21, 101, FALSE), (22, 102, FALSE);
               INSERT INTO "KothApiTeamTokens" VALUES
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

        let context = load_active_context(&pool, 7, 9)
            .await
            .unwrap()
            .unwrap()
            .opaque_context(7, 9);
        let valid_hash = hex::encode(Sha256::digest(b"current-token-a"));
        let unknown_hash = hex::encode(Sha256::digest(b"stale-token"));
        let body = serde_json::to_vec(&serde_json::json!({
            "context": context,
            "objectiveIds": ["quality", "throughput"],
            "teams": [
                {
                    "tokenHash": valid_hash,
                    "activity": {"earned": 4, "possible": 5},
                    "objectives": [
                        {"earned": 1, "possible": 10},
                        {"earned": 900, "possible": 1000}
                    ]
                },
                {
                    "tokenHash": unknown_hash,
                    "activity": {"earned": 1, "possible": 1},
                    "objectives": [
                        {"earned": 1, "possible": 1},
                        {"earned": 1, "possible": 1}
                    ]
                }
            ]
        }))
        .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("current-token-a"));
        let timestamp = Utc::now().timestamp_millis().to_string();
        let headers = signed_headers("observer-secret", &timestamp, 7, 9, &body);
        let accepted = accept_observation(&pool, 7, 9, &headers, &body)
            .await
            .unwrap();
        assert_eq!(
            (accepted.submitted_teams, accepted.recognized_teams),
            (2, 1)
        );
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
        let staged: (i32, i64, i64, i16) = sqlx::query_as(
            r#"SELECT participation_id, objective_earned, objective_possible,
                      objective_count
                 FROM "KothApiSnapshotScores""#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(staged, (11, 1_000_000, 2_000_000, 2));

        let replay = accept_observation(&pool, 7, 9, &headers, &body)
            .await
            .unwrap_err();
        assert_eq!(replay.status(), axum::http::StatusCode::CONFLICT);

        // The frozen scoring dimensions belong to the challenge, not the
        // credential. Revoking and recreating the referee cannot change them.
        sqlx::raw_sql(
            r#"DELETE FROM "KothApiObservers";
               INSERT INTO "KothApiObservers" VALUES
                 (9, 7, 'observer-secret-rotated', NULL);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let current_context = load_active_context(&pool, 7, 9)
            .await
            .unwrap()
            .unwrap()
            .opaque_context(7, 9);
        let changed_scheme_body = serde_json::to_vec(&serde_json::json!({
            "context": current_context,
            "objectiveIds": ["quality"],
            "teams": [{
                "tokenHash": valid_hash,
                "activity": {"earned": 1, "possible": 1},
                "objectives": [{"earned": 1, "possible": 1}]
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
        let older_headers =
            signed_headers("observer-secret-rotated", &older_timestamp, 7, 9, &body);
        let older = accept_observation(&pool, 7, 9, &older_headers, &body)
            .await
            .unwrap_err();
        assert_eq!(older.status(), axum::http::StatusCode::CONFLICT);
    }
}
