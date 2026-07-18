//! Ported from RSCTF `Controllers/AdminController.cs` (+ `Services/Config/ConfigService.cs`).
//!
//! Route prefix `/api/admin`, every endpoint requires `AdminUser`. Paths mirror
//! the documented frontend contract exactly — all lowercase except the `MyIp`
//! diagnostic, which the client requests with capitalised casing.
//!
//! Core endpoints (Config, Users, Teams, Logs, Dashboard, Instances, Reviews,
//! Writeups, SubmissionTrend) are implemented faithfully against the sea-orm
//! entities. The long-tail admin surface (auto-build pipeline, repo bindings,
//! anti-cheat, cheat reports, captcha/SMTP diagnostics, files, logo upload,
//! bulk rebuild, container stats) is registered and returns a VALID, WELL-TYPED
//! empty/default success (never a 4xx) so the UI stays functional — see the
//! per-route `// TODO` notes.

pub mod ad;
mod flag_egress;

use std::collections::BTreeMap;
use std::io::{Cursor, Write};

use crate::middlewares::rate_limiter::{limited, Policy};
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use bollard::image::{ListImagesOptions, RemoveImageOptions};
use bollard::Docker;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{AdminUser, CurrentUser};
use crate::models::data::{
    anti_cheat_block, api_token, build_record, challenge_review, config, container, division,
    flag_context, game, game_challenge, game_instance, game_manager, local_file, log_entry,
    participation, repo_binding, repo_binding_scan, submission, suspicion_event, team, team_member,
    user, user_participation,
};
use crate::utils::codec::random_hex;
use crate::utils::crypto_utils::hash_password;
use crate::utils::enums::{
    ChallengeBuildStatus, ChallengeCategory, ParticipationStatus, RepoWatchStatus, ReviewRating,
    Role,
};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{ArrayResponse, MessageResponse, RequestResponse};
pub use flag_egress::*;

// ─── DTOs ──────────────────────────────────────────────────────────────────

/// Paginated user/team list query (`?count=&skip=&search=`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub skip: u64,
    #[serde(default)]
    pub search: Option<String>,
}

fn default_count() -> u64 {
    100
}

/// Body of the `users/search` and `teams/search` endpoints.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchModel {
    #[serde(default)]
    pub hint: String,
}

/// RSCTF `ParticipationEditModel`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationEditModel {
    #[serde(default)]
    pub status: Option<ParticipationStatus>,
    #[serde(default)]
    pub division_id: Option<i32>,
}

