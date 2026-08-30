//! Track player-requested Leaderboard capability rotations independently from
//! security reconciliation.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "KothApiTeamTokens"
    ADD COLUMN IF NOT EXISTS last_player_rotated_at TIMESTAMPTZ NULL;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: older binaries ignore this nullable cooldown marker.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn player_rotation_marker_is_nullable_and_idempotent() {
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS last_player_rotated_at"));
        assert!(UP_SQL.contains("TIMESTAMPTZ NULL"));
        assert!(!UP_SQL.contains("UPDATE \"KothApiTeamTokens\""));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_is_replay_safe_and_does_not_infer_player_rotations() {
        use sqlx::{Connection as _, Executor as _};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let mut connection = sqlx::PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"CREATE TEMP TABLE "KothApiTeamTokens" (
                 game_id INTEGER NOT NULL,
                 challenge_id INTEGER NOT NULL,
                 participation_id INTEGER NOT NULL,
                 token TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 rotated_at TIMESTAMPTZ NOT NULL
               );
               INSERT INTO "KothApiTeamTokens" VALUES
                 (7, 9, 11, 'koth_security_rotated', 4, clock_timestamp());"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::raw_sql(UP_SQL)
            .execute(&mut connection)
            .await
            .unwrap();

        let state: (i64, bool) = sqlx::query_as(
            r#"SELECT COUNT(*)::bigint,
                      BOOL_AND(last_player_rotated_at IS NULL)
                 FROM "KothApiTeamTokens""#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(state, (1, true));
    }
}
