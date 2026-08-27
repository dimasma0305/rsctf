//! Admin container-instance listing + destroy + stats — split from admin/mod.rs.
use super::*;
use crate::models::data::container;
use crate::services::container::{ContainerManager, ContainerStatus};
use futures::{stream, StreamExt};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration as StdDuration;
use tokio::sync::{RwLock, Semaphore};
use tokio::time::Instant;

/// RSCTF `ChallengeModel` (nested challenge reference).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeModel {
    pub id: i32,
    pub title: String,
    pub category: ChallengeCategory,
}

/// What owns a managed container when no concrete team can be attached to it.
///
/// A shared challenge is intentionally teamless: one platform-launched workload
/// serves every participant. Keeping that distinct from an unknown owner prevents
/// the admin UI from inventing a team or hiding useful ownership information.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ContainerOwnerKind {
    Team,
    Shared,
    AdminTest,
    Exercise,
    Unassigned,
}

/// RSCTF `ContainerInstanceModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInstanceModel {
    pub team: Option<TeamModel>,
    pub challenge: Option<ChallengeModel>,
    pub owner_kind: ContainerOwnerKind,
    pub owner_name: Option<String>,
    pub image: String,
    pub container_guid: Uuid,
    pub container_id: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expect_stop_at: DateTime<Utc>,
    pub ip: String,
    pub port: i32,
    pub is_proxy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_stats: Option<ContainerRuntimeStatsModel>,
}

/// Whether the runtime returned a sample for this managed container.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ContainerRuntimeAvailability {
    Available,
    Unavailable,
}

/// A runtime sample attached to an admin inventory page.
///
/// Metrics that the selected backend cannot measure are `null`, never an
/// authoritative-looking zero. A missing/stopped runtime is represented by an
/// `Unavailable` sample so one stale row cannot fail or trigger retries for the
/// whole page.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerRuntimeStatsModel {
    pub availability: ContainerRuntimeAvailability,
    pub cpu_percent: Option<f64>,
    pub memory_used_bytes: Option<i64>,
    pub memory_limit_bytes: Option<i64>,
    pub net_rx_bytes: Option<i64>,
    pub net_tx_bytes: Option<i64>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub sampled_at: DateTime<Utc>,
}

const INSTANCE_RUNTIME_BATCH_MAX: u64 = 50;
const INSTANCE_RUNTIME_CONCURRENCY: usize = 8;
const INSTANCE_RUNTIME_CACHE_CAPACITY: usize = 2_048;
const INSTANCE_RUNTIME_CACHE_TTL: StdDuration = StdDuration::from_secs(8);
const INSTANCE_RUNTIME_QUERY_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const INSTANCE_FILTER_OPTION_MAX: u64 = 50;
const INSTANCE_FILTER_SEARCH_MAX_CHARS: usize = 100;

static INSTANCE_RUNTIME_SLOTS: Semaphore = Semaphore::const_new(INSTANCE_RUNTIME_CONCURRENCY);
static INSTANCE_RUNTIME_CACHE: LazyLock<RuntimeSampleCache> = LazyLock::new(|| {
    RuntimeSampleCache::new(INSTANCE_RUNTIME_CACHE_TTL, INSTANCE_RUNTIME_CACHE_CAPACITY)
});

#[derive(Clone)]
struct CachedRuntimeSample {
    sample: ContainerRuntimeStatsModel,
    expires_at: Instant,
}

struct RuntimeSampleCache {
    entries: RwLock<HashMap<String, CachedRuntimeSample>>,
    ttl: StdDuration,
    capacity: usize,
}