// ─── Router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<SharedState> {
    Router::new()
        // --- Diagnostics ---
        .route("/api/admin/MyIp", get(my_ip))
        // --- Config ---
        .route("/api/admin/config", get(get_config).put(update_config))
        .route(
            "/api/admin/config/logo",
            post(logo_upload).delete(logo_delete),
        )
        // --- Dashboard / trends / reviews / cheat reports / writeups ---
        .route("/api/admin/dashboard", get(dashboard))
        .route("/api/admin/Games/{id}/FlagEgress", get(get_flag_egress))
        .route("/api/admin/submissiontrend", get(submission_trend))
        .route("/api/admin/reviews", get(reviews))
        .route("/api/admin/cheat-reports", get(cheat_reports))
        .route("/api/admin/writeups", get(all_writeups))
        .route("/api/admin/writeups/{id}", get(game_writeups))
        .route("/api/admin/writeups/{id}/all", get(download_all_writeups))
        // --- Users ---
        .route("/api/admin/users", get(users).post(add_users))
        .route("/api/admin/users/import", post(import_users))
        .route("/api/admin/users/credentials/send", post(send_credentials))
        .route("/api/admin/users/search", post(search_users))
        .route(
            "/api/admin/users/{userid}",
            get(user_info).put(update_user).delete(delete_user),
        )
        .route("/api/admin/users/{userid}/password", delete(reset_password))
        // --- Teams ---
        .route("/api/admin/teams", get(teams))
        .route("/api/admin/teams/search", post(search_teams))
        .route(
            "/api/admin/teams/{id}",
            put(update_team).delete(delete_team),
        )
        // --- Participation ---
        .route("/api/admin/participation/{id}", put(update_participation))
        // --- Logs ---
        .route("/api/admin/logs", get(logs))
        // --- Instances ---
        .route("/api/admin/instances", get(instances))
        .route("/api/admin/instances/{id}", delete(destroy_instance))
        .route("/api/admin/instances/{id}/stats", get(instance_stats))
        // --- Files ---
        .route("/api/admin/files", get(files))
        // --- Diagnostics: captcha / email test ---
        .route(
            "/api/admin/captcha/test",
            limited(Policy::Concurrency, post(test_captcha)),
        )
        .route(
            "/api/admin/email/test",
            limited(Policy::Concurrency, post(test_email)),
        )
        // --- Bulk rebuild ---
        .route("/api/admin/games/{gameId}/bulkrebuild", post(bulk_rebuild))
        // --- Anti-cheat ---
        .route("/api/admin/anticheatblocks", get(list_anti_cheat_blocks))
        .route(
            "/api/admin/anticheatblocks/{id}",
            delete(delete_anti_cheat_block),
        )
        // --- Auto-build pipeline ---
        .route("/api/admin/builds", get(list_builds))
        .route("/api/admin/builds/inprogress", get(builds_in_progress))
        .route(
            "/api/admin/builds/images",
            get(build_images).delete(delete_build_image),
        )
        .route("/api/admin/builds/bulkdelete", post(bulk_delete_builds))
        .route("/api/admin/builds/prunefailed", post(prune_failed_builds))
        .route("/api/admin/builds/pruneimages", post(prune_images))
        .route("/api/admin/builds/{auditId}", delete(delete_build))
        .route(
            "/api/admin/builds/{auditId}/reenqueue",
            post(reenqueue_build),
        )
        // --- Repo bindings ---
        .route(
            "/api/admin/repobindings",
            get(list_repo_bindings).post(create_repo_binding),
        )
        .route(
            "/api/admin/repobindings/{id}",
            put(update_repo_binding).delete(delete_repo_binding),
        )
        .route("/api/admin/repobindings/{id}/scan", post(scan_repo_binding))
        .route(
            "/api/admin/repobindings/{id}/scans",
            get(repo_binding_scans),
        )
        // Admin A&D controller (round advance, service registration) under admin.
        .merge(ad::router())
}

// ─── Participation ─────────────────────────────────────────────────────────────

