//! Player-facing event and joined-event challenge discovery.
//!
//! Both catalogs are one-query, paginated projections. The challenge catalog
//! starts from the caller's accepted participation rows, so filters can never
//! widen access beyond challenges the player may already open in an event.

use super::*;

const MAX_CATALOG_SEARCH_CHARS: usize = 100;

fn normalized_catalog_search(search: Option<&str>) -> AppResult<Option<String>> {
    let Some(search) = search.map(str::trim).filter(|search| !search.is_empty()) else {
        return Ok(None);
    };
    if search.chars().count() > MAX_CATALOG_SEARCH_CHARS {
        return Err(AppError::bad_request(
            "Search must be at most 100 characters",
        ));
    }
    Ok(Some(search.to_owned()))
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GameMembershipFilter {
    #[default]
    All,
    Joined,
    NotJoined,
}

impl GameMembershipFilter {
    fn query_value(self) -> i16 {
        match self {
            Self::All => 0,
            Self::Joined => 1,
            Self::NotJoined => 2,
        }
    }
}

/// `GET /api/game` — paginated list of visible (non-hidden) games.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameListQuery {
    #[serde(default = "default_game_list_count")]
    count: u64,
    #[serde(default)]
    skip: u64,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    membership: GameMembershipFilter,
}

fn default_game_list_count() -> u64 {
    PageParams::default().count
}

impl GameListQuery {
    fn page(&self) -> PageParams {
        PageParams {
            count: self.count,
            skip: self.skip,
        }
    }
}

#[derive(sqlx::FromRow)]
struct GameListRow {
    id: i32,
    title: String,
    summary: String,
    poster_hash: Option<String>,
    team_member_count_limit: i32,
    start_time_utc: DateTime<Utc>,
    end_time_utc: DateTime<Utc>,
    participation_status: Option<i16>,
    joined: bool,
    total_count: i64,
}

