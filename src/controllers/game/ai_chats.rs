//! Team-scoped AI chat links on solved Jeopardy challenges, and the monitor
//! review list. Links are validated against the admin provider registry and
//! are never fetched by the server.

use std::collections::HashSet;

use axum::extract::Query;
use axum::response::IntoResponse;
use sea_orm::ActiveEnum;

use super::*;
use crate::services::ai_chat_links::{self, MatchedLink};

const DISABLED_MESSAGE: &str = "AI chat links are not enabled for this event";
const MAX_MONITOR_PAGE: i64 = 100;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatProviderRule {
    pub key: String,
    pub label: String,
    pub pattern: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatLinkState {
    pub editable: bool,
    pub solved: bool,
    #[serde(with = "crate::utils::datetime::millis")]
    pub editable_until: DateTime<Utc>,
    pub max_links: usize,
    pub providers: Vec<AiChatProviderRule>,
    pub links: Vec<MatchedLink>,
    pub revision: i32,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub updated_at: Option<DateTime<Utc>>,
    pub submitted_by: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveAiChatLinks {
    pub links: Vec<String>,
    pub expected_revision: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredLink {
    url: String,
    provider_key: String,
    provider_label: String,
}

impl From<StoredLink> for MatchedLink {
    fn from(link: StoredLink) -> Self {
        Self {
            url: link.url,
            provider_key: link.provider_key,
            provider_label: link.provider_label,
        }
    }
}

#[derive(sqlx::FromRow)]
struct SavedRow {
    links: sqlx::types::Json<Vec<StoredLink>>,
    revision: i32,
    updated_at: DateTime<Utc>,
    submitted_by: Option<String>,
}

/// Links close with the later of the event end and the writeup deadline, so
/// disclosures cannot be rewritten while organizers review them.
fn editable_until(game: &game::Model) -> DateTime<Utc> {
    game.end_time_utc.max(game.writeup_deadline)
}

fn jeopardy(challenge_type: ChallengeType) -> bool {
    !challenge_type.uses_ad_engine()
}

async fn challenge_type<'e, E>(
    executor: E,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<ChallengeType>
where
    E: sqlx::PgExecutor<'e>,
{
    let value: Option<i16> = sqlx::query_scalar(
        r#"SELECT "Type" FROM "GameChallenges"
            WHERE id = $1 AND game_id = $2 AND is_enabled"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .fetch_optional(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let challenge_type = value
        .and_then(|value| ChallengeType::try_from_value(&value).ok())
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    if !jeopardy(challenge_type) {
        return Err(AppError::not_found(
            "AI chat links apply only to Jeopardy challenges",
        ));
    }
    Ok(challenge_type)
}

async fn solved<'e, E>(executor: E, participation_id: i32, challenge_id: i32) -> AppResult<bool>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "Submissions"
                WHERE participation_id = $1 AND challenge_id = $2 AND status = $3
           )"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .bind(AnswerResult::Accepted as i16)
    .fetch_one(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn load_state(
    st: &SharedState,
    game: &game::Model,
    participation_id: i32,
    challenge_id: i32,
) -> AppResult<AiChatLinkState> {
    let solved = solved(st.pg(), participation_id, challenge_id).await?;
    let saved = sqlx::query_as::<_, SavedRow>(
        r#"SELECT link.links, link.revision, link.updated_at, account.user_name AS submitted_by
             FROM "AiChatLinks" link
             LEFT JOIN "AspNetUsers" account ON account.id = link.submitted_by
            WHERE link.game_id = $1 AND link.participation_id = $2 AND link.challenge_id = $3"#,
    )
    .bind(game.id)
    .bind(participation_id)
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let rows = ai_chat_links::load_rows(st.pg()).await?;
    let providers = ai_chat_links::enabled_providers(&rows)
        .into_iter()
        .map(|provider| AiChatProviderRule {
            key: provider.key,
            label: provider.label,
            pattern: provider.pattern,
        })
        .collect();
    let until = editable_until(game);
    Ok(AiChatLinkState {
        editable: solved && Utc::now() <= until,
        solved,
        editable_until: until,
        max_links: ai_chat_links::MAX_LINKS_PER_SOLVE,
        providers,
        links: saved
            .as_ref()
            .map(|row| row.links.0.iter().cloned().map(Into::into).collect())
            .unwrap_or_default(),
        revision: saved.as_ref().map_or(0, |row| row.revision),
        updated_at: saved.as_ref().map(|row| row.updated_at),
        submitted_by: saved.and_then(|row| row.submitted_by),
    })
}

fn private_json(value: impl Serialize) -> Response {
    (
        [(header::CACHE_CONTROL, "private, no-store")],
        axum::Json(value),
    )
        .into_response()
}

/// `GET /api/game/{id}/challenges/{challengeId}/ai-chats` — the caller's own
/// team links for one challenge plus the currently accepted providers.
pub async fn get_ai_chat_links(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let ctx = context_info(&st, &user, id, false).await?;
    if !ctx.game.ai_chat_links_enabled {
        return Err(AppError::not_found(DISABLED_MESSAGE));
    }
    challenge_type(st.pg(), id, challenge_id).await?;
    Ok(private_json(
        load_state(&st, &ctx.game, ctx.participation.id, challenge_id).await?,
    ))
}

const SAVE_SQL: &str = r#"
INSERT INTO "AiChatLinks" AS saved
       (game_id, participation_id, challenge_id, links, revision, submitted_by, updated_at)
SELECT $1, $2, $3, $4, 1, $5, now()
 WHERE $6 = 0
    OR EXISTS (SELECT 1 FROM "AiChatLinks"
                WHERE game_id = $1 AND participation_id = $2 AND challenge_id = $3)
ON CONFLICT (game_id, participation_id, challenge_id) DO UPDATE
   SET links = EXCLUDED.links,
       revision = saved.revision + 1,
       submitted_by = EXCLUDED.submitted_by,
       updated_at = now()
 WHERE saved.revision = $6
RETURNING revision
"#;

const DELETE_SQL: &str = r#"
DELETE FROM "AiChatLinks"
 WHERE game_id = $1 AND participation_id = $2 AND challenge_id = $3 AND revision = $4
RETURNING revision
"#;

/// `PUT /api/game/{id}/challenges/{challengeId}/ai-chats` — replace the
/// team's link set with compare-and-swap on `expectedRevision`.
pub async fn save_ai_chat_links(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
    axum::Json(model): axum::Json<SaveAiChatLinks>,
) -> AppResult<Response> {
    if model.expected_revision < 0 {
        return Err(AppError::bad_request(
            "expectedRevision must not be negative",
        ));
    }
    let ctx = context_info(&st, &user, id, false).await?;
    if !ctx.game.ai_chat_links_enabled {
        return Err(AppError::not_found(DISABLED_MESSAGE));
    }
    if Utc::now() > editable_until(&ctx.game) {
        return Err(AppError::bad_request(
            "AI chat links are closed for this event",
        ));
    }
    let rows = ai_chat_links::load_rows(st.pg()).await?;
    let providers = ai_chat_links::enabled_providers(&rows);
    let links = ai_chat_links::validate_submission(&model.links, &providers)?;

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    crate::utils::single_flight::acquire_transaction_advisory_lock_shared(
        &mut transaction,
        &crate::services::live_roster::lock_key(ctx.participation.team_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !crate::services::live_roster::participation_caller_is_live_on(
        &mut *transaction,
        user.id,
        &user.security_stamp,
        id,
        ctx.participation.team_id,
        ctx.participation.id,
        true,
    )
    .await?
    {
        return Err(AppError::Forbidden);
    }
    // Re-check the challenge and the accepted solve inside the roster-fenced
    // transaction so a kick or challenge change cannot race the write.
    challenge_type(&mut *transaction, id, challenge_id).await?;
    if !solved(&mut *transaction, ctx.participation.id, challenge_id).await? {
        return Err(AppError::bad_request(
            "Solve the challenge before attaching AI chat links",
        ));
    }
    let written = if links.is_empty() {
        let deleted: Option<i32> = sqlx::query_scalar(DELETE_SQL)
            .bind(id)
            .bind(ctx.participation.id)
            .bind(challenge_id)
            .bind(model.expected_revision)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        deleted.is_some() || model.expected_revision == 0
    } else {
        sqlx::query_scalar::<_, i32>(SAVE_SQL)
            .bind(id)
            .bind(ctx.participation.id)
            .bind(challenge_id)
            .bind(sqlx::types::Json(&links))
            .bind(user.id)
            .bind(model.expected_revision)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .is_some()
    };
    if !written {
        return Err(AppError::conflict(
            "AI chat links changed; refresh and try again",
        ));
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(private_json(
        load_state(&st, &ctx.game, ctx.participation.id, challenge_id).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatMonitorQuery {
    pub count: Option<i64>,
    pub skip: Option<i64>,
    pub challenge_id: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorAiChatLink {
    pub url: String,
    pub provider_key: String,
    pub provider_label: String,
    pub provider_active: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatLinkRecord {
    pub participation_id: i32,
    pub team_id: i32,
    pub team_name: String,
    pub challenge_id: i32,
    pub challenge_title: String,
    pub category: ChallengeCategory,
    pub links: Vec<MonitorAiChatLink>,
    pub submitted_by: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub updated_at: DateTime<Utc>,
    pub revision: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatLinkPage {
    pub total: i64,
    pub items: Vec<AiChatLinkRecord>,
}

#[derive(sqlx::FromRow)]
struct MonitorRow {
    participation_id: i32,
    team_id: i32,
    team_name: String,
    challenge_id: i32,
    challenge_title: String,
    category: i16,
    links: sqlx::types::Json<Vec<StoredLink>>,
    submitted_by: Option<String>,
    updated_at: DateTime<Utc>,
    revision: i32,
}

/// `GET /api/game/{id}/ai-chats` — every team's saved links, newest first.
/// History stays readable after the event switch is turned off.
pub async fn list_ai_chat_links(
    State(st): State<SharedState>,
    MonitorUser(_user): MonitorUser,
    Path(id): Path<i32>,
    Query(query): Query<AiChatMonitorQuery>,
) -> AppResult<Response> {
    load_game(&st, id).await?;
    let count = query.count.unwrap_or(50).clamp(1, MAX_MONITOR_PAGE);
    let skip = query.skip.unwrap_or(0).max(0);
    let total: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "AiChatLinks"
            WHERE game_id = $1 AND ($2::integer IS NULL OR challenge_id = $2)"#,
    )
    .bind(id)
    .bind(query.challenge_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let rows = sqlx::query_as::<_, MonitorRow>(
        r#"SELECT link.participation_id, participation.team_id, team.name AS team_name,
                  link.challenge_id, challenge.title AS challenge_title, challenge.category,
                  link.links, account.user_name AS submitted_by, link.updated_at, link.revision
             FROM "AiChatLinks" link
             JOIN "Participations" participation ON participation.id = link.participation_id
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "GameChallenges" challenge ON challenge.id = link.challenge_id
             LEFT JOIN "AspNetUsers" account ON account.id = link.submitted_by
            WHERE link.game_id = $1 AND ($2::integer IS NULL OR link.challenge_id = $2)
            ORDER BY link.updated_at DESC, link.participation_id, link.challenge_id
            LIMIT $3 OFFSET $4"#,
    )
    .bind(id)
    .bind(query.challenge_id)
    .bind(count)
    .bind(skip)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let active: HashSet<String> =
        ai_chat_links::enabled_providers(&ai_chat_links::load_rows(st.pg()).await?)
            .into_iter()
            .map(|provider| provider.key)
            .collect();
    let items = rows
        .into_iter()
        .map(|row| {
            let category = ChallengeCategory::try_from_value(&row.category)
                .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(AiChatLinkRecord {
                participation_id: row.participation_id,
                team_id: row.team_id,
                team_name: row.team_name,
                challenge_id: row.challenge_id,
                challenge_title: row.challenge_title,
                category,
                links: row
                    .links
                    .0
                    .into_iter()
                    .map(|link| MonitorAiChatLink {
                        provider_active: active.contains(&link.provider_key),
                        url: link.url,
                        provider_key: link.provider_key,
                        provider_label: link.provider_label,
                    })
                    .collect(),
                submitted_by: row.submitted_by,
                updated_at: row.updated_at,
                revision: row.revision,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(private_json(AiChatLinkPage { total, items }))
}

#[cfg(test)]
#[path = "ai_chats_tests.rs"]
mod tests;
