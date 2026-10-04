//! Team-scoped AI chat disclosure on solved Jeopardy challenges: public share
//! links or an explicit "no AI used" declaration. Links are validated against
//! the admin provider registry and are never fetched by the server. Every
//! create, edit and clear is appended to `AiChatLinkEvents` for cheat review.

use axum::response::IntoResponse;
use sea_orm::ActiveEnum;

use super::*;
use crate::services::ai_chat_links::{self, MatchedLink};

pub(super) const DISABLED_MESSAGE: &str = "AI chat links are not enabled for this event";
const NOT_JEOPARDY: &str = "AI chat links apply only to Jeopardy challenges";
const MAX_PENDING_CHALLENGES: i64 = 500;

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
    /// The event requires a disclosure after every competitive solve.
    pub required: bool,
    /// Required, solved inside the competition window, and not yet disclosed.
    pub pending: bool,
    pub declared_no_ai: bool,
    #[serde(with = "crate::utils::datetime::millis")]
    pub editable_until: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub solved_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub first_disclosed_at: Option<DateTime<Utc>>,
    pub edit_count: i64,
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
    #[serde(default)]
    pub no_ai_used: bool,
    pub expected_revision: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct StoredLink {
    pub(super) url: String,
    pub(super) provider_key: String,
    pub(super) provider_label: String,
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

impl From<&MatchedLink> for StoredLink {
    fn from(link: &MatchedLink) -> Self {
        Self {
            url: link.url.clone(),
            provider_key: link.provider_key.clone(),
            provider_label: link.provider_label.clone(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct SavedRow {
    links: sqlx::types::Json<Vec<StoredLink>>,
    declared_no_ai: bool,
    revision: i32,
    updated_at: DateTime<Utc>,
    submitted_by: Option<String>,
}

#[derive(sqlx::FromRow)]
struct CurrentRow {
    links: sqlx::types::Json<Vec<StoredLink>>,
    declared_no_ai: bool,
    revision: i32,
}

#[derive(sqlx::FromRow)]
struct SolveTiming {
    solved_at: Option<DateTime<Utc>>,
    in_window: bool,
    first_disclosed_at: Option<DateTime<Utc>>,
    edit_count: i64,
}

/// Disclosures close with the later of the event end and the writeup
/// deadline, so they cannot be rewritten while organizers review them.
pub(super) fn editable_until(game: &game::Model) -> DateTime<Utc> {
    game.end_time_utc.max(game.writeup_deadline)
}

pub(super) fn required(game: &game::Model) -> bool {
    game.ai_chat_links_enabled && game.ai_chat_links_required
}

fn jeopardy(challenge_type: ChallengeType) -> bool {
    !challenge_type.uses_ad_engine()
}

/// An enabled Jeopardy challenge of the game; `not_jeopardy` is the 404
/// message for an Attack-Defense or KotH challenge.
pub(super) async fn jeopardy_challenge<'e, E>(
    executor: E,
    game_id: i32,
    challenge_id: i32,
    not_jeopardy: &'static str,
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
        return Err(AppError::not_found(not_jeopardy));
    }
    Ok(challenge_type)
}

pub(super) async fn solved<'e, E>(
    executor: E,
    participation_id: i32,
    challenge_id: i32,
) -> AppResult<bool>
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

/// The team's canonical solve time, whether it fell inside the official
/// competition window, and the disclosure history summary.
async fn solve_timing(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
) -> AppResult<SolveTiming> {
    sqlx::query_as::<_, SolveTiming>(
        r#"SELECT solve.submit_time_utc AS solved_at,
                  COALESCE(solve.submit_time_utc >= game.start_time_utc
                           AND solve.submit_time_utc < game.end_time_utc, FALSE) AS in_window,
                  history.first_disclosed_at,
                  COALESCE(history.edit_count, 0) AS edit_count
             FROM "Games" game
             LEFT JOIN LATERAL (
                 SELECT submission.submit_time_utc
                   FROM "FirstSolves" first_solve
                   JOIN "Submissions" submission ON submission.id = first_solve.submission_id
                  WHERE first_solve.participation_id = $2 AND first_solve.challenge_id = $3
                    AND submission.game_id = game.id
             ) solve ON TRUE
             LEFT JOIN LATERAL (
                 SELECT MIN(event.occurred_at) FILTER (WHERE event.action <> 'Cleared')
                            AS first_disclosed_at,
                        COUNT(*) FILTER (WHERE event.action = 'Edited') AS edit_count
                   FROM "AiChatLinkEvents" event
                  WHERE event.game_id = game.id AND event.participation_id = $2
                    AND event.challenge_id = $3
             ) history ON TRUE
            WHERE game.id = $1"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(challenge_id)
    .fetch_one(pool)
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
        r#"SELECT link.links, link.declared_no_ai, link.revision, link.updated_at,
                  account.user_name AS submitted_by
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
    let timing = solve_timing(st.pg(), game.id, participation_id, challenge_id).await?;
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
    let required = required(game);
    Ok(AiChatLinkState {
        editable: solved && Utc::now() <= until,
        solved,
        required,
        pending: required && timing.in_window && saved.is_none(),
        declared_no_ai: saved.as_ref().is_some_and(|row| row.declared_no_ai),
        editable_until: until,
        solved_at: timing.solved_at,
        first_disclosed_at: timing.first_disclosed_at,
        edit_count: timing.edit_count,
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

pub(super) fn private_json(value: impl Serialize) -> Response {
    (
        [(header::CACHE_CONTROL, "private, no-store")],
        axum::Json(value),
    )
        .into_response()
}

/// `GET /api/game/{id}/challenges/{challengeId}/ai-chats` — the caller's own
/// team disclosure for one challenge plus the currently accepted providers.
pub async fn get_ai_chat_links(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    let ctx = context_info(&st, &user, id, false).await?;
    if !ctx.game.ai_chat_links_enabled {
        return Err(AppError::not_found(DISABLED_MESSAGE));
    }
    jeopardy_challenge(st.pg(), id, challenge_id, NOT_JEOPARDY).await?;
    Ok(private_json(
        load_state(&st, &ctx.game, ctx.participation.id, challenge_id).await?,
    ))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatPendingList {
    pub required: bool,
    pub challenge_ids: Vec<i32>,
}

/// `GET /api/game/{id}/ai-chats/pending` — the caller's team's competitive
/// Jeopardy solves that still need a required disclosure. Not polled.
pub async fn pending_ai_chat_links(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<Response> {
    let ctx = context_info(&st, &user, id, false).await?;
    if !ctx.game.ai_chat_links_enabled {
        return Err(AppError::not_found(DISABLED_MESSAGE));
    }
    let required = required(&ctx.game);
    let challenge_ids = if required {
        sqlx::query_scalar::<_, i32>(
            r#"SELECT first_solve.challenge_id
                 FROM "FirstSolves" first_solve
                 JOIN "Submissions" submission ON submission.id = first_solve.submission_id
                 JOIN "Games" game ON game.id = submission.game_id
                 JOIN "GameChallenges" challenge
                   ON challenge.id = first_solve.challenge_id AND challenge.game_id = game.id
                WHERE first_solve.participation_id = $1 AND game.id = $2
                  AND challenge.is_enabled AND challenge."Type" = ANY($3)
                  AND submission.submit_time_utc >= game.start_time_utc
                  AND submission.submit_time_utc < game.end_time_utc
                  AND NOT EXISTS (
                      SELECT 1 FROM "AiChatLinks" link
                       WHERE link.game_id = game.id
                         AND link.participation_id = first_solve.participation_id
                         AND link.challenge_id = first_solve.challenge_id
                  )
                ORDER BY submission.submit_time_utc, first_solve.challenge_id
                LIMIT $4"#,
        )
        .bind(ctx.participation.id)
        .bind(id)
        .bind(JEOPARDY_TYPES.map(|kind| kind as i16).to_vec())
        .bind(MAX_PENDING_CHALLENGES)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
    } else {
        Vec::new()
    };
    Ok(private_json(AiChatPendingList {
        required,
        challenge_ids,
    }))
}

pub(super) const JEOPARDY_TYPES: [ChallengeType; 4] = [
    ChallengeType::StaticAttachment,
    ChallengeType::StaticContainer,
    ChallengeType::DynamicAttachment,
    ChallengeType::DynamicContainer,
];

const SAVE_SQL: &str = r#"
INSERT INTO "AiChatLinks" AS saved
       (game_id, participation_id, challenge_id, links, declared_no_ai, revision,
        submitted_by, updated_at)
SELECT $1, $2, $3, $4, $7, 1, $5, now()
 WHERE $6 = 0
    OR EXISTS (SELECT 1 FROM "AiChatLinks"
                WHERE game_id = $1 AND participation_id = $2 AND challenge_id = $3)
ON CONFLICT (game_id, participation_id, challenge_id) DO UPDATE
   SET links = EXCLUDED.links,
       declared_no_ai = EXCLUDED.declared_no_ai,
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

/// One append-only telemetry row. The database supplies `occurred_at` and the
/// solve-relative delay; the client never supplies a time.
const EVENT_SQL: &str = r#"
INSERT INTO "AiChatLinkEvents"
       (game_id, participation_id, challenge_id, user_id, action, revision,
        previous_links, links, added_urls, removed_urls,
        previous_declared_no_ai, declared_no_ai, solved_at, seconds_since_solve,
        remote_ip_hash)
SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
       solve.submit_time_utc,
       CASE WHEN solve.submit_time_utc IS NULL THEN NULL
            ELSE floor(EXTRACT(EPOCH FROM clock_timestamp() - solve.submit_time_utc))::bigint
       END,
       $13
  FROM (SELECT (SELECT submission.submit_time_utc
                  FROM "FirstSolves" first_solve
                  JOIN "Submissions" submission ON submission.id = first_solve.submission_id
                 WHERE first_solve.participation_id = $2 AND first_solve.challenge_id = $3
                   AND submission.game_id = $1) AS submit_time_utc) solve
"#;

/// URL-level difference between two link sets, preserving order.
fn diff_urls(previous: &[StoredLink], next: &[StoredLink]) -> (Vec<String>, Vec<String>) {
    let added = next
        .iter()
        .filter(|link| !previous.iter().any(|old| old.url == link.url))
        .map(|link| link.url.clone())
        .collect();
    let removed = previous
        .iter()
        .filter(|link| !next.iter().any(|new| new.url == link.url))
        .map(|link| link.url.clone())
        .collect();
    (added, removed)
}

/// `PUT /api/game/{id}/challenges/{challengeId}/ai-chats` — replace the team
/// disclosure with compare-and-swap on `expectedRevision`. `noAiUsed` records
/// an explicit declaration; an empty set without it clears the disclosure.
pub async fn save_ai_chat_links(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    axum::Json(model): axum::Json<SaveAiChatLinks>,
) -> AppResult<Response> {
    if model.expected_revision < 0 {
        return Err(AppError::bad_request(
            "expectedRevision must not be negative",
        ));
    }
    if model.no_ai_used && !model.links.is_empty() {
        return Err(AppError::bad_request(
            "Choose either AI chat links or \"No AI used\", not both",
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
    let next_links = ai_chat_links::validate_submission(&model.links, &providers)?
        .iter()
        .map(StoredLink::from)
        .collect::<Vec<_>>();
    let remote_ip_hash = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()))
        .and_then(|ip| {
            crate::services::anti_cheat::hash_ip_identity(st.config.as_ref(), &ip)
                .map(|identity| identity.exact)
        });

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
    jeopardy_challenge(&mut *transaction, id, challenge_id, NOT_JEOPARDY).await?;
    if !solved(&mut *transaction, ctx.participation.id, challenge_id).await? {
        return Err(AppError::bad_request(
            "Solve the challenge before attaching AI chat links",
        ));
    }
    let current = sqlx::query_as::<_, CurrentRow>(
        r#"SELECT links, declared_no_ai, revision FROM "AiChatLinks"
            WHERE game_id = $1 AND participation_id = $2 AND challenge_id = $3
            FOR UPDATE"#,
    )
    .bind(id)
    .bind(ctx.participation.id)
    .bind(challenge_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if current.as_ref().map_or(0, |row| row.revision) != model.expected_revision {
        return Err(AppError::conflict(
            "AI chat links changed; refresh and try again",
        ));
    }
    let previous_links = current
        .as_ref()
        .map(|row| row.links.0.clone())
        .unwrap_or_default();
    let previous_declared = current.as_ref().is_some_and(|row| row.declared_no_ai);
    let clearing = next_links.is_empty() && !model.no_ai_used;
    let unchanged = match &current {
        Some(_) => {
            !clearing && previous_links == next_links && previous_declared == model.no_ai_used
        }
        None => clearing,
    };
    if !unchanged {
        let action = if clearing {
            "Cleared"
        } else if current.is_none() {
            "Created"
        } else {
            "Edited"
        };
        let revision = if clearing {
            sqlx::query_scalar::<_, i32>(DELETE_SQL)
                .bind(id)
                .bind(ctx.participation.id)
                .bind(challenge_id)
                .bind(model.expected_revision)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?
                .map(|_| 0)
        } else {
            sqlx::query_scalar::<_, i32>(SAVE_SQL)
                .bind(id)
                .bind(ctx.participation.id)
                .bind(challenge_id)
                .bind(sqlx::types::Json(&next_links))
                .bind(user.id)
                .bind(model.expected_revision)
                .bind(model.no_ai_used)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?
        };
        let Some(revision) = revision else {
            return Err(AppError::conflict(
                "AI chat links changed; refresh and try again",
            ));
        };
        let (added, removed) = diff_urls(&previous_links, &next_links);
        sqlx::query(EVENT_SQL)
            .bind(id)
            .bind(ctx.participation.id)
            .bind(challenge_id)
            .bind(user.id)
            .bind(action)
            .bind(revision)
            .bind(sqlx::types::Json(&previous_links))
            .bind(sqlx::types::Json(&next_links))
            .bind(sqlx::types::Json(&added))
            .bind(sqlx::types::Json(&removed))
            .bind(previous_declared)
            .bind(!clearing && model.no_ai_used)
            .bind(remote_ip_hash.as_deref())
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !clearing && model.no_ai_used {
        // A "No AI used" declaration on a challenge whose own solver carries an
        // agent artifact is a contradiction. Evidence only; never fails the save.
        match crate::services::agent_artifacts::evaluate_contradiction(
            &st,
            id,
            ctx.participation.id,
            challenge_id,
        )
        .await
        {
            Ok(true) => crate::controllers::game::invalidate_cheat_report(&st, id).await,
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, "AI declaration contradiction check failed"),
        }
    }
    Ok(private_json(
        load_state(&st, &ctx.game, ctx.participation.id, challenge_id).await?,
    ))
}

#[cfg(test)]
#[path = "ai_chats_tests.rs"]
pub(super) mod tests;
