//! Replica-safe writeup reference swaps and game-scoped cleanup.

use super::*;
use crate::utils::enums::{ParticipationStatus, Role};

/// Clear and release every writeup for one game exactly once, even when two
/// admin replicas issue the cleanup concurrently.
pub async fn clear_game_writeups(pool: &PgPool, game_id: i32) -> AppResult<Vec<String>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let file_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT writeup_id
             FROM "Participations"
            WHERE game_id = $1 AND writeup_id IS NOT NULL
            ORDER BY id
            FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(database_error)?;
    if file_ids.is_empty() {
        transaction.commit().await.map_err(database_error)?;
        return Ok(Vec::new());
    }

    let mut files =
        sqlx::query_as::<_, (i32, String)>(r#"SELECT id, hash FROM "Files" WHERE id = ANY($1)"#)
            .bind(&file_ids)
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
    files.sort_unstable_by(|left, right| left.1.cmp(&right.1));
    files.dedup_by_key(|file| file.0);
    for (_, hash) in &files {
        lock_hash(&mut transaction, hash)
            .await
            .map_err(database_error)?;
    }
    sqlx::query(
        r#"UPDATE "Participations"
              SET writeup_id = NULL
            WHERE game_id = $1 AND writeup_id IS NOT NULL"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;

    let mut releases = std::collections::BTreeMap::<i32, usize>::new();
    for file_id in file_ids {
        *releases.entry(file_id).or_default() += 1;
    }
    let mut deleted_hashes = Vec::new();
    for (file_id, count) in releases {
        for _ in 0..count {
            if let Some(hash) = release_locked(&mut transaction, file_id)
                .await
                .map_err(database_error)?
                .deleted_hash
            {
                deleted_hashes.push(hash);
            }
        }
    }
    transaction.commit().await.map_err(database_error)?;
    Ok(deleted_hashes)
}

#[cfg(test)]
async fn lock_writeup_hashes(
    transaction: &mut Transaction<'_, Postgres>,
    participation_id: i32,
    new_hash: &str,
) -> AppResult<Option<(i32, String)>> {
    let current = sqlx::query_as::<_, (Option<i32>,)>(
        r#"SELECT writeup_id
             FROM "Participations"
            WHERE id = $1
            FOR UPDATE"#,
    )
    .bind(participation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::not_found("Participation not found"))?;

    lock_writeup_hashes_from_current(transaction, current.0, new_hash).await
}

async fn lock_writeup_hashes_from_current(
    transaction: &mut Transaction<'_, Postgres>,
    current_file_id: Option<i32>,
    new_hash: &str,
) -> AppResult<Option<(i32, String)>> {
    let old = match current_file_id {
        Some(id) => sqlx::query_scalar::<_, String>(r#"SELECT hash FROM "Files" WHERE id = $1"#)
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(database_error)?
            .map(|old_hash| (id, old_hash)),
        None => None,
    };

    // Every multi-hash operation locks in lexical order, preventing two
    // replacements that swap hashes from deadlocking.
    let mut hashes = vec![new_hash];
    if let Some((_, old_hash)) = &old {
        hashes.push(old_hash);
    }
    hashes.sort_unstable();
    hashes.dedup();
    for hash in hashes {
        lock_hash(transaction, hash).await.map_err(database_error)?;
    }
    Ok(old)
}

async fn lock_eligible_writeup_hashes(
    transaction: &mut Transaction<'_, Postgres>,
    game_id: i32,
    participation_id: i32,
    user_id: uuid::Uuid,
    new_hash: &str,
) -> AppResult<Option<(i32, String)>> {
    // Hard game deletion locks the game before its participations. Match that
    // order so a writer either commits a durable writeup first or observes the
    // deletion marker before any bytes or metadata are created.
    let game_eligible = sqlx::query_scalar::<_, i32>(
        r#"SELECT id
             FROM "Games"
            WHERE id = $1
              AND deletion_pending = FALSE
              AND start_time_utc <= clock_timestamp()
              AND writeup_required = TRUE
              AND clock_timestamp() <= writeup_deadline
            FOR SHARE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .is_some();
    if !game_eligible {
        return Err(AppError::conflict(
            "Writeup submission is no longer eligible",
        ));
    }

    let current = sqlx::query_as::<_, (Option<i32>,)>(
        r#"SELECT participation.writeup_id
             FROM "Participations" participation
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "UserParticipations" membership
               ON membership.game_id = participation.game_id
              AND membership.user_id = $3
              AND membership.participation_id = participation.id
             JOIN "AspNetUsers" account ON account.id = membership.user_id
            WHERE participation.id = $2
              AND participation.game_id = $1
              AND participation.status = $4
              AND team.deletion_pending = FALSE
              AND account.role <> $5
            FOR UPDATE OF participation"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(user_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::conflict("Writeup participation is no longer eligible"))?;

    lock_writeup_hashes_from_current(transaction, current.0, new_hash).await
}

async fn replace_writeup_locked(
    transaction: &mut Transaction<'_, Postgres>,
    participation_id: i32,
    old: Option<(i32, String)>,
    hash: &str,
    name: &str,
    size: i64,
) -> Result<Option<String>, sqlx::Error> {
    let new_id = acquire_locked(transaction, hash, name, size).await?;
    sqlx::query(
        r#"UPDATE "Participations"
              SET writeup_id = $2
            WHERE id = $1"#,
    )
    .bind(participation_id)
    .bind(new_id)
    .execute(&mut **transaction)
    .await?;

    match old {
        Some((old_id, _)) => Ok(release_locked(transaction, old_id).await?.deleted_hash),
        None => Ok(None),
    }
}

/// Atomically replace a participation's writeup reference and return the old
/// hash only when its final metadata row was removed.
#[cfg(test)]
pub(super) async fn replace_writeup(
    pool: &PgPool,
    participation_id: i32,
    hash: &str,
    name: &str,
    size: i64,
) -> AppResult<Option<String>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let old = lock_writeup_hashes(&mut transaction, participation_id, hash).await?;
    let deleted_hash =
        replace_writeup_locked(&mut transaction, participation_id, old, hash, name, size)
            .await
            .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(deleted_hash)
}

/// Store and atomically replace a participation writeup under the distributed
/// content-hash lock. The final eligibility snapshot and reference swap share
/// the game-to-participation lock order used by hard deletion.
pub async fn store_and_replace_writeup(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    game_id: i32,
    participation_id: i32,
    user_id: uuid::Uuid,
    name: &str,
    bytes: &[u8],
) -> AppResult<(StoredBlob, Option<String>)> {
    let expected_hash = sha256_hex(bytes);
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let old = lock_eligible_writeup_hashes(
        &mut transaction,
        game_id,
        participation_id,
        user_id,
        &expected_hash,
    )
    .await?;
    let blob = storage.store(name, bytes).await?;
    if blob.hash != expected_hash {
        return Err(AppError::internal(
            "blob storage returned a hash that does not match its content",
        ));
    }
    let deleted_hash = replace_writeup_locked(
        &mut transaction,
        participation_id,
        old,
        &blob.hash,
        name,
        blob.size,
    )
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok((blob, deleted_hash))
}
