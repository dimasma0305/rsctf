//! Give each suspicion incident a stable, retry-safe identity.
//!
//! The detector write path uses `evidence_key` as part of its `ON CONFLICT`
//! target. Submission-backed evidence can therefore retain one immutable row per
//! submission while aggregate checks remain idempotent under concurrent sweeps.
//! Existing aggregate rows receive the same global/challenge key used by the
//! new writers, so the first post-upgrade sweep does not duplicate them.
//! Repeated submission evidence and any legacy key collision remain preserved
//! under unique `legacy:<id>` keys. Historical rows keep a nullable score delta;
//! no evidence is deleted or assigned a weight it did not persist.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    LOCK TABLE "SuspicionEvents" IN SHARE ROW EXCLUSIVE MODE;

    ALTER TABLE "SuspicionEvents"
      ADD COLUMN IF NOT EXISTS evidence_key TEXT,
      ADD COLUMN IF NOT EXISTS score_delta INTEGER;

    WITH backfill AS (
        SELECT id,
               kind,
               CASE
                 -- Burst (kind 9) is participation-wide. Its row retains the
                 -- triggering challenge for audit context, but the writer uses
                 -- one global evidence key for the whole game.
                 WHEN kind = 9 THEN 'global'
                 WHEN challenge_id IS NULL THEN 'global'
                 ELSE 'challenge:' || challenge_id::text
               END AS canonical_key,
               row_number() OVER (
                   PARTITION BY game_id, participation_id, kind,
                       CASE
                         WHEN kind = 9 THEN 'global'
                         WHEN challenge_id IS NULL THEN 'global'
                         ELSE 'challenge:' || challenge_id::text
                       END
                   ORDER BY created_at, id
               ) AS occurrence_number
          FROM "SuspicionEvents"
         WHERE evidence_key IS NULL OR btrim(evidence_key) = ''
    )
    UPDATE "SuspicionEvents" event
       SET evidence_key = CASE
           WHEN backfill.kind = 0 OR backfill.occurrence_number > 1
             THEN 'legacy:' || event.id::text
           ELSE backfill.canonical_key
       END
      FROM backfill
     WHERE event.id = backfill.id;

    ALTER TABLE "SuspicionEvents"
      ALTER COLUMN evidence_key SET NOT NULL;

    ALTER TABLE "SuspicionEvents"
      DROP CONSTRAINT IF EXISTS ck_suspicionevents_evidence_key;
    ALTER TABLE "SuspicionEvents"
      ADD CONSTRAINT ck_suspicionevents_evidence_key
      CHECK (btrim(evidence_key) <> '' AND char_length(evidence_key) <= 128);

    DROP INDEX IF EXISTS ux_suspicionevents_game_participation_kind;
    DROP INDEX IF EXISTS ux_suspicionevents_incident;

    CREATE UNIQUE INDEX ux_suspicionevents_incident
      ON "SuspicionEvents"(game_id, participation_id, kind, evidence_key);
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP INDEX IF EXISTS ux_suspicionevents_incident;
                ALTER TABLE "SuspicionEvents"
                  DROP COLUMN IF EXISTS score_delta,
                  DROP COLUMN IF EXISTS evidence_key;
                "#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn upgrade_preserves_legacy_rows_and_replaces_the_coarse_index() {
        assert!(!UP_SQL.contains("DELETE FROM"));
        assert!(UP_SQL.contains("WHEN backfill.kind = 0 OR backfill.occurrence_number > 1"));
        assert!(UP_SQL.contains("'legacy:' || event.id::text"));
        assert_eq!(UP_SQL.matches("WHEN kind = 9 THEN 'global'").count(), 2);
        assert!(UP_SQL.contains("ELSE 'challenge:' || challenge_id::text"));
        assert!(UP_SQL.contains("DROP INDEX IF EXISTS ux_suspicionevents_game_participation_kind"));
        assert!(UP_SQL
            .contains("CHECK (btrim(evidence_key) <> '' AND char_length(evidence_key) <= 128)"));
        assert!(UP_SQL
            .contains("ON \"SuspicionEvents\"(game_id, participation_id, kind, evidence_key)"));
    }
}
