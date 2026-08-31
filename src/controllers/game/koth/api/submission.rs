use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, QueryBuilder};

use crate::app_state::SharedState;
use crate::controllers::game::koth::api_contract::{
    flatten_waves, parse_and_normalize, KothArenaSnapshotInput, MAX_BODY_BYTES,
};
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

use super::{
    admission::{referee_database_error, WeightedAdmission},
    load_active_context, parse_signature, parse_timestamp, retry_after_response, verify_signature,
    KothObservationAcceptedModel, INSERT_REPLAY_SQL, STALE_CONTEXT_MESSAGE,
};

mod evidence;
#[cfg(test)]
mod tests;

const OBSERVATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);
const OBSERVATION_LEASE_SECONDS: i64 = 20;
const OBSERVATION_GLOBAL_WEIGHT: usize = 48;
const OBSERVATION_SIGNER_WEIGHT: usize = 12;
const DUPLICATE_WAIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);
static OBSERVATION_ADMISSION: std::sync::LazyLock<WeightedAdmission> =
    std::sync::LazyLock::new(|| WeightedAdmission::new(OBSERVATION_GLOBAL_WEIGHT));
static OBSERVATION_DUPLICATE_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<KothObservationAcceptedModel>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

use evidence::{
    ensure_finalized_waves_are_append_only, load_stored_waves, resolve_current_capabilities,
    validate_resolved_crowns, ResolvedWave,
};

#[derive(Clone, Copy)]
enum SigningCredentialKind {
    Observer,
    TargetReporter { cycle_id: i64, reset_attempt: i32 },
}

struct SigningCredential {
    kind: SigningCredentialKind,
    secret: String,
}

impl SigningCredential {
    fn scope(&self, game_id: i32, challenge_id: i32) -> String {
        match self.kind {
            SigningCredentialKind::Observer => {
                format!("observer:{game_id}:{challenge_id}")
            }
            SigningCredentialKind::TargetReporter {
                cycle_id,
                reset_attempt,
            } => format!("target:{game_id}:{challenge_id}:{cycle_id}:{reset_attempt}"),
        }
    }
}

struct ObservationReservation {
    request_digest: [u8; 32],
    lease_token: uuid::Uuid,
}

enum ObservationReservationResult {
    Owner(ObservationReservation),
    Completed(KothObservationAcceptedModel),
}

struct VerifiedObservation {
    now: DateTime<Utc>,
    timestamp: i64,
    signature: [u8; 32],
    credential: SigningCredential,
    input: KothArenaSnapshotInput,
}

fn canonical_input_digest(input: &KothArenaSnapshotInput) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:koth-observation-body:v2\0");
    digest.update((input.context.len() as u64).to_be_bytes());
    digest.update(input.context.as_bytes());
    digest.update((input.objective_ids.len() as u64).to_be_bytes());
    for objective in &input.objective_ids {
        digest.update((objective.len() as u64).to_be_bytes());
        digest.update(objective.as_bytes());
    }
    let mut waves: Vec<_> = input.waves.iter().collect();
    waves.sort_by(|left, right| {
        left.ended_at_unix_ms
            .cmp(&right.ended_at_unix_ms)
            .then_with(|| left.wave_id.cmp(&right.wave_id))
    });
    digest.update((waves.len() as u64).to_be_bytes());
    for wave in waves {
        digest.update((wave.wave_id.len() as u64).to_be_bytes());
        digest.update(wave.wave_id.as_bytes());
        digest.update(wave.ended_at_unix_ms.to_be_bytes());
        let mut teams: Vec<_> = wave.teams.iter().collect();
        teams.sort_by(|left, right| left.token_hash.cmp(&right.token_hash));
        digest.update((teams.len() as u64).to_be_bytes());
        for team in teams {
            digest.update(team.token_hash.as_bytes());
            digest.update(team.activity.earned.to_be_bytes());
            digest.update(team.activity.possible.to_be_bytes());
            digest.update((team.objectives.len() as u64).to_be_bytes());
            for objective in &team.objectives {
                digest.update(objective.earned.to_be_bytes());
                digest.update(objective.possible.to_be_bytes());
            }
            digest.update([u8::from(team.is_crown)]);
        }
    }
    digest.finalize().into()
}

