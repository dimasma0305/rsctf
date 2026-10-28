//! Ref-counted blob ownership for immutable final A&D service snapshots.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::storage::BlobStorage;
use crate::utils::error::{AppError, AppResult};

use super::{database_error, lock_hash, purge_if_unreferenced};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ServiceSnapshotBlob {
    pub id: i64,
    pub team_service_id: i32,
    pub hash: String,
    pub name: String,
    pub file_size: i64,
    pub source_container_id: String,
    pub captured_at_utc: DateTime<Utc>,
    pub expires_at_utc: Option<DateTime<Utc>>,
}

/// Exact runtime and event window observed before exporting the filesystem.
#[derive(Clone, Copy)]
pub struct ServiceSnapshotSource<'a> {
    pub team_service_id: i32,
    pub container_id: &'a str,
    pub end_time_utc: DateTime<Utc>,
}

const SELECT_SNAPSHOT_SQL: &str = r#"
    SELECT snapshot.id, snapshot.team_service_id,
           file.hash, file.name, file.file_size,
           snapshot.source_container_id, snapshot.captured_at_utc,
           snapshot.expires_at_utc
      FROM "AdServiceSnapshots" snapshot
      JOIN "Files" file ON file.id = snapshot.local_file_id
     WHERE snapshot.team_service_id = $1
"#;

const SELECT_SNAPSHOT_FOR_UPDATE_SQL: &str = r#"
    SELECT snapshot.id, snapshot.team_service_id,
           file.hash, file.name, file.file_size,
           snapshot.source_container_id, snapshot.captured_at_utc,
           snapshot.expires_at_utc
      FROM "AdServiceSnapshots" snapshot
      JOIN "Files" file ON file.id = snapshot.local_file_id
     WHERE snapshot.team_service_id = $1
       FOR UPDATE OF snapshot, file
"#;

const SELECT_AVAILABLE_SNAPSHOT_SQL: &str = r#"
    SELECT snapshot.id, snapshot.team_service_id,
           file.hash, file.name, file.file_size,
           snapshot.source_container_id, snapshot.captured_at_utc,
           snapshot.expires_at_utc
      FROM "AdServiceSnapshots" snapshot
      JOIN "Files" file ON file.id = snapshot.local_file_id
      JOIN "AdTeamServices" service ON service.id = snapshot.team_service_id
      JOIN "Games" game ON game.id = service.game_id
     WHERE snapshot.team_service_id = $1
       AND snapshot.captured_at_utc >= game.end_time_utc
       AND (service.container_id IS NULL OR service.container_id = snapshot.source_container_id)
       AND (snapshot.expires_at_utc IS NULL
            OR snapshot.expires_at_utc > clock_timestamp())
       AND file.reference_count > 0
"#;

async fn select_snapshot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_service_id: i32,
) -> AppResult<Option<ServiceSnapshotBlob>> {
    sqlx::query_as::<_, ServiceSnapshotBlob>(SELECT_SNAPSHOT_FOR_UPDATE_SQL)
        .bind(team_service_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)
}

