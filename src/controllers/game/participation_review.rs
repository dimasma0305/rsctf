//! Event-participation review projections and compatibility API.
//!
//! The bounded page deliberately excludes member identities and profile fields;
//! the admin UI loads those fields for one team only after opening it. The
//! original raw-array endpoint remains available for external API compatibility.

use super::*;
use crate::models::data::{game_manager, user_participation};

const DEFAULT_REVIEW_PAGE_SIZE: u64 = 10;
const MAX_REVIEW_PAGE_SIZE: u64 = 50;
const MAX_REVIEW_SEARCH_CHARS: usize = 100;
const MAX_REVIEW_SKIP: u64 = 1_000_000;

/// RSCTF `TeamWithDetailedUserInfo` used by the legacy participation endpoint.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamWithDetailedUserInfo {
    pub id: i32,
    pub locked: bool,
    pub captain_id: Uuid,
    pub name: Option<String>,
    pub bio: Option<String>,
    pub avatar: Option<String>,
    pub members: Vec<Json>,
}

/// RSCTF `ParticipationInfoModel` returned by the compatibility endpoint.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationInfoModel {
    pub id: i32,
    pub team: TeamWithDetailedUserInfo,
    /// User-id GUIDs of the members registered for this participation.
    pub registered_members: Vec<Uuid>,
    pub division_id: Option<i32>,
    pub status: ParticipationStatus,
}