impl RuntimeSampleCache {
    fn new(ttl: StdDuration, capacity: usize) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl,
            capacity: capacity.max(1),
        }
    }

    async fn get(&self, runtime_id: &str) -> Option<ContainerRuntimeStatsModel> {
        let now = Instant::now();
        let entries = self.entries.read().await;
        entries
            .get(runtime_id)
            .filter(|entry| entry.expires_at > now)
            .map(|entry| entry.sample.clone())
    }

    async fn insert(&self, runtime_id: String, sample: ContainerRuntimeStatsModel) {
        let now = Instant::now();
        let mut entries = self.entries.write().await;
        entries.retain(|_, entry| entry.expires_at > now);
        if entries.len() >= self.capacity && !entries.contains_key(&runtime_id) {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            runtime_id,
            CachedRuntimeSample {
                sample,
                expires_at: now + self.ttl,
            },
        );
    }

    async fn remove(&self, runtime_id: &str) {
        self.entries.write().await.remove(runtime_id);
    }
}

fn available_runtime_sample(
    status: ContainerStatus,
    sampled_at: DateTime<Utc>,
) -> ContainerRuntimeStatsModel {
    ContainerRuntimeStatsModel {
        availability: ContainerRuntimeAvailability::Available,
        cpu_percent: status
            .cpu_usage
            .map(|value| value * 100.0)
            .filter(|value| value.is_finite() && *value >= 0.0),
        memory_used_bytes: status
            .memory_bytes
            .and_then(|value| i64::try_from(value).ok()),
        // ContainerStatus intentionally exposes neither the configured memory
        // ceiling nor network counters. Keep those metrics explicitly absent.
        memory_limit_bytes: None,
        net_rx_bytes: None,
        net_tx_bytes: None,
        sampled_at,
    }
}

fn unavailable_runtime_sample(sampled_at: DateTime<Utc>) -> ContainerRuntimeStatsModel {
    ContainerRuntimeStatsModel {
        availability: ContainerRuntimeAvailability::Unavailable,
        cpu_percent: None,
        memory_used_bytes: None,
        memory_limit_bytes: None,
        net_rx_bytes: None,
        net_tx_bytes: None,
        sampled_at,
    }
}

async fn sample_runtime(
    manager: &Arc<dyn ContainerManager>,
    cache: &RuntimeSampleCache,
    slots: &Semaphore,
    runtime_id: &str,
) -> ContainerRuntimeStatsModel {
    if let Some(sample) = cache.get(runtime_id).await {
        return sample;
    }

    // Concurrent tabs and overlapping in-process requests
    // wait behind one key, then re-check the cache instead of dogpiling the
    // runtime exactly when its sample expires.
    let flight_key = format!("admin-instance-runtime:{runtime_id}");
    let _flight = crate::utils::single_flight::coalesce(&flight_key).await;
    if let Some(sample) = cache.get(runtime_id).await {
        return sample;
    }

    let sample = match slots.acquire().await {
        Ok(_permit) => {
            let sampled_at = Utc::now();
            match tokio::time::timeout(INSTANCE_RUNTIME_QUERY_TIMEOUT, manager.query(runtime_id))
                .await
            {
                Ok(Ok(status)) => available_runtime_sample(status, sampled_at),
                Ok(Err(_)) | Err(_) => unavailable_runtime_sample(sampled_at),
            }
        }
        Err(_) => unavailable_runtime_sample(Utc::now()),
    };
    cache.insert(runtime_id.to_owned(), sample.clone()).await;
    sample
}