/// `PUT /api/admin/participation/{id}` — update a participation's status /
/// division (registration review).
///
/// RSCTF's `AdminController.Participation` is `[RequireUser]`, not
/// `[RequireAdmin]`: a platform Admin OR an EventManager of the participation's
/// game may review it. We mirror that — take a plain `CurrentUser` and gate on
/// `is_admin()` OR a `GameManagers` row for `(game_id, user_id)`. 404-before-403
/// ordering matches RSCTF (it loads the participation, then checks authz).
pub async fn update_participation(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<ParticipationEditModel>,
) -> AppResult<MessageResponse> {
    let mut p = participation::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Participation not found"))?;

    let game_id = p.game_id;

    // Authorization: platform Admin OR a manager (EventManager) of this game.
    if !user.is_admin() {
        let is_manager = game_manager::Entity::find()
            .filter(game_manager::Column::GameId.eq(game_id))
            .filter(game_manager::Column::UserId.eq(user.id))
            .count(&st.db)
            .await?
            > 0;
        if !is_manager {
            return Err(AppError::Forbidden);
        }
    }

    let team_id = p.team_id;
    let roster_guard = if model.status.is_some() {
        let key = format!("team-roster:{team_id}");
        Some(crate::utils::single_flight::coalesce(&key).await)
    } else {
        None
    };
    let distributed_roster = if model.status.is_some() {
        let key = format!("team-roster:{team_id}");
        Some(crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &key).await?)
    } else {
        None
    };
    let mut scoring_control = None;
    if let Some(requested_status) = model.status {
        let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
        p = participation::Entity::find_by_id(id)
            .one(&st.db)
            .await?
            .ok_or_else(|| AppError::not_found("Participation not found"))?;
        let scoring_started = crate::controllers::edit::ad_epoch_scoring_started_locked(
            &mut **control.transaction_mut(),
            game_id,
        )
        .await?;
        crate::controllers::edit::ensure_ad_roster_status_mutable(
            scoring_started,
            Some(p.status),
            requested_status,
        )?;
        scoring_control = Some(control);
    }
    let mut am: participation::ActiveModel = p.into();

    // RSCTF ParticipationRepository.UpdateDivision: the requested division must
    // belong to the participation's game (GetDivision(part.GameId, divId)). A
    // provided divisionId is applied only if such a division exists for this game
    // — an out-of-game / unknown id is ignored, leaving the current value. An
    // explicit null (absent maps to the same in the C# `int?` model) clears it.
    match model.division_id {
        Some(division_id) => {
            let in_game = division::Entity::find()
                .filter(division::Column::Id.eq(division_id))
                .filter(division::Column::GameId.eq(game_id))
                .count(&st.db)
                .await?
                > 0;
            if in_game {
                am.division_id = Set(Some(division_id));
            }
        }
        None => {
            am.division_id = Set(None);
        }
    }

    if let Some(status) = model.status {
        am.status = Set(status);
        // RSCTF UpdateParticipationStatus: clear the division when Rejected. This
        // runs after UpdateDivision, so it overrides any division just set above.
        if status == ParticipationStatus::Rejected {
            am.division_id = Set(None);
        }
    }
    am.update(&st.db).await?;
    if let Some(control) = scoring_control {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }

    if model
        .status
        .is_some_and(|status| status != ParticipationStatus::Accepted)
    {
        crate::controllers::team::revoke_participation_capabilities(&st, id).await?;
    }

    // RSCTF UpdateParticipationStatus: when a participation is Accepted, lock the
    // team so its roster is frozen, then provision its play resources —
    // ParticipationRepository.EnsureInstances (a GameInstance per enabled+Active
    // challenge) plus, for a self-hosted A&D game, the team's service containers
    // (best-effort on a Docker outage). Runs AFTER the status update is persisted.
    let accepting = model.status == Some(ParticipationStatus::Accepted);
    if accepting {
        if let Some(t) = team::Entity::find_by_id(team_id).one(&st.db).await? {
            let mut tm: team::ActiveModel = t.into();
            tm.locked = Set(true);
            tm.update(&st.db).await?;
        }
    }
    if let Some(lock) = distributed_roster {
        lock.release().await?;
    }
    drop(roster_guard);
    if accepting {
        crate::controllers::edit::provision_accepted_participation(&st, game_id, id).await?;
    }

    // RSCTF FlushScoreboardCache (+ FlushAdScoreboardCacheIncludingFrozen for
    // A&D/KotH): participation status is a scoring input, so a review ruling must
    // evict the cached boards. Clear the whole scoreboard cache family for the
    // game — a superset of RSCTF's jeopardy-always + AD/KotH-conditional eviction;
    // removing an absent key is a no-op. Mirrors edit::reviews::flush_scoreboard.
    for key in [
        format!("_ScoreBoard_{game_id}"),
        format!("_ScoreBoardFrozen_{game_id}"),
        format!("_KothScoreBoard_{game_id}"),
        format!("_KothScoreBoardFrozen_{game_id}"),
        format!("_KothTimeline_{game_id}"),
        format!("_KothTimelineFrozen_{game_id}"),
    ] {
        st.cache.remove(&key).await;
    }
    crate::controllers::game::ad::hard_invalidate_ad_scoreboard(&st, game_id).await;
    // A review ruling (accept / reject / ban) changes the team's access — flush every
    // member's cached participation so it takes effect at once, not on the 5s TTL.
    crate::controllers::game::ad::flush_participation_cache(&st, game_id, id).await;

    Ok(MessageResponse::ok(""))
}

// ─── Dashboard ───────────────────────────────────────────────────────────────

/// RSCTF `SystemStatsModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatsModel {
    pub user_count: i64,
    pub team_count: i64,
    pub active_container_count: i64,
}

