use std::collections::BTreeMap;

use sea_orm::DatabaseConnection;

use crate::utils::error::{AppError, AppResult};

fn game_lock_key(game_id: i32) -> String {
    format!("koth-control:{game_id}")
}

/// Serializes KotH capability and holder mutations both within this process and
/// across replicas. Taking the local gate first prevents same-process waiters
/// from each occupying a pooled PostgreSQL connection while the advisory lock is
/// held by the current writer.
pub(crate) struct GameControlLock {
    database: crate::utils::single_flight::PgAdvisoryLock,
    local: crate::utils::single_flight::CoalesceGuard,
}

impl GameControlLock {
    pub(crate) fn transaction_mut(&mut self) -> &mut sqlx::Transaction<'static, sqlx::Postgres> {
        self.database.transaction_mut()
    }

    pub(crate) async fn release(self) -> anyhow::Result<()> {
        let Self { database, local } = self;
        let result = database.release().await;
        drop(local);
        result
    }
}

pub(crate) async fn acquire_game_lock(
    db: &DatabaseConnection,
    game_id: i32,
) -> AppResult<GameControlLock> {
    let key = game_lock_key(game_id);
    let local = crate::utils::single_flight::coalesce(&key).await;
    let database = crate::utils::single_flight::PgAdvisoryLock::acquire(
        db.get_postgres_connection_pool(),
        &key,
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(GameControlLock { database, local })
}

/// Clear the published holder for one hill while its game control lock is held.
pub(crate) async fn clear_challenge_control(
    db: &DatabaseConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "KothTargets"
              SET holder_participation_id = NULL, held_since = NULL
            WHERE game_id = $1 AND challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Revoke live KotH control credentials and unseat their holders. Token rows are
/// retained because issuance is immutable scoring evidence. The checker holds
/// the same per-game lock, so it cannot restore a stale holder after this
/// revocation returns.
pub(crate) async fn revoke_koth_capabilities(
    db: &DatabaseConnection,
    cache: &dyn crate::services::cache::Cache,
    participation_ids: &[i32],
) -> AppResult<()> {
    if participation_ids.is_empty() {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, (i32, i32)>(
        r#"SELECT id, game_id
             FROM "Participations"
            WHERE id = ANY($1)
            ORDER BY game_id, id"#,
    )
    .bind(participation_ids)
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut by_game = BTreeMap::<i32, Vec<i32>>::new();
    for (participation_id, game_id) in rows {
        by_game.entry(game_id).or_default().push(participation_id);
    }

    for (game_id, ids) in by_game {
        let mut lock = acquire_game_lock(db, game_id).await?;
        let latest_round: Option<i32> =
            sqlx::query_scalar(r#"SELECT MAX(number) FROM "AdRounds" WHERE game_id = $1"#)
                .bind(game_id)
                .fetch_one(&mut **lock.transaction_mut())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        let challenge_ids: Vec<i32> =
            sqlx::query_scalar(r#"SELECT challenge_id FROM "KothTargets" WHERE game_id = $1"#)
                .bind(game_id)
                .fetch_all(&mut **lock.transaction_mut())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        sqlx::query(
            r#"WITH revoked AS (
                   UPDATE "KothTokens" token
                      SET revoked_at = COALESCE(token.revoked_at, clock_timestamp())
                     FROM "Participations" participation
                    WHERE participation.id = token.participation_id
                      AND participation.game_id = $2
                      AND token.participation_id = ANY($1)
                   RETURNING token.id
               )
               UPDATE "KothTargets"
                  SET holder_participation_id = NULL, held_since = NULL
                WHERE game_id = $2
                  AND holder_participation_id = ANY($1)"#,
        )
        .bind(&ids)
        .bind(game_id)
        .execute(&mut **lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;

        // Token responses contain bearer capabilities, so revocation must evict
        // both response shapes as well as any stale shared round pointer.
        cache.remove(&format!("latestround:{game_id}")).await;
        if let Some(round) = latest_round {
            for participation_id in ids {
                for challenge_id in &challenge_ids {
                    cache
                        .remove(&format!(
                            "kothtoken:{game_id}:{challenge_id}:{participation_id}:{round}"
                        ))
                        .await;
                }
                cache
                    .remove(&format!(
                        "kothtokensall:{game_id}:{participation_id}:{round}"
                    ))
                    .await;
            }
        }
    }
    Ok(())
}
