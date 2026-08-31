//! Bounded container and orphan maintenance.

use super::*;
use crate::models::data::container;
use crate::utils::enums::ContainerStatus;

const REAPER_BATCH_SIZE: i64 = 32;
const ORPHAN_SCAN_BATCH_SIZE: usize = 512;
const ORPHAN_DESTROY_BATCH_SIZE: usize = 32;
const MAINTENANCE_PASS_BUDGET: StdDuration = StdDuration::from_secs(20);
// Player/exercise owners have a 120-second absolute create deadline. Keep a
// larger discovery grace so a backend accepted just before timeout can be
// recorded or reconciled before generic orphan cleanup considers it.
const ORPHAN_GRACE_SECS: u64 = 300;
const ORPHAN_TRACKING_LIMIT: usize = 4_096;
const REAP_MARKER_CLEANUP_BATCH: i64 = 64;

static ORPHAN_FIRST_SEEN: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(Default::default);
static ORPHAN_SCAN_CURSOR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(sqlx::FromRow)]
struct ReapCandidate {
    id: uuid::Uuid,
    image: String,
    container_id: String,
    status: i16,
    started_at: chrono::DateTime<Utc>,
    expect_stop_at: chrono::DateTime<Utc>,
    is_proxy: bool,
    ip: String,
    port: i32,
    public_ip: Option<String>,
    public_port: Option<i32>,
    game_instance_id: Option<i32>,
    exercise_instance_id: Option<i32>,
    ad_team_service_id: Option<i32>,
}

impl ReapCandidate {
    fn into_model(self) -> AppResult<container::Model> {
        let status = match self.status {
            0 => ContainerStatus::Pending,
            1 => ContainerStatus::Running,
            2 => ContainerStatus::Destroyed,
            value => {
                return Err(crate::utils::error::AppError::internal(format!(
                    "container {} has invalid status {value}",
                    self.id
                )))
            }
        };
        Ok(container::Model {
            id: self.id,
            image: self.image,
            container_id: self.container_id,
            status,
            started_at: self.started_at,
            expect_stop_at: self.expect_stop_at,
            is_proxy: self.is_proxy,
            ip: self.ip,
            port: self.port,
            public_ip: self.public_ip,
            public_port: self.public_port,
            game_instance_id: self.game_instance_id,
            exercise_instance_id: self.exercise_instance_id,
            ad_team_service_id: self.ad_team_service_id,
        })
    }
}

/// Destroy a bounded oldest-first page. The singleton maintenance lease and
/// per-owner destroy lock prevent duplicate external work; later rows remain an
/// explicit backlog for the next tick instead of extending this pass forever.
pub(super) async fn reap_expired_containers(state: &SharedState) -> AppResult<u64> {
    sqlx::query(
        r#"WITH stale AS (
               SELECT reap.backend_id
                 FROM "ManagedContainerReapOperations" reap
                WHERE reap.lease_expires_at_utc <= clock_timestamp()
                  AND NOT EXISTS (
                      SELECT 1 FROM "Containers" container
                       WHERE container.id = reap.container_id
                         AND container.container_id = reap.backend_id
                  )
                ORDER BY reap.lease_expires_at_utc, reap.backend_id
                LIMIT $1
           )
           DELETE FROM "ManagedContainerReapOperations" reap
            USING stale WHERE reap.backend_id = stale.backend_id"#,
    )
    .bind(REAP_MARKER_CLEANUP_BATCH)
    .execute(state.pg())
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    let mut transaction = state
        .pg()
        .begin()
        .await
        .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    let candidates = sqlx::query_as::<_, ReapCandidate>(
        r#"SELECT id, image, container_id, status, started_at, expect_stop_at,
                  is_proxy, ip, port, public_ip, public_port, game_instance_id,
                  exercise_instance_id, ad_team_service_id
             FROM "Containers"
            WHERE expect_stop_at < clock_timestamp()
            ORDER BY expect_stop_at, id
            FOR UPDATE SKIP LOCKED
            LIMIT $1"#,
    )
    .bind(REAPER_BATCH_SIZE + 1)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    let backlog = candidates.len() > REAPER_BATCH_SIZE as usize;
    let started = std::time::Instant::now();
    let mut reaped = 0u64;
    let mut attempted = 0u64;
    for candidate in candidates.into_iter().take(REAPER_BATCH_SIZE as usize) {
        if started.elapsed() >= MAINTENANCE_PASS_BUDGET {
            break;
        }
        let candidate = candidate.into_model()?;
        attempted += 1;
        match crate::controllers::game::destroy_managed_container_row(state, &candidate, true).await
        {
            Ok(true) => reaped += 1,
            Ok(false) => {}
            Err(error) => tracing::warn!(
                container = %candidate.id,
                backend_id = %candidate.container_id,
                %error,
                "cron: endpoint revocation failed; retaining expired container"
            ),
        }
    }
    tracing::debug!(
        attempted,
        reaped,
        backlog,
        elapsed_ms = started.elapsed().as_millis(),
        "cron: bounded expired-container pass"
    );
    Ok(reaped)
}