fn observation_request_digest(
    game_id: i32,
    challenge_id: i32,
    signer_scope: &str,
    body_digest: &[u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:koth-observation-operation:v2\0");
    digest.update(game_id.to_be_bytes());
    digest.update(challenge_id.to_be_bytes());
    digest.update((signer_scope.len() as u64).to_be_bytes());
    digest.update(signer_scope.as_bytes());
    digest.update(body_digest);
    digest.finalize().into()
}

fn observation_weight(body_len: usize, input: &KothArenaSnapshotInput) -> usize {
    let rows = input
        .waves
        .iter()
        .map(|wave| wave.teams.len())
        .sum::<usize>();
    1 + body_len.div_ceil(128 * 1_024) + rows.div_ceil(500)
}

fn retryable_database_error(error: sqlx::Error) -> AppError {
    referee_database_error(error, "Leaderboard observation work is busy; retry later")
}

async fn completed_observation(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    request_digest: &[u8; 32],
) -> AppResult<Option<KothObservationAcceptedModel>> {
    let response: Option<serde_json::Value> = sqlx::query_scalar(
        r#"SELECT response FROM "KothApiObservationOperations"
            WHERE challenge_id = $1 AND request_digest = $2
              AND response IS NOT NULL
              AND expires_at > clock_timestamp()"#,
    )
    .bind(challenge_id)
    .bind(request_digest.as_slice())
    .fetch_optional(pool)
    .await
    .map_err(retryable_database_error)?;
    response
        .map(|response| {
            serde_json::from_value(response).map_err(|error| AppError::internal(error.to_string()))
        })
        .transpose()
}

async fn wait_for_completed_observation(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    request_digest: [u8; 32],
) -> Option<KothObservationAcceptedModel> {
    // Leave headroom inside the two-second single-flight deadline for every
    // indexed query, including the final cross-replica completion check.
    for delay_ms in [0_u64, 25, 75, 150, 300, 500, 500] {
        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        match completed_observation(pool, challenge_id, &request_digest).await {
            Ok(Some(response)) => return Some(response),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    challenge_id,
                    error = %error,
                    "failed to join an in-flight KotH observation"
                );
                return None;
            }
        }
    }
    None
}