/// RSCTF `BasicGameInfoModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BasicGameInfoModel {
    pub id: i32,
    pub title: String,
    pub summary: String,
    pub poster: Option<String>,
    pub limit: i32,
    pub team_count: i64,
    pub user_count: i64,
    /// Fraction of positive (Like) ratings among decisive (Like/Dislike)
    /// challenge reviews for the game; `null` when there are none — mirrors
    /// RSCTF's nullable `AverageRating`.
    pub average_rating: Option<f64>,
    /// Total number of challenge-review rows for the game (RSCTF `ReviewCount`).
    pub review_count: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub start: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub end: DateTime<Utc>,
}

/// RSCTF `AdminDashboardModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDashboardModel {
    pub system_stats: SystemStatsModel,
    pub top_games: Vec<BasicGameInfoModel>,
}

/// `GET /api/admin/dashboard` — platform-wide stats + top games by team count.
pub async fn dashboard(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<RequestResponse<AdminDashboardModel>> {
    let user_count = user::Entity::find().count(&st.db).await? as i64;
    let team_count = team::Entity::find().count(&st.db).await? as i64;
    let active_container_count = container::Entity::find().count(&st.db).await? as i64;

    let games = game::Entity::find()
        .order_by_desc(game::Column::Id)
        .limit(50)
        .all(&st.db)
        .await?;

    let mut top_games = Vec::with_capacity(games.len());
    for g in games {
        let tc = participation::Entity::find()
            .filter(participation::Column::GameId.eq(g.id))
            .count(&st.db)
            .await? as i64;
        top_games.push(BasicGameInfoModel {
            id: g.id,
            title: g.title,
            summary: g.summary,
            poster: g.poster_hash.map(|h| format!("/assets/{h}/poster")),
            limit: g.team_member_count_limit,
            team_count: tc,
            user_count: tc,
            average_rating: None,
            review_count: 0,
            start: g.start_time_utc,
            end: g.end_time_utc,
        });
    }
    top_games.sort_by_key(|game| std::cmp::Reverse(game.team_count));
    top_games.truncate(5);

    // Per top game, derive the review stats RSCTF's dashboard reports: the total
    // review count, and the average rating as the fraction of decisive reviews
    // that are positive. RSCTF's `ReviewRating` is stored numerically
    // (Dislike = 1, Like = 2); rsctf's enum shares those discriminants, so we
    // read the raw `i16` value rather than the (differently-named) variants.
    for g in top_games.iter_mut() {
        let reviews = challenge_review::Entity::find()
            .filter(challenge_review::Column::GameId.eq(g.id))
            .all(&st.db)
            .await?;
        g.review_count = reviews.len() as i32;
        let mut decisive = 0i64;
        let mut likes = 0i64;
        for r in &reviews {
            match r.rating as i16 {
                2 => {
                    likes += 1;
                    decisive += 1;
                }
                1 => decisive += 1,
                _ => {}
            }
        }
        g.average_rating = (decisive > 0).then(|| likes as f64 / decisive as f64);
    }

    Ok(RequestResponse::ok(AdminDashboardModel {
        system_stats: SystemStatsModel {
            user_count,
            team_count,
            active_container_count,
        },
        top_games,
    }))
}

// ─── Container instances ───────────────────────────────────────────────────────

/// `GET /api/admin/files` — paginated uploaded-file listing.
pub async fn files(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<ArrayResponse<LocalFileModel>> {
    let count = q.count.clamp(0, 500);
    let total = local_file::Entity::find()
        .filter(local_file::Column::ReferenceCount.gt(0))
        .count(&st.db)
        .await? as i64;
    let rows = local_file::Entity::find()
        .filter(local_file::Column::ReferenceCount.gt(0))
        .order_by_asc(local_file::Column::Id)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;

    let data = rows
        .into_iter()
        .map(|f| LocalFileModel {
            hash: f.hash,
            name: f.name,
        })
        .collect();
    Ok(ArrayResponse::new(data, total))
}

// ─── Challenge reviews ─────────────────────────────────────────────────────────

/// RSCTF `ChallengeReviewDetailModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeReviewDetailModel {
    pub id: i32,
    pub challenge_id: i32,
    pub challenge_name: String,
    pub game_title: String,
    pub user_id: Uuid,
    pub user_name: String,
    pub rating: ReviewRating,
    pub comment: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub submit_time_utc: DateTime<Utc>,
}

/// `GET /api/admin/reviews` — recent challenge reviews (raw array), newest first.
pub async fn reviews(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<RequestResponse<Vec<ChallengeReviewDetailModel>>> {
    let count = q.count.clamp(0, 1000);
    let rows = challenge_review::Entity::find()
        .order_by_desc(challenge_review::Column::SubmitTimeUtc)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;

    let mut data = Vec::with_capacity(rows.len());
    for r in rows {
        let (challenge_name, game_title) = match game_challenge::Entity::find_by_id(r.challenge_id)
            .one(&st.db)
            .await?
        {
            Some(ch) => {
                let title = game::Entity::find_by_id(ch.game_id)
                    .one(&st.db)
                    .await?
                    .map(|g| g.title)
                    .unwrap_or_default();
                (ch.title, title)
            }
            None => (String::new(), String::new()),
        };
        let user_name = user::Entity::find_by_id(r.user_id)
            .one(&st.db)
            .await?
            .and_then(|u| u.user_name)
            .unwrap_or_default();

        data.push(ChallengeReviewDetailModel {
            id: r.id,
            challenge_id: r.challenge_id,
            challenge_name,
            game_title,
            user_id: r.user_id,
            user_name,
            rating: r.rating,
            comment: r.comment,
            submit_time_utc: r.submit_time_utc,
        });
    }

    Ok(RequestResponse::ok(data))
}

// ─── Submission trend ──────────────────────────────────────────────────────────

/// RSCTF `SubmissionTrendModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionTrendModel {
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
    pub count: i64,
}

/// Query for `GET /api/admin/submissiontrend`: the RSCTF `range` selector
/// (`Day` | `Week` | `Month` | `Year`).
#[derive(Debug, Default, Deserialize)]
pub struct SubmissionTrendQuery {
    pub range: Option<String>,
}

/// `GET /api/admin/submissiontrend` — submissions over a window bucketed per
/// RSCTF `AdminController.GetSubmissionTrend`:
///   * `Day` (default) — last 24h, by hour.
///   * `Week` — last 7 days, by day.
///   * `Month` — last 30 days, by day.
///   * `Year` — last 12 months, by month.
///
/// Returns a raw array ascending by bucket time.
pub async fn submission_trend(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<SubmissionTrendQuery>,
) -> AppResult<RequestResponse<Vec<SubmissionTrendModel>>> {
    let range = q.range.unwrap_or_default().to_lowercase();
    let now = Utc::now();
    let since = match range.as_str() {
        "week" => now - Duration::days(7),
        "month" => now - Duration::days(30),
        "year" => now - Duration::days(365),
        // "day" and anything else
        _ => now - Duration::hours(24),
    };

    let subs = submission::Entity::find()
        .filter(submission::Column::SubmitTimeUtc.gte(since))
        .all(&st.db)
        .await?;

    let mut buckets: BTreeMap<DateTime<Utc>, i64> = BTreeMap::new();
    for s in subs {
        let t = s.submit_time_utc;
        let key = match range.as_str() {
            // Group by month (first day of month, midnight UTC).
            "year" => NaiveDate::from_ymd_opt(t.year(), t.month(), 1)
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|dt| dt.and_utc())
                .unwrap_or(t),
            // Group by day (midnight UTC).
            "week" | "month" => t
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .map(|dt| dt.and_utc())
                .unwrap_or(t),
            // Group by hour.
            _ => t
                .with_minute(0)
                .and_then(|t| t.with_second(0))
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(t),
        };
        *buckets.entry(key).or_insert(0) += 1;
    }

    let data = buckets
        .into_iter()
        .map(|(time, count)| SubmissionTrendModel { time, count })
        .collect();
    Ok(RequestResponse::ok(data))
}

