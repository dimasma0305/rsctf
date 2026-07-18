//! Unique constraints that make A&D round advancement + the KotH/A&D result upserts
//! safe under concurrent advances. `advance_round` is *read latest → insert next*,
//! with NO per-game lock; two concurrent advances (a double-clicked "Advance round",
//! two organizers, or the cron auto-advance racing a manual click) both read the same
//! latest round and insert the SAME next number → **duplicate rounds** → duplicate
//! flags / check results / KotH control results → the scoreboard (which sums over all
//! rows) double-counts the score.
//!
//! These unique indexes make the round insert (and each result upsert) the atomic
//! gate: the second racer's insert fails instead of creating a duplicate, and the
//! advance/checker code now treats that conflict as a no-op. De-dups any existing
//! duplicates (keep the lowest id + drop orphaned children) before adding each index,
//! so it is safe on a DB that already raced. Idempotent.
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // 1. AdRounds(game_id, number) — the round-advance serialization gate. De-dup
        //    duplicate rounds (from a prior race) + their orphaned children first.
        db.execute_unprepared(
            r#"
            WITH dup AS (
              SELECT id FROM (
                SELECT id, row_number() OVER (PARTITION BY game_id, number ORDER BY id) rn
                FROM "AdRounds"
              ) t WHERE rn > 1
            )
            DELETE FROM "KothControlResults" WHERE ad_round_id IN (SELECT id FROM dup);
            WITH dup AS (
              SELECT id FROM (
                SELECT id, row_number() OVER (PARTITION BY game_id, number ORDER BY id) rn
                FROM "AdRounds"
              ) t WHERE rn > 1
            )
            DELETE FROM "AdCheckResults" WHERE round_id IN (SELECT id FROM dup);
            WITH dup AS (
              SELECT id FROM (
                SELECT id, row_number() OVER (PARTITION BY game_id, number ORDER BY id) rn
                FROM "AdRounds"
              ) t WHERE rn > 1
            )
            DELETE FROM "AdFlags" WHERE round_id IN (SELECT id FROM dup);
            DELETE FROM "AdRounds" WHERE id IN (
              SELECT id FROM (
                SELECT id, row_number() OVER (PARTITION BY game_id, number ORDER BY id) rn
                FROM "AdRounds"
              ) t WHERE rn > 1
            );
            CREATE UNIQUE INDEX IF NOT EXISTS ux_adrounds_game_number
              ON "AdRounds"(game_id, number);
            "#,
        )
        .await?;

        // 2. KothControlResults(game_id, challenge_id, ad_round_id) — exactly one row
        //    per (hill, round); the checker + manual-advance placeholder both write it.
        db.execute_unprepared(
            r#"
            DELETE FROM "KothControlResults" WHERE id IN (
              SELECT id FROM (
                SELECT id, row_number() OVER
                  (PARTITION BY game_id, challenge_id, ad_round_id ORDER BY id) rn
                FROM "KothControlResults"
              ) t WHERE rn > 1
            );
            CREATE UNIQUE INDEX IF NOT EXISTS ux_kothcontrol_hill_round
              ON "KothControlResults"(game_id, challenge_id, ad_round_id);
            "#,
        )
        .await?;

        // 3. AdCheckResults(round_id, team_service_id) — one verdict per (round, service).
        db.execute_unprepared(
            r#"
            DELETE FROM "AdCheckResults" WHERE id IN (
              SELECT id FROM (
                SELECT id, row_number() OVER
                  (PARTITION BY round_id, team_service_id ORDER BY id) rn
                FROM "AdCheckResults"
              ) t WHERE rn > 1
            );
            CREATE UNIQUE INDEX IF NOT EXISTS ux_adcheck_round_service
              ON "AdCheckResults"(round_id, team_service_id);
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP INDEX IF EXISTS ux_adrounds_game_number;
                DROP INDEX IF EXISTS ux_kothcontrol_hill_round;
                DROP INDEX IF EXISTS ux_adcheck_round_service;
                "#,
            )
            .await?;
        Ok(())
    }
}