#[derive(Default)]
struct KnownContainerIds {
    exact: std::collections::HashSet<String>,
    docker_prefixes: std::collections::HashSet<String>,
}

impl KnownContainerIds {
    fn insert(&mut self, id: String) {
        if docker_id_shape(&id) {
            self.docker_prefixes.insert(id[..12].to_ascii_lowercase());
        }
        self.exact.insert(id);
    }

    fn contains(&self, id: &str) -> bool {
        self.exact.contains(id)
            || (docker_id_shape(id)
                && self
                    .docker_prefixes
                    .contains(&id[..12].to_ascii_lowercase()))
    }
}

async fn load_known_container_ids(
    state: &SharedState,
    candidates: &[String],
) -> AppResult<KnownContainerIds> {
    let docker_prefixes = candidates
        .iter()
        .filter(|id| docker_id_shape(id))
        .map(|id| id[..12].to_ascii_lowercase())
        .collect::<Vec<_>>();
    let (containers, services, targets, cycles, player_operations, exercise_operations) =
        tokio::try_join!(
            sqlx::query_scalar::<_, String>(
                r#"SELECT container_id FROM "Containers"
                WHERE container_id = ANY($1)
                   OR LOWER(LEFT(container_id, 12)) = ANY($2)"#,
            )
            .bind(candidates)
            .bind(&docker_prefixes)
            .fetch_all(state.pg()),
            sqlx::query_scalar::<_, String>(
                r#"SELECT container_id FROM "AdTeamServices"
                WHERE container_id = ANY($1)
                   OR LOWER(LEFT(container_id, 12)) = ANY($2)"#,
            )
            .bind(candidates)
            .bind(&docker_prefixes)
            .fetch_all(state.pg()),
            sqlx::query_scalar::<_, String>(
                r#"SELECT container_id FROM "KothTargets"
                WHERE container_id = ANY($1)
                   OR LOWER(LEFT(container_id, 12)) = ANY($2)"#,
            )
            .bind(candidates)
            .bind(&docker_prefixes)
            .fetch_all(state.pg()),
            sqlx::query_scalar::<_, String>(
                r#"SELECT DISTINCT runtime_id
                 FROM "KothCrownCycles" cycle
                 CROSS JOIN LATERAL unnest(ARRAY[
                   cycle.old_container_id, cycle.replacement_container_id
                 ]) runtime(runtime_id)
                WHERE cycle.phase <> 'Ended'
                  AND (runtime_id = ANY($1)
                       OR LOWER(LEFT(runtime_id, 12)) = ANY($2))"#,
            )
            .bind(candidates)
            .bind(&docker_prefixes)
            .fetch_all(state.pg()),
            sqlx::query_scalar::<_, String>(
                r#"SELECT backend_id FROM "PlayerContainerOperations"
                WHERE backend_id IS NOT NULL AND state = 'Running'
                  AND lease_expires_at_utc > clock_timestamp() - interval '5 minutes'
                  AND (backend_id = ANY($1)
                       OR LOWER(LEFT(backend_id, 12)) = ANY($2))"#,
            )
            .bind(candidates)
            .bind(&docker_prefixes)
            .fetch_all(state.pg()),
            sqlx::query_scalar::<_, String>(
                r#"SELECT backend_id FROM "ExerciseContainerOperations"
                WHERE backend_id IS NOT NULL AND state = 'Running'
                  AND lease_expires_at_utc > clock_timestamp() - interval '5 minutes'
                  AND (backend_id = ANY($1)
                       OR LOWER(LEFT(backend_id, 12)) = ANY($2))"#,
            )
            .bind(candidates)
            .bind(&docker_prefixes)
            .fetch_all(state.pg()),
        )
        .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    let mut known = KnownContainerIds::default();
    for id in containers
        .into_iter()
        .chain(services)
        .chain(targets)
        .chain(cycles)
        .chain(player_operations)
        .chain(exercise_operations)
    {
        known.insert(id);
    }
    Ok(known)
}

