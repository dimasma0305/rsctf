//! Read models for the A&D/KotH operator console.
//!
//! The compatibility `State` response carries the larger grid snapshot. The
//! five-second `Live` response contains only round/flag/verdict deltas, and its
//! latest-verdict lookup performs one indexed probe per configured service.

use super::*;

/// One indexed latest-row probe is performed for each requested service.
pub(crate) const LATEST_AD_CHECKS_SQL: &str = r#"
    SELECT requested.team_service_id,
           latest.id AS last_check_id,
           latest.status AS last_check_status
      FROM unnest($1::integer[]) AS requested(team_service_id)
 LEFT JOIN LATERAL (
           SELECT result.id, result.status
             FROM "AdCheckResults" result
            WHERE result.team_service_id = requested.team_service_id
            ORDER BY result.checked_at DESC, result.id DESC
            LIMIT 1
      ) latest ON TRUE
     ORDER BY requested.team_service_id
"#;

/// The post-authorization live-state projection executes exactly these two reads:
/// one game/round row and one bounded row per accepted A&D service.
#[cfg(test)]
pub(crate) const AD_LIVE_STATE_QUERY_COUNT: usize = 2;

pub(crate) const AD_LIVE_SERVICES_SQL: &str = r#"
    SELECT service.id AS ad_team_service_id,
           latest.id AS last_check_id,
           latest.status AS last_check_status,
           flag.flag AS current_flag
      FROM "AdTeamServices" service
      JOIN "Participations" participation
        ON participation.id = service.participation_id
       AND participation.game_id = service.game_id
       AND participation.status = $2
      JOIN "GameChallenges" challenge
        ON challenge.id = service.challenge_id
       AND challenge.game_id = service.game_id
       AND challenge."Type" = $3
 LEFT JOIN LATERAL (
           SELECT result.id, result.status
             FROM "AdCheckResults" result
            WHERE result.team_service_id = service.id
            ORDER BY result.checked_at DESC, result.id DESC
            LIMIT 1
      ) latest ON TRUE
 LEFT JOIN "AdFlags" flag
        ON flag.round_id = $4
       AND flag.team_service_id = service.id
     WHERE service.game_id = $1
     ORDER BY service.id
"#;

/// A&D admin — per-challenge state (`Api.ts` `AdChallengeStateModel`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdChallengeStateModel {
    pub challenge_id: i32,
    pub title: String,
    pub is_enabled: bool,
    pub tick_seconds: i32,
    pub flag_lifetime_ticks: i32,
    pub teams_with_live_container: Option<i32>,
}

/// A&D admin — per-cell (team × challenge) state (`Api.ts` `AdTeamCellModel`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTeamCellModel {
    pub ad_team_service_id: i32,
    pub challenge_id: i32,
    pub container_ip: Option<String>,
    pub container_port: Option<i32>,
    pub container_guid: Option<String>,
    pub last_check_status: Option<String>,
    pub last_check_id: Option<i32>,
    pub current_flag: Option<String>,
    pub snapshot_available: bool,
    pub changed_file_count: Option<i32>,
    pub self_hosted: bool,
}

/// A&D admin — one team row in the grid (`Api.ts` `AdTeamRowModel`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTeamRowModel {
    pub participation_id: i32,
    pub team_name: String,
    pub services: Vec<AdTeamCellModel>,
}

/// A&D admin — the compatibility grid snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdGameStateModel {
    pub current_round: Option<i32>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub round_started_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub round_ends_at: Option<DateTime<Utc>>,
    pub scoring_paused: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub scoring_paused_at: Option<DateTime<Utc>>,
    pub challenges: Vec<AdChallengeStateModel>,
    pub teams: Vec<AdTeamRowModel>,
}

/// Cheap operator metadata used before mounting either engine poll.
#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AdEngineMetadataModel {
    pub has_attack_defense: bool,
    pub has_koth: bool,
    #[serde(with = "crate::utils::datetime::millis")]
    pub start: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub end: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub server_time: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AdLiveCellModel {
    pub ad_team_service_id: i32,
    pub last_check_id: Option<i32>,
    pub last_check_status: Option<String>,
    pub current_flag: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdLiveStateModel {
    pub current_round: Option<i32>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub round_started_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub round_ends_at: Option<DateTime<Utc>>,
    pub scoring_paused: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub scoring_paused_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub server_time: DateTime<Utc>,
    pub services: Vec<AdLiveCellModel>,
}

#[derive(Debug, sqlx::FromRow)]
struct LatestAdCheckRow {
    team_service_id: i32,
    last_check_id: Option<i32>,
    last_check_status: Option<i16>,
}

#[derive(Debug, sqlx::FromRow)]
struct AdLiveGameRow {
    scoring_paused: bool,
    scoring_paused_at: Option<DateTime<Utc>>,
    round_id: Option<i32>,
    current_round: Option<i32>,
    round_started_at: Option<DateTime<Utc>>,
    round_ends_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct AdLiveCellRow {
    ad_team_service_id: i32,
    last_check_id: Option<i32>,
    last_check_status: Option<i16>,
    current_flag: Option<String>,
}

#[cfg(test)]
static AD_LIVE_QUERY_EXECUTIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
fn record_ad_live_query() {
    AD_LIVE_QUERY_EXECUTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(not(test))]
fn record_ad_live_query() {}

async fn latest_ad_checks(
    pool: &sqlx::PgPool,
    service_ids: &[i32],
) -> AppResult<std::collections::HashMap<i32, LatestAdCheckRow>> {
    if service_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows = sqlx::query_as::<_, LatestAdCheckRow>(LATEST_AD_CHECKS_SQL)
        .bind(service_ids)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| (row.team_service_id, row))
        .collect())
}

