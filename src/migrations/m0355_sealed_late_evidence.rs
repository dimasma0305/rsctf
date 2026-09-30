//! Settle late evidence on sealed games.
//!
//! Nothing reconciles a game after its final pass seals it, yet historical
//! writers keep appending evidence afterwards (agent-artifact scans of solvers
//! and writeups that arrive after the end, for example). m0284 acknowledged
//! only container-access and roster writes on a terminal game, so any other
//! late write left a source dirty and the game's generation unsettled forever.
//! Every source is now acknowledged in the writing transaction once the game
//! is terminal, and games already stranded that way are settled.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE OR REPLACE FUNCTION rsctf_next_anticheat_reconciliation_version(
    dirty_game_id INTEGER,
    dirty_source_kind SMALLINT
) RETURNS BIGINT LANGUAGE plpgsql AS $$
DECLARE
    stamped_version BIGINT;
    became_dirty BOOLEAN;
    terminal_auto_ack BOOLEAN;
BEGIN
    IF dirty_game_id IS NULL OR dirty_source_kind NOT BETWEEN 0 AND 9 THEN
        RAISE EXCEPTION 'invalid anti-cheat reconciliation source';
    END IF;
    INSERT INTO "AntiCheatReconciliationQueue" (game_id)
    VALUES (dirty_game_id) ON CONFLICT (game_id) DO NOTHING;
    -- Every runtime path takes the shared game row before a source row. This
    -- avoids cross-source inversions when one transaction emits two families.
    SELECT (
               queue.final_applied_at_utc IS NOT NULL
               OR EXISTS (
                   SELECT 1 FROM "SuspicionReconciliationState" reconciliation
                    WHERE reconciliation.game_id = dirty_game_id
                      AND reconciliation.sealed_at_utc IS NOT NULL
               )
           )
      INTO terminal_auto_ack
      FROM "AntiCheatReconciliationQueue" queue
     WHERE queue.game_id = dirty_game_id
     FOR UPDATE;
    INSERT INTO "AntiCheatReconciliationSources"
        (game_id, source_kind, dirty_version, applied_version,
         dirtied_at_utc, applied_at_utc)
    VALUES (
        dirty_game_id, dirty_source_kind, 1,
        CASE WHEN terminal_auto_ack THEN 1 ELSE 0 END,
        clock_timestamp(),
        CASE WHEN terminal_auto_ack THEN clock_timestamp() ELSE NULL END
    )
    ON CONFLICT (game_id, source_kind) DO UPDATE
      SET dirty_version = "AntiCheatReconciliationSources".dirty_version + 1,
          applied_version = CASE WHEN terminal_auto_ack
              THEN "AntiCheatReconciliationSources".dirty_version + 1
              ELSE "AntiCheatReconciliationSources".applied_version END,
          dirtied_at_utc = clock_timestamp(),
          applied_at_utc = CASE WHEN terminal_auto_ack
              THEN clock_timestamp()
              ELSE "AntiCheatReconciliationSources".applied_at_utc END
    RETURNING dirty_version,
              NOT terminal_auto_ack
              AND dirty_version = applied_version + 1
         INTO stamped_version, became_dirty;
    IF became_dirty THEN
        UPDATE "AntiCheatReconciliationQueue"
           SET desired_generation = desired_generation + 1,
               available_at_utc = LEAST(available_at_utc, clock_timestamp()),
               updated_at_utc = clock_timestamp()
         WHERE game_id = dirty_game_id;
    END IF;
    RETURN stamped_version;
END
$$;

UPDATE "AntiCheatReconciliationSources" source
   SET applied_version = source.dirty_version,
       applied_at_utc = clock_timestamp()
  FROM "AntiCheatReconciliationQueue" queue
  LEFT JOIN "SuspicionReconciliationState" reconciliation
    ON reconciliation.game_id = queue.game_id
 WHERE queue.game_id = source.game_id
   AND (queue.final_applied_at_utc IS NOT NULL
        OR reconciliation.sealed_at_utc IS NOT NULL)
   AND source.dirty_version > source.applied_version;

UPDATE "AntiCheatReconciliationQueue" queue
   SET applied_generation = queue.desired_generation,
       updated_at_utc = clock_timestamp()
  FROM "SuspicionReconciliationState" reconciliation
 WHERE reconciliation.game_id = queue.game_id
   AND (queue.final_applied_at_utc IS NOT NULL
        OR reconciliation.sealed_at_utc IS NOT NULL)
   AND queue.applied_generation < queue.desired_generation;
"#;

const DOWN_SQL: &str = r#"
CREATE OR REPLACE FUNCTION rsctf_next_anticheat_reconciliation_version(
    dirty_game_id INTEGER,
    dirty_source_kind SMALLINT
) RETURNS BIGINT LANGUAGE plpgsql AS $$
DECLARE
    stamped_version BIGINT;
    became_dirty BOOLEAN;
    terminal_auto_ack BOOLEAN;
