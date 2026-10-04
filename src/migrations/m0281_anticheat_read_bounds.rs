//! Indexes for bounded anti-cheat cursor, report, and identity reads.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_cheatinfo_game_id_delta
    ON "CheatInfo" (game_id, id);

CREATE INDEX IF NOT EXISTS ix_suspicionevents_game_created_id
    ON "SuspicionEvents" (game_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS ix_identityobservations_game_recent
    ON "IdentityObservations" (game_id, observed_at_utc DESC, id DESC)
    WHERE game_id IS NOT NULL
      AND team_id IS NOT NULL
      AND participation_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_submissions_anticheat_challenge_status_time
    ON "Submissions"
       (game_id, challenge_id, status, submit_time_utc DESC, id DESC);

CREATE INDEX IF NOT EXISTS ix_submissions_anticheat_participation_status_time
    ON "Submissions"
       (game_id, participation_id, status, submit_time_utc DESC, id DESC);

CREATE INDEX IF NOT EXISTS ix_containeraccess_anticheat_scope_time
    ON "ContainerAccessEvents"
       (game_id, challenge_id, container_owner_participation_id, container_id,
        connected_at_utc DESC, id DESC)
    WHERE is_monitor = FALSE;

CREATE INDEX IF NOT EXISTS ix_identityobservations_game_kind_value_recent
    ON "IdentityObservations"
       (game_id, kind, value_hash, observed_at_utc DESC, id DESC)
    WHERE game_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_identityobservations_game_team_kind_value_recent
    ON "IdentityObservations"
       (game_id, team_id, kind, value_hash, observed_at_utc DESC, id DESC)
    WHERE game_id IS NOT NULL
      AND team_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_identityobservations_game_subnet_recent
    ON "IdentityObservations"
       (game_id, subnet_group_hash, observed_at_utc DESC, id DESC)
    WHERE game_id IS NOT NULL
      AND subnet_group_hash IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_identityobservations_game_user_kind_recent
    ON "IdentityObservations"
       (game_id, user_id, kind, observed_at_utc DESC, id DESC)
    WHERE game_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_identityobservations_game_part_kind_recent
    ON "IdentityObservations"
       (game_id, participation_id, kind, observed_at_utc DESC, id DESC)
    WHERE game_id IS NOT NULL
      AND participation_id IS NOT NULL;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: deployed query plans may rely on these indexes, and a
        // rollback must not reintroduce the unbounded production reads.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_indexes_install_idempotently() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("rsctf_anticheat_bounds_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = crate::migrations::test_pg_connect_options(&database_url)
            .options([("search_path", schema.as_str())]);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "CheatInfo" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
            CREATE TABLE "SuspicionEvents" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                created_at TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "Submissions" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                participation_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL,
                status SMALLINT NOT NULL,
                submit_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "ContainerAccessEvents" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                challenge_id INTEGER NOT NULL,
                container_owner_participation_id INTEGER NOT NULL,
                container_id UUID NOT NULL,
                is_monitor BOOLEAN NULL,
                connected_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "IdentityObservations" (
                id BIGINT PRIMARY KEY,
                user_id UUID NOT NULL,
                game_id INTEGER NULL,
                team_id INTEGER NULL,
                participation_id INTEGER NULL,
                kind TEXT NOT NULL,
                value_hash BYTEA NOT NULL,
                subnet_group_hash BYTEA NULL,
                observed_at_utc TIMESTAMPTZ NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let installed: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::BIGINT
                 FROM pg_indexes
                WHERE schemaname = current_schema()
                  AND indexname IN (
                      'ix_cheatinfo_game_id_delta',
                      'ix_suspicionevents_game_created_id',
                      'ix_identityobservations_game_recent',
                      'ix_submissions_anticheat_challenge_status_time',
                      'ix_submissions_anticheat_participation_status_time',
                      'ix_containeraccess_anticheat_scope_time',
                      'ix_identityobservations_game_kind_value_recent',
                      'ix_identityobservations_game_team_kind_value_recent',
                      'ix_identityobservations_game_subnet_recent',
                      'ix_identityobservations_game_user_kind_recent',
                      'ix_identityobservations_game_part_kind_recent'
                  )"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(installed, 11);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }

    #[test]
    fn indexes_match_every_bounded_feed_order() {
        assert!(UP_SQL.contains("\"CheatInfo\" (game_id, id)"));
        assert!(UP_SQL.contains("game_id, created_at DESC, id DESC"));
        assert!(UP_SQL.contains("game_id, observed_at_utc DESC, id DESC"));
        assert!(UP_SQL.contains("game_id, challenge_id, status, submit_time_utc DESC, id DESC"));
        assert!(UP_SQL.contains("game_id, participation_id, status, submit_time_utc DESC, id DESC"));
        assert!(UP_SQL.contains("container_owner_participation_id, container_id,"));
        assert!(UP_SQL.contains("game_id, kind, value_hash, observed_at_utc DESC, id DESC"));
        assert!(UP_SQL.contains("game_id, subnet_group_hash, observed_at_utc DESC, id DESC"));
        assert!(UP_SQL.contains("game_id, user_id, kind, observed_at_utc DESC, id DESC"));
        assert!(UP_SQL.contains("game_id, participation_id, kind, observed_at_utc DESC, id DESC"));
        assert_eq!(UP_SQL.matches("CREATE INDEX IF NOT EXISTS").count(), 11);
    }
}
