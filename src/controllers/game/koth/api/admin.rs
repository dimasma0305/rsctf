use axum::extract::{Path, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{MessageResponse, RequestResponse};

use super::super::admin::require_game_admin;

const OBSERVER_SECRET_BYTES: usize = 32;
const OBSERVER_SECRET_PREFIX: &str = "koth_api_";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminKothObserverModel {
    pub challenge_id: i32,
    pub claim_source: String,
    pub configured: bool,
    pub secret_hint: Option<String>,
    /// Frozen by the first accepted snapshot containing a recognized team.
    pub objective_count: Option<i16>,
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
    pub secret: Option<String>,
}

type ObserverMetaRow = (
    Option<String>,
    Option<String>,
    Option<i16>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    bool,
);

fn paths(game_id: i32, challenge_id: i32) -> (String, String) {
    let base = format!("/api/v1/koth/games/{game_id}/challenges/{challenge_id}");
    (format!("{base}/context"), format!("{base}/observations"))
}

async fn observer_model(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    secret: Option<String>,
) -> AppResult<AdminKothObserverModel> {
    let row = sqlx::query_as::<_, ObserverMetaRow>(
        r#"SELECT
                  CASE
                    WHEN config.game_id IS NOT NULL
                      THEN COALESCE(NULLIF(frozen.item->>'claimSource', ''), 'Marker')
                    WHEN observer.challenge_id IS NOT NULL THEN 'Api'
                    ELSE 'Marker'
                  END AS claim_source,
                  observer.secret_hint, scheme.objective_count,
                  observer.created_at, observer.rotated_at,
                  observer.last_used_at, snapshot.accepted_at,
                  observer.challenge_id IS NOT NULL AS configured
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
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("KotH challenge not found"))?;
    let (context_path, observation_path) = paths(game_id, challenge_id);
    Ok(AdminKothObserverModel {
        challenge_id,
        claim_source: row.0.unwrap_or_else(|| "Marker".to_string()),
        configured: row.7,
        secret_hint: row.1,
        objective_count: row.2,
        created_at: row.3,
        rotated_at: row.4,
        last_used_at: row.5,
        last_observation_at: row.6,
        context_path,
        observation_path,
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
        observer_model(&st, game_id, challenge_id, None).await?,
    ))
}

/// Enable or rotate the referee. The plaintext secret is returned exactly once.
pub async fn rotate_observer(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    require_game_admin(&st, &user, game_id).await?;
    let secret = format!(
        "{OBSERVER_SECRET_PREFIX}{}",
        crate::utils::codec::random_token(OBSERVER_SECRET_BYTES)
    );
    let hint = format!("…{}", &secret[secret.len() - 6..]);
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    require_observer_can_be_enabled(control.transaction_mut(), game_id, challenge_id).await?;
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
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    clear_referee_input(control.transaction_mut(), game_id, challenge_id).await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut response =
        RequestResponse::ok(observer_model(&st, game_id, challenge_id, Some(secret)).await?)
            .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    Ok(response)
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

pub async fn revoke_observer(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<MessageResponse> {
    require_game_admin(&st, &user, game_id).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    require_observer_can_be_enabled(control.transaction_mut(), game_id, challenge_id).await?;
    clear_referee_input(control.transaction_mut(), game_id, challenge_id).await?;
    sqlx::query(
        r#"DELETE FROM "KothApiObservers"
            WHERE game_id = $1 AND challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(MessageResponse::ok("KotH API arena referee revoked"))
}