// ─── Writeups ──────────────────────────────────────────────────────────────────

/// RSCTF `WriteupInfo`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteupInfo {
    pub id: i32,
    pub team: TeamInfoModel,
    pub game_title: String,
    pub url: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub upload_time_utc: DateTime<Utc>,
    pub division_id: Option<i32>,
}

/// RSCTF `WriteupInfoModel` (per-game view).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteupInfoModel {
    pub divisions: BTreeMap<String, String>,
    pub writeups: Vec<WriteupInfo>,
}

/// Materialise a single participation's writeup, if it carries one.
async fn writeup_for(st: &SharedState, p: &participation::Model) -> AppResult<Option<WriteupInfo>> {
    let Some(wid) = p.writeup_id else {
        return Ok(None);
    };
    let Some(f) = local_file::Entity::find_by_id(wid).one(&st.db).await? else {
        return Ok(None);
    };

    let team = team::Entity::find_by_id(p.team_id)
        .one(&st.db)
        .await?
        .map(TeamInfoModel::from)
        .unwrap_or_else(|| TeamInfoModel {
            id: p.team_id,
            name: String::new(),
            bio: None,
            avatar: None,
            locked: false,
            members: Vec::new(),
        });
    let game_title = game::Entity::find_by_id(p.game_id)
        .one(&st.db)
        .await?
        .map(|g| g.title)
        .unwrap_or_default();

    Ok(Some(WriteupInfo {
        id: p.id,
        team,
        game_title,
        url: format!("/assets/{}/{}", f.hash, f.name),
        upload_time_utc: f.upload_time_utc,
        division_id: p.division_id,
    }))
}

