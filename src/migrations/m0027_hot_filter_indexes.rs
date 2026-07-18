//! Indexes on the per-poll filter columns of `AdTeamServices` and `KothTargets`. Both
//! had only their PK index, so every `WHERE game_id = $1` (run on every Ad/State,
//! Ad/Targets, scoreboard and KotH poll, plus the per-tick checker sweep) is a seq scan.
//! Harmless while a game has a handful of services, but a BYOC-heavy game registers one
//! `AdTeamService` per self-hosted service (hundreds seen under load), at which point the
//! full scan runs on every poll. `register_service` also looks a row up by
//! `(participation_id, challenge_id)` on every tunnel connect. Idempotent.
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE INDEX IF NOT EXISTS ix_adteamservices_game
                  ON "AdTeamServices"(game_id);
                CREATE INDEX IF NOT EXISTS ix_adteamservices_part_challenge
                  ON "AdTeamServices"(participation_id, challenge_id);
                CREATE INDEX IF NOT EXISTS ix_kothtargets_game
                  ON "KothTargets"(game_id);
                CREATE INDEX IF NOT EXISTS ix_kothtargets_game_challenge
                  ON "KothTargets"(game_id, challenge_id);
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
                DROP INDEX IF EXISTS ix_adteamservices_game;
                DROP INDEX IF EXISTS ix_adteamservices_part_challenge;
                DROP INDEX IF EXISTS ix_kothtargets_game;
                DROP INDEX IF EXISTS ix_kothtargets_game_challenge;
                "#,
            )
            .await?;
        Ok(())
    }
}