async fn sample_runtime_batch(
    manager: &Arc<dyn ContainerManager>,
    cache: &RuntimeSampleCache,
    slots: &Semaphore,
    instances: &[(Uuid, String)],
) -> HashMap<Uuid, ContainerRuntimeStatsModel> {
    stream::iter(instances.iter().cloned())
        .map(|(guid, runtime_id)| async move {
            let sample = sample_runtime(manager, cache, slots, &runtime_id).await;
            (guid, sample)
        })
        .buffer_unordered(INSTANCE_RUNTIME_CONCURRENCY)
        .collect()
        .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceListQuery {
    #[serde(default = "default_count")]
    count: u64,
    #[serde(default)]
    skip: u64,
    #[serde(default)]
    include_runtime_stats: bool,
    #[serde(default)]
    team_id: Option<i32>,
    #[serde(default)]
    challenge_id: Option<i32>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub enum ContainerInstanceFilterKind {
    Team,
    Challenge,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceFilterOptionsQuery {
    kind: ContainerInstanceFilterKind,
    #[serde(default)]
    search: String,
    #[serde(default = "default_filter_option_count")]
    count: u64,
}

fn default_filter_option_count() -> u64 {
    30
}

fn instance_filter_option_count(query: &InstanceFilterOptionsQuery) -> i64 {
    query.count.min(INSTANCE_FILTER_OPTION_MAX) as i64
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInstanceFilterOptionModel {
    pub id: i32,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<ChallengeCategory>,
}

fn instance_page_count(query: &InstanceListQuery) -> i64 {
    let limit = if query.include_runtime_stats {
        INSTANCE_RUNTIME_BATCH_MAX
    } else {
        500
    };
    query.count.min(limit) as i64
}

#[derive(Debug, sqlx::FromRow)]
struct ContainerInstanceRow {
    team_id: Option<i32>,
    team_name: Option<String>,
    team_avatar_hash: Option<String>,
    challenge_id: Option<i32>,
    challenge_title: Option<String>,
    challenge_category: Option<i16>,
    owner_kind: String,
    owner_name: Option<String>,
    image: String,
    container_guid: Uuid,
    container_id: String,
    started_at: DateTime<Utc>,
    expect_stop_at: DateTime<Utc>,
    ip: String,
    port: i32,
    is_proxy: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct ContainerInstanceFilterOptionRow {
    id: Option<i32>,
    label: Option<String>,
    avatar: Option<String>,
    category: Option<i16>,
    total: i64,
}

const INSTANCE_PROJECTION_SQL: &str = r#"
    SELECT COALESCE(game_team.id, service_team.id) AS team_id,
           COALESCE(game_team.name, service_team.name) AS team_name,
           COALESCE(game_team.avatar_hash, service_team.avatar_hash) AS team_avatar_hash,
           COALESCE(
               game_challenge.id,
               service_challenge.id,
               shared_challenge.id,
               test_challenge.id,
               exercise_challenge.id
           ) AS challenge_id,
           COALESCE(
               game_challenge.title,
               service_challenge.title,
               shared_challenge.title,
               test_challenge.title,
               exercise_challenge.title
           ) AS challenge_title,
           COALESCE(
               game_challenge.category,
               service_challenge.category,
               shared_challenge.category,
               test_challenge.category,
               exercise_challenge.category
           ) AS challenge_category,
           CASE
               WHEN game_team.id IS NOT NULL OR service_team.id IS NOT NULL THEN 'Team'
               WHEN shared_challenge.id IS NOT NULL THEN 'Shared'
               WHEN test_challenge.id IS NOT NULL THEN 'AdminTest'
               WHEN exercise_instance.id IS NOT NULL THEN 'Exercise'
               ELSE 'Unassigned'
           END AS owner_kind,
           CASE
               WHEN exercise_instance.id IS NOT NULL
               THEN COALESCE(exercise_user.user_name, NULLIF(exercise_user.real_name, ''))
               ELSE NULL
           END AS owner_name,
           container.image,
           container.id AS container_guid,
           container.container_id,
           container.started_at,
           container.expect_stop_at,
           COALESCE(container.public_ip, container.ip) AS ip,
           COALESCE(container.public_port, container.port) AS port,
           container.is_proxy
      FROM "Containers" container
 LEFT JOIN "GameInstances" game_instance
        ON game_instance.id = container.game_instance_id
 LEFT JOIN "GameChallenges" game_challenge
        ON game_challenge.id = game_instance.challenge_id
 LEFT JOIN "Participations" game_participation
        ON game_participation.id = game_instance.participation_id
 LEFT JOIN "Teams" game_team
        ON game_team.id = game_participation.team_id
 LEFT JOIN LATERAL (
               SELECT service.id,
                      service.participation_id,
                      service.challenge_id
                 FROM "AdTeamServices" service
                WHERE service.id = container.ad_team_service_id
                   OR (
                       container.ad_team_service_id IS NULL
                       AND service.container_id = container.container_id
                   )
                ORDER BY (service.id = container.ad_team_service_id) DESC, service.id
                LIMIT 1
           ) service ON TRUE
 LEFT JOIN "GameChallenges" service_challenge
        ON service_challenge.id = service.challenge_id
 LEFT JOIN "Participations" service_participation
        ON service_participation.id = service.participation_id
 LEFT JOIN "Teams" service_team
        ON service_team.id = service_participation.team_id
 LEFT JOIN LATERAL (
               SELECT challenge.id, challenge.title, challenge.category
                 FROM "GameChallenges" challenge
                WHERE challenge.shared_container_id = container.id
                ORDER BY challenge.id
                LIMIT 1
           ) shared_challenge ON TRUE
 LEFT JOIN LATERAL (
               SELECT challenge.id, challenge.title, challenge.category
                 FROM "GameChallenges" challenge
                WHERE challenge.test_container_id = container.id
                ORDER BY challenge.id
                LIMIT 1
           ) test_challenge ON TRUE
 LEFT JOIN LATERAL (
               SELECT instance.id, instance.exercise_id, instance.user_id
                 FROM "ExerciseInstances" instance
                WHERE instance.id = container.exercise_instance_id
                   OR (
                       container.exercise_instance_id IS NULL
                       AND instance.container_id = container.id
                   )
                ORDER BY (instance.id = container.exercise_instance_id) DESC, instance.id
                LIMIT 1
           ) exercise_instance ON TRUE
 LEFT JOIN "ExerciseChallenges" exercise_challenge
        ON exercise_challenge.id = exercise_instance.exercise_id
 LEFT JOIN "AspNetUsers" exercise_user
        ON exercise_user.id = exercise_instance.user_id
"#;

static INSTANCE_COUNT_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"
        WITH projected AS ({INSTANCE_PROJECTION_SQL})
        SELECT COUNT(*)
          FROM projected
         WHERE ($1::INTEGER IS NULL OR projected.team_id = $1)
           AND ($2::INTEGER IS NULL OR projected.challenge_id = $2)
        "#
    )
});

static INSTANCE_PAGE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"
        WITH projected AS ({INSTANCE_PROJECTION_SQL})
        SELECT *
          FROM projected
         WHERE ($1::INTEGER IS NULL OR projected.team_id = $1)
           AND ($2::INTEGER IS NULL OR projected.challenge_id = $2)
         ORDER BY projected.started_at, projected.container_guid
         LIMIT $3 OFFSET $4
        "#
    )
});