/// `GET /api/game/{id}/participations` — the existing RSCTF-compatible raw
/// participation array. The admin UI uses the separate bounded `/page` route,
/// while this endpoint remains stable for existing API consumers.
pub async fn participations(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<ParticipationInfoModel>>> {
    let _ = load_game(&st, id).await?;

    if !user.is_admin()
        && game_manager::Entity::find()
            .filter(game_manager::Column::GameId.eq(id))
            .filter(game_manager::Column::UserId.eq(user.id))
            .count(&st.db)
            .await?
            == 0
    {
        return Err(AppError::Forbidden);
    }

    let parts = participation::Entity::find()
        .filter(participation::Column::GameId.eq(id))
        .order_by_asc(participation::Column::TeamId)
        .all(&st.db)
        .await?;
    let team_ids: Vec<i32> = parts.iter().map(|part| part.team_id).collect();
    let teams: HashMap<i32, team::Model> = if team_ids.is_empty() {
        HashMap::new()
    } else {
        team::Entity::find()
            .filter(team::Column::Id.is_in(team_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|team| (team.id, team))
            .collect()
    };

    let links = user_participation::Entity::find()
        .filter(user_participation::Column::GameId.eq(id))
        .all(&st.db)
        .await?;
    let mut members_by_part: HashMap<i32, Vec<Uuid>> = HashMap::new();
    for link in &links {
        members_by_part
            .entry(link.participation_id)
            .or_default()
            .push(link.user_id);
    }

    let roster_rows = if teams.is_empty() {
        Vec::new()
    } else {
        team_member::Entity::find()
            .filter(team_member::Column::TeamId.is_in(teams.keys().copied().collect::<Vec<_>>()))
            .all(&st.db)
            .await?
    };
    let mut roster_by_team: HashMap<i32, Vec<Uuid>> = HashMap::new();
    for row in &roster_rows {
        roster_by_team
            .entry(row.team_id)
            .or_default()
            .push(row.user_id);
    }
    let mut member_ids: HashSet<Uuid> = roster_rows.iter().map(|row| row.user_id).collect();
    for team in teams.values() {
        member_ids.insert(team.captain_id);
    }
    let member_users: HashMap<Uuid, user::Model> = if member_ids.is_empty() {
        HashMap::new()
    } else {
        user::Entity::find()
            .filter(user::Column::Id.is_in(member_ids.into_iter().collect::<Vec<_>>()))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|user| (user.id, user))
            .collect()
    };
    let member_info = |user: &user::Model| -> Json {
        serde_json::json!({
            "userId": user.id,
            "role": user.role,
            "userName": user.user_name,
            "email": user.email,
            "bio": user.bio,
            "phone": user.phone_number,
            "realName": user.real_name,
            "stdNumber": user.std_number,
            "avatar": user.avatar_url(),
            "hasManagedGames": false,
        })
    };

    let data = parts
        .into_iter()
        .map(|part| {
            let team = teams.get(&part.team_id);
            let mut member_user_ids = Vec::new();
            let mut seen = HashSet::new();
            if let Some(team) = team {
                if seen.insert(team.captain_id) {
                    member_user_ids.push(team.captain_id);
                }
            }
            for user_id in roster_by_team.get(&part.team_id).into_iter().flatten() {
                if seen.insert(*user_id) {
                    member_user_ids.push(*user_id);
                }
            }
            let members = member_user_ids
                .into_iter()
                .filter_map(|user_id| member_users.get(&user_id).map(member_info))
                .collect();
            ParticipationInfoModel {
                id: part.id,
                team: TeamWithDetailedUserInfo {
                    id: part.team_id,
                    locked: team.map(|team| team.locked).unwrap_or(false),
                    captain_id: team.map(|team| team.captain_id).unwrap_or_default(),
                    name: team.map(|team| team.name.clone()),
                    bio: team.and_then(|team| team.bio.clone()),
                    avatar: team.and_then(|team| team.avatar_url()),
                    members,
                },
                registered_members: members_by_part.remove(&part.id).unwrap_or_default(),
                division_id: part.division_id,
                status: part.status,
            }
        })
        .collect();
    Ok(RequestResponse::ok(data))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationReviewQuery {
    #[serde(default = "default_review_page_size")]
    count: u64,
    #[serde(default)]
    skip: u64,
    #[serde(default)]
    status: Option<ParticipationStatus>,
    #[serde(default)]
    division_id: Option<i32>,
    #[serde(default)]
    search: Option<String>,
}

fn default_review_page_size() -> u64 {
    DEFAULT_REVIEW_PAGE_SIZE
}

impl Default for ParticipationReviewQuery {
    fn default() -> Self {
        Self {
            count: DEFAULT_REVIEW_PAGE_SIZE,
            skip: 0,
            status: None,
            division_id: None,
            search: None,
        }
    }
}

#[derive(Debug)]
struct NormalizedParticipationReviewQuery {
    count: i64,
    skip: i64,
    status: Option<i16>,
    division_id: Option<i32>,
    search: Option<String>,
}

impl ParticipationReviewQuery {
    fn normalized(&self) -> AppResult<NormalizedParticipationReviewQuery> {
        let search = self
            .search
            .as_deref()
            .map(str::trim)
            .filter(|search| !search.is_empty());
        if search.is_some_and(|search| search.chars().count() > MAX_REVIEW_SEARCH_CHARS) {
            return Err(AppError::bad_request(
                "Search must be at most 100 characters",
            ));
        }
        if self.division_id.is_some_and(|division_id| division_id <= 0) {
            return Err(AppError::bad_request("Division id must be positive"));
        }

        Ok(NormalizedParticipationReviewQuery {
            count: self.count.clamp(1, MAX_REVIEW_PAGE_SIZE) as i64,
            skip: self.skip.min(MAX_REVIEW_SKIP) as i64,
            status: self.status.map(|status| status as i16),
            division_id: self.division_id,
            search: super::monitor_history::normalized_search_pattern(search),
        })
    }
}

/// Compact, PII-free row rendered in the participation review list.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationReviewSummaryModel {
    pub id: i32,
    pub team_id: i32,
    pub team_name: String,
    pub team_avatar: Option<String>,
    pub registered_member_count: i64,
    pub team_member_count: i64,
    pub division_id: Option<i32>,
    pub status: ParticipationStatus,
}

/// The minimum member profile needed by the operator's expanded roster.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationReviewMemberModel {
    pub user_id: Uuid,
    pub user_name: Option<String>,
    pub email: Option<String>,
    pub real_name: Option<String>,
    pub std_number: Option<String>,
    pub phone: Option<String>,
    pub avatar: Option<String>,
    pub is_registered: bool,
    pub is_captain: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationReviewDetailModel {
    pub id: i32,
    pub team_id: i32,
    pub team_name: String,
    pub team_avatar: Option<String>,
    pub members: Vec<ParticipationReviewMemberModel>,
}

#[derive(Debug, sqlx::FromRow)]
struct ParticipationReviewListRow {
    authorized_game_id: i32,
    id: Option<i32>,
    team_id: Option<i32>,
    team_name: Option<String>,
    team_avatar_hash: Option<String>,
    registered_member_count: Option<i64>,
    team_member_count: Option<i64>,
    division_id: Option<i32>,
    status: Option<i16>,
    total_count: i64,
}

macro_rules! participation_review_page_sql {
    ($search_predicate:literal) => {
        concat!(
            r#"
WITH authorized_game AS (
    SELECT game.id
      FROM "Games" game
     WHERE game.id = $1
       AND (
            $3::boolean
            OR EXISTS (
                SELECT 1
                  FROM "GameManagers" manager
                 WHERE manager.game_id = game.id
                   AND manager.user_id = $2
            )
       )
),
filtered AS NOT MATERIALIZED (
    SELECT participation.id,
           participation.team_id,
           team.name AS team_name,
           team.avatar_hash AS team_avatar_hash,
           team.captain_id,
           participation.division_id,
           participation.status
      FROM authorized_game game
      JOIN "Participations" participation ON participation.game_id = game.id
      JOIN "Teams" team ON team.id = participation.team_id
     WHERE ($4::smallint IS NULL OR participation.status = $4)
       AND ($5::integer IS NULL OR participation.division_id = $5)
       AND "#,
            $search_predicate,
            r#"
),
page AS (
    SELECT filtered.*, COUNT(*) OVER () AS total_count
      FROM filtered
     ORDER BY filtered.team_id ASC, filtered.id ASC
     OFFSET $7 LIMIT $8
)
SELECT game.id AS authorized_game_id,
       page.id,
       page.team_id,
       page.team_name,
       page.team_avatar_hash,
       registered.registered_member_count,
       roster.team_member_count,
       page.division_id,
       page.status,
       COALESCE(page.total_count, (SELECT COUNT(*) FROM filtered)) AS total_count
  FROM authorized_game game
  LEFT JOIN page ON TRUE
  LEFT JOIN LATERAL (
      SELECT COUNT(*)::bigint AS registered_member_count
        FROM "UserParticipations" membership
       WHERE membership.game_id = game.id
         AND membership.participation_id = page.id
         AND membership.team_id = page.team_id
  ) registered ON page.id IS NOT NULL
  LEFT JOIN LATERAL (
      SELECT COUNT(*)::bigint AS team_member_count
        FROM (
            SELECT page.captain_id AS user_id
            UNION
            SELECT member.user_id
              FROM "TeamMembers" member
             WHERE member.team_id = page.team_id
        ) roster_member
  ) roster ON page.id IS NOT NULL
 ORDER BY page.team_id ASC NULLS LAST, page.id ASC NULLS LAST
"#
        )
    };
}

pub(crate) const PARTICIPATION_REVIEW_PAGE_SQL: &str =
    participation_review_page_sql!("$6::text IS NULL");
pub(crate) const PARTICIPATION_REVIEW_SEARCH_PAGE_SQL: &str =
    participation_review_page_sql!(r#"LOWER(team.name) LIKE $6 ESCAPE '\'"#);

async fn participation_review_page(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    is_admin: bool,
    game_id: i32,
    query: &ParticipationReviewQuery,
) -> AppResult<Option<ArrayResponse<ParticipationReviewSummaryModel>>> {
    let query = query.normalized()?;
    // Separate statement identities keep PostgreSQL's prepared generic plans
    // from compromising between an unfiltered page and a selective GIN scan.
    let has_search = query.search.is_some();
    let page_sql = if has_search {
        PARTICIPATION_REVIEW_SEARCH_PAGE_SQL
    } else {
        PARTICIPATION_REVIEW_PAGE_SQL
    };
    let rows = sqlx::query_as::<_, ParticipationReviewListRow>(page_sql)
        .bind(game_id)
        .bind(user_id)
        .bind(is_admin)
        .bind(query.status)
        .bind(query.division_id)
        .bind(query.search.as_deref())
        .bind(query.skip)
        .bind(query.count)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let Some(first) = rows.first() else {
        return Ok(None);
    };
    debug_assert_eq!(first.authorized_game_id, game_id);
    let total = first.total_count;
    let data =
        rows.into_iter()
            .filter_map(|row| {
                let id = row.id?;
                Some((|| {
                    Ok(ParticipationReviewSummaryModel {
                        id,
                        team_id: row
                            .team_id
                            .ok_or_else(|| AppError::internal("Participation team is missing"))?,
                        team_name: row.team_name.ok_or_else(|| {
                            AppError::internal("Participation team name is missing")
                        })?,
                        team_avatar: row
                            .team_avatar_hash
                            .map(|hash| format!("/assets/{hash}/avatar")),
                        registered_member_count: row.registered_member_count.unwrap_or(0),
                        team_member_count: row.team_member_count.unwrap_or(0),
                        division_id: row.division_id,
                        status: super::membership::participation_status(row.status.ok_or_else(
                            || AppError::internal("Participation status is missing"),
                        )?)?,
                    })
                })())
            })
            .collect::<AppResult<Vec<_>>>()?;

    Ok(Some(ArrayResponse::new(data, total)))
}

/// `GET /api/game/{id}/participations/page` — one authorized, filtered SQL page.
pub async fn participation_page(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Query(query): Query<ParticipationReviewQuery>,
) -> AppResult<ArrayResponse<ParticipationReviewSummaryModel>> {
    match participation_review_page(st.pg(), user.id, user.is_admin(), id, &query).await? {
        Some(page) => Ok(page),
        None if user.is_admin() => Err(AppError::not_found("Game not found")),
        None => Err(AppError::Forbidden),
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ParticipationReviewDetailRow {
    authorized_participation_id: i32,
    team_id: i32,
    team_name: String,
    team_avatar_hash: Option<String>,
    user_id: Option<Uuid>,
    user_name: Option<String>,
    email: Option<String>,
    real_name: Option<String>,
    std_number: Option<String>,
    phone_number: Option<String>,
    avatar_hash: Option<String>,
    is_registered: Option<bool>,
    is_captain: Option<bool>,
}

pub(crate) const PARTICIPATION_REVIEW_DETAIL_SQL: &str = r#"
WITH authorized_participation AS (
    SELECT participation.id,
           participation.game_id,
           participation.team_id,
           team.name AS team_name,
           team.avatar_hash AS team_avatar_hash,
           team.captain_id
      FROM "Games" game
      JOIN "Participations" participation ON participation.game_id = game.id
      JOIN "Teams" team ON team.id = participation.team_id
     WHERE game.id = $1
       AND participation.id = $4
       AND (
            $3::boolean
            OR EXISTS (
                SELECT 1
                  FROM "GameManagers" manager
                 WHERE manager.game_id = game.id
                   AND manager.user_id = $2
            )
       )
),
roster AS (
    SELECT participation.id AS participation_id,
           participation.captain_id AS user_id
      FROM authorized_participation participation
    UNION
    SELECT participation.id,
           member.user_id
      FROM authorized_participation participation
      JOIN "TeamMembers" member ON member.team_id = participation.team_id
)
SELECT participation.id AS authorized_participation_id,
       participation.team_id,
       participation.team_name,
       participation.team_avatar_hash,
       roster.user_id,
       account.user_name,
       account.email,
       NULLIF(account.real_name, '') AS real_name,
       NULLIF(account.std_number, '') AS std_number,
       account.phone_number,
       account.avatar_hash,
       EXISTS (
           SELECT 1
             FROM "UserParticipations" membership
            WHERE membership.user_id = roster.user_id
              AND membership.game_id = participation.game_id
              AND membership.team_id = participation.team_id
              AND membership.participation_id = participation.id
       ) AS is_registered,
       roster.user_id = participation.captain_id AS is_captain
  FROM authorized_participation participation
  LEFT JOIN roster ON roster.participation_id = participation.id
  LEFT JOIN "AspNetUsers" account ON account.id = roster.user_id
 ORDER BY (roster.user_id = participation.captain_id) DESC NULLS LAST,
          LOWER(account.user_name) ASC NULLS LAST,
          roster.user_id ASC NULLS LAST
"#;

async fn participation_review_detail(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    is_admin: bool,
    game_id: i32,
    participation_id: i32,
) -> AppResult<Option<ParticipationReviewDetailModel>> {
    let rows = sqlx::query_as::<_, ParticipationReviewDetailRow>(PARTICIPATION_REVIEW_DETAIL_SQL)
        .bind(game_id)
        .bind(user_id)
        .bind(is_admin)
        .bind(participation_id)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    debug_assert_eq!(first.authorized_participation_id, participation_id);

    let members = rows
        .iter()
        .filter_map(|row| {
            row.user_id.map(|user_id| ParticipationReviewMemberModel {
                user_id,
                user_name: row.user_name.clone(),
                email: row.email.clone(),
                real_name: row.real_name.clone(),
                std_number: row.std_number.clone(),
                phone: row.phone_number.clone(),
                avatar: row
                    .avatar_hash
                    .as_ref()
                    .map(|hash| format!("/assets/{hash}/avatar")),
                is_registered: row.is_registered.unwrap_or(false),
                is_captain: row.is_captain.unwrap_or(false),
            })
        })
        .collect();

    Ok(Some(ParticipationReviewDetailModel {
        id: first.authorized_participation_id,
        team_id: first.team_id,
        team_name: first.team_name.clone(),
        team_avatar: first
            .team_avatar_hash
            .as_ref()
            .map(|hash| format!("/assets/{hash}/avatar")),
        members,
    }))
}

fn private_no_store_detail(detail: ParticipationReviewDetailModel) -> Response {
    let mut response = RequestResponse::ok(detail).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::PRAGMA,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    response
}

/// `GET /api/game/{id}/participations/{participationId}` — lazy roster detail.
pub async fn participation_detail(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, participation_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    match participation_review_detail(st.pg(), user.id, user.is_admin(), game_id, participation_id)
        .await?
    {
        Some(detail) => Ok(private_no_store_detail(detail)),
        None if user.is_admin() => Err(AppError::not_found("Participation not found")),
        None => Err(AppError::Forbidden),
    }
}

#[cfg(test)]
#[path = "participation_review_tests.rs"]
mod tests;