/// Store the current final snapshot and acquire exactly one `Files` reference.
///
/// `None` means the configured retention deadline elapsed before capture. The
/// per-service advisory lock makes concurrent maintenance/API capture attempts
/// converge on the first committed blob for this runtime/event window without
/// acquiring duplicate references. A stale snapshot is replaced only after the
/// new bytes are staged and its source is revalidated under domain row locks.
pub async fn store_service_snapshot(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    source: ServiceSnapshotSource<'_>,
    expires_at_utc: Option<DateTime<Utc>>,
    name: &str,
    bytes: &[u8],
) -> AppResult<Option<ServiceSnapshotBlob>> {
    let ServiceSnapshotSource {
        team_service_id,
        container_id: source_container_id,
        end_time_utc: expected_end_time_utc,
    } = source;
    if expires_at_utc.is_some_and(|expires| expires <= Utc::now()) {
        return Ok(None);
    }
    let source_exists: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "AdTeamServices" service
               JOIN "Games" game ON game.id = service.game_id
                WHERE service.id = $1 AND service.container_id = $2
                  AND game.end_time_utc = $3 AND game.end_time_utc <= clock_timestamp()
           )"#,
    )
    .bind(team_service_id)
    .bind(source_container_id)
    .bind(expected_end_time_utc)
    .fetch_one(pool)
    .await
    .map_err(database_error)?;
    if !source_exists {
        return Err(AppError::conflict(
            "A&D service backend or event window changed before snapshot publication",
        ));
    }
    let staged = super::stage_blob(
        pool,
        storage,
        uuid::Uuid::new_v4(),
        &format!("ad-service-snapshot:{team_service_id}"),
        None,
        name,
        bytes,
    )
    .await?;
    let expected_hash = staged.blob.hash.clone();
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let mut replaced_hash = None;
    let operation: AppResult<Option<ServiceSnapshotBlob>> = async {
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            &mut transaction,
            &format!("ad-service-snapshot:{team_service_id}"),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

        // Match the event editor's parent-before-child lock order. Storage I/O
        // has already completed; these locks cover only publication/commit.
        let game_id =
            sqlx::query_scalar::<_, i32>(r#"SELECT game_id FROM "AdTeamServices" WHERE id = $1"#)
                .bind(team_service_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(database_error)?
                .ok_or_else(|| {
                    AppError::conflict("A&D service was removed before snapshot publication")
                })?;
        let event_matches = sqlx::query_scalar::<_, bool>(
            r#"SELECT end_time_utc = $2 AND end_time_utc <= clock_timestamp()
                 FROM "Games" WHERE id = $1 FOR SHARE"#,
        )
        .bind(game_id)
        .bind(expected_end_time_utc)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .unwrap_or(false);
        let source_exists = sqlx::query_scalar::<_, bool>(
            r#"SELECT TRUE FROM "AdTeamServices"
                WHERE id = $1 AND container_id = $2 AND game_id = $3 FOR SHARE"#,
        )
        .bind(team_service_id)
        .bind(source_container_id)
        .bind(game_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .unwrap_or(false);
        if !event_matches || !source_exists {
            return Err(AppError::conflict(
                "A&D service backend or event window changed before snapshot publication",
            ));
        }

        // Retention also locks hash -> snapshot/file. Fence both old and new
        // hashes in canonical order before taking any snapshot/file row lock.
        let old_hash = sqlx::query_scalar::<_, String>(
            r#"SELECT file.hash FROM "AdServiceSnapshots" snapshot
                 JOIN "Files" file ON file.id = snapshot.local_file_id
                WHERE snapshot.team_service_id = $1"#,
        )
        .bind(team_service_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        super::lock_direct_hashes_locked(
            &mut transaction,
            old_hash
                .as_deref()
                .into_iter()
                .chain(std::iter::once(expected_hash.as_str())),
        )
        .await?;
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) = select_snapshot(&mut transaction, team_service_id).await? {
            if existing.source_container_id == source_container_id
                && existing.captured_at_utc >= expected_end_time_utc
                && existing.expires_at_utc.is_none_or(|expires| expires > now)
            {
                return Ok(Some(existing));
            }
            if expires_at_utc.is_some_and(|expires| expires <= now) {
                return Ok(None);
            }
            sqlx::query(r#"DELETE FROM "AdServiceSnapshots" WHERE id = $1"#)
                .bind(existing.id)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
            replaced_hash = Some(existing.hash);
        }
        if expires_at_utc.is_some_and(|expires| expires <= now) {
            return Ok(None);
        }

        let file_id = super::publish_staged_blob(&mut transaction, &staged).await?;
        let id = sqlx::query_scalar::<_, i64>(
            r#"INSERT INTO "AdServiceSnapshots"
                    (team_service_id, local_file_id, source_container_id,
                     captured_at_utc, expires_at_utc)
               VALUES ($1, $2, $3, clock_timestamp(), $4)
               ON CONFLICT (team_service_id) DO NOTHING
            RETURNING id"#,
        )
        .bind(team_service_id)
        .bind(file_id)
        .bind(source_container_id)
        .bind(expires_at_utc)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::conflict("A&D snapshot publication owner changed"))?;
        let snapshot = sqlx::query_as::<_, ServiceSnapshotBlob>(&format!(
            "{SELECT_SNAPSHOT_SQL} AND snapshot.id = $2"
        ))
        .bind(team_service_id)
        .bind(id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        Ok(Some(snapshot))
    }
    .await;

    let snapshot = match operation {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = transaction.rollback().await;
            if let Err(cleanup_error) =
                super::discard_unpublished_stage(pool, storage, &staged).await
            {
                tracing::warn!(
                    %cleanup_error,
                    hash = %expected_hash,
                    "failed A&D snapshot rollback cleanup deferred"
                );
            }
            return Err(error);
        }
    };
    if let Err(error) = transaction.commit().await.map_err(database_error) {
        if let Err(cleanup_error) = super::discard_unpublished_stage(pool, storage, &staged).await {
            tracing::warn!(
                %cleanup_error,
                hash = %expected_hash,
                "uncertain A&D snapshot cleanup deferred"
            );
        }
        return Err(error);
    }
    // A concurrent capture may have won publication. Discard only our unused
    // stage; a committed/uncertain published receipt is never released here.
    if let Err(error) = super::discard_unpublished_stage(pool, storage, &staged).await {
        tracing::warn!(%error, hash = %expected_hash, "unused A&D snapshot stage cleanup deferred");
    }
    if let Some(hash) = replaced_hash.filter(|hash| hash != &expected_hash) {
        if let Err(error) = purge_if_unreferenced(pool, storage, &hash).await {
            tracing::warn!(%error, %hash, "replaced A&D snapshot blob purge deferred");
        }
    }
    Ok(snapshot)
}

pub async fn load_service_snapshot(
    pool: &PgPool,
    team_service_id: i32,
) -> AppResult<Option<ServiceSnapshotBlob>> {
    sqlx::query_as::<_, ServiceSnapshotBlob>(SELECT_AVAILABLE_SNAPSHOT_SQL)
        .bind(team_service_id)
        .fetch_optional(pool)
        .await
        .map_err(database_error)
}

pub async fn available_service_snapshots(
    pool: &PgPool,
    team_service_ids: &[i32],
) -> AppResult<HashSet<i32>> {
    if team_service_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let ids = sqlx::query_scalar::<_, i32>(
        r#"SELECT snapshot.team_service_id
             FROM "AdServiceSnapshots" snapshot
             JOIN "Files" file ON file.id = snapshot.local_file_id
             JOIN "AdTeamServices" service ON service.id = snapshot.team_service_id
             JOIN "Games" game ON game.id = service.game_id
            WHERE snapshot.team_service_id = ANY($1)
              AND snapshot.captured_at_utc >= game.end_time_utc
              AND (service.container_id IS NULL OR service.container_id = snapshot.source_container_id)
              AND (snapshot.expires_at_utc IS NULL
                   OR snapshot.expires_at_utc > clock_timestamp())
              AND file.reference_count > 0"#,
    )
    .bind(team_service_ids)
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    Ok(ids.into_iter().collect())
}

/// Release a bounded batch of expired snapshot owners. The migration's delete
/// trigger decrements the matching `Files` reference for both explicit expiry
/// and any future parent-row cascade; physical storage is purged after commit.
pub async fn purge_expired_service_snapshots(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    limit: i64,
) -> AppResult<u64> {
    let candidates = sqlx::query_as::<_, (i64, String)>(
        r#"SELECT snapshot.id, file.hash
             FROM "AdServiceSnapshots" snapshot
             JOIN "Files" file ON file.id = snapshot.local_file_id
            WHERE snapshot.expires_at_utc <= clock_timestamp()
            ORDER BY snapshot.expires_at_utc, snapshot.id
            LIMIT $1"#,
    )
    .bind(limit.clamp(1, 256))
    .fetch_all(pool)
    .await
    .map_err(database_error)?;

    let mut purged = 0;
    for (snapshot_id, hash) in candidates {
        let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
            .await
            .map_err(database_error)?;
        lock_hash(&mut transaction, &hash)
            .await
            .map_err(database_error)?;
        let deleted = sqlx::query(
            r#"DELETE FROM "AdServiceSnapshots"
                WHERE id = $1 AND expires_at_utc <= clock_timestamp()"#,
        )
        .bind(snapshot_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        transaction.commit().await.map_err(database_error)?;
        if deleted == 0 {
            continue;
        }
        purged += 1;
        if let Err(error) = purge_if_unreferenced(pool, storage, &hash).await {
            tracing::warn!(%error, %hash, "expired A&D snapshot blob purge deferred");
        }
    }
    Ok(purged)
}

#[cfg(test)]
mod tests;