static INSTANCE_TEAM_FILTER_OPTIONS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"
        WITH projected AS ({INSTANCE_PROJECTION_SQL}),
        options AS (
            SELECT DISTINCT team_id AS id,
                            team_name AS label,
                            CASE
                                WHEN team_avatar_hash IS NULL THEN NULL
                                ELSE '/assets/' || team_avatar_hash || '/avatar'
                            END AS avatar,
                            NULL::SMALLINT AS category
              FROM projected
             WHERE team_id IS NOT NULL
               AND team_name IS NOT NULL
        ),
        filtered AS MATERIALIZED (
            SELECT id, label, avatar, category
              FROM options
             WHERE $1 = ''
                OR label ILIKE '%' || $1 || '%'
                OR id::TEXT = $1
        ),
        page AS (
            SELECT id, label, avatar, category
              FROM filtered
             ORDER BY LOWER(label), id
             LIMIT $2
        )
        SELECT page.id, page.label, page.avatar, page.category, summary.total
          FROM (SELECT COUNT(*)::BIGINT AS total FROM filtered) summary
     LEFT JOIN page ON TRUE
         ORDER BY LOWER(page.label) NULLS LAST, page.id NULLS LAST
        "#
    )
});

static INSTANCE_CHALLENGE_FILTER_OPTIONS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"
        WITH projected AS ({INSTANCE_PROJECTION_SQL}),
        options AS (
            SELECT DISTINCT challenge_id AS id,
                            challenge_title AS label,
                            NULL::TEXT AS avatar,
                            challenge_category AS category
              FROM projected
             WHERE challenge_id IS NOT NULL
               AND challenge_title IS NOT NULL
               AND challenge_category IS NOT NULL
        ),
        filtered AS MATERIALIZED (
            SELECT id, label, avatar, category
              FROM options
             WHERE $1 = ''
                OR label ILIKE '%' || $1 || '%'
                OR id::TEXT = $1
        ),
        page AS (
            SELECT id, label, avatar, category
              FROM filtered
             ORDER BY LOWER(label), id
             LIMIT $2
        )
        SELECT page.id, page.label, page.avatar, page.category, summary.total
          FROM (SELECT COUNT(*)::BIGINT AS total FROM filtered) summary
     LEFT JOIN page ON TRUE
         ORDER BY LOWER(page.label) NULLS LAST, page.id NULLS LAST
        "#
    )
});

