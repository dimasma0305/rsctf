use std::collections::BTreeMap;

use sea_orm::DatabaseConnection;

use crate::utils::error::{AppError, AppResult};

pub(crate) fn game_lock_key(game_id: i32) -> String {
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

/// Revoke one game's capabilities and clear every mutable holder projection in
/// the same transaction. Immutable tokens, acquisitions, and control results
/// remain as audit/scoring evidence.
pub(crate) async fn revoke_game_capabilities(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    participation_ids: &[i32],
) -> AppResult<()> {
    if participation_ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"WITH revoked AS (
               UPDATE "KothTokens" token
                  SET revoked_at = COALESCE(token.revoked_at, clock_timestamp())
                 FROM "Participations" participation
                WHERE participation.id = token.participation_id
                  AND participation.game_id = $2
                  AND token.participation_id = ANY($1)
               RETURNING token.id
           ), cleared_claims AS (
               UPDATE "KothClaimStates" claim
                  SET token_id = CASE
                        WHEN claim.token_id IN (SELECT id FROM revoked)
                          OR claim.provisional_participation_id = ANY($1)
                        THEN NULL ELSE claim.token_id END,
                      token_window_round = CASE
                        WHEN claim.token_id IN (SELECT id FROM revoked)
                          OR claim.provisional_participation_id = ANY($1)
                        THEN NULL ELSE claim.token_window_round END,
                      provisional_participation_id = CASE
                        WHEN claim.provisional_participation_id = ANY($1)
                        THEN NULL ELSE claim.provisional_participation_id END,
                      confirmation_streak = CASE
                        WHEN claim.token_id IN (SELECT id FROM revoked)
                          OR claim.provisional_participation_id = ANY($1)
                        THEN 0 ELSE claim.confirmation_streak END,
                      confirmed_participation_id = CASE
                        WHEN claim.confirmed_participation_id = ANY($1)
                        THEN NULL ELSE claim.confirmed_participation_id END,
                      updated_at = clock_timestamp()
                 FROM "KothTargets" target
                WHERE target.id = claim.target_id AND target.game_id = $2
                  AND (
                      claim.token_id IN (SELECT id FROM revoked)
                      OR claim.provisional_participation_id = ANY($1)
                      OR claim.confirmed_participation_id = ANY($1)
                  )
               RETURNING claim.target_id
           ), cleared_cycles AS (
               UPDATE "KothCrownCycles" cycle
                  SET provisional_participation_id = CASE
                        WHEN cycle.provisional_participation_id = ANY($1)
                        THEN NULL ELSE cycle.provisional_participation_id END,
                      confirmed_participation_id = CASE
                        WHEN cycle.confirmed_participation_id = ANY($1)
                        THEN NULL ELSE cycle.confirmed_participation_id END,
                      confirmation_progress = CASE
                        WHEN cycle.provisional_participation_id = ANY($1)
                          OR cycle.confirmed_participation_id = ANY($1)
                        THEN 0 ELSE cycle.confirmation_progress END,
                      updated_at = clock_timestamp()
                WHERE cycle.game_id = $2
                  AND cycle.phase NOT IN ('Completed', 'Ended')
                  AND (
                      cycle.provisional_participation_id = ANY($1)
                      OR cycle.confirmed_participation_id = ANY($1)
                  )
               RETURNING cycle.id
           )
           UPDATE "KothTargets" target
              SET holder_participation_id = NULL, held_since = NULL
            WHERE target.game_id = $2
              AND target.holder_participation_id = ANY($1)"#,
    )
    .bind(participation_ids)
    .bind(game_id)
    .execute(&mut *connection)
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
        revoke_game_capabilities(&mut *lock.transaction_mut(), game_id, &ids).await?;
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

#[cfg(test)]
mod tests {
    use sqlx::{Connection, PgConnection};

    use super::revoke_game_capabilities;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn revocation_clears_live_projection_without_rewriting_history() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL
            );
            CREATE TEMP TABLE "KothTokens" (
              id INTEGER PRIMARY KEY, participation_id INTEGER NOT NULL,
              revoked_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothTargets" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              holder_participation_id INTEGER, held_since TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothClaimStates" (
              target_id INTEGER PRIMARY KEY, token_id INTEGER,
              token_window_round INTEGER,
              provisional_participation_id INTEGER,
              confirmation_streak INTEGER NOT NULL,
              confirmed_participation_id INTEGER,
              updated_at TIMESTAMPTZ NOT NULL
            );
            CREATE TEMP TABLE "KothCrownCycles" (
              id BIGINT PRIMARY KEY, game_id INTEGER NOT NULL, phase TEXT NOT NULL,
              provisional_participation_id INTEGER,
              confirmed_participation_id INTEGER,
              confirmation_progress INTEGER NOT NULL,
              updated_at TIMESTAMPTZ NOT NULL
            );
            CREATE TEMP TABLE "KothControlResults" (
              id INTEGER PRIMARY KEY, confirmed_participation_id INTEGER
            );
            CREATE TEMP TABLE "KothAcquisitions" (
              id INTEGER PRIMARY KEY, participation_id INTEGER NOT NULL
            );
            INSERT INTO "Participations" VALUES (11, 7), (12, 7);
            INSERT INTO "KothTokens" VALUES (101, 11, NULL), (102, 12, NULL);
            INSERT INTO "KothTargets"
              VALUES (3, 7, 11, clock_timestamp());
            INSERT INTO "KothClaimStates"
              VALUES (3, 102, 5, 12, 2, 11, clock_timestamp());
            INSERT INTO "KothCrownCycles"
              VALUES (41, 7, 'Active', 12, 11, 2, clock_timestamp()),
                     (40, 7, 'Completed', NULL, 11, 0, clock_timestamp());
            INSERT INTO "KothControlResults" VALUES (1, 11);
            INSERT INTO "KothAcquisitions" VALUES (1, 11);
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        revoke_game_capabilities(&mut connection, 7, &[11])
            .await
            .unwrap();

        let revoked: Vec<(i32, bool)> =
            sqlx::query_as(r#"SELECT id, revoked_at IS NOT NULL FROM "KothTokens" ORDER BY id"#)
                .fetch_all(&mut connection)
                .await
                .unwrap();
        assert_eq!(revoked, vec![(101, true), (102, false)]);
        let claim: (Option<i32>, Option<i32>, i32, Option<i32>) = sqlx::query_as(
            r#"SELECT token_id, provisional_participation_id,
                      confirmation_streak, confirmed_participation_id
                 FROM "KothClaimStates" WHERE target_id = 3"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(claim, (Some(102), Some(12), 2, None));
        let active: (Option<i32>, Option<i32>, i32) = sqlx::query_as(
            r#"SELECT provisional_participation_id,
                      confirmed_participation_id, confirmation_progress
                 FROM "KothCrownCycles" WHERE id = 41"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(active, (Some(12), None, 0));
        let historical: Option<i32> = sqlx::query_scalar(
            r#"SELECT confirmed_participation_id
                 FROM "KothCrownCycles" WHERE id = 40"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(historical, Some(11));
        let holder: Option<i32> =
            sqlx::query_scalar(r#"SELECT holder_participation_id FROM "KothTargets" WHERE id = 3"#)
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(holder, None);
        let immutable_evidence: (i64, i64) = sqlx::query_as(
            r#"SELECT (SELECT COUNT(*) FROM "KothControlResults"),
                      (SELECT COUNT(*) FROM "KothAcquisitions")"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(immutable_evidence, (1, 1));
    }
}
