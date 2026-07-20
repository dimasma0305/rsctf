//! Publication and teardown fencing for per-team A&D service endpoints.

use std::future::Future;

use crate::app_state::SharedState;
use crate::utils::enums::{
    AdCheckStatus, ChallengeReviewStatus, ChallengeType, ParticipationStatus,
};
use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Copy)]
pub(crate) struct ManagedBackendPublication<'a> {
    pub(crate) game_id: i32,
    pub(crate) participation_id: i32,
    pub(crate) challenge_id: i32,
    pub(crate) host: &'a str,
    pub(crate) port: i32,
    pub(crate) backend_id: &'a str,
}

pub(crate) fn service_lock_key(participation_id: i32, challenge_id: i32) -> String {
    format!("ad-service:{participation_id}:{challenge_id}")
}

fn publication_barrier_key(game_id: i32) -> String {
    format!("ad-service-publication-game:{game_id}")
}

fn challenge_publication_barrier_key(challenge_id: i32) -> String {
    format!("ad-service-publication-challenge:{challenge_id}")
}

/// Take shared game/challenge publication parents before the established
/// per-pair writer lock, all on one PostgreSQL transaction. Publishers for
/// distinct pairs remain concurrent; rare eligibility transitions take an
/// exclusive parent without creating a parent/leaf lock inversion.
pub(crate) async fn acquire_publication_lock(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
) -> AppResult<crate::utils::single_flight::PgAdvisoryLock> {
    let pair_key = service_lock_key(participation_id, challenge_id);
    let parents = [
        publication_barrier_key(game_id),
        challenge_publication_barrier_key(challenge_id),
    ];
    crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning_below_shared(
        pool, &parents, &pair_key,
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Exclusive challenge publication parent used by active topology flips. It
/// may be retained while existing leaf rows are torn down because publishers
/// always take the shared parent before their per-pair leaf.
pub(crate) async fn acquire_challenge_publication_fence(
    pool: &sqlx::PgPool,
    challenge_id: i32,
) -> AppResult<crate::utils::single_flight::PgAdvisoryLock> {
    let key = challenge_publication_barrier_key(challenge_id);
    crate::utils::single_flight::PgAdvisoryLock::acquire(pool, &key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

/// Drain creators that crossed the caller's now-committed eligibility fence.
///
/// The caller must first make new publication ineligible (disabled challenge,
/// ended game, or suspended participation). Taking and immediately releasing
/// the exclusive barrier then proves every older creator either published and
/// completed its rollback or released its durable backend identity before the
/// deletion takes its service-row snapshot. Later creators observe the marker
/// on their first eligibility read and cannot launch a backend.
pub(crate) async fn drain_publications(
    pool: &sqlx::PgPool,
    game_ids: impl IntoIterator<Item = i32>,
) -> AppResult<()> {
    let mut game_ids = game_ids.into_iter().collect::<Vec<_>>();
    game_ids.sort_unstable();
    game_ids.dedup();
    for game_id in game_ids {
        let key = publication_barrier_key(game_id);
        let lock = crate::utils::single_flight::PgAdvisoryLock::acquire(pool, &key)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(())
}

/// Publish only while all durable owners remain eligible. This single SQL
/// statement closes the last check-to-insert window and preserves
/// `last_reset_at` when it fills an existing offline placeholder.
pub(crate) async fn publish_managed_backend_if_eligible(
    pool: &sqlx::PgPool,
    publication: ManagedBackendPublication<'_>,
) -> AppResult<bool> {
    let published = sqlx::query_scalar::<_, i32>(
        r#"INSERT INTO "AdTeamServices"
             (game_id, participation_id, challenge_id, host, port, status,
              container_id, last_reset_at)
           SELECT game.id, participation.id, challenge.id, $4, $5, $6, $7, NULL
             FROM "Games" game
             JOIN "Participations" participation
               ON participation.game_id = game.id
              AND participation.id = $2
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "GameChallenges" challenge
               ON challenge.game_id = game.id
              AND challenge.id = $3
            WHERE game.id = $1
              AND game.deletion_pending = FALSE
              AND game.end_time_utc >= clock_timestamp()
              AND participation.status = $8
              AND team.deletion_pending = FALSE
              AND challenge.is_enabled = TRUE
              AND challenge.deletion_pending = FALSE
              AND challenge.review_status = $9
              AND challenge."Type" = $10
              AND challenge.ad_self_hosted = FALSE
           ON CONFLICT (participation_id, challenge_id) DO UPDATE
                 SET game_id = EXCLUDED.game_id,
                     host = EXCLUDED.host,
                     port = EXCLUDED.port,
                     status = EXCLUDED.status,
                     container_id = EXCLUDED.container_id
               WHERE "AdTeamServices".game_id = EXCLUDED.game_id
           RETURNING id"#,
    )
    .bind(publication.game_id)
    .bind(publication.participation_id)
    .bind(publication.challenge_id)
    .bind(publication.host)
    .bind(publication.port)
    .bind(AdCheckStatus::Ok as i16)
    .bind(publication.backend_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(published.is_some())
}

/// Attach the freshly-created backend as an inactive retry owner before any
/// endpoint can be published. Eligibility may already be fenced, but hard
/// deletion keeps the game/participation/challenge ownership rows until after
/// the publication drain, so it can discover and retry a failed direct destroy.
pub(crate) async fn retain_created_backend_identity(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    backend_id: &str,
) -> AppResult<bool> {
    let retained = sqlx::query_scalar::<_, i32>(
        r#"INSERT INTO "AdTeamServices"
             (game_id, participation_id, challenge_id, host, port, status,
              container_id, last_reset_at)
           SELECT game.id, participation.id, challenge.id, '', 0, $5, $4, NULL
             FROM "Games" game
             JOIN "Participations" participation
               ON participation.game_id = game.id
              AND participation.id = $2
             JOIN "GameChallenges" challenge
               ON challenge.game_id = game.id
              AND challenge.id = $3
            WHERE game.id = $1
           ON CONFLICT (participation_id, challenge_id) DO UPDATE
                 SET game_id = EXCLUDED.game_id,
                     host = '',
                     port = 0,
                     status = EXCLUDED.status,
                     container_id = EXCLUDED.container_id
               WHERE "AdTeamServices".game_id = EXCLUDED.game_id
           RETURNING id"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(challenge_id)
    .bind(backend_id)
    .bind(AdCheckStatus::Offline as i16)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(retained.is_some())
}

/// Deactivate only the endpoint that still owns the newly-created backend, then
/// invoke the caller's direct backend destroy even when the owner row has
/// already cascaded. Reconciliation must succeed first so a stale kernel rule
/// can never route to an address after the backend releases it. A surviving
/// replacement is never touched.
pub(crate) async fn rollback_created_backend_with<R, D>(
    pool: &sqlx::PgPool,
    participation_id: i32,
    challenge_id: i32,
    backend_id: &str,
    reconcile: R,
    destroy: D,
) -> AppResult<()>
where
    R: Future<Output = AppResult<()>>,
    D: Future<Output = AppResult<()>>,
{
    sqlx::query(
        r#"UPDATE "AdTeamServices"
              SET host = '', port = 0, status = $4
            WHERE participation_id = $1
              AND challenge_id = $2
              AND container_id = $3"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .bind(backend_id)
    .bind(AdCheckStatus::Offline as i16)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    // Rebuild from current durable state even when the exact row vanished. It
    // may have been routable immediately before a cascading delete, and freeing
    // its address while an old kernel rule survives would expose a future reuse.
    reconcile.await?;
    destroy.await
}

pub(crate) async fn rollback_created_backend(
    state: &SharedState,
    participation_id: i32,
    challenge_id: i32,
    backend_id: &str,
) -> AppResult<()> {
    rollback_created_backend_with(
        state.pg(),
        participation_id,
        challenge_id,
        backend_id,
        crate::services::ad_vpn::ensure_hub_and_sync(&state.db),
        crate::services::traffic::destroy_container_after_capture_fence(state, backend_id),
    )
    .await
}

/// Re-read and tear down one row while its per-pair lock is held. The backend
/// identity remains attached until capture fencing and runtime destruction both
/// succeed, making a failed hard delete exactly retryable.
pub(crate) async fn destroy_persisted_service(
    state: &SharedState,
    service_id: i32,
) -> AppResult<()> {
    let backend_id = sqlx::query_scalar::<_, Option<String>>(
        r#"SELECT container_id FROM "AdTeamServices" WHERE id = $1"#,
    )
    .bind(service_id)
    .fetch_optional(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(backend_id) = backend_id else {
        return Ok(());
    };
    crate::services::ad_vpn::deactivate_team_service(&state.db, service_id).await?;
    if let Some(backend_id) = backend_id {
        crate::services::traffic::destroy_container_after_capture_fence(state, &backend_id).await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "service_lifecycle_tests.rs"]
mod tests;
