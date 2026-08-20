//! `Ad/Targets` endpoint — every other team's container per enabled A&D challenge.

use super::*;
use axum::response::Response;
use serde::Deserialize;

/// RSCTF `Game.AdTickSeconds` fallback when the column is unset (mirrors RSCTF's
/// literal `?? 60` in `AdGameController.Targets`).
const DEFAULT_TICK_SECONDS: i64 = 60;
const LIVE_HILL_SNAPSHOT_TTL: std::time::Duration = std::time::Duration::from_secs(1);
static AD_TARGET_ROSTER_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);
static LIVE_HILL_SNAPSHOT_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

/// `AdTeamTarget` — one team's container for a challenge (Targets endpoint).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTeamTarget {
    pub participation_id: i32,
    pub team_name: String,
    pub division: Option<String>,
    pub ip: Option<String>,
    pub port: Option<i32>,
    pub last_check_status: Option<String>,
}

/// `AdHillTarget` — the shared KotH hill target for one challenge. Populated for
/// `KingOfTheHill` challenges only (they have no per-team rows in `teams`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdHillTarget {
    pub ip: Option<String>,
    pub port: Option<i32>,
    /// Public reset generation used to correlate this address with the KotH
    /// state response without disclosing the underlying Docker identity.
    #[serde(default)]
    pub cycle_number: i32,
    pub last_check_status: Option<String>,
    pub last_refresh_round: i32,
}

/// `AdChallengeTargets` — every team's container per enabled challenge.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdChallengeTargets {
    pub challenge_id: i32,
    pub title: String,
    pub tick_seconds: i64,
    pub teams: Vec<AdTeamTarget>,
    /// Populated for King of the Hill challenges only — the shared hill's live
    /// address + last functional verdict. `None` for A&D challenges.
    pub hill: Option<AdHillTarget>,
}

/// `AdTargetsModel` — GET `Ad/Targets` response (excludes caller's own team).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTargetsModel {
    pub current_round: i32,
    pub challenges: Vec<AdChallengeTargets>,
}

/// `GET /api/Game/{id}/Ad/Targets` — every OTHER accepted team's container
/// (host/port) per enabled A&D challenge, plus each service's last check verdict,
/// so attack-first players know where to aim. The caller's own team is excluded.
/// Dual auth (same as Submit): `Bearer ad_...` token or the interactive session.
pub async fn targets(
    State(st): State<SharedState>,
    maybe_user: MaybeUser,
    Path(id): Path<i32>,
    headers: HeaderMap,
    verified: Option<axum::Extension<crate::services::ad::api_token::VerifiedTeamToken>>,
    rejected: Option<axum::Extension<crate::services::ad::api_token::RejectedTeamToken>>,
) -> AppResult<Response> {
    // Initial auth identifies the team whose final fence must be taken. Only the
    // game-global challenge/roster skeleton is cached; mutable service endpoints
    // and verdicts are loaded from PostgreSQL on every request so a BYOC reconnect
    // cannot leave opponents targeting a retired relay port.
    let caller = LiveTargetCaller::resolve(
        &st,
        &headers,
        verified.as_ref().map(|extension| &extension.0),
        rejected.as_ref().map(|extension| &extension.0),
        maybe_user,
        id,
    )
    .await?;
    let (bytes, current_round) = tokio::try_join!(
        target_roster_json(&st, id),
        crate::controllers::game::koth::load_latest_round_cached(&st, id),
    )?;
    let mut model: AdTargetsModel =
        serde_json::from_slice(&bytes).map_err(|e| AppError::internal(e.to_string()))?;
    apply_current_round(&mut model, current_round);

    if current_round > 0 {
        let needs_hills = model
            .challenges
            .iter()
            .any(|challenge| challenge.hill.is_some());
        let (live_services, live_hills) = tokio::try_join!(
            fetch_live_ad_service_identities(&st, id),
            load_live_hills_if_needed(&st, id, needs_hills),
        )?;
        apply_live_ad_service_identities(&mut model, &live_services);
        apply_hill_identities(&mut model, &live_hills);
    }
    exclude_caller(&mut model, caller.participation.id);
    // Revalidate only after all pool-backed reads have completed. If revocation
    // won the race the model is discarded; otherwise it must wait for JSON
    // serialization under the retained roster/token fence.
    caller.finish_response(st.pg(), model).await
}