async fn reserve_observation(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
    signer_scope: &str,
    context: &str,
    body_digest: [u8; 32],
    request_digest: [u8; 32],
) -> AppResult<ObservationReservationResult> {
    sqlx::query(
        r#"DELETE FROM "KothApiObservationOperations" operation
            USING (
              SELECT challenge_id, request_digest
                FROM "KothApiObservationOperations"
               WHERE expires_at <= clock_timestamp()
               ORDER BY expires_at
               LIMIT 128 FOR UPDATE SKIP LOCKED
            ) expired
            WHERE operation.challenge_id = expired.challenge_id
              AND operation.request_digest = expired.request_digest"#,
    )
    .execute(pool)
    .await
    .map_err(retryable_database_error)?;

    let lease_token = uuid::Uuid::new_v4();
    let owned: Option<uuid::Uuid> = sqlx::query_scalar(
        r#"INSERT INTO "KothApiObservationOperations"
              (challenge_id, game_id, request_digest, signer_scope,
               body_digest, context_hash, lease_token, lease_expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7,
                    clock_timestamp() + make_interval(secs => $8))
            ON CONFLICT (challenge_id, request_digest) DO UPDATE SET
              signer_scope = EXCLUDED.signer_scope,
              body_digest = EXCLUDED.body_digest,
              context_hash = EXCLUDED.context_hash,
              lease_token = EXCLUDED.lease_token,
              lease_expires_at = EXCLUDED.lease_expires_at,
              created_at = clock_timestamp(),
              expires_at = clock_timestamp() + interval '10 minutes'
            WHERE "KothApiObservationOperations".response IS NULL
              AND "KothApiObservationOperations".lease_expires_at
                    <= clock_timestamp()
            RETURNING lease_token"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .bind(request_digest.as_slice())
    .bind(signer_scope)
    .bind(body_digest.as_slice())
    .bind(context)
    .bind(lease_token)
    .bind(OBSERVATION_LEASE_SECONDS as f64)
    .fetch_optional(pool)
    .await
    .map_err(retryable_database_error)?;
    if owned == Some(lease_token) {
        return Ok(ObservationReservationResult::Owner(
            ObservationReservation {
                request_digest,
                lease_token,
            },
        ));
    }
    if let Some(response) = completed_observation(pool, challenge_id, &request_digest).await? {
        return Ok(ObservationReservationResult::Completed(response));
    }
    let pool = pool.clone();
    let flight_key = format!("koth-observation-{}", hex::encode(request_digest));
    let response = OBSERVATION_DUPLICATE_SF
        .run_with_timeout(&flight_key, DUPLICATE_WAIT_DEADLINE, move || async move {
            wait_for_completed_observation(&pool, challenge_id, request_digest).await
        })
        .await;
    response
        .map(ObservationReservationResult::Completed)
        .ok_or_else(|| {
            AppError::unavailable(
                "An identical Leaderboard observation is still being committed; retry later",
            )
        })
}

async fn release_observation(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    reservation: &ObservationReservation,
) {
    if let Err(error) = sqlx::query(
        r#"DELETE FROM "KothApiObservationOperations"
            WHERE challenge_id = $1 AND request_digest = $2
              AND lease_token = $3 AND response IS NULL"#,
    )
    .bind(challenge_id)
    .bind(reservation.request_digest.as_slice())
    .bind(reservation.lease_token)
    .execute(pool)
    .await
    {
        tracing::warn!(
            challenge_id,
            error = %error,
            "failed to release a rejected KotH observation reservation"
        );
    }
}

async fn load_signing_credentials(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<Vec<SigningCredential>> {
    let observer: Option<String> = sqlx::query_scalar(
        r#"SELECT observer.hmac_secret
             FROM "KothApiObservers" observer
             JOIN "GameChallenges" challenge
               ON challenge.game_id = observer.game_id
              AND challenge.id = observer.challenge_id
              AND challenge."Type" = $3
            WHERE observer.game_id = $1 AND observer.challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(pool)
    .await
    .map_err(retryable_database_error)?;
    let Some(observer) = observer else {
        return Ok(Vec::new());
    };
    let reporter = sqlx::query_as::<_, (i64, i32, String)>(
        r#"SELECT reporter.cycle_id, reporter.reset_attempt,
                  reporter.hmac_secret
             FROM "KothTargetReporters" reporter
             JOIN "KothCrownCycles" cycle
               ON cycle.id = reporter.cycle_id
              AND cycle.game_id = reporter.game_id
              AND cycle.challenge_id = reporter.challenge_id
              AND cycle.reset_attempt = reporter.reset_attempt
              AND cycle.phase = 'Active'
             JOIN "KothTargets" target
               ON target.game_id = cycle.game_id
              AND target.challenge_id = cycle.challenge_id
              AND target.container_id = cycle.replacement_container_id
            WHERE reporter.game_id = $1
              AND reporter.challenge_id = $2
              AND clock_timestamp() < reporter.expires_at
            ORDER BY cycle.cycle_number DESC
            LIMIT 1"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(pool)
    .await
    .map_err(retryable_database_error)?;
    let mut credentials = vec![SigningCredential {
        kind: SigningCredentialKind::Observer,
        secret: observer,
    }];
    if let Some((cycle_id, reset_attempt, secret)) = reporter {
        credentials.push(SigningCredential {
            kind: SigningCredentialKind::TargetReporter {
                cycle_id,
                reset_attempt,
            },
            secret,
        });
    }
    Ok(credentials)
}

fn match_signing_credential(
    credentials: Vec<SigningCredential>,
    timestamp: &str,
    game_id: i32,
    challenge_id: i32,
    body: &[u8],
    signature: &[u8; 32],
) -> AppResult<SigningCredential> {
    let mut matched = None;
    for credential in credentials {
        if verify_signature(
            &credential.secret,
            timestamp,
            game_id,
            challenge_id,
            body,
            signature,
        )
        .is_ok()
        {
            matched = Some(credential);
        }
    }
    matched.ok_or(AppError::Unauthorized)
}

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

fn wave_ends_inside_window(
    ended_at: DateTime<Utc>,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> bool {
    ended_at >= window_start && ended_at < window_end
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
        .map_err(retryable_database_error)?;
    sqlx::query(r#"DELETE FROM "KothApiSnapshotWaves" WHERE target_id = $1"#)
        .bind(target_id)
        .execute(&mut *connection)
        .await
        .map_err(retryable_database_error)?;
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
        .map_err(retryable_database_error)?;

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
        .map_err(retryable_database_error)?;
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
    .map_err(retryable_database_error)?;
    let request_hash: [u8; 32] = Sha256::digest(signature).into();
    let inserted = sqlx::query(INSERT_REPLAY_SQL)
        .bind(request_hash.as_slice())
        .bind(challenge_id)
        .execute(&mut *connection)
        .await
        .map_err(retryable_database_error)?
        .rows_affected();
    if inserted != 1 {
        return Err(AppError::conflict(
            "Leaderboard reporting request was already accepted",
        ));
    }
    Ok(())
}

fn observation_error_response(error: AppError) -> AppResult<Response> {
    match error {
        error @ AppError::TooManyRequests { .. } => {
            Ok(retry_after_response(error, "koth_observation_admission", 1))
        }
        error @ AppError::ServiceUnavailable(_) => {
            Ok(retry_after_response(error, "koth_observation_busy", 1))
        }
        AppError::Conflict(title) if title == STALE_CONTEXT_MESSAGE => Ok(retry_after_response(
            AppError::Conflict(title),
            "stale_context",
            1,
        )),
        error => Err(error),
    }
}

/// Store normalized current-tick input only. The checker still brackets it
/// around an independent functional probe before any durable score is written.
pub async fn submit_observation(
    State(st): State<SharedState>,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let started = std::time::Instant::now();
    let request_bytes = body.len();
    let accepted = match tokio::time::timeout(
        OBSERVATION_DEADLINE,
        accept_observation(st.pg(), game_id, challenge_id, &headers, &body),
    )
    .await
    {
        Ok(Ok(accepted)) => accepted,
        Ok(Err(error)) => return observation_error_response(error),
        Err(_) => {
            return Ok(retry_after_response(
                AppError::unavailable("Leaderboard observation timed out; retry later"),
                "koth_observation_timeout",
                1,
            ));
        }
    };
    tracing::info!(
        referee_operation = "observation",
        game_id,
        challenge_id,
        request_bytes,
        submitted_waves = accepted.submitted_waves,
        submitted_teams = accepted.submitted_teams,
        recognized_teams = accepted.recognized_teams,
        elapsed_ms = started.elapsed().as_millis(),
        "accepted KotH referee observation"
    );
    Ok(RequestResponse::ok(accepted).into_response())
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
    let credential = match_signing_credential(
        load_signing_credentials(pool, game_id, challenge_id).await?,
        timestamp_raw,
        game_id,
        challenge_id,
        body,
        &signature,
    )?;
    let input = parse_and_normalize(body)?;
    let signer_scope = credential.scope(game_id, challenge_id);
    let body_digest = canonical_input_digest(&input);
    let request_digest =
        observation_request_digest(game_id, challenge_id, &signer_scope, &body_digest);
    let reservation = match reserve_observation(
        pool,
        game_id,
        challenge_id,
        &signer_scope,
        &input.context,
        body_digest,
        request_digest,
    )
    .await?
    {
        ObservationReservationResult::Completed(response) => return Ok(response),
        ObservationReservationResult::Owner(reservation) => reservation,
    };
    let weight = observation_weight(body.len(), &input);
    let Some(_permit) = OBSERVATION_ADMISSION.try_acquire(
        format!("observation:{signer_scope}"),
        weight,
        OBSERVATION_SIGNER_WEIGHT,
    ) else {
        release_observation(pool, challenge_id, &reservation).await;
        return Err(AppError::too_many_requests(1));
    };
    let verified = VerifiedObservation {
        now,
        timestamp,
        signature,
        credential,
        input,
    };
    let result = process_observation(pool, game_id, challenge_id, verified, &reservation).await;
    if result.is_err() {
        release_observation(pool, challenge_id, &reservation).await;
    }
    result
}

async fn lock_observer(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<Option<String>> {
    sqlx::query_scalar(
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
    .fetch_optional(connection)
    .await
    .map_err(retryable_database_error)
}

async fn process_observation(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
    verified: VerifiedObservation,
    reservation: &ObservationReservation,
) -> AppResult<KothObservationAcceptedModel> {
    let VerifiedObservation {
        now,
        timestamp,
        signature,
        credential,
        input,
    } = verified;
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
        .map_err(retryable_database_error)?;
    sqlx::query("SET LOCAL lock_timeout = '500ms'")
        .execute(&mut *transaction)
        .await
        .map_err(retryable_database_error)?;
    sqlx::query("SET LOCAL statement_timeout = '12s'")
        .execute(&mut *transaction)
        .await
        .map_err(retryable_database_error)?;
    let locked_observer = lock_observer(&mut transaction, game_id, challenge_id).await?;
    let Some(locked_secret) = locked_observer else {
        return Err(AppError::Unauthorized);
    };
    let context = load_active_context(&mut *transaction, game_id, challenge_id)
        .await?
        .ok_or_else(|| AppError::conflict("Leaderboard KotH context is not active"))?;
    match credential.kind {
        SigningCredentialKind::Observer if locked_secret != credential.secret => {
            return Err(AppError::Unauthorized);
        }
        SigningCredentialKind::Observer => {}
        SigningCredentialKind::TargetReporter {
            cycle_id,
            reset_attempt,
        } => {
            let locked_reporter: Option<String> = sqlx::query_scalar(
                r#"SELECT reporter.hmac_secret
                     FROM "KothTargetReporters" reporter
                     JOIN "KothCrownCycles" cycle
                       ON cycle.id = reporter.cycle_id
                      AND cycle.game_id = reporter.game_id
                      AND cycle.challenge_id = reporter.challenge_id
                      AND cycle.reset_attempt = reporter.reset_attempt
                      AND cycle.phase = 'Active'
                     JOIN "KothTargets" target
                       ON target.game_id = cycle.game_id
                      AND target.challenge_id = cycle.challenge_id
                      AND target.container_id = cycle.replacement_container_id
                    WHERE reporter.game_id = $1
                      AND reporter.challenge_id = $2
                      AND reporter.cycle_id = $3
                      AND reporter.reset_attempt = $4
                      AND target.id = $5
                      AND cycle.replacement_container_id = $6
                      AND clock_timestamp() < reporter.expires_at
                    FOR UPDATE OF reporter"#,
            )
            .bind(game_id)
            .bind(challenge_id)
            .bind(cycle_id)
            .bind(reset_attempt)
            .bind(context.target_id)
            .bind(&context.container_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(retryable_database_error)?;
            if locked_reporter.as_deref() != Some(credential.secret.as_str())
                || cycle_id != context.cycle_id
                || reset_attempt != context.reset_attempt
            {
                return Err(AppError::Unauthorized);
            }
        }
    }
    let eligible_capabilities =
        super::load_eligible_capabilities(&mut *transaction, game_id, challenge_id).await?;
    let eligible_tokens: Vec<_> = eligible_capabilities
        .iter()
        .map(|capability| capability.token.clone())
        .collect();
    if input.context != context.opaque_context(game_id, challenge_id, &eligible_tokens) {
        return Err(AppError::conflict(STALE_CONTEXT_MESSAGE));
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
    .map_err(retryable_database_error)?;
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
        .map_err(retryable_database_error)?;
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
        .map_err(retryable_database_error)?;
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
        if !wave_ends_inside_window(ended_at, wave_window_start, wave_window_end) {
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
    let waves = resolve_current_capabilities(normalized_waves, &eligible_capabilities)?;
    validate_resolved_crowns(&waves)?;
    let eligible_participation_ids: Vec<_> = eligible_capabilities
        .iter()
        .map(|capability| capability.participation_id)
        .collect();
    crate::services::ad::koth_api_capability::retain_eligible_unsettled_scores(
        &mut transaction,
        game_id,
        challenge_id,
        context.target_id,
        &eligible_participation_ids,
    )
    .await?;
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
    .map_err(retryable_database_error)?
    .ok_or_else(|| {
        AppError::conflict("KotH arena snapshot is late or older than the accepted snapshot")
    })?;
    replace_snapshot_rows(&mut transaction, context.target_id, &waves).await?;
    match credential.kind {
        SigningCredentialKind::Observer => {
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
            .map_err(retryable_database_error)?;
        }
        SigningCredentialKind::TargetReporter {
            cycle_id,
            reset_attempt,
        } => {
            sqlx::query(
                r#"UPDATE "KothTargetReporters"
                      SET last_used_at = clock_timestamp()
                    WHERE cycle_id = $1 AND reset_attempt = $2
                      AND (last_used_at IS NULL
                           OR last_used_at < clock_timestamp() - interval '30 seconds')"#,
            )
            .bind(cycle_id)
            .bind(reset_attempt)
            .execute(&mut *transaction)
            .await
            .map_err(retryable_database_error)?;
        }
    }
    let accepted_at = DateTime::from_timestamp_millis(accepted_at.timestamp_millis())
        .ok_or_else(|| AppError::internal("accepted observation timestamp is out of range"))?;
    let response = KothObservationAcceptedModel {
        accepted: true,
        cycle_number: context.cycle_number,
        reset_attempt: context.reset_attempt,
        round_number: context.round_number,
        submitted_waves,
        submitted_teams,
        recognized_teams: recognized_team_ids.len(),
        accepted_at,
    };
    let stored = sqlx::query(
        r#"UPDATE "KothApiObservationOperations"
              SET response = $4,
                  completed_at = clock_timestamp(),
                  expires_at = clock_timestamp() + interval '10 minutes'
            WHERE challenge_id = $1 AND request_digest = $2
              AND lease_token = $3 AND response IS NULL"#,
    )
    .bind(challenge_id)
    .bind(reservation.request_digest.as_slice())
    .bind(reservation.lease_token)
    .bind(serde_json::to_value(&response).map_err(|error| AppError::internal(error.to_string()))?)
    .execute(&mut *transaction)
    .await
    .map_err(retryable_database_error)?
    .rows_affected();
    if stored != 1 {
        return Err(AppError::unavailable(
            "Leaderboard observation ownership changed before commit; retry later",
        ));
    }
    transaction
        .commit()
        .await
        .map_err(retryable_database_error)?;
    Ok(response)
}