async fn runtime_has_durable_owner(state: &SharedState, id: &str) -> AppResult<bool> {
    let prefix = docker_id_shape(id).then(|| id[..12].to_ascii_lowercase());
    sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "Containers" WHERE container_id = $1
                  OR ($2::text IS NOT NULL AND LOWER(LEFT(container_id, 12)) = $2)
               UNION ALL
               SELECT 1 FROM "AdTeamServices" WHERE container_id = $1
                  OR ($2::text IS NOT NULL AND LOWER(LEFT(container_id, 12)) = $2)
               UNION ALL
               SELECT 1 FROM "KothTargets" WHERE container_id = $1
                  OR ($2::text IS NOT NULL AND LOWER(LEFT(container_id, 12)) = $2)
               UNION ALL
               SELECT 1 FROM "KothCrownCycles" cycle
                CROSS JOIN LATERAL unnest(ARRAY[
                    cycle.old_container_id, cycle.replacement_container_id
                ]) runtime(runtime_id)
                WHERE cycle.phase <> 'Ended'
                  AND (runtime_id = $1 OR ($2::text IS NOT NULL
                       AND LOWER(LEFT(runtime_id, 12)) = $2))
               UNION ALL
               SELECT 1 FROM "PlayerContainerOperations"
                WHERE backend_id IS NOT NULL AND state = 'Running'
                  AND lease_expires_at_utc > clock_timestamp() - interval '5 minutes'
                  AND (backend_id = $1 OR ($2::text IS NOT NULL
                       AND LOWER(LEFT(backend_id, 12)) = $2))
               UNION ALL
               SELECT 1 FROM "ExerciseContainerOperations"
                WHERE backend_id IS NOT NULL AND state = 'Running'
                  AND lease_expires_at_utc > clock_timestamp() - interval '5 minutes'
                  AND (backend_id = $1 OR ($2::text IS NOT NULL
                       AND LOWER(LEFT(backend_id, 12)) = $2))
           )"#,
    )
    .bind(id)
    .bind(prefix)
    .fetch_one(state.pg())
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))
}

fn rotating_batch(mut managed: Vec<String>) -> (Vec<String>, usize) {
    managed.sort_unstable();
    managed.dedup();
    let total = managed.len();
    if total <= ORPHAN_SCAN_BATCH_SIZE {
        return (managed, total);
    }
    let start = ORPHAN_SCAN_CURSOR
        .fetch_add(ORPHAN_SCAN_BATCH_SIZE, std::sync::atomic::Ordering::Relaxed)
        % total;
    let batch = (0..ORPHAN_SCAN_BATCH_SIZE)
        .map(|offset| managed[(start + offset) % total].clone())
        .collect();
    (batch, total)
}