fn owner_kind(value: &str) -> AppResult<ContainerOwnerKind> {
    match value {
        "Team" => Ok(ContainerOwnerKind::Team),
        "Shared" => Ok(ContainerOwnerKind::Shared),
        "AdminTest" => Ok(ContainerOwnerKind::AdminTest),
        "Exercise" => Ok(ContainerOwnerKind::Exercise),
        "Unassigned" => Ok(ContainerOwnerKind::Unassigned),
        _ => Err(AppError::internal("Unknown container owner kind")),
    }
}

fn challenge_category(value: i16) -> AppResult<ChallengeCategory> {
    <ChallengeCategory as sea_orm::ActiveEnum>::try_from_value(&value)
        .map_err(|error| AppError::internal(error.to_string()))
}

fn project_instance(row: ContainerInstanceRow) -> AppResult<ContainerInstanceModel> {
    let team = match (row.team_id, row.team_name) {
        (Some(id), Some(name)) => Some(TeamModel {
            id,
            name,
            avatar: row
                .team_avatar_hash
                .map(|hash| format!("/assets/{hash}/avatar")),
        }),
        _ => None,
    };
    let challenge = match (
        row.challenge_id,
        row.challenge_title,
        row.challenge_category,
    ) {
        (Some(id), Some(title), Some(category)) => Some(ChallengeModel {
            id,
            title,
            category: challenge_category(category)?,
        }),
        _ => None,
    };

    Ok(ContainerInstanceModel {
        team,
        challenge,
        owner_kind: owner_kind(&row.owner_kind)?,
        owner_name: row.owner_name,
        image: row.image,
        container_guid: row.container_guid,
        container_id: row.container_id,
        started_at: row.started_at,
        expect_stop_at: row.expect_stop_at,
        ip: row.ip,
        port: row.port,
        is_proxy: row.is_proxy,
        runtime_stats: None,
    })
}

fn project_filter_option(
    row: ContainerInstanceFilterOptionRow,
) -> AppResult<Option<ContainerInstanceFilterOptionModel>> {
    let (id, label) = match (row.id, row.label) {
        (Some(id), Some(label)) => (id, label),
        (None, None) => return Ok(None),
        _ => return Err(AppError::internal("Incomplete instance filter option")),
    };
    Ok(Some(ContainerInstanceFilterOptionModel {
        id,
        label,
        avatar: row.avatar,
        category: row.category.map(challenge_category).transpose()?,
    }))
}