async fn load_ad_live_projection(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<(AdLiveGameRow, Vec<AdLiveCellRow>)> {
    record_ad_live_query();
    let game = sqlx::query_as::<_, AdLiveGameRow>(
        r#"SELECT game.ad_scoring_paused AS scoring_paused,
                  game.ad_scoring_paused_at AS scoring_paused_at,
                  round.id AS round_id, round.number AS current_round,
                  round.start_time_utc AS round_started_at,
                  round.end_time_utc AS round_ends_at
             FROM "Games" game
        LEFT JOIN LATERAL (
                  SELECT id, number, start_time_utc, end_time_utc
                    FROM "AdRounds"
                   WHERE game_id = game.id
                   ORDER BY number DESC, id DESC
                   LIMIT 1
             ) round ON TRUE
            WHERE game.id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    record_ad_live_query();
    let services = sqlx::query_as::<_, AdLiveCellRow>(AD_LIVE_SERVICES_SQL)
        .bind(game_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .bind(game.round_id)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((game, services))
}

/// `GET /api/edit/games/{id}/ad/Engines` — one authorized, history-free query.
pub async fn ad_engine_metadata(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<AdEngineMetadataModel>> {
    manager_or_admin(&st, &user, game_id).await?;
    let (has_attack_defense, has_koth, start, end) =
        sqlx::query_as::<_, (bool, bool, DateTime<Utc>, DateTime<Utc>)>(
            r#"SELECT EXISTS(
                     SELECT 1 FROM "GameChallenges" challenge
                      WHERE challenge.game_id = game.id AND challenge."Type" = $2
                  ),
                  EXISTS(
                     SELECT 1 FROM "GameChallenges" challenge
                      WHERE challenge.game_id = game.id AND challenge."Type" = $3
                  ),
                  game.start_time_utc, game.end_time_utc
             FROM "Games" game
            WHERE game.id = $1"#,
        )
        .bind(game_id)
        .bind(ChallengeType::AttackDefense as i16)
        .bind(ChallengeType::KingOfTheHill as i16)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    Ok(RequestResponse::ok(AdEngineMetadataModel {
        has_attack_defense,
        has_koth,
        start,
        end,
        server_time: Utc::now(),
    }))
}

/// `GET /api/edit/games/{id}/ad/Live` — the five-second A&D delta.
pub async fn ad_live_state(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<AdLiveStateModel>> {
    manager_or_admin(&st, &user, game_id).await?;
    let (game, services) = load_ad_live_projection(st.pg(), game_id).await?;
    let services = services
        .into_iter()
        .map(|row| AdLiveCellModel {
            ad_team_service_id: row.ad_team_service_id,
            last_check_id: row.last_check_id,
            last_check_status: row
                .last_check_status
                .map(|status| ad_check_status_label(status).to_string()),
            current_flag: row.current_flag,
        })
        .collect();
    Ok(RequestResponse::ok(AdLiveStateModel {
        current_round: game.current_round,
        round_started_at: game.round_started_at,
        round_ends_at: game.round_ends_at,
        scoring_paused: game.scoring_paused,
        scoring_paused_at: game.scoring_paused_at,
        server_time: Utc::now(),
        services,
    }))
}

/// `GET /api/edit/games/{id}/ad/State` -> `AdGameStateModel`.
///
/// This remains the compatibility snapshot and is loaded separately from the
/// small five-second delta. Latest verdicts use bounded indexed lateral probes.
pub async fn ad_state(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<AdGameStateModel>> {
    manager_or_admin(&st, &user, game_id).await?;
    let game = load_game(&st, game_id).await?;

    let ad_challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::AttackDefense))
        .order_by_asc(game_challenge::Column::Id)
        .all(&st.db)
        .await?;
    let current_round = ad_round::Entity::find()
        .filter(ad_round::Column::GameId.eq(game_id))
        .order_by_desc(ad_round::Column::Number)
        .one(&st.db)
        .await?;
    let participations = participation::Entity::find()
        .filter(participation::Column::GameId.eq(game_id))
        .filter(participation::Column::Status.eq(ParticipationStatus::Accepted))
        .all(&st.db)
        .await?;
    let part_ids: Vec<i32> = participations
        .iter()
        .map(|participation| participation.id)
        .collect();
    let team_ids: Vec<i32> = {
        let mut seen = std::collections::HashSet::new();
        participations
            .iter()
            .map(|participation| participation.team_id)
            .filter(|id| seen.insert(*id))
            .collect()
    };
    let team_names: std::collections::HashMap<i32, String> = if team_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        team::Entity::find()
            .filter(team::Column::Id.is_in(team_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|team| (team.id, team.name))
            .collect()
    };
    let services = if part_ids.is_empty() {
        Vec::new()
    } else {
        ad_team_service::Entity::find()
            .filter(ad_team_service::Column::GameId.eq(game_id))
            .filter(ad_team_service::Column::ParticipationId.is_in(part_ids))
            .all(&st.db)
            .await?
    };
    let service_ids: Vec<i32> = services.iter().map(|service| service.id).collect();
    let snapshot_service_ids =
        crate::services::blob_refs::available_service_snapshots(st.pg(), &service_ids).await?;
    let last_check_by_service = latest_ad_checks(st.pg(), &service_ids).await?;
    let current_flags: std::collections::HashMap<i32, String> = match &current_round {
        Some(round) if !service_ids.is_empty() => ad_flag::Entity::find()
            .filter(ad_flag::Column::RoundId.eq(round.id))
            .filter(ad_flag::Column::TeamServiceId.is_in(service_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|flag| (flag.team_service_id, flag.flag))
            .collect(),
        _ => std::collections::HashMap::new(),
    };
    let byoc_challenge_ids: std::collections::HashSet<i32> = ad_challenges
        .iter()
        .filter(|challenge| challenge.ad_self_hosted)
        .map(|challenge| challenge.id)
        .collect();
    let teams = participations
        .iter()
        .map(|participation| {
            let cells = services
                .iter()
                .filter(|service| service.participation_id == participation.id)
                .map(|service| {
                    let is_byoc = byoc_challenge_ids.contains(&service.challenge_id);
                    let latest = last_check_by_service.get(&service.id);
                    AdTeamCellModel {
                        ad_team_service_id: service.id,
                        challenge_id: service.challenge_id,
                        container_ip: (!is_byoc).then(|| service.host.clone()),
                        container_port: (!is_byoc).then_some(service.port),
                        container_guid: if is_byoc {
                            Some(format!(
                                "byoc:{}:{}",
                                service.participation_id, service.challenge_id
                            ))
                        } else {
                            service.container_id.clone().filter(|id| !id.is_empty())
                        },
                        last_check_status: latest.and_then(|row| {
                            row.last_check_status
                                .map(|status| ad_check_status_label(status).to_string())
                        }),
                        last_check_id: latest.and_then(|row| row.last_check_id),
                        current_flag: current_flags.get(&service.id).cloned(),
                        snapshot_available: snapshot_service_ids.contains(&service.id),
                        changed_file_count: None,
                        self_hosted: is_byoc,
                    }
                })
                .collect();
            AdTeamRowModel {
                participation_id: participation.id,
                team_name: team_names
                    .get(&participation.team_id)
                    .cloned()
                    .unwrap_or_default(),
                services: cells,
            }
        })
        .collect();
    let tick_seconds = game.ad_tick_seconds.unwrap_or(60);
    let flag_lifetime_ticks = game.ad_flag_lifetime_ticks.unwrap_or(5);
    let challenges = ad_challenges
        .iter()
        .map(|challenge| AdChallengeStateModel {
            challenge_id: challenge.id,
            title: challenge.title.clone(),
            is_enabled: challenge.is_enabled,
            tick_seconds,
            flag_lifetime_ticks,
            teams_with_live_container: Some(
                services
                    .iter()
                    .filter(|service| {
                        service.challenge_id == challenge.id && service.container_id.is_some()
                    })
                    .count() as i32,
            ),
        })
        .collect();
    Ok(RequestResponse::ok(AdGameStateModel {
        current_round: current_round.as_ref().map(|round| round.number),
        round_started_at: current_round.as_ref().map(|round| round.start_time_utc),
        round_ends_at: current_round.as_ref().map(|round| round.end_time_utc),
        scoring_paused: game.ad_scoring_paused,
        scoring_paused_at: game.ad_scoring_paused_at,
        challenges,
        teams,
    }))
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