/// Scan a rotating bounded runtime page, use constant-time ownership checks,
/// and destroy at most one bounded batch after the grace window.
pub(super) async fn sweep_orphan_containers(state: &SharedState) -> AppResult<u64> {
    let (managed, managed_total) = rotating_batch(state.containers.list_managed().await);
    if managed.is_empty() {
        return Ok(0);
    }
    let known = load_known_container_ids(state, &managed).await?;
    let now = std::time::Instant::now();
    let mut ready = Vec::new();
    {
        let mut first_seen = ORPHAN_FIRST_SEEN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Keep observations from other rotating pages so a large inventory can
        // still satisfy the grace interval; the hard cap below bounds vanished
        // or never-revisited IDs.
        first_seen.retain(|id, _| !known.contains(id));
        for id in &managed {
            if known.contains(id) {
                continue;
            }
            if first_seen.len() >= ORPHAN_TRACKING_LIMIT && !first_seen.contains_key(id) {
                if let Some(oldest) = first_seen
                    .iter()
                    .min_by_key(|(_, seen)| **seen)
                    .map(|(id, _)| id.clone())
                {
                    first_seen.remove(&oldest);
                }
            }
            let seen = first_seen.entry(id.clone()).or_insert(now);
            if now.duration_since(*seen) >= StdDuration::from_secs(ORPHAN_GRACE_SECS) {
                ready.push(id.clone());
            }
        }
    }
    ready.sort_unstable();
    ready.truncate(ORPHAN_DESTROY_BATCH_SIZE);
    let started = std::time::Instant::now();
    let mut swept = 0u64;
    for id in ready {
        if started.elapsed() >= MAINTENANCE_PASS_BUDGET {
            break;
        }
        if runtime_has_durable_owner(state, &id).await? {
            ORPHAN_FIRST_SEEN
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            continue;
        }
        if let Err(error) =
            crate::services::ad_vpn::deactivate_backend_endpoint(&state.db, &id).await
        {
            tracing::warn!(backend_id = %id, %error, "cron: orphan endpoint revocation failed");
            continue;
        }
        if let Err(error) = state.containers.destroy(&id).await {
            tracing::warn!(backend_id = %id, %error, "cron: orphan destroy failed");
        } else {
            ORPHAN_FIRST_SEEN
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            swept += 1;
        }
    }
    tracing::debug!(
        managed_total,
        scanned = managed.len(),
        swept,
        elapsed_ms = started.elapsed().as_millis(),
        "cron: bounded orphan pass"
    );
    Ok(swept)
}

#[cfg(test)]
fn container_id_is_known(id: &str, known: &[String]) -> bool {
    let mut indexed = KnownContainerIds::default();
    for candidate in known {
        indexed.insert(candidate.clone());
    }
    indexed.contains(id)
}

fn docker_id_shape(value: &str) -> bool {
    (12..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::{container_id_is_known, rotating_batch};

    #[test]
    fn orphan_identity_matching_accepts_full_and_daemon_short_ids_only() {
        let known = vec!["abcdef1234567890".to_string()];
        assert!(container_id_is_known("abcdef1234567890", &known));
        assert!(container_id_is_known("abcdef123456", &known));
        assert!(container_id_is_known("abcdef1234567890ffff", &known));
        assert!(!container_id_is_known("fedcba123456", &known));
        assert!(!container_id_is_known("abc", &known));

        let named = vec!["rsctf-koth-cycle-17".to_string()];
        assert!(container_id_is_known("rsctf-koth-cycle-17", &named));
        assert!(!container_id_is_known("rsctf-koth-cycle", &named));
        assert!(!container_id_is_known("rsctf-koth-cycle-17-extra", &named));
    }

    #[test]
    fn orphan_inventory_scan_is_deduplicated_and_bounded() {
        let managed = (0..700)
            .flat_map(|id| [format!("runtime-{id:04}"), format!("runtime-{id:04}")])
            .collect();
        let (batch, total) = rotating_batch(managed);
        assert_eq!(total, 700);
        assert_eq!(batch.len(), 512);
        assert_eq!(
            batch.iter().collect::<std::collections::HashSet<_>>().len(),
            512
        );
    }
}
