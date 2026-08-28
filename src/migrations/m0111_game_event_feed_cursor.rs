//! Give committed game events a reconnect-safe cursor.
//!
//! `GameEvents.id` is a stable row identity, but PostgreSQL sequences allocate
//! values before commit. Two concurrent writers can therefore commit ids in the
//! opposite order and make a plain `id > cursor` backfill skip the late commit.
//! A deferred constraint trigger assigns `feed_cursor` at transaction end while
//! holding a very short per-game advisory lock. For one game, cursor order now
//! matches commit order without serializing the longer submission transaction.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE SEQUENCE IF NOT EXISTS rsctf_game_event_feed_cursor_seq AS BIGINT;

ALTER TABLE "GameEvents"
    ADD COLUMN IF NOT EXISTS feed_cursor BIGINT;

DO $migration$
DECLARE
    current_max BIGINT;
BEGIN
    SELECT MAX(feed_cursor) INTO current_max FROM "GameEvents";
    IF current_max IS NULL THEN
        UPDATE "GameEvents"
           SET feed_cursor = id::BIGINT
         WHERE feed_cursor IS NULL;
    ELSE
        PERFORM setval('rsctf_game_event_feed_cursor_seq', current_max, TRUE);
        UPDATE "GameEvents"
           SET feed_cursor = nextval('rsctf_game_event_feed_cursor_seq')
         WHERE feed_cursor IS NULL;
    END IF;

    SELECT MAX(feed_cursor) INTO current_max FROM "GameEvents";
    IF current_max IS NULL THEN
        PERFORM setval('rsctf_game_event_feed_cursor_seq', 1, FALSE);
    ELSE
        PERFORM setval('rsctf_game_event_feed_cursor_seq', current_max, TRUE);
    END IF;
END
$migration$;

CREATE UNIQUE INDEX IF NOT EXISTS ux_gameevents_feed_cursor
    ON "GameEvents" (feed_cursor)
    WHERE feed_cursor IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_gameevents_game_feed_cursor
    ON "GameEvents" (game_id, feed_cursor)
    WHERE feed_cursor IS NOT NULL;

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

DROP TRIGGER IF EXISTS tr_gameevents_feed_cursor ON "GameEvents";
CREATE CONSTRAINT TRIGGER tr_gameevents_feed_cursor
    AFTER INSERT ON "GameEvents"
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION rsctf_assign_game_event_feed_cursor();
"#;

const DOWN_SQL: &str = r#"
DROP TRIGGER IF EXISTS tr_gameevents_feed_cursor ON "GameEvents";
DROP FUNCTION IF EXISTS rsctf_assign_game_event_feed_cursor();
DROP INDEX IF EXISTS ix_gameevents_game_feed_cursor;
DROP INDEX IF EXISTS ux_gameevents_feed_cursor;
ALTER TABLE "GameEvents" DROP COLUMN IF EXISTS feed_cursor;
DROP SEQUENCE IF EXISTS rsctf_game_event_feed_cursor_seq;
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
    use super::*;

    #[test]
    fn cursor_assignment_is_deferred_indexed_and_idempotent() {
        assert!(UP_SQL.contains("DEFERRABLE INITIALLY DEFERRED"));
        assert!(UP_SQL.contains("pg_advisory_xact_lock(1195722068, NEW.game_id)"));
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS feed_cursor BIGINT"));
        assert!(UP_SQL.contains("ix_gameevents_game_feed_cursor"));
        assert!(UP_SQL.contains("ux_gameevents_feed_cursor"));
    }
}