/// The immutable-during-play challenge/roster skeleton, cached for five seconds
/// with single-flight. Live A&D endpoints and verdicts are deliberately absent.
async fn target_roster_json(st: &SharedState, id: i32) -> AppResult<bytes::Bytes> {
    let key = format!("adtargetroster:{id}");
    if let Some(b) = st.cache.get(&key).await {
        return Ok(b);
    }
    let st = st.clone();
    let key_for_fill = key.clone();
    AD_TARGET_ROSTER_SF
        .run(&key, move || async move {
            if let Some(bytes) = st.cache.get(&key_for_fill).await {
                return Some(bytes);
            }
            let model = match build_target_roster(&st, id).await {
                Ok(model) => model,
                Err(error) => {
                    tracing::warn!(game = id, %error, "A&D target roster cache fill failed");
                    return None;
                }
            };
            let json = match serde_json::to_vec(&model) {
                Ok(json) => bytes::Bytes::from(json),
                Err(error) => {
                    tracing::warn!(game = id, %error, "A&D target roster serialization failed");
                    return None;
                }
            };
            st.cache
                .set(
                    &key_for_fill,
                    &json,
                    Some(std::time::Duration::from_secs(5)),
                )
                .await;
            Some(json)
        })
        .await
        .ok_or_else(|| AppError::internal("A&D target roster cache fill failed"))
}

/// Mutable A&D endpoint identity and its newest completed checker verdict.
/// This is internal projection data and is never serialized onto the wire.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct LiveAdServiceIdentity {
    challenge_id: i32,
    participation_id: i32,
    host: String,
    port: i32,
    last_check_status: Option<i16>,
}