/// `GET /api/admin/writeups` — every submitted writeup across all games (raw array).
pub async fn all_writeups(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<RequestResponse<Vec<WriteupInfo>>> {
    let count = q.count.clamp(0, 1000);
    let parts = participation::Entity::find()
        .filter(participation::Column::WriteupId.is_not_null())
        .order_by_desc(participation::Column::Id)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;

    let mut data = Vec::with_capacity(parts.len());
    for p in parts {
        if let Some(w) = writeup_for(&st, &p).await? {
            data.push(w);
        }
    }
    Ok(RequestResponse::ok(data))
}

/// `GET /api/admin/writeups/{id}` — writeups submitted for a single game.
pub async fn game_writeups(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<WriteupInfoModel>> {
    game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;

    // All of the game's divisions (id -> name), like RSCTF's GetWriteups.
    let divisions = division::Entity::find()
        .filter(division::Column::GameId.eq(id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|d| (d.id.to_string(), d.name))
        .collect();

    let parts = participation::Entity::find()
        .filter(participation::Column::GameId.eq(id))
        .filter(participation::Column::WriteupId.is_not_null())
        .all(&st.db)
        .await?;

    let mut writeups = Vec::with_capacity(parts.len());
    for p in parts {
        if let Some(w) = writeup_for(&st, &p).await? {
            writeups.push(w);
        }
    }

    Ok(RequestResponse::ok(WriteupInfoModel {
        divisions,
        writeups,
    }))
}

/// `GET /api/admin/writeups/{id}/all` — download every writeup for a game as a
/// single zip archive (RSCTF streams a tar; a zip is the in-crate equivalent and
/// what the task calls for). Each participation's writeup blob is pulled from
/// `st.storage` and added under a per-team, collision-free entry name. A blob
/// that fails to load is skipped rather than failing the whole download.
pub async fn download_all_writeups(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<Response> {
    let game = game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;

    let parts = participation::Entity::find()
        .filter(participation::Column::GameId.eq(id))
        .filter(participation::Column::WriteupId.is_not_null())
        .all(&st.db)
        .await?;

    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        for p in parts {
            let Some(wid) = p.writeup_id else { continue };
            let Some(file) = local_file::Entity::find_by_id(wid).one(&st.db).await? else {
                continue;
            };
            // Degrade instead of 500ing when a blob is missing from storage.
            let Ok(bytes) = st.storage.load(&file.hash).await else {
                continue;
            };

            let team_name = team::Entity::find_by_id(p.team_id)
                .one(&st.db)
                .await?
                .map(|t| t.name)
                .unwrap_or_else(|| format!("team-{}", p.team_id));
            let entry = format!("{}-{}-{}", p.id, sanitize_entry(&team_name), file.name);

            if zip.start_file(entry, options).is_err() {
                continue;
            }
            let _ = zip.write_all(&bytes);
        }

        zip.finish()
            .map_err(|e| AppError::internal(format!("zip finish: {e}")))?;
    }

    let filename = format!(
        "Writeups-{}-{}.zip",
        sanitize_entry(&game.title),
        Utc::now().format("%Y%m%d-%H.%M.%S")
    );
    let disposition = format!("attachment; filename=\"{filename}\"");

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        Body::from(buf.into_inner()),
    )
        .into_response())
}

/// Strip path separators / control characters from a zip entry component so a
/// crafted team or game name can't escape the archive or break the header.
fn sanitize_entry(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | '\n' | '\r' | '"' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

// ─── Auto-build / repo-binding typed stubs ──────────────────────────────────────

/// RSCTF `BulkRebuildResultModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkRebuildResultModel {
    pub enqueued: i32,
    pub skipped: i32,
    pub messages: Vec<String>,
}

/// RSCTF `ChallengeAuditModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeAuditModel {
    pub yaml_text: Option<String>,
    pub files: Vec<Value>,
    pub previews: BTreeMap<String, String>,
    pub archive_available: bool,
    pub build_status: Option<String>,
    pub last_build_log: Option<String>,
}

