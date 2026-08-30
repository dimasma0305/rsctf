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
    request_api_revocation: bool,
) -> AppResult<()> {
    if participation_ids.is_empty() {
        return Ok(());
    }
    // Reporter admission reads target, then capability, then snapshot. Lock
    // targets in that same order before deleting capabilities so a stale API
    // holder cannot create a target↔token deadlock with an in-flight report.
    sqlx::query_scalar::<_, i32>(
        r#"SELECT id FROM "KothTargets"
            WHERE game_id = $1
            ORDER BY id
            FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if request_api_revocation {
        crate::services::ad::koth_api_capability::force_rotate_event_capabilities(
            connection,
            game_id,
            participation_ids,
        )
        .await?;
    } else {
        // Participation review records the pending fence in the same
        // transaction as Accepted -> non-Accepted. Teardown may be retried
        // after the checker already reconciled it, so it must apply only that
        // durable request rather than create another bearer generation.
        crate::services::ad::koth_api_capability::reconcile_pending_event_capabilities(
            connection,
            game_id,
            None,
            Some(participation_ids),
        )
        .await?;
    }
    sqlx::query(
        r#"WITH projection_clearance AS MATERIALIZED (
               SELECT game.end_time_utc > clock_timestamp()
                      OR NOT EXISTS (
                          SELECT 1 FROM "AdRounds" round
                           WHERE round.game_id = game.id
                             AND round.finalized = FALSE
                      ) AS allowed
                 FROM "Games" game
                WHERE game.id = $2
           ), revoked AS (
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
                  AND COALESCE((SELECT allowed FROM projection_clearance), FALSE)
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
                  AND COALESCE((SELECT allowed FROM projection_clearance), FALSE)
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
              AND COALESCE((SELECT allowed FROM projection_clearance), FALSE)
              AND target.holder_participation_id = ANY($1)"#,
    )
    .bind(participation_ids)
    .bind(game_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

#[derive(Debug)]
struct KothGameCacheInvalidation {
    game_id: i32,
}

/// Projection cache keys captured while the caller's game locks are held.
#[derive(Debug, Default)]
pub(crate) struct KothCapabilityCacheInvalidation {
    games: Vec<KothGameCacheInvalidation>,
}

impl KothCapabilityCacheInvalidation {
    pub(crate) async fn apply(self, cache: &dyn crate::services::cache::Cache) {
        for game in self.games {
            invalidate_capability_cache(cache, &game).await;
        }
    }
}

async fn invalidate_capability_cache(
    cache: &dyn crate::services::cache::Cache,
    game: &KothGameCacheInvalidation,
) {
    cache.remove(&format!("latestround:{}", game.game_id)).await;
}

async fn load_games_for_participations(
    db: &DatabaseConnection,
    participation_ids: &[i32],
) -> AppResult<BTreeMap<i32, Vec<i32>>> {
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
    Ok(by_game)
}

/// Revoke KotH state through a transaction that already owns the ordered game
/// locks acquired by the roster-change policy. Reacquiring those locks on a
/// second connection would self-deadlock. Cache eviction is returned to the
/// caller and must run after commit.
pub(crate) async fn revoke_koth_capabilities_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    participation_ids: &[i32],
) -> AppResult<KothCapabilityCacheInvalidation> {
    if participation_ids.is_empty() {
        return Ok(KothCapabilityCacheInvalidation::default());
    }
    let rows = sqlx::query_as::<_, (i32, i32)>(
        r#"SELECT id, game_id
             FROM "Participations"
            WHERE id = ANY($1)
            ORDER BY game_id, id"#,
    )
    .bind(participation_ids)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut by_game = BTreeMap::<i32, Vec<i32>>::new();
    for (participation_id, game_id) in rows {
        by_game.entry(game_id).or_default().push(participation_id);
    }

    let mut invalidation = KothCapabilityCacheInvalidation::default();
    for (game_id, ids) in by_game {
        revoke_game_capabilities(transaction, game_id, &ids, true).await?;
        invalidation
            .games
            .push(KothGameCacheInvalidation { game_id });
    }
    Ok(invalidation)
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
    let by_game = load_games_for_participations(db, participation_ids).await?;
    for (game_id, ids) in by_game {
        let mut lock = acquire_game_lock(db, game_id).await?;
        revoke_game_capabilities(&mut *lock.transaction_mut(), game_id, &ids, true).await?;
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;

        // Token responses contain bearer capabilities, so revocation must evict
        // both response shapes as well as any stale shared round pointer.
        invalidate_capability_cache(cache, &KothGameCacheInvalidation { game_id }).await;
    }
    Ok(())
}

/// Finish an Accepted -> non-Accepted participation review fence without
/// manufacturing a second request when external teardown or its retry runs
/// after checker reconciliation.
pub(crate) async fn reconcile_koth_capability_revocations(
    db: &DatabaseConnection,
    cache: &dyn crate::services::cache::Cache,
    participation_ids: &[i32],
) -> AppResult<()> {
    let by_game = load_games_for_participations(db, participation_ids).await?;
    for (game_id, ids) in by_game {
        let mut lock = acquire_game_lock(db, game_id).await?;
        revoke_game_capabilities(&mut *lock.transaction_mut(), game_id, &ids, false).await?;
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        invalidate_capability_cache(cache, &KothGameCacheInvalidation { game_id }).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::{Connection, PgConnection};

    use super::{game_lock_key, revoke_game_capabilities, revoke_koth_capabilities_locked};

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
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TEMP TABLE "KothTokens" (
              id INTEGER PRIMARY KEY, participation_id INTEGER NOT NULL,
              revoked_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothApiTeamTokens" (
              game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, token TEXT NOT NULL UNIQUE,
              generation INTEGER NOT NULL DEFAULT 1,
              rotated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
              last_used_at TIMESTAMPTZ,
              revocation_pending BOOLEAN NOT NULL DEFAULT FALSE,
              PRIMARY KEY (game_id, challenge_id, participation_id)
            );
            CREATE TEMP TABLE "KothApiSnapshots" (
              target_id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, snapshot_hash BYTEA NOT NULL
            );
            CREATE TEMP TABLE "KothApiSnapshotScores" (
              target_id INTEGER NOT NULL, wave_id TEXT NOT NULL,
              participation_id INTEGER NOT NULL,
              activity_earned BIGINT NOT NULL,
              activity_possible BIGINT NOT NULL,
              objective_earned BIGINT NOT NULL,
              objective_possible BIGINT NOT NULL,
              objective_count SMALLINT NOT NULL,
              is_crown BOOLEAN NOT NULL,
              PRIMARY KEY (target_id, wave_id, participation_id)
            );
            CREATE UNIQUE INDEX uq_test_koth_api_crown
              ON "KothApiSnapshotScores" (target_id, wave_id)
              WHERE is_crown;
            CREATE TEMP TABLE "KothTargets" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              holder_participation_id INTEGER, held_since TIMESTAMPTZ
            );
            CREATE TEMP TABLE "AdRounds" (
              game_id INTEGER NOT NULL, number INTEGER NOT NULL,
              finalized BOOLEAN NOT NULL
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
            INSERT INTO "Games" VALUES (7, clock_timestamp() + interval '1 hour');
            INSERT INTO "Participations" VALUES (11, 7), (12, 7);
            INSERT INTO "KothTokens" VALUES (101, 11, NULL), (102, 12, NULL);
            INSERT INTO "KothApiTeamTokens"
              (game_id, challenge_id, participation_id, token) VALUES
              (7, 70, 11, 'koth_team_11'), (7, 70, 12, 'koth_team_12'),
              (7, 71, 11, 'koth_team_11_no_score');
            INSERT INTO "KothApiSnapshots" VALUES
              (3, 7, 70, decode(repeat('11', 32), 'hex')),
              (4, 7, 71, decode(repeat('22', 32), 'hex'));
            INSERT INTO "KothApiSnapshotScores" VALUES
              (3, 'wave-1', 11, 1, 1, 2, 3, 1, TRUE),
              (3, 'wave-1', 12, 1, 1, 1, 2, 1, FALSE),
              (4, 'wave-1', 12, 1, 1, 1, 1, 1, TRUE);
            INSERT INTO "KothTargets"
              VALUES (3, 7, 70, 11, clock_timestamp()),
                     (4, 7, 71, NULL, NULL);
            INSERT INTO "AdRounds" VALUES (7, 5, FALSE);
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

        let mut revocation = connection.begin().await.unwrap();
        crate::services::ad::koth_api_capability::request_event_capability_revocation(
            &mut revocation,
            7,
            &[11],
        )
        .await
        .unwrap();
        crate::services::ad::koth_api_capability::reconcile_pending_event_capabilities(
            &mut revocation,
            7,
            None,
            Some(&[11]),
        )
        .await
        .unwrap();
        revoke_game_capabilities(&mut revocation, 7, &[11], false)
            .await
            .unwrap();
        revocation.commit().await.unwrap();

        let revoked: Vec<(i32, bool)> =
            sqlx::query_as(r#"SELECT id, revoked_at IS NOT NULL FROM "KothTokens" ORDER BY id"#)
                .fetch_all(&mut connection)
                .await
                .unwrap();
        assert_eq!(revoked, vec![(101, true), (102, false)]);
        let api_state: (i64, i64, bool, bool, bool, bool, bool) = sqlx::query_as(
            r#"SELECT (SELECT COUNT(*) FROM "KothApiTeamTokens"),
                      (SELECT COUNT(*) FROM "KothApiSnapshotScores"),
                      (SELECT is_crown FROM "KothApiSnapshotScores"
                        WHERE target_id = 3 AND participation_id = 12),
                      (SELECT snapshot_hash <> decode(repeat('11', 32), 'hex')
                         FROM "KothApiSnapshots" WHERE target_id = 3),
                      (SELECT snapshot_hash <> decode(repeat('22', 32), 'hex')
                         FROM "KothApiSnapshots" WHERE target_id = 4),
                      (SELECT COUNT(*) = 2 FROM "KothApiTeamTokens"
                        WHERE participation_id = 11 AND generation = 2
                          AND token NOT IN ('koth_team_11', 'koth_team_11_no_score')),
                      NOT EXISTS (SELECT 1 FROM "KothApiTeamTokens"
                        WHERE token IN ('koth_team_11', 'koth_team_11_no_score'))"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(api_state, (3, 2, true, true, true, true, true));
        let before_retry: (Vec<(i32, i32, String)>, Vec<(i32, Vec<u8>)>) = (
            sqlx::query_as(
                r#"SELECT challenge_id, generation, token
                     FROM "KothApiTeamTokens"
                    WHERE participation_id = 11
                    ORDER BY challenge_id"#,
            )
            .fetch_all(&mut connection)
            .await
            .unwrap(),
            sqlx::query_as(
                r#"SELECT target_id, snapshot_hash
                     FROM "KothApiSnapshots" ORDER BY target_id"#,
            )
            .fetch_all(&mut connection)
            .await
            .unwrap(),
        );
        let mut teardown_retry = connection.begin().await.unwrap();
        revoke_game_capabilities(&mut teardown_retry, 7, &[11], false)
            .await
            .unwrap();
        revoke_game_capabilities(&mut teardown_retry, 7, &[11], false)
            .await
            .unwrap();
        teardown_retry.commit().await.unwrap();
        let after_retry: (Vec<(i32, i32, String)>, Vec<(i32, Vec<u8>)>) = (
            sqlx::query_as(
                r#"SELECT challenge_id, generation, token
                     FROM "KothApiTeamTokens"
                    WHERE participation_id = 11
                    ORDER BY challenge_id"#,
            )
            .fetch_all(&mut connection)
            .await
            .unwrap(),
            sqlx::query_as(
                r#"SELECT target_id, snapshot_hash
                     FROM "KothApiSnapshots" ORDER BY target_id"#,
            )
            .fetch_all(&mut connection)
            .await
            .unwrap(),
        );
        assert_eq!(after_retry, before_retry);
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

        let mut transaction = connection.begin().await.unwrap();
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            &mut transaction,
            &game_lock_key(7),
        )
        .await
        .unwrap();
        let invalidation = revoke_koth_capabilities_locked(&mut transaction, &[12])
            .await
            .expect("locked revocation tried to reacquire its game lock");
        let second_revoked: bool =
            sqlx::query_scalar(r#"SELECT revoked_at IS NOT NULL FROM "KothTokens" WHERE id = 102"#)
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
        assert!(second_revoked);
        let second_api_rotation: (i32, bool) = sqlx::query_as(
            r#"SELECT generation, token <> 'koth_team_12'
                 FROM "KothApiTeamTokens"
                WHERE game_id = 7 AND challenge_id = 70
                  AND participation_id = 12"#,
        )
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        assert_eq!(second_api_rotation, (2, true));
        assert_eq!(invalidation.games.len(), 1);
        assert_eq!(invalidation.games[0].game_id, 7);
        transaction.commit().await.unwrap();

        sqlx::raw_sql(
            r#"UPDATE "Games"
                  SET end_time_utc = clock_timestamp() - interval '1 second'
                WHERE id = 7;
               UPDATE "AdRounds" SET finalized = FALSE WHERE game_id = 7;
               UPDATE "KothTokens" SET revoked_at = NULL WHERE id = 101;
               UPDATE "KothTargets"
                  SET holder_participation_id = 11, held_since = clock_timestamp()
                WHERE id = 3;
               UPDATE "KothClaimStates"
                  SET token_id = 101, token_window_round = 5,
                      provisional_participation_id = 11,
                      confirmation_streak = 2,
                      confirmed_participation_id = 11
                WHERE target_id = 3;
               UPDATE "KothCrownCycles"
                  SET phase = 'Active', provisional_participation_id = 11,
                      confirmed_participation_id = 11,
                      confirmation_progress = 2
                WHERE id = 41;"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        revoke_game_capabilities(&mut connection, 7, &[11], true)
            .await
            .unwrap();
        let pending: (bool, Option<i32>, Option<i32>, Option<i32>, i32) = sqlx::query_as(
            r#"SELECT token.revoked_at IS NOT NULL,
                      target.holder_participation_id,
                      claim.confirmed_participation_id,
                      cycle.confirmed_participation_id,
                      cycle.confirmation_progress
                 FROM "KothTokens" token
                 JOIN "KothTargets" target ON target.id = 3
                 JOIN "KothClaimStates" claim ON claim.target_id = target.id
                 JOIN "KothCrownCycles" cycle ON cycle.id = 41
                WHERE token.id = 101"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(pending, (true, Some(11), Some(11), Some(11), 2));

        sqlx::query(r#"UPDATE "AdRounds" SET finalized = TRUE WHERE game_id = 7"#)
            .execute(&mut connection)
            .await
            .unwrap();
        revoke_game_capabilities(&mut connection, 7, &[11], true)
            .await
            .unwrap();
        let settled: (Option<i32>, Option<i32>, Option<i32>, i32) = sqlx::query_as(
            r#"SELECT target.holder_participation_id,
                      claim.confirmed_participation_id,
                      cycle.confirmed_participation_id,
                      cycle.confirmation_progress
                 FROM "KothTargets" target
                 JOIN "KothClaimStates" claim ON claim.target_id = target.id
                 JOIN "KothCrownCycles" cycle ON cycle.id = 41
                WHERE target.id = 3"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(settled, (None, None, None, 0));
    }
}
