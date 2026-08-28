//! Give flag-egress inserts and aggregate updates a commit-ordered feed cursor.
//!
//! The stable row id identifies one forensic endpoint, but an upsert keeps that
//! id while changing `hit_count` and `last_seen_utc`. A deferred trigger assigns
//! a fresh cursor at transaction end for both inserts and updates, under a short
//! per-game advisory lock, so reconnecting clients can recover every committed
//! state transition without using timestamps as identities.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE SEQUENCE IF NOT EXISTS rsctf_flag_egress_feed_cursor_seq AS BIGINT;

ALTER TABLE "FlagEgressEvents"
    ADD COLUMN IF NOT EXISTS feed_cursor BIGINT;

DO $migration$
DECLARE
    current_max BIGINT;
BEGIN
    SELECT MAX(feed_cursor) INTO current_max FROM "FlagEgressEvents";
    IF current_max IS NULL THEN
        UPDATE "FlagEgressEvents"
           SET feed_cursor = id::BIGINT
         WHERE feed_cursor IS NULL;
    ELSE
        PERFORM setval('rsctf_flag_egress_feed_cursor_seq', current_max, TRUE);
        UPDATE "FlagEgressEvents"
           SET feed_cursor = nextval('rsctf_flag_egress_feed_cursor_seq')
         WHERE feed_cursor IS NULL;
    END IF;

    SELECT MAX(feed_cursor) INTO current_max FROM "FlagEgressEvents";
    IF current_max IS NULL THEN
        PERFORM setval('rsctf_flag_egress_feed_cursor_seq', 1, FALSE);
    ELSE
        PERFORM setval('rsctf_flag_egress_feed_cursor_seq', current_max, TRUE);
    END IF;
END
$migration$;

CREATE UNIQUE INDEX IF NOT EXISTS ux_flagegress_feed_cursor
    ON "FlagEgressEvents" (feed_cursor)
    WHERE feed_cursor IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_flagegress_game_feed_cursor
    ON "FlagEgressEvents" (game_id, feed_cursor)
    WHERE feed_cursor IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_flagegress_game_last_seen_cursor
    ON "FlagEgressEvents" (game_id, last_seen_utc DESC, feed_cursor DESC)
    WHERE feed_cursor IS NOT NULL;

CREATE OR REPLACE FUNCTION rsctf_assign_flag_egress_feed_cursor()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
    -- The cursor-only UPDATE issued below fires this trigger once more. It must
    -- not allocate another cursor for its own bookkeeping write.
    IF TG_OP = 'UPDATE' AND NEW.feed_cursor IS DISTINCT FROM OLD.feed_cursor THEN
        RETURN NULL;
    END IF;
    IF TG_OP = 'INSERT' AND NEW.feed_cursor IS NOT NULL THEN
        RETURN NULL;
    END IF;

    -- Namespace 1195722073 is reserved for the flag-egress commit fence.
    PERFORM pg_advisory_xact_lock(1195722073, NEW.game_id);
    UPDATE "FlagEgressEvents"
       SET feed_cursor = nextval('rsctf_flag_egress_feed_cursor_seq')
     WHERE id = NEW.id
       AND feed_cursor IS NOT DISTINCT FROM NEW.feed_cursor;
    RETURN NULL;
END
$function$;

DROP TRIGGER IF EXISTS tr_flagegress_feed_cursor ON "FlagEgressEvents";
CREATE CONSTRAINT TRIGGER tr_flagegress_feed_cursor
    AFTER INSERT OR UPDATE ON "FlagEgressEvents"
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION rsctf_assign_flag_egress_feed_cursor();
"#;

const DOWN_SQL: &str = r#"
DROP TRIGGER IF EXISTS tr_flagegress_feed_cursor ON "FlagEgressEvents";
DROP FUNCTION IF EXISTS rsctf_assign_flag_egress_feed_cursor();
DROP INDEX IF EXISTS ix_flagegress_game_last_seen_cursor;
DROP INDEX IF EXISTS ix_flagegress_game_feed_cursor;
DROP INDEX IF EXISTS ux_flagegress_feed_cursor;
ALTER TABLE "FlagEgressEvents" DROP COLUMN IF EXISTS feed_cursor;
DROP SEQUENCE IF EXISTS rsctf_flag_egress_feed_cursor_seq;
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
    fn update_cursor_assignment_is_deferred_indexed_and_idempotent() {
        assert!(UP_SQL.contains("AFTER INSERT OR UPDATE"));
        assert!(UP_SQL.contains("DEFERRABLE INITIALLY DEFERRED"));
        assert!(UP_SQL.contains("pg_advisory_xact_lock(1195722073, NEW.game_id)"));
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS feed_cursor BIGINT"));
        assert!(UP_SQL.contains("ix_flagegress_game_feed_cursor"));
        assert!(UP_SQL.contains("ix_flagegress_game_last_seen_cursor"));
        assert!(UP_SQL.contains("ux_flagegress_feed_cursor"));
        assert!(UP_SQL.contains("NEW.feed_cursor IS DISTINCT FROM OLD.feed_cursor"));
    }
}