pub async fn games(
    State(st): State<SharedState>,
    MaybeUser(user): MaybeUser,
    Query(query): Query<GameListQuery>,
) -> AppResult<ArrayResponse<BasicGameInfoModel>> {
    let search = normalized_catalog_search(query.search.as_deref())?;
    let page = query.page();
    let rows = sqlx::query_as::<_, GameListRow>(
        r#"SELECT game.id, game.title, game.summary, game.poster_hash,
                  game.team_member_count_limit, game.start_time_utc,
                  game.end_time_utc, participation.status AS participation_status,
                  COALESCE(participation.status IN ($2, $3, $4), FALSE) AS joined,
                  COUNT(*) OVER () AS total_count
             FROM "Games" game
             LEFT JOIN "UserParticipations" membership
               ON membership.user_id = $1 AND membership.game_id = game.id
             LEFT JOIN "Participations" participation
               ON participation.id = membership.participation_id
              AND participation.game_id = membership.game_id
              AND participation.team_id = membership.team_id
            WHERE game.hidden = FALSE
              AND ($5::text IS NULL
                   OR STRPOS(LOWER(CONCAT_WS(' ', game.title, game.summary)), LOWER($5)) > 0
                   OR game.id::text = $5)
              AND ($6 = 0
                   OR ($6 = 1 AND participation.status IN ($2, $3, $4))
                   OR ($6 = 2 AND (participation.status IS NULL OR participation.status = $7)))
            ORDER BY game.start_time_utc DESC, game.id DESC
            OFFSET $8 LIMIT $9"#,
    )
    .bind(user.as_ref().map(|user| user.id))
    .bind(ParticipationStatus::Pending as i16)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ParticipationStatus::Suspended as i16)
    .bind(search.as_deref())
    .bind(query.membership.query_value())
    .bind(ParticipationStatus::Rejected as i16)
    .bind(page.skip.min(i64::MAX as u64) as i64)
    .bind(page.limit().min(100) as i64)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let total = rows.first().map_or(0, |row| row.total_count);
    let server_time = Utc::now();
    let data = rows
        .into_iter()
        .map(|row| {
            let participation_status = row
                .participation_status
                .map(super::membership::participation_status)
                .transpose()?;
            Ok(BasicGameInfoModel {
                id: row.id,
                title: row.title,
                summary: row.summary,
                poster: row.poster_hash.map(|hash| format!("/assets/{hash}/poster")),
                limit: row.team_member_count_limit,
                team_count: 0,
                user_count: 0,
                average_rating: 0.0,
                review_count: 0,
                joined: row.joined,
                participation_status,
                start: row.start_time_utc,
                end: row.end_time_utc,
                server_time,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(ArrayResponse::new(data, total))
}

fn default_challenge_catalog_count() -> u64 {
    24
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ChallengeCatalogMode {
    Jeopardy,
    Koth,
    AttackDefense,
}

impl ChallengeCatalogMode {
    fn as_query_value(self) -> &'static str {
        match self {
            Self::Jeopardy => "jeopardy",
            Self::Koth => "koth",
            Self::AttackDefense => "attackDefense",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeCatalogQuery {
    #[serde(default = "default_challenge_catalog_count")]
    count: u64,
    #[serde(default)]
    skip: u64,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    game_id: Option<i32>,
    #[serde(default)]
    category: Option<ChallengeCategory>,
    #[serde(default)]
    mode: Option<ChallengeCatalogMode>,
    #[serde(default, rename = "type")]
    challenge_type: Option<ChallengeType>,
    #[serde(default)]
    solved: Option<bool>,
}

impl Default for ChallengeCatalogQuery {
    fn default() -> Self {
        Self {
            count: default_challenge_catalog_count(),
            skip: 0,
            search: None,
            game_id: None,
            category: None,
            mode: None,
            challenge_type: None,
            solved: None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeCatalogItem {
    pub id: i32,
    pub title: String,
    pub category: ChallengeCategory,
    #[serde(rename = "type")]
    pub challenge_type: ChallengeType,
    pub score: i32,
    pub solve_count: i32,
    pub solved: bool,
    pub game_id: i32,
    pub game_title: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub game_start: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub game_end: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ChallengeCatalogRow {
    id: i32,
    title: String,
    category: i16,
    challenge_type: i16,
    original_score: i32,
    min_score_rate: f64,
    difficulty: f64,
    accepted_count: i32,
    score_curve: i16,
    solved: bool,
    game_id: i32,
    game_title: String,
    game_start: DateTime<Utc>,
    game_end: DateTime<Utc>,
    total_count: i64,
}

fn challenge_category(value: i16) -> AppResult<ChallengeCategory> {
    <ChallengeCategory as sea_orm::ActiveEnum>::try_from_value(&value)
        .map_err(|error| AppError::internal(error.to_string()))
}

fn challenge_type(value: i16) -> AppResult<ChallengeType> {
    <ChallengeType as sea_orm::ActiveEnum>::try_from_value(&value)
        .map_err(|error| AppError::internal(error.to_string()))
}

fn score_curve(value: i16) -> AppResult<ScoreCurve> {
    <ScoreCurve as sea_orm::ActiveEnum>::try_from_value(&value)
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn load_challenge_catalog(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    query: &ChallengeCatalogQuery,
) -> AppResult<(Vec<ChallengeCatalogItem>, i64)> {
    let search = normalized_catalog_search(query.search.as_deref())?;
    let rows = sqlx::query_as::<_, ChallengeCatalogRow>(
        r#"WITH eligible AS MATERIALIZED (
                SELECT participation.id, participation.game_id, participation.division_id
                  FROM "UserParticipations" membership
                  JOIN "Participations" participation
                    ON participation.id = membership.participation_id
                   AND participation.game_id = membership.game_id
                   AND participation.team_id = membership.team_id
                 WHERE membership.user_id = $1
                   AND participation.status = $2
            ), catalog AS (
                SELECT challenge.id, challenge.title, challenge.category,
                       challenge."Type" AS challenge_type,
                       challenge.original_score, challenge.min_score_rate,
                       challenge.difficulty, challenge.accepted_count,
                       challenge.score_curve,
                       first_solve.challenge_id IS NOT NULL AS solved,
                       game.id AS game_id, game.title AS game_title,
                       game.start_time_utc AS game_start,
                       game.end_time_utc AS game_end
                  FROM eligible
                  JOIN "Games" game ON game.id = eligible.game_id
                  JOIN "GameChallenges" challenge ON challenge.game_id = game.id
                  LEFT JOIN "Divisions" division
                    ON division.id = eligible.division_id
                   AND division.game_id = eligible.game_id
                  LEFT JOIN "DivisionChallengeConfigs" permission
                    ON permission.division_id = eligible.division_id
                   AND permission.challenge_id = challenge.id
                  LEFT JOIN "FirstSolves" first_solve
                    ON first_solve.participation_id = eligible.id
                   AND first_solve.challenge_id = challenge.id
                 WHERE game.hidden = FALSE
                   AND game.start_time_utc <= clock_timestamp()
                   AND challenge.is_enabled = TRUE
                   AND challenge.review_status = $3
                   AND (
                       eligible.division_id IS NULL
                       OR (
                           division.id IS NOT NULL
                           AND (COALESCE(permission.permissions, division.default_permissions, 0) & $4) = $4
                       )
                   )
            )
            SELECT catalog.*, COUNT(*) OVER () AS total_count
              FROM catalog
             WHERE ($5::text IS NULL
                    OR STRPOS(LOWER(CONCAT_WS(' ', catalog.title, catalog.game_title)), LOWER($5)) > 0
                    OR catalog.id::text = $5
                    OR catalog.game_id::text = $5)
               AND ($6::int IS NULL OR catalog.game_id = $6)
               AND ($7::smallint IS NULL OR catalog.category = $7)
               AND (
                    $8::text IS NULL
                    OR ($8 = 'jeopardy' AND catalog.challenge_type NOT IN ($9, $10))
                    OR ($8 = 'attackDefense' AND catalog.challenge_type = $9)
                    OR ($8 = 'koth' AND catalog.challenge_type = $10)
               )
               AND ($11::smallint IS NULL OR catalog.challenge_type = $11)
               AND ($12::boolean IS NULL OR catalog.solved = $12)
             ORDER BY catalog.solved, catalog.game_start DESC,
                      catalog.game_id DESC, catalog.category, catalog.id
             OFFSET $13 LIMIT $14"#,
    )
    .bind(user_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(GamePermission::VIEW_CHALLENGE)
    .bind(search.as_deref())
    .bind(query.game_id)
    .bind(query.category.map(|category| category as i16))
    .bind(query.mode.map(ChallengeCatalogMode::as_query_value))
    .bind(ChallengeType::AttackDefense as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .bind(query.challenge_type.map(|kind| kind as i16))
    .bind(query.solved)
    .bind(query.skip.min(i64::MAX as u64) as i64)
    .bind(query.count.clamp(1, 100) as i64)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let total = rows.first().map_or(0, |row| row.total_count);
    let items = rows
        .into_iter()
        .map(|row| {
            let category = challenge_category(row.category)?;
            let challenge_type = challenge_type(row.challenge_type)?;
            let curve = score_curve(row.score_curve)?;
            let score = if challenge_type.uses_ad_engine() {
                0
            } else {
                calculate_challenge_score(
                    row.original_score,
                    row.min_score_rate,
                    row.difficulty,
                    row.accepted_count,
                    curve,
                )
            };
            Ok(ChallengeCatalogItem {
                id: row.id,
                title: row.title,
                category,
                challenge_type,
                score,
                solve_count: row.accepted_count,
                solved: row.solved,
                game_id: row.game_id,
                game_title: row.game_title,
                game_start: row.game_start,
                game_end: row.game_end,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok((items, total))
}

/// `GET /api/game/challenges` — challenges from accepted, started events only.
pub async fn challenge_catalog(
    State(st): State<SharedState>,
    user: CurrentUser,
    Query(query): Query<ChallengeCatalogQuery>,
) -> AppResult<ArrayResponse<ChallengeCatalogItem>> {
    let (items, total) = load_challenge_catalog(st.pg(), user.id, &query).await?;
    Ok(ArrayResponse::new(items, total))
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