fn filter_option_response(
    rows: Vec<ContainerInstanceFilterOptionRow>,
) -> AppResult<ArrayResponse<ContainerInstanceFilterOptionModel>> {
    let total = rows.first().map(|row| row.total).unwrap_or(0);
    let data = rows
        .into_iter()
        .filter_map(|row| project_filter_option(row).transpose())
        .collect::<AppResult<Vec<_>>>()?;
    Ok(ArrayResponse::new(data, total))
}

/// `GET /api/admin/instances` — paginated list of managed containers with their
/// concrete team or non-team ownership scope and challenge.
pub async fn instances(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<InstanceListQuery>,
) -> AppResult<ArrayResponse<ContainerInstanceModel>> {
    let count = instance_page_count(&q);
    let skip = i64::try_from(q.skip).unwrap_or(i64::MAX);
    let total = if q.team_id.is_none() && q.challenge_id.is_none() {
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Containers""#)
            .fetch_one(st.pg())
            .await
    } else {
        sqlx::query_scalar::<_, i64>(INSTANCE_COUNT_SQL.as_str())
            .bind(q.team_id)
            .bind(q.challenge_id)
            .fetch_one(st.pg())
            .await
    }
    .map_err(|error| AppError::internal(error.to_string()))?;
    let rows = sqlx::query_as::<_, ContainerInstanceRow>(INSTANCE_PAGE_SQL.as_str())
        .bind(q.team_id)
        .bind(q.challenge_id)
        .bind(count)
        .bind(skip)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut data = rows
        .into_iter()
        .map(project_instance)
        .collect::<AppResult<Vec<_>>>()?;

    if q.include_runtime_stats && !data.is_empty() {
        let identities = data
            .iter()
            .map(|instance| (instance.container_guid, instance.container_id.clone()))
            .collect::<Vec<_>>();
        let mut samples = sample_runtime_batch(
            &st.containers,
            &INSTANCE_RUNTIME_CACHE,
            &INSTANCE_RUNTIME_SLOTS,
            &identities,
        )
        .await;
        for instance in &mut data {
            instance.runtime_stats = samples.remove(&instance.container_guid);
        }
    }

    Ok(ArrayResponse::new(data, total))
}

/// `GET /api/admin/instances/filter-options` — bounded server-side discovery
/// of teams or challenges that own at least one active managed container.
pub async fn instance_filter_options(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<InstanceFilterOptionsQuery>,
) -> AppResult<ArrayResponse<ContainerInstanceFilterOptionModel>> {
    let search = q.search.trim();
    if search.chars().count() > INSTANCE_FILTER_SEARCH_MAX_CHARS {
        return Err(AppError::bad_request("Filter search is too long"));
    }

    let count = instance_filter_option_count(&q);
    let sql = match q.kind {
        ContainerInstanceFilterKind::Team => INSTANCE_TEAM_FILTER_OPTIONS_SQL.as_str(),
        ContainerInstanceFilterKind::Challenge => INSTANCE_CHALLENGE_FILTER_OPTIONS_SQL.as_str(),
    };
    let rows = sqlx::query_as::<_, ContainerInstanceFilterOptionRow>(sql)
        .bind(search)
        .bind(count)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    filter_option_response(rows)
}

/// `DELETE /api/admin/instances/{id}` — forcibly destroy a container.
pub async fn destroy_instance(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<MessageResponse> {
    let c = container::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Container instance not found"))?;

    crate::controllers::game::destroy_managed_container_row(&st, &c, false).await?;
    INSTANCE_RUNTIME_CACHE.remove(&c.container_id).await;
    Ok(MessageResponse::ok(""))
}

/// RSCTF `ContainerStatsModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerStatsModel {
    pub cpu_percent: f64,
    pub memory_used_bytes: i64,
    pub memory_limit_bytes: i64,
    pub net_rx_bytes: i64,
    pub net_tx_bytes: i64,
    #[serde(with = "crate::utils::datetime::millis")]
    pub sampled_at: DateTime<Utc>,
}

