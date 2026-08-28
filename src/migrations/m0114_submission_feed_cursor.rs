//! Give committed monitor submissions a reconnect-safe cursor.
//!
//! `Submissions.id` is stable, but sequence allocation can precede commit. A
//! deferred trigger makes a non-blocking cursor-assignment attempt at commit;
//! a bounded application reconciler fills any rows that lost the per-game
//! assignment race so reconnect backfills cannot skip a late commit.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE SEQUENCE IF NOT EXISTS rsctf_submission_feed_cursor_seq AS BIGINT;

ALTER TABLE "Submissions"
    ADD COLUMN IF NOT EXISTS feed_cursor BIGINT;

DO $migration$
DECLARE
    current_max BIGINT;
BEGIN
    SELECT MAX(feed_cursor) INTO current_max FROM "Submissions";
    IF current_max IS NULL THEN
        UPDATE "Submissions"
           SET feed_cursor = id::BIGINT
         WHERE feed_cursor IS NULL;
    ELSE
        PERFORM setval('rsctf_submission_feed_cursor_seq', current_max, TRUE);
        UPDATE "Submissions"
           SET feed_cursor = nextval('rsctf_submission_feed_cursor_seq')
         WHERE feed_cursor IS NULL;
    END IF;

    SELECT MAX(feed_cursor) INTO current_max FROM "Submissions";
    IF current_max IS NULL THEN
        PERFORM setval('rsctf_submission_feed_cursor_seq', 1, FALSE);
    ELSE
        PERFORM setval('rsctf_submission_feed_cursor_seq', current_max, TRUE);
    END IF;
END
$migration$;

CREATE UNIQUE INDEX IF NOT EXISTS ux_submissions_feed_cursor
    ON "Submissions" (feed_cursor)
    WHERE feed_cursor IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_submissions_game_feed_cursor
    ON "Submissions" (game_id, feed_cursor)
    WHERE feed_cursor IS NOT NULL;

-- Remove the superseded transient-row indexes if an unreleased development
-- deployment already applied an earlier form of this migration.
DROP INDEX IF EXISTS ix_submissions_pending_feed_age;
DROP INDEX IF EXISTS ix_submissions_pending_feed_cursor;

CREATE TABLE IF NOT EXISTS "SubmissionFeedPending" (
    submission_id INTEGER PRIMARY KEY
        REFERENCES "Submissions" (id) ON DELETE CASCADE,
    game_id INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS ix_submissionfeedpending_game_submission
    ON "SubmissionFeedPending" (game_id, submission_id);

CREATE OR REPLACE FUNCTION rsctf_assign_submission_feed_cursor()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
    IF NEW.feed_cursor IS NULL THEN
        -- Namespace 1398097485 is reserved for the submission commit fence.
        IF pg_try_advisory_xact_lock(1398097485, NEW.game_id) THEN
            UPDATE "Submissions"
               SET feed_cursor = nextval('rsctf_submission_feed_cursor_seq')
             WHERE id = NEW.id
               AND feed_cursor IS NULL;
        ELSE
            INSERT INTO "SubmissionFeedPending" (submission_id, game_id)
            VALUES (NEW.id, NEW.game_id)
            ON CONFLICT (submission_id) DO NOTHING;
        END IF;
    END IF;
    RETURN NULL;
END
$function$;

DROP TRIGGER IF EXISTS tr_submissions_feed_cursor ON "Submissions";
CREATE CONSTRAINT TRIGGER tr_submissions_feed_cursor
    AFTER INSERT ON "Submissions"
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION rsctf_assign_submission_feed_cursor();
"#;

const DOWN_SQL: &str = r#"
DROP TRIGGER IF EXISTS tr_submissions_feed_cursor ON "Submissions";
DROP FUNCTION IF EXISTS rsctf_assign_submission_feed_cursor();
DROP TABLE IF EXISTS "SubmissionFeedPending";
DROP INDEX IF EXISTS ix_submissions_game_feed_cursor;
DROP INDEX IF EXISTS ux_submissions_feed_cursor;
ALTER TABLE "Submissions" DROP COLUMN IF EXISTS feed_cursor;
DROP SEQUENCE IF EXISTS rsctf_submission_feed_cursor_seq;
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
    fn cursor_assignment_is_deferred_nonblocking_indexed_and_idempotent() {
        assert!(UP_SQL.contains("DEFERRABLE INITIALLY DEFERRED"));
        assert!(UP_SQL.contains("pg_try_advisory_xact_lock(1398097485, NEW.game_id)"));
        assert!(!UP_SQL.contains("PERFORM pg_advisory_xact_lock"));
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS feed_cursor BIGINT"));
        assert!(UP_SQL.contains("ix_submissions_game_feed_cursor"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"SubmissionFeedPending\""));
        assert!(UP_SQL.contains("ix_submissionfeedpending_game_submission"));
        assert!(UP_SQL.contains("ON CONFLICT (submission_id) DO NOTHING"));
        assert!(UP_SQL.contains("ux_submissions_feed_cursor"));
    }
}
