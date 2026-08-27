use axum::extract::{Path, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

use super::super::admin::require_game_admin;

const OBSERVER_SECRET_BYTES: usize = 32;
const OBSERVER_SECRET_PREFIX: &str = "koth_api_";
const MAX_OBSERVER_REVISION: i64 = 9_007_199_254_740_990;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObserverMutationRequest {
    pub operation_id: Uuid,
    pub expected_revision: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ObserverOperationKind {
    Rotate,
    Revoke,
}

impl ObserverOperationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rotate => "Rotate",
            Self::Revoke => "Revoke",
        }
    }

    fn parse(value: &str) -> AppResult<Self> {
        match value {
            "Rotate" => Ok(Self::Rotate),
            "Revoke" => Ok(Self::Revoke),
            _ => Err(AppError::internal(
                "invalid stored KotH observer operation kind",
            )),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminKothObserverModel {
    pub challenge_id: i32,
    pub revision: i64,
    pub claim_source: String,
    pub configured: bool,
    pub secret_hint: Option<String>,
    /// Frozen by the first accepted signed Leaderboard snapshot.
    pub objective_count: Option<i16>,
    pub objective_ids: Option<Vec<String>>,
    pub objective_schema_hash: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub rotated_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_observation_at: Option<DateTime<Utc>>,
    pub context_path: String,
    pub observation_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

type ObserverMetaRow = (
    Option<String>,
    Option<String>,
    Option<i16>,
    Option<Vec<String>>,
    Option<Vec<u8>>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    bool,
    i64,
);

fn paths(game_id: i32, challenge_id: i32) -> (String, String) {
    let base = format!("/api/v1/koth/games/{game_id}/challenges/{challenge_id}");
    (format!("{base}/context"), format!("{base}/observations"))
}

async fn observer_model<'e, E>(
    executor: E,
    game_id: i32,
    challenge_id: i32,
    operation_id: Option<Uuid>,
    secret: Option<String>,
) -> AppResult<AdminKothObserverModel>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let row = sqlx::query_as::<_, ObserverMetaRow>(
        r#"SELECT
                  CASE
                    WHEN config.game_id IS NOT NULL
                      THEN COALESCE(NULLIF(frozen.item->>'claimSource', ''), 'Marker')
                    WHEN observer.challenge_id IS NOT NULL THEN 'Api'
                    ELSE 'Marker'
                  END AS claim_source,
                  observer.secret_hint, scheme.objective_count,
                  scheme.objective_ids, scheme.objective_schema_hash,
                  observer.created_at, observer.rotated_at,
                  observer.last_used_at, snapshot.accepted_at,
                  observer.challenge_id IS NOT NULL AS configured,
                  COALESCE(revision.revision, 0)::bigint AS revision
             FROM "GameChallenges" challenge
             LEFT JOIN "KothOfficialConfigs" config
               ON config.game_id = challenge.game_id
             LEFT JOIN LATERAL (
               SELECT item
                 FROM jsonb_array_elements(config.hills_snapshot) item
                WHERE (item->>'challengeId')::integer = challenge.id
                LIMIT 1
             ) frozen ON TRUE
             LEFT JOIN "KothApiObservers" observer
               ON observer.game_id = challenge.game_id
              AND observer.challenge_id = challenge.id
             LEFT JOIN "KothApiObserverRevisions" revision
               ON revision.game_id = challenge.game_id
              AND revision.challenge_id = challenge.id
             LEFT JOIN "KothApiArenaSchemes" scheme
               ON scheme.game_id = challenge.game_id
              AND scheme.challenge_id = challenge.id
             LEFT JOIN "KothTargets" target
               ON target.game_id = challenge.game_id
              AND target.challenge_id = challenge.id
             LEFT JOIN "KothApiSnapshots" snapshot
               ON snapshot.target_id = target.id
            WHERE challenge.game_id = $1 AND challenge.id = $2
              AND challenge."Type" = $3"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("KotH challenge not found"))?;
    let (context_path, observation_path) = paths(game_id, challenge_id);
    Ok(AdminKothObserverModel {
        challenge_id,
        revision: row.10,
        claim_source: row.0.unwrap_or_else(|| "Marker".to_string()),
        configured: row.9,
        secret_hint: row.1,
        objective_count: row.2,
        objective_ids: row.3,
        objective_schema_hash: row.4.map(hex::encode),
        created_at: row.5,
        rotated_at: row.6,
        last_used_at: row.7,
        last_observation_at: row.8,
        context_path,
        observation_path,
        operation_id,
        secret,
    })
}

async fn require_observer_can_be_enabled(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let row = sqlx::query_as::<_, (Option<String>, bool)>(
        r#"SELECT frozen.item->>'claimSource' AS frozen_source,
                  config.game_id IS NOT NULL AS snapshotted
             FROM "GameChallenges" challenge
             LEFT JOIN "KothOfficialConfigs" config
               ON config.game_id = challenge.game_id
             LEFT JOIN LATERAL (
               SELECT item
                 FROM jsonb_array_elements(config.hills_snapshot) item
                WHERE (item->>'challengeId')::integer = challenge.id
                LIMIT 1
             ) frozen ON TRUE
            WHERE challenge.game_id = $1 AND challenge.id = $2
              AND challenge."Type" = $3
            FOR SHARE OF challenge"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("KotH challenge not found"))?;
    if row.1 && row.0.as_deref() != Some("Api") {
        return Err(AppError::conflict(
            "the official KotH snapshot fixed this hill to marker scoring",
        ));
    }
    Ok(())
}

pub async fn get_observer(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<AdminKothObserverModel>> {
    require_game_admin(&st, &user, game_id).await?;
    Ok(RequestResponse::ok(
        observer_model(st.pg(), game_id, challenge_id, None, None).await?,
    ))
}

fn private_no_store(model: AdminKothObserverModel) -> Response {
    let mut response = RequestResponse::ok(model).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

async fn clear_referee_input(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "KothApiSnapshots" snapshot
            USING "KothTargets" target
            WHERE snapshot.target_id = target.id
              AND target.game_id = $1 AND target.challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "KothApiRequestReplays" WHERE challenge_id = $1"#)
        .bind(challenge_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

type StoredOperationRow = (
    i32,
    i32,
    Option<Uuid>,
    String,
    i64,
    Option<i64>,
    Option<serde_json::Value>,
);

pub(super) struct ObserverMutationOutcome {
    pub(super) model: AdminKothObserverModel,
    pub(super) kind: ObserverOperationKind,
    pub(super) fresh: bool,
}

async fn purge_expired_operation(
    connection: &mut sqlx::PgConnection,
    operation_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "KothApiObserverOperations"
            WHERE operation_id = $1 AND expires_at <= clock_timestamp()"#,
    )
    .bind(operation_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "KothApiObserverOperations"
            WHERE operation_id IN (
              SELECT operation_id
                FROM "KothApiObserverOperations"
               WHERE expires_at <= clock_timestamp()
               ORDER BY expires_at
               LIMIT 127
            )"#,
    )
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn stored_operation(
    connection: &mut sqlx::PgConnection,
    operation_id: Uuid,
) -> AppResult<Option<StoredOperationRow>> {
    sqlx::query_as(
        r#"SELECT game_id, challenge_id, actor_user_id, operation_kind,
                  expected_revision, result_revision, result
             FROM "KothApiObserverOperations"
            WHERE operation_id = $1 AND expires_at > clock_timestamp()
            FOR UPDATE"#,
    )
    .bind(operation_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn disclose_stored_operation(
    connection: &mut sqlx::PgConnection,
    operation_id: Uuid,
    game_id: i32,
    challenge_id: i32,
    actor_user_id: Uuid,
    expected: Option<(ObserverOperationKind, i64)>,
) -> AppResult<Option<ObserverMutationOutcome>> {
    let Some(row) = stored_operation(connection, operation_id).await? else {
        return Ok(None);
    };
    let kind = ObserverOperationKind::parse(&row.3)?;
    if row.0 != game_id || row.1 != challenge_id || row.2 != Some(actor_user_id) {
        return Err(AppError::not_found("referee mutation operation not found"));
    }
    if expected.is_some_and(|(expected_kind, expected_revision)| {
        kind != expected_kind || row.4 != expected_revision
    }) {
        return Err(AppError::conflict(
            "the operation identity is already bound to different mutation input",
        ));
    }
    let (Some(result_revision), Some(result)) = (row.5, row.6) else {
        return Err(AppError::unavailable(
            "the referee mutation is still completing; retry its operation identity",
        ));
    };
    let model: AdminKothObserverModel = serde_json::from_value(result)
        .map_err(|_| AppError::internal("invalid stored KotH observer result"))?;
    if model.revision != result_revision || model.operation_id != Some(operation_id) {
        return Err(AppError::internal(
            "stored KotH observer operation result failed its revision fence",
        ));
    }
    sqlx::query(
        r#"UPDATE "KothApiObserverOperations"
              SET disclosure_count = disclosure_count + 1,
                  last_disclosed_at = clock_timestamp()
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(Some(ObserverMutationOutcome {
        model,
        kind,
        fresh: false,
    }))
}

async fn ensure_observer_revision(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<i64> {
    // The migration gives credentials that already exist revision 1. A missing
    // row is what the metadata read exposed as revision 0, including restored
    // legacy state, so initialize it to that same observable precondition.
    sqlx::query(
        r#"INSERT INTO "KothApiObserverRevisions"
             (challenge_id, game_id, revision, updated_at)
           SELECT challenge.id, challenge.game_id, 0, clock_timestamp()
             FROM "GameChallenges" challenge
            WHERE challenge.game_id = $1 AND challenge.id = $2
           ON CONFLICT (challenge_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query_scalar(
        r#"SELECT revision
             FROM "KothApiObserverRevisions"
            WHERE game_id = $1 AND challenge_id = $2
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("KotH challenge not found"))
}

async fn reserve_operation(
    connection: &mut sqlx::PgConnection,
    operation_id: Uuid,
    game_id: i32,
    challenge_id: i32,
    actor_user_id: Uuid,
    kind: ObserverOperationKind,
    expected_revision: i64,
) -> AppResult<()> {
    let result = sqlx::query(
        r#"INSERT INTO "KothApiObserverOperations"
             (operation_id, challenge_id, game_id, actor_user_id,
              operation_kind, expected_revision)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(operation_id)
    .bind(challenge_id)
    .bind(game_id)
    .bind(actor_user_id)
    .bind(kind.as_str())
    .bind(expected_revision)
    .execute(&mut *connection)
    .await;
    match result {
        Ok(_) => Ok(()),
        Err(error) if crate::utils::error::is_unique_violation(&error) => Err(AppError::conflict(
            "the operation identity is already bound to another referee mutation",
        )),
        Err(error) => Err(AppError::internal(error.to_string())),
    }
}

async fn advance_revision(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    expected_revision: i64,
) -> AppResult<i64> {
    sqlx::query_scalar(
        r#"UPDATE "KothApiObserverRevisions"
              SET revision = revision + 1, updated_at = clock_timestamp()
            WHERE game_id = $1 AND challenge_id = $2 AND revision = $3
        RETURNING revision"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(expected_revision)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| {
        AppError::conflict("the referee credential changed; refresh its revision before retrying")
    })
}

async fn rotate_credential(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<String> {
    let secret = format!(
        "{OBSERVER_SECRET_PREFIX}{}",
        crate::utils::codec::random_token(OBSERVER_SECRET_BYTES)
    );
    let hint = format!("…{}", &secret[secret.len() - 6..]);
    sqlx::query(
        r#"INSERT INTO "KothApiObservers"
             (challenge_id, game_id, hmac_secret, secret_hint,
              created_at, rotated_at, last_used_at)
           VALUES ($2, $1, $3, $4, clock_timestamp(), clock_timestamp(), NULL)
           ON CONFLICT (challenge_id) DO UPDATE SET
             game_id = EXCLUDED.game_id,
             hmac_secret = EXCLUDED.hmac_secret,
             secret_hint = EXCLUDED.secret_hint,
             rotated_at = clock_timestamp(),
             last_used_at = NULL"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(&secret)
    .bind(&hint)
    .execute(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to persist KotH referee credential"))?;
    Ok(secret)
}

async fn revoke_credential(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let configured = sqlx::query_scalar::<_, i32>(
        r#"SELECT challenge_id FROM "KothApiObservers"
            WHERE game_id = $1 AND challenge_id = $2
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if configured.is_none() {
        return Err(AppError::conflict(
            "the KotH referee credential is already revoked",
        ));
    }
    sqlx::query(
        r#"DELETE FROM "KothApiObservers"
            WHERE game_id = $1 AND challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(super) async fn mutate_observer_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    actor_user_id: Uuid,
    kind: ObserverOperationKind,
    request: &ObserverMutationRequest,
) -> AppResult<ObserverMutationOutcome> {
    if request.operation_id.is_nil()
        || request.expected_revision < 0
        || request.expected_revision > MAX_OBSERVER_REVISION
    {
        return Err(AppError::bad_request(
            "operationId must be opaque and expectedRevision must be a valid revision",
        ));
    }
    purge_expired_operation(connection, request.operation_id).await?;
    if let Some(outcome) = disclose_stored_operation(
        connection,
        request.operation_id,
        game_id,
        challenge_id,
        actor_user_id,
        Some((kind, request.expected_revision)),
    )
    .await?
    {
        return Ok(outcome);
    }

    require_observer_can_be_enabled(connection, game_id, challenge_id).await?;
    let current_revision = ensure_observer_revision(connection, game_id, challenge_id).await?;
    if current_revision != request.expected_revision {
        return Err(AppError::conflict(
            "the referee credential changed; refresh its revision before retrying",
        ));
    }
    reserve_operation(
        connection,
        request.operation_id,
        game_id,
        challenge_id,
        actor_user_id,
        kind,
        request.expected_revision,
    )
    .await?;
    let result_revision =
        advance_revision(connection, game_id, challenge_id, request.expected_revision).await?;
    let secret = match kind {
        ObserverOperationKind::Rotate => {
            Some(rotate_credential(connection, game_id, challenge_id).await?)
        }
        ObserverOperationKind::Revoke => {
            revoke_credential(connection, game_id, challenge_id).await?;
            None
        }
    };
    clear_referee_input(connection, game_id, challenge_id).await?;
    let model = observer_model(
        &mut *connection,
        game_id,
        challenge_id,
        Some(request.operation_id),
        secret,
    )
    .await?;
    if model.revision != result_revision {
        return Err(AppError::internal(
            "KotH observer mutation produced an inconsistent revision",
        ));
    }
    let result = serde_json::to_value(&model)
        .map_err(|error| AppError::internal(format!("serialize observer result: {error}")))?;
    let persisted = sqlx::query(
        r#"UPDATE "KothApiObserverOperations"
              SET result_revision = $2, result = $3,
                  completed_at = clock_timestamp(),
                  disclosure_count = 1,
                  last_disclosed_at = clock_timestamp()
            WHERE operation_id = $1 AND completed_at IS NULL"#,
    )
    .bind(request.operation_id)
    .bind(result_revision)
    .bind(result)
    .execute(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to persist KotH referee operation result"))?
    .rows_affected();
    if persisted != 1 {
        return Err(AppError::internal(
            "KotH observer operation result was not persisted exactly once",
        ));
    }
    Ok(ObserverMutationOutcome {
        model,
        kind,
        fresh: true,
    })
}

async fn recover_observer_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
) -> AppResult<ObserverMutationOutcome> {
    if operation_id.is_nil() {
        return Err(AppError::not_found("referee mutation operation not found"));
    }
    purge_expired_operation(connection, operation_id).await?;
    disclose_stored_operation(
        connection,
        operation_id,
        game_id,
        challenge_id,
        actor_user_id,
        None,
    )
    .await?
    .ok_or_else(|| AppError::not_found("referee mutation operation not found"))
}

async fn audit_observer_result(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
    outcome: &ObserverMutationOutcome,
) {
    if outcome.fresh {
        let verb = match outcome.kind {
            ObserverOperationKind::Rotate => "Rotated",
            ObserverOperationKind::Revoke => "Revoked",
        };
        crate::services::audit::info(
            st,
            "KothObserverController",
            Some(user.name.clone()),
            None,
            format!(
                "{verb} KotH referee credential for game {game_id}, challenge {}, revision {}",
                outcome.model.challenge_id, outcome.model.revision
            ),
        )
        .await;
    }
    crate::services::audit::info(
        st,
        "KothObserverController",
        Some(user.name.clone()),
        None,
        format!(
            "Disclosed KotH referee {} result for game {game_id}, challenge {}, revision {}",
            outcome.kind.as_str().to_lowercase(),
            outcome.model.challenge_id,
            outcome.model.revision
        ),
    )
    .await;
}

async fn mutate_observer(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
    challenge_id: i32,
    kind: ObserverOperationKind,
    request: ObserverMutationRequest,
) -> AppResult<Response> {
    require_game_admin(st, user, game_id).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let outcome = mutate_observer_locked(
        control.transaction_mut(),
        game_id,
        challenge_id,
        user.id,
        kind,
        &request,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    audit_observer_result(st, user, game_id, &outcome).await;
    Ok(private_no_store(outcome.model))
}

/// Enable or rotate the referee. An exact authorized retry recovers the same secret.
pub async fn rotate_observer(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    Json(request): Json<ObserverMutationRequest>,
) -> AppResult<Response> {
    mutate_observer(
        &st,
        &user,
        game_id,
        challenge_id,
        ObserverOperationKind::Rotate,
        request,
    )
    .await
}

pub async fn revoke_observer(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    Json(request): Json<ObserverMutationRequest>,
) -> AppResult<Response> {
    mutate_observer(
        &st,
        &user,
        game_id,
        challenge_id,
        ObserverOperationKind::Revoke,
        request,
    )
    .await
}

pub async fn recover_observer_operation(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id, operation_id)): Path<(i32, i32, Uuid)>,
) -> AppResult<Response> {
    require_game_admin(&st, &user, game_id).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let outcome = recover_observer_locked(
        control.transaction_mut(),
        game_id,
        challenge_id,
        user.id,
        operation_id,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    audit_observer_result(&st, &user, game_id, &outcome).await;
    Ok(private_no_store(outcome.model))
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;