/// `GET /api/admin/instances/{id}/stats` — point-in-time container stats.
///
/// Mirrors RSCTF `AdminController.GetInstanceStats`: look up the container row by
/// its database GUID, then sample the live runtime via `st.containers.query`,
/// which reads the Docker stats API and returns a `ContainerStatus` carrying
/// CPU (as a fraction of one core) and memory (bytes); it does not expose
/// a memory limit or per-interface network counters, so those DTO fields stay `0`
/// (matching the "stats the backend can provide" contract). `cpu_usage` is scaled
/// ×100 to the `cpuPercent` (0–100 × cores) the client renders.
///
/// When the runtime can't provide a sample — no Docker backend configured, the
/// daemon is unreachable, or the container is already gone — `query` errors; we
/// degrade to a 404 with a null payload, exactly like RSCTF returns when
/// `GetStatsAsync` yields `null`.
pub async fn instance_stats(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<RequestResponse<ContainerStatsModel>> {
    let runtime_id =
        sqlx::query_scalar::<_, String>(r#"SELECT container_id FROM "Containers" WHERE id = $1"#)
            .bind(id)
            .fetch_optional(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .ok_or_else(|| AppError::not_found("Container instance not found"))?;

    let sample = sample_runtime(
        &st.containers,
        &INSTANCE_RUNTIME_CACHE,
        &INSTANCE_RUNTIME_SLOTS,
        &runtime_id,
    )
    .await;
    if sample.availability == ContainerRuntimeAvailability::Unavailable {
        return Err(AppError::not_found("Stats unavailable for this container."));
    }

    Ok(RequestResponse::ok(ContainerStatsModel {
        cpu_percent: sample.cpu_percent.unwrap_or(0.0),
        memory_used_bytes: sample.memory_used_bytes.unwrap_or(0),
        // The coarse ContainerStatus sample carries no memory limit or network
        // counters; leave them zero until the backend surfaces them.
        memory_limit_bytes: 0,
        net_rx_bytes: 0,
        net_tx_bytes: 0,
        sampled_at: sample.sampled_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::services::container::{ContainerInfo, ContainerSpec};

    struct TrackingRuntime {
        calls: AtomicUsize,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        missing: bool,
    }

    impl TrackingRuntime {
        fn new(missing: bool) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                missing,
            }
        }
    }

    struct InFlight<'a>(&'a AtomicUsize);

    impl Drop for InFlight<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl ContainerManager for TrackingRuntime {
        async fn create(&self, _spec: ContainerSpec) -> AppResult<ContainerInfo> {
            Err(AppError::bad_request("not used by runtime sampling test"))
        }

        async fn destroy(&self, _id: &str) -> AppResult<()> {
            Err(AppError::bad_request("not used by runtime sampling test"))
        }

        async fn query(&self, id: &str) -> AppResult<ContainerStatus> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(current, Ordering::SeqCst);
            let _in_flight = InFlight(&self.in_flight);
            tokio::time::sleep(StdDuration::from_millis(15)).await;
            if self.missing {
                return Err(AppError::not_found("runtime missing"));
            }
            Ok(ContainerStatus {
                id: id.to_owned(),
                status: "running".to_owned(),
                memory_bytes: Some(1_024),
                cpu_usage: Some(0.5),
            })
        }
    }

    #[test]
    fn ownership_projection_rejects_unknown_database_values() {
        assert_eq!(owner_kind("Shared").unwrap(), ContainerOwnerKind::Shared);
        assert!(owner_kind("LegacyMystery").is_err());
    }

    #[test]
    fn runtime_pages_and_inventory_pages_have_hard_size_bounds() {
        assert_eq!(
            instance_page_count(&InstanceListQuery {
                count: 100,
                skip: 0,
                include_runtime_stats: true,
                team_id: None,
                challenge_id: None,
            }),
            50
        );
        assert_eq!(
            instance_page_count(&InstanceListQuery {
                count: 1_000,
                skip: 0,
                include_runtime_stats: false,
                team_id: None,
                challenge_id: None,
            }),
            500
        );
        assert_eq!(
            instance_filter_option_count(&InstanceFilterOptionsQuery {
                kind: ContainerInstanceFilterKind::Team,
                search: String::new(),
                count: 500,
            }),
            50
        );
        assert_eq!(
            instance_filter_option_count(&InstanceFilterOptionsQuery {
                kind: ContainerInstanceFilterKind::Challenge,
                search: String::new(),
                count: 0,
            }),
            0
        );
    }

    #[test]
    fn filter_option_discovery_retains_backend_admin_authentication() {
        let source = include_str!("instances.rs");
        let start = source
            .find("pub async fn instance_filter_options")
            .expect("filter-options handler exists");
        let end = (start + 260).min(source.len());
        assert!(source[start..end].contains("_admin: AdminUser"));
    }

    #[tokio::test]
    async fn runtime_batch_caps_backend_concurrency() {
        let tracked = Arc::new(TrackingRuntime::new(false));
        let manager: Arc<dyn ContainerManager> = tracked.clone();
        let cache = RuntimeSampleCache::new(StdDuration::from_secs(60), 128);
        let slots = Semaphore::new(4);
        let instances = (1_u128..=100)
            .map(|value| (Uuid::from_u128(value), format!("runtime-{value}")))
            .collect::<Vec<_>>();

        let samples = sample_runtime_batch(&manager, &cache, &slots, &instances).await;

        assert_eq!(samples.len(), 100);
        assert_eq!(tracked.calls.load(Ordering::SeqCst), 100);
        assert!(tracked.max_in_flight.load(Ordering::SeqCst) <= 4);
        assert!(samples.values().all(|sample| {
            sample.availability == ContainerRuntimeAvailability::Available
                && sample.cpu_percent == Some(50.0)
                && sample.memory_limit_bytes.is_none()
                && sample.net_rx_bytes.is_none()
        }));
    }

    #[tokio::test]
    async fn missing_runtime_is_coalesced_and_negatively_cached() {
        let tracked = Arc::new(TrackingRuntime::new(true));
        let manager: Arc<dyn ContainerManager> = tracked.clone();
        let cache = RuntimeSampleCache::new(StdDuration::from_secs(60), 128);
        let slots = Semaphore::new(4);
        let instances = (1_u128..=100)
            .map(|value| (Uuid::from_u128(value), "missing-runtime".to_owned()))
            .collect::<Vec<_>>();

        let first = sample_runtime_batch(&manager, &cache, &slots, &instances).await;
        let second = sample_runtime_batch(&manager, &cache, &slots, &instances).await;

        assert_eq!(first.len(), 100);
        assert_eq!(second.len(), 100);
        assert_eq!(tracked.calls.load(Ordering::SeqCst), 1);
        assert!(first.values().chain(second.values()).all(|sample| {
            sample.availability == ContainerRuntimeAvailability::Unavailable
                && sample.cpu_percent.is_none()
                && sample.memory_used_bytes.is_none()
        }));
    }

    #[tokio::test]
    async fn runtime_sample_cache_has_a_hard_entry_bound() {
        let cache = RuntimeSampleCache::new(StdDuration::from_secs(60), 2);
        for runtime_id in ["runtime-a", "runtime-b", "runtime-c"] {
            cache
                .insert(
                    runtime_id.to_owned(),
                    unavailable_runtime_sample(Utc::now()),
                )
                .await;
        }

        assert_eq!(cache.entries.read().await.len(), 2);
    }
}

#[cfg(test)]
#[path = "instances/postgres_tests.rs"]
mod postgres_tests;

// ─── Files ─────────────────────────────────────────────────────────────────────

/// RSCTF `LocalFile`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFileModel {
    pub hash: String,
    pub name: String,
}