/// Load all currently publishable A&D endpoints in one bounded query. The
/// LATERAL seek uses the per-service result index and returns at most one verdict
/// per service, avoiding a scan over the game's historical checker results.
async fn fetch_live_ad_service_identities(
    st: &SharedState,
    game_id: i32,
) -> AppResult<Vec<LiveAdServiceIdentity>> {
    sqlx::query_as::<_, LiveAdServiceIdentity>(
        r#"SELECT service.challenge_id, service.participation_id,
                  service.host, service.port,
                  verdict.status AS last_check_status
             FROM "AdTeamServices" service
             JOIN "Participations" participation
               ON participation.id = service.participation_id
              AND participation.game_id = service.game_id
              AND participation.status = $2
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $3
              AND challenge."Type" = $4
             LEFT JOIN LATERAL (
               SELECT result.status
                 FROM "AdCheckResults" result
                WHERE result.team_service_id = service.id
                  AND result.sla_credit IS NOT NULL
                ORDER BY result.round_id DESC
                LIMIT 1
             ) verdict ON TRUE
            WHERE service.game_id = $1
              AND (
                service.container_id IS NOT NULL
                OR (
                  challenge.ad_self_hosted = TRUE
                  AND service.host <> ''
                  AND service.port > 0
                )
              )
            ORDER BY service.challenge_id, service.participation_id"#,
    )
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Replace every cached placeholder with its current endpoint atomically at the
/// model level. Missing identities are removed, so a tunnel teardown cannot
/// leave a stale address in a previously cached roster body.
fn apply_live_ad_service_identities(
    model: &mut AdTargetsModel,
    identities: &[LiveAdServiceIdentity],
) {
    let identities: HashMap<(i32, i32), &LiveAdServiceIdentity> = identities
        .iter()
        .map(|identity| ((identity.challenge_id, identity.participation_id), identity))
        .collect();

    for challenge in &mut model.challenges {
        if challenge.hill.is_some() {
            continue;
        }
        challenge.teams.retain_mut(|team| {
            let Some(identity) = identities.get(&(challenge.challenge_id, team.participation_id))
            else {
                return false;
            };
            team.ip = (!identity.host.is_empty()).then(|| identity.host.clone());
            team.port = Some(identity.port);
            team.last_check_status = identity.last_check_status.map(status_str);
            true
        });
    }
}

fn exclude_caller(model: &mut AdTargetsModel, caller_id: i32) {
    for challenge in &mut model.challenges {
        challenge
            .teams
            .retain(|team| team.participation_id != caller_id);
    }
}

fn live_hill_snapshot_cache_key(game_id: i32) -> String {
    format!("adlivehills:{game_id}")
}

/// Evict the shared endpoint projection after a hill ownership or lifecycle
/// transition. The one-second TTL remains a bounded cross-replica backstop for
/// a missed invalidation or a cache fill racing a transition.
pub(crate) async fn invalidate_live_hill_snapshot(st: &SharedState, game_id: i32) {
    invalidate_live_hill_snapshot_cache(st.cache.as_ref(), game_id).await;
    crate::controllers::game::koth::invalidate_live_lifecycle_cache(st.cache.as_ref(), game_id)
        .await;
}

async fn invalidate_live_hill_snapshot_cache(
    cache: &dyn crate::services::cache::Cache,
    game_id: i32,
) {
    cache.remove(&live_hill_snapshot_cache_key(game_id)).await;
}

/// Load every hill identity for a game once per second, shared across all
/// teams. The previous implementation ran this query, including its indexed
/// verdict LATERAL seek, once for every team's poll.
async fn load_live_hill_snapshot(
    st: &SharedState,
    game_id: i32,
) -> AppResult<Vec<LiveHillIdentity>> {
    let key = live_hill_snapshot_cache_key(game_id);
    if let Some(bytes) = st.cache.get(&key).await {
        if let Ok(snapshot) = serde_json::from_slice(&bytes) {
            return Ok(snapshot);
        }
        st.cache.remove(&key).await;
    }

    let st = st.clone();
    let key_for_fill = key.clone();
    let bytes = LIVE_HILL_SNAPSHOT_SF
        .run(&key, move || async move {
            if let Some(bytes) = st.cache.get(&key_for_fill).await {
                if serde_json::from_slice::<Vec<LiveHillIdentity>>(&bytes).is_ok() {
                    return Some(bytes);
                }
                st.cache.remove(&key_for_fill).await;
            }
            let identities = match fetch_live_hill_snapshot(&st, game_id).await {
                Ok(identities) => identities,
                Err(error) => {
                    tracing::warn!(game = game_id, %error, "live KotH target cache fill failed");
                    return None;
                }
            };
            let bytes = match serde_json::to_vec(&identities) {
                Ok(json) => bytes::Bytes::from(json),
                Err(error) => {
                    tracing::warn!(game = game_id, %error, "live KotH target serialization failed");
                    return None;
                }
            };
            st.cache
                .set(&key_for_fill, &bytes, Some(LIVE_HILL_SNAPSHOT_TTL))
                .await;
            Some(bytes)
        })
        .await
        .ok_or_else(|| AppError::internal("live KotH target cache fill failed"))?;
    serde_json::from_slice(&bytes).map_err(|error| AppError::internal(error.to_string()))
}

async fn fetch_live_hill_snapshot(
    st: &SharedState,
    game_id: i32,
) -> AppResult<Vec<LiveHillIdentity>> {
    sqlx::query_as::<_, LiveHillIdentity>(
        r#"SELECT target.challenge_id, target.host, target.port,
                  target.container_id AS target_container_id,
                  cycle.id IS NOT NULL AS managed_v2,
                  CASE WHEN cycle.phase = 'Active'
                             AND cycle.replacement_container_id = target.container_id
                        THEN cycle.cycle_number ELSE 0 END AS cycle_number,
                  verdict.container_id AS verdict_container_id,
                  verdict.status AS verdict_status,
                  verdict.round_number AS verdict_round_number
             FROM "KothTargets" target
             LEFT JOIN LATERAL (
               SELECT crown.id, crown.cycle_number, crown.phase,
                      crown.replacement_container_id
                 FROM "KothCrownCycles" crown
                WHERE crown.game_id = target.game_id
                  AND crown.challenge_id = target.challenge_id
                ORDER BY crown.cycle_number DESC
                LIMIT 1
             ) cycle ON TRUE
             LEFT JOIN LATERAL (
               SELECT result.container_id, result.status,
                      round.number AS round_number
                 FROM "KothControlResults" result
                 JOIN "AdRounds" round
                   ON round.id = result.ad_round_id
                  AND round.game_id = result.game_id
                WHERE result.game_id = target.game_id
                  AND result.challenge_id = target.challenge_id
                ORDER BY result.ad_round_id DESC, result.id DESC
                LIMIT 1
             ) verdict ON TRUE
            WHERE target.game_id = $1
            ORDER BY target.challenge_id"#,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Load freshness-critical KotH identities only when the cached skeleton has a
/// hill slot. This runs in parallel with the per-team A&D endpoint projection,
/// before the final caller revalidation fence is acquired.
async fn load_live_hills_if_needed(
    st: &SharedState,
    game_id: i32,
    needed: bool,
) -> AppResult<Vec<LiveHillIdentity>> {
    if !needed {
        return Ok(Vec::new());
    }
    load_live_hill_snapshot(st, game_id).await
}

#[derive(Debug, Deserialize, Serialize, sqlx::FromRow)]
struct LiveHillIdentity {
    challenge_id: i32,
    host: String,
    port: i32,
    target_container_id: Option<String>,
    managed_v2: bool,
    cycle_number: i32,
    verdict_container_id: Option<String>,
    verdict_status: Option<i16>,
    verdict_round_number: Option<i32>,
}

impl LiveHillIdentity {
    fn published_container_id(&self) -> Option<&str> {
        self.target_container_id
            .as_deref()
            .filter(|container_id| !container_id.is_empty())
    }

    fn publishes_endpoint(&self) -> bool {
        !self.host.is_empty()
            && self.port > 0
            && (!self.managed_v2
                || (self.cycle_number > 0 && self.published_container_id().is_some()))
    }

    fn verdict_is_current(&self) -> bool {
        // External/legacy targets do not necessarily have a platform
        // container identity. The shared rule accepts their intentional
        // null/null case, while managed-v2 evidence requires a non-null exact
        // container match.
        crate::controllers::game::koth::control_evidence_is_current(
            self.managed_v2,
            self.verdict_container_id.as_deref(),
            self.target_container_id.as_deref(),
        )
    }
}

fn apply_hill_identities(model: &mut AdTargetsModel, identities: &[LiveHillIdentity]) {
    for challenge in &mut model.challenges {
        let Some(hill) = &mut challenge.hill else {
            continue;
        };
        match identities
            .iter()
            .find(|identity| identity.challenge_id == challenge.challenge_id)
        {
            Some(identity) if identity.publishes_endpoint() => {
                hill.ip = Some(identity.host.clone());
                hill.port = Some(identity.port);
                hill.cycle_number = identity.cycle_number;
                if identity.verdict_is_current() {
                    hill.last_check_status = identity.verdict_status.map(status_str);
                    hill.last_refresh_round = identity.verdict_round_number.unwrap_or(0);
                } else {
                    hill.last_check_status = None;
                    hill.last_refresh_round = 0;
                }
            }
            Some(identity) => {
                hill.ip = None;
                hill.port = None;
                hill.cycle_number = identity.cycle_number;
                hill.last_check_status = None;
                hill.last_refresh_round = 0;
            }
            _ => {
                hill.ip = None;
                hill.port = None;
                hill.cycle_number = 0;
                hill.last_check_status = None;
                hill.last_refresh_round = 0;
            }
        }
    }
}

fn apply_current_round(model: &mut AdTargetsModel, current_round: i32) {
    model.current_round = current_round;
    if current_round == 0 {
        model.challenges.clear();
    }
}

/// Build the game-global roster and challenge skeleton. Live round, A&D
/// endpoint/verdict, and hill identity fields are overlaid after this five-second
/// snapshot is loaded; caller exclusion is also applied per request by [`targets`].
async fn build_target_roster(st: &SharedState, id: i32) -> AppResult<AdTargetsModel> {
    // Enabled A&D + KotH challenges, ordered by id — the same column set as the
    // board. KotH hills appear as challenge rows with an empty `teams` list and a
    // populated `hill` (the hill is a single shared container, not per-team).
    let mut challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .filter(
            game_challenge::Column::ChallengeType
                .eq(ChallengeType::AttackDefense)
                .or(game_challenge::Column::ChallengeType.eq(ChallengeType::KingOfTheHill)),
        )
        .all(&st.db)
        .await?;
    challenges.sort_by_key(|c| c.id);
    // The accepted roster is stable once A&D/KotH scoring starts. Cache this
    // compact metadata independently from service rows so a newly published
    // endpoint already has a placeholder to occupy without rebuilding the cache.
    let mut roster: Vec<(i32, String)> = sqlx::query_as(
        r#"SELECT participation.id, team.name
             FROM "Participations" participation
             JOIN "Teams" team ON team.id = participation.team_id
            WHERE participation.game_id = $1
              AND participation.status = $2"#,
    )
    .bind(id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    roster.sort_by(|left, right| {
        left.1
            .to_lowercase()
            .cmp(&right.1.to_lowercase())
            .then_with(|| left.0.cmp(&right.0))
    });

    // Tick length is game-wide (RSCTF `Game.AdTickSeconds ?? 60`) — one value for
    // every challenge row below, sourced from the game row rather than an env
    // default so it reflects the operator's configured tick.
    let tick_seconds = game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .and_then(|g| g.ad_tick_seconds)
        .map(|s| s as i64)
        .unwrap_or(DEFAULT_TICK_SECONDS);

    let challenge_targets: Vec<AdChallengeTargets> = challenges
        .iter()
        .map(|c| {
            // KotH is a single shared hill — no per-team targets; surface the
            // hill's address + verdict instead so players can retarget after a
            // refresh rotates its IP.
            if c.challenge_type == ChallengeType::KingOfTheHill {
                return AdChallengeTargets {
                    challenge_id: c.id,
                    title: c.title.clone(),
                    tick_seconds,
                    teams: Vec::new(),
                    // The one-second live overlay supplies the endpoint. Keeping
                    // an empty slot here means a target created after this five-
                    // second roster snapshot can appear immediately.
                    hill: Some(AdHillTarget {
                        ip: None,
                        port: None,
                        cycle_number: 0,
                        last_check_status: None,
                        last_refresh_round: 0,
                    }),
                };
            }

            let teams: Vec<AdTeamTarget> = roster
                .iter()
                .map(|(participation_id, team_name)| AdTeamTarget {
                    participation_id: *participation_id,
                    team_name: team_name.clone(),
                    division: None,
                    ip: None,
                    port: None,
                    last_check_status: None,
                })
                .collect();
            AdChallengeTargets {
                challenge_id: c.id,
                title: c.title.clone(),
                tick_seconds,
                teams,
                hill: None,
            }
        })
        .collect();

    Ok(AdTargetsModel {
        // The handler replaces this placeholder with the shared authoritative
        // one-second round pointer on every response.
        current_round: 0,
        challenges: challenge_targets,
    })
}

#[cfg(test)]
#[path = "targets_tests.rs"]
mod tests;
