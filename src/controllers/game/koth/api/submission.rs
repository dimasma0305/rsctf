use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, QueryBuilder};

use crate::app_state::SharedState;
use crate::controllers::game::koth::api_contract::{
    flatten_waves, parse_and_normalize, MAX_BODY_BYTES,
};
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

use super::{
    load_active_context, parse_signature, parse_timestamp, verify_signature,
    KothObservationAcceptedModel, INSERT_REPLAY_SQL,
};

mod evidence;

use evidence::{
    ensure_finalized_waves_are_append_only, load_stored_waves, resolve_current_capabilities,
    validate_resolved_crowns, ResolvedWave,
};

fn snapshot_hash(
    context: &str,
    objective_schema_hash: &[u8; 32],
    waves: &[ResolvedWave],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(context.as_bytes());
    digest.update(objective_schema_hash);
    digest.update((waves.len() as u64).to_be_bytes());
    for wave in waves {
        digest.update((wave.wave_id.len() as u64).to_be_bytes());
        digest.update(wave.wave_id.as_bytes());
        digest.update(wave.ended_at.timestamp_millis().to_be_bytes());
        digest.update((wave.rows.len() as u64).to_be_bytes());
        for row in &wave.rows {
            digest.update(row.participation_id.to_be_bytes());
            digest.update(row.activity_earned.to_be_bytes());
            digest.update(row.activity_possible.to_be_bytes());
            digest.update(row.objective_earned.to_be_bytes());
            digest.update(row.objective_possible.to_be_bytes());
            digest.update(row.objective_count.to_be_bytes());
            digest.update([u8::from(row.is_crown)]);
        }
    }
    digest.finalize().into()
}

async fn replace_snapshot_rows(
    connection: &mut sqlx::PgConnection,
    target_id: i32,
    waves: &[ResolvedWave],
) -> AppResult<()> {
    sqlx::query(r#"DELETE FROM "KothApiSnapshotScores" WHERE target_id = $1"#)
        .bind(target_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "KothApiSnapshotWaves" WHERE target_id = $1"#)
        .bind(target_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if waves.is_empty() {
        return Ok(());
    }
    let mut wave_query = QueryBuilder::<Postgres>::new(
        r#"INSERT INTO "KothApiSnapshotWaves" (target_id, wave_id, ended_at) "#,
    );
    wave_query.push_values(waves, |mut values, wave| {
        values
            .push_bind(target_id)
            .push_bind(&wave.wave_id)
            .push_bind(wave.ended_at);
    });
    wave_query
        .build()
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let rows: Vec<_> = waves
        .iter()
        .flat_map(|wave| wave.rows.iter().map(move |row| (&wave.wave_id, row)))
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    let mut query = QueryBuilder::<Postgres>::new(
        r#"INSERT INTO "KothApiSnapshotScores"
           (target_id, wave_id, participation_id,
            activity_earned, activity_possible,
            objective_earned, objective_possible, objective_count, is_crown) "#,
    );
    query.push_values(rows, |mut values, (wave_id, row)| {
        values
            .push_bind(target_id)
            .push_bind(wave_id)
            .push_bind(row.participation_id)
            .push_bind(row.activity_earned)
            .push_bind(row.activity_possible)
            .push_bind(row.objective_earned)
            .push_bind(row.objective_possible)
            .push_bind(row.objective_count)
            .push_bind(row.is_crown);
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
    let submitted_waves = input.waves.len();
    let submitted_team_hashes: std::collections::HashSet<_> = input
        .waves
        .iter()
        .flat_map(|wave| wave.teams.iter().map(|team| team.token_hash.as_str()))
        .collect();
    let submitted_teams = submitted_team_hashes.len();
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
    let eligible_tokens =
        super::load_eligible_tokens(&mut *transaction, game_id, challenge_id).await?;
    if input.context != context.opaque_context(game_id, challenge_id, &eligible_tokens) {
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
    let normalized_waves = flatten_waves(input.waves, objective_count as usize);
    let (wave_window_start, wave_window_end) = context.wave_window();
    for wave in &normalized_waves {
        let Some(ended_at) = DateTime::from_timestamp_millis(wave.ended_at_unix_ms) else {
            return Err(AppError::bad_request(
                "Leaderboard wave timestamp is out of range",
            ));
        };
        if ended_at < wave_window_start || ended_at >= wave_window_end {
            return Err(AppError::conflict(
                "Leaderboard waves must end inside the active settlement window",
            ));
        }
        if ended_at > now {
            return Err(AppError::conflict(
                "Leaderboard waves cannot be finalized in the future",
            ));
        }
    }
    let waves =
        resolve_current_capabilities(&mut transaction, game_id, challenge_id, normalized_waves)
            .await?;
    validate_resolved_crowns(&waves)?;
    if let Some(stored) = load_stored_waves(
        &mut transaction,
        context.target_id,
        context.round_id,
        context.cycle_id,
        context.reset_attempt,
        &context.container_id,
    )
    .await?
    {
        ensure_finalized_waves_are_append_only(&stored, &waves)?;
    }
    let recognized_team_ids: std::collections::HashSet<_> = waves
        .iter()
        .flat_map(|wave| wave.rows.iter().map(|row| row.participation_id))
        .collect();
    let digest = snapshot_hash(&input.context, &objective_schema_hash, &waves);
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
    replace_snapshot_rows(&mut transaction, context.target_id, &waves).await?;
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
        submitted_waves,
        submitted_teams,
        recognized_teams: recognized_team_ids.len(),
        accepted_at,
    })
}

#[cfg(test)]
mod tests {
    use super::evidence::ResolvedInputRow;
    use super::*;
    use axum::http::HeaderValue;
    use hmac::{Hmac, KeyInit, Mac};
    use sqlx::postgres::PgPoolOptions;

    use crate::utils::enums::ParticipationStatus;

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

        let active_context = load_active_context(&pool, 7, 9).await.unwrap().unwrap();
        // Future rounds are created lazily. The observer context must still
        // expose the cycle boundary using only the current authoritative tick.
        assert_eq!(
            active_context.cycle_ends_at - active_context.round_ends_at,
            active_context.round_ends_at - active_context.round_starts_at
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
        let headers = signed_headers("observer-secret", &timestamp, 7, 9, &body);
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

        let replay = accept_observation(&pool, 7, 9, &headers, &body)
            .await
            .unwrap_err();
        assert_eq!(replay.status(), axum::http::StatusCode::CONFLICT);

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
        // The old referee fence is rejected, but the new context cannot rewrite
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
        let mut tampered_rotated: serde_json::Value =
            serde_json::from_slice(&rotated_body).unwrap();
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
        let future_headers =
            signed_headers("observer-secret", &future_timestamp, 7, 9, &future_body);
        let future = accept_observation(&pool, 7, 9, &future_headers, &future_body)
            .await
            .unwrap_err();
        assert_eq!(future.status(), axum::http::StatusCode::CONFLICT);

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
            .opaque_context(
                7,
                9,
                &["current-token-a".to_string(), "rotated-token-b".to_string()],
            );
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
        let older_headers =
            signed_headers("observer-secret-rotated", &older_timestamp, 7, 9, &body);
        let older = accept_observation(&pool, 7, 9, &older_headers, &body)
            .await
            .unwrap_err();
        assert_eq!(older.status(), axum::http::StatusCode::CONFLICT);
    }
}