/// Generic empty-array success for `Model[]` endpoints that aren't backed here.
// TODO: implement the auto-build / repo-binding / anti-cheat pipelines.
pub async fn empty_array(_admin: AdminUser) -> RequestResponse<Vec<Value>> {
    RequestResponse::ok(Vec::new())
}

/// Generic `void` success (200, empty envelope).
pub async fn void_ok(_admin: AdminUser) -> MessageResponse {
    MessageResponse::ok("")
}

/// Default `PruneResultModel` success.
pub async fn prune_result(_admin: AdminUser) -> RequestResponse<PruneResultModel> {
    RequestResponse::ok(PruneResultModel {
        removed: 0,
        messages: Vec::new(),
    })
}

/// Default `BulkRebuildResultModel` success.
pub async fn bulk_rebuild(
    _admin: AdminUser,
    Path(_game_id): Path<i32>,
) -> RequestResponse<BulkRebuildResultModel> {
    RequestResponse::ok(BulkRebuildResultModel {
        enqueued: 0,
        skipped: 0,
        messages: Vec::new(),
    })
}

/// Default `ChallengeAuditModel` success.
pub async fn challenge_audit(
    _admin: AdminUser,
    Path(_audit_id): Path<i64>,
) -> RequestResponse<ChallengeAuditModel> {
    RequestResponse::ok(ChallengeAuditModel {
        yaml_text: None,
        files: Vec::new(),
        previews: BTreeMap::new(),
        archive_available: false,
        build_status: None,
        last_build_log: None,
    })
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

async fn load_user(st: &SharedState, id: Uuid) -> AppResult<user::Model> {
    user::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("User not found"))
}

/// Generate a random, human-typable reset password.
fn generate_password() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let uuid = Uuid::new_v4();
    let bytes = uuid.as_bytes();
    let mut out = String::with_capacity(16);
    for b in bytes.iter().take(16) {
        out.push(ALPHABET[(*b as usize) % ALPHABET.len()] as char);
    }
    out
}

// ─── Submodules ────────────────────────────────────────────────────────────────

mod anti_cheat;
mod builds;
mod diagnostics;
mod instances;
mod logs;
mod repo_bindings;
mod settings;
mod teams;
mod users;
mod users_credentials;
mod users_mutate;
pub use anti_cheat::*;
pub use builds::*;
pub use diagnostics::*;
pub use instances::*;
pub use logs::*;
pub use repo_bindings::*;
pub use settings::*;
pub use teams::*;
pub use users::*;
pub use users_credentials::*;
pub use users_mutate::*;
