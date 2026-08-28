//! Make deferred game-event cursor assignment non-blocking.
//!
//! The shipped game-event cursor trigger takes a per-game advisory lock at
//! transaction end. Replace that behavior in a forward migration so a writer
//! that loses the lock race records bounded reconciliation work instead of
//! delaying its commit.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
-- Remove indexes created by an unreleased transient-row queue implementation.
DROP INDEX IF EXISTS ix_gameevents_pending_feed_age;
DROP INDEX IF EXISTS ix_gameevents_pending_feed_cursor;

CREATE TABLE IF NOT EXISTS "GameEventFeedPending" (
    event_id INTEGER PRIMARY KEY
        REFERENCES "GameEvents" (id) ON DELETE CASCADE,
    game_id INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS ix_gameeventfeedpending_game_event
    ON "GameEventFeedPending" (game_id, event_id);

CREATE OR REPLACE FUNCTION rsctf_assign_game_event_feed_cursor()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
    IF NEW.feed_cursor IS NULL THEN
        -- Namespace 1195722068 is reserved for the game-event commit fence.
        IF pg_try_advisory_xact_lock(1195722068, NEW.game_id) THEN
            UPDATE "GameEvents"
               SET feed_cursor = nextval('rsctf_game_event_feed_cursor_seq')
             WHERE id = NEW.id
               AND feed_cursor IS NULL;
        ELSE
            INSERT INTO "GameEventFeedPending" (event_id, game_id)
            VALUES (NEW.id, NEW.game_id)
            ON CONFLICT (event_id) DO NOTHING;
        END IF;
    END IF;
    RETURN NULL;
END
$function$;

DROP TRIGGER IF EXISTS tr_gameevents_feed_cursor ON "GameEvents";
CREATE CONSTRAINT TRIGGER tr_gameevents_feed_cursor
    AFTER INSERT ON "GameEvents"
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION rsctf_assign_game_event_feed_cursor();
"#;

const DOWN_SQL: &str = r#"
DROP TRIGGER IF EXISTS tr_gameevents_feed_cursor ON "GameEvents";

CREATE OR REPLACE FUNCTION rsctf_assign_game_event_feed_cursor()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
    IF NEW.feed_cursor IS NULL THEN
        -- Namespace 1195722068 is reserved for the game-event commit fence.
        PERFORM pg_advisory_xact_lock(1195722068, NEW.game_id);
        UPDATE "GameEvents"
           SET feed_cursor = nextval('rsctf_game_event_feed_cursor_seq')
         WHERE id = NEW.id
           AND feed_cursor IS NULL;
    END IF;
    RETURN NULL;
END
$function$;

CREATE CONSTRAINT TRIGGER tr_gameevents_feed_cursor
    AFTER INSERT ON "GameEvents"
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION rsctf_assign_game_event_feed_cursor();

DROP TABLE IF EXISTS "GameEventFeedPending";
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm_migration::sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::PgPoolOptions;

    use super::UP_SQL;
    use crate::migrations::{Migrator, MigratorTrait, GAME_EVENT_FEED_CURSOR_SQL};

    #[test]
    fn pending_cursor_assignment_is_deferred_nonblocking_and_idempotent() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"GameEventFeedPending\""));
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS ix_gameeventfeedpending_game_event"));
        assert!(UP_SQL.contains("pg_try_advisory_xact_lock(1195722068, NEW.game_id)"));
        assert!(!UP_SQL.contains("PERFORM pg_advisory_xact_lock"));
        assert!(UP_SQL.contains("ON CONFLICT (event_id) DO NOTHING"));
        assert!(UP_SQL.contains("DEFERRABLE INITIALLY DEFERRED"));

        assert!(GAME_EVENT_FEED_CURSOR_SQL
            .contains("PERFORM pg_advisory_xact_lock(1195722068, NEW.game_id)"));
        assert!(!GAME_EVENT_FEED_CURSOR_SQL.contains("pg_try_advisory_xact_lock"));
        assert!(!GAME_EVENT_FEED_CURSOR_SQL.contains("GameEventFeedPending"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn upgrades_database_that_recorded_m0111_to_nonblocking_pending_trigger() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin_options = crate::migrations::test_pg_connect_options(&database_url);
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(admin_options)
            .await
            .unwrap();
        let schema = format!("rsctf_m0116_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = crate::migrations::test_pg_connect_options(&database_url)
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());

        Migrator::up(&db, Some(111)).await.unwrap();
        let m0111_recorded: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM seaql_migrations WHERE version = 'm0111_game_event_feed_cursor')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(m0111_recorded);

        let pending_before: bool =
            sqlx::query_scalar(r#"SELECT to_regclass('"GameEventFeedPending"') IS NOT NULL"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(!pending_before);
        let function_before: String = sqlx::query_scalar(
            "SELECT pg_get_functiondef(to_regprocedure('rsctf_assign_game_event_feed_cursor()'))",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(function_before.contains("pg_advisory_xact_lock"));
        assert!(!function_before.contains("pg_try_advisory_xact_lock"));

        Migrator::up(&db, Some(5)).await.unwrap();
        assert_forward_state(&pool).await;

        let m0116_recorded: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM seaql_migrations WHERE version = 'm0116_game_event_feed_pending')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(m0116_recorded);

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        assert_forward_state(&pool).await;

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }

    async fn assert_forward_state(pool: &sqlx::PgPool) {
        let relations = sqlx::query_as::<_, (bool, bool)>(
            r#"SELECT
                 to_regclass('"GameEventFeedPending"') IS NOT NULL,
                 to_regclass('ix_gameeventfeedpending_game_event') IS NOT NULL"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(relations, (true, true));

        let function: String = sqlx::query_scalar(
            "SELECT pg_get_functiondef(to_regprocedure('rsctf_assign_game_event_feed_cursor()'))",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(function.contains("pg_try_advisory_xact_lock"));
        assert!(function.contains("GameEventFeedPending"));

        let trigger = sqlx::query_as::<_, (i64, bool, bool)>(
            r#"SELECT COUNT(*)::BIGINT,
                      BOOL_AND(trigger.tgdeferrable),
                      BOOL_AND(trigger.tginitdeferred)
                 FROM pg_trigger trigger
                 JOIN pg_class relation ON relation.oid = trigger.tgrelid
                WHERE relation.oid = '"GameEvents"'::regclass
                  AND trigger.tgname = 'tr_gameevents_feed_cursor'
                  AND NOT trigger.tgisinternal"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(trigger, (1, true, true));
    }
}