BEGIN
    IF dirty_game_id IS NULL OR dirty_source_kind NOT BETWEEN 0 AND 9 THEN
        RAISE EXCEPTION 'invalid anti-cheat reconciliation source';
    END IF;
    INSERT INTO "AntiCheatReconciliationQueue" (game_id)
    VALUES (dirty_game_id) ON CONFLICT (game_id) DO NOTHING;
    -- Every runtime path takes the shared game row before a source row. This
    -- avoids cross-source inversions when one transaction emits two families.
    SELECT dirty_source_kind IN (6, 9)
           AND (
               queue.final_applied_at_utc IS NOT NULL
               OR EXISTS (
                   SELECT 1 FROM "SuspicionReconciliationState" reconciliation
                    WHERE reconciliation.game_id = dirty_game_id
                      AND reconciliation.sealed_at_utc IS NOT NULL
               )
           )
      INTO terminal_auto_ack
      FROM "AntiCheatReconciliationQueue" queue
     WHERE queue.game_id = dirty_game_id
     FOR UPDATE;
    INSERT INTO "AntiCheatReconciliationSources"
        (game_id, source_kind, dirty_version, applied_version,
         dirtied_at_utc, applied_at_utc)
    VALUES (
        dirty_game_id, dirty_source_kind, 1,
        CASE WHEN terminal_auto_ack THEN 1 ELSE 0 END,
        clock_timestamp(),
        CASE WHEN terminal_auto_ack THEN clock_timestamp() ELSE NULL END
    )
    ON CONFLICT (game_id, source_kind) DO UPDATE
      SET dirty_version = "AntiCheatReconciliationSources".dirty_version + 1,
          applied_version = CASE WHEN terminal_auto_ack
              THEN "AntiCheatReconciliationSources".dirty_version + 1
              ELSE "AntiCheatReconciliationSources".applied_version END,
          dirtied_at_utc = clock_timestamp(),
          applied_at_utc = CASE WHEN terminal_auto_ack
              THEN clock_timestamp()
              ELSE "AntiCheatReconciliationSources".applied_at_utc END
    RETURNING dirty_version,
              NOT terminal_auto_ack
              AND dirty_version = applied_version + 1
         INTO stamped_version, became_dirty;
    IF became_dirty THEN
        UPDATE "AntiCheatReconciliationQueue"
           SET desired_generation = desired_generation + 1,
               available_at_utc = LEAST(available_at_utc, clock_timestamp()),
               updated_at_utc = clock_timestamp()
         WHERE game_id = dirty_game_id;
    END IF;
    RETURN stamped_version;
END
$$;
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
    fn every_source_is_acknowledged_on_a_terminal_game() {
        assert!(UP_SQL
            .contains("CREATE OR REPLACE FUNCTION rsctf_next_anticheat_reconciliation_version"));
        assert!(!UP_SQL.contains("IN (6, 9)"));
        assert!(UP_SQL.contains("queue.final_applied_at_utc IS NOT NULL"));
        assert!(UP_SQL.contains("NOT BETWEEN 0 AND 9"));
        assert!(UP_SQL.contains("applied_generation = queue.desired_generation"));
        assert!(DOWN_SQL.contains("dirty_source_kind IN (6, 9)"));
    }

    #[tokio::test]
    #[ignore = "requires migrated disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_stranded_sealed_games_settle_and_late_writes_stay_settled() {
        use sqlx::postgres::PgPoolOptions;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let game_id: i32 = sqlx::query_scalar(
            r#"SELECT game_id FROM "AntiCheatReconciliationQueue" ORDER BY game_id LIMIT 1"#,
        )
        .fetch_one(&pool)
        .await
        .expect("the disposable database needs one game");
        let mut transaction = pool.begin().await.unwrap();
        // A game sealed before this migration, stranded by a late event.
        for sql in [
            r#"UPDATE "SuspicionReconciliationState"
                  SET evidence_closed_at_utc = COALESCE(evidence_closed_at_utc, clock_timestamp()),
                      sealed_at_utc = COALESCE(sealed_at_utc, clock_timestamp())
                WHERE game_id = $1"#,
            r#"UPDATE "AntiCheatReconciliationQueue"
                  SET desired_generation = applied_generation + 1,
                      final_requested_at_utc = COALESCE(final_requested_at_utc, clock_timestamp()),
                      final_applied_at_utc = COALESCE(final_applied_at_utc, clock_timestamp())
                WHERE game_id = $1"#,
            r#"INSERT INTO "AntiCheatReconciliationSources"
                   (game_id, source_kind, dirty_version, applied_version, dirtied_at_utc)
               VALUES ($1, 7, 3, 1, clock_timestamp())
               ON CONFLICT (game_id, source_kind) DO UPDATE
                 SET dirty_version = "AntiCheatReconciliationSources".applied_version + 2"#,
        ] {
            sqlx::query(sql)
                .bind(game_id)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        sqlx::raw_sql(UP_SQL)
            .execute(&mut *transaction)
            .await
            .unwrap();
        async fn settled(connection: &mut sqlx::PgConnection, game_id: i32) -> (i64, bool) {
            sqlx::query_as(
                r#"SELECT (SELECT COUNT(*) FROM "AntiCheatReconciliationSources"
                            WHERE game_id = $1 AND dirty_version > applied_version),
                          (SELECT desired_generation = applied_generation
                             FROM "AntiCheatReconciliationQueue" WHERE game_id = $1)"#,
            )
            .bind(game_id)
            .fetch_one(connection)
            .await
            .unwrap()
        }
        assert_eq!(settled(&mut transaction, game_id).await, (0, true));

        // A later SuspicionEvents-style write is acknowledged in place.
        sqlx::raw_sql(
            r#"CREATE TEMP TABLE sealed_source7_probe (
                   game_id INTEGER NOT NULL,
                   reconciliation_version BIGINT NULL
               ) ON COMMIT DROP;
               CREATE TRIGGER sealed_source7_stamp
               BEFORE INSERT ON sealed_source7_probe
               FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_insert('7');"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sealed_source7_probe (game_id) VALUES ($1)")
            .bind(game_id)
            .execute(&mut *transaction)
            .await
            .unwrap();
        assert_eq!(settled(&mut transaction, game_id).await, (0, true));
        transaction.rollback().await.unwrap();
        pool.close().await;
    }
}
