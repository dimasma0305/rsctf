//! Let an authorized event-history purge delete suspicion evaluation jobs.
//!
//! m0089 made `SuspicionEvaluationOutbox` rows undeletable so a failed detector
//! attempt can never be erased silently. m0343 later introduced the
//! transaction-scoped purge authorization and taught the other immutable
//! ledgers to honor it, but the outbox guard was left unconditional, so every
//! purge of an event that ever recorded a submission failed at
//! `DELETE FROM "SuspicionEvaluationOutbox"`. This mirrors the m0343 contract:
//! only a pending purge operation named in the current transaction may delete
//! the event's jobs; identity fields stay immutable for everyone.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE OR REPLACE FUNCTION rsctf_guard_outbox_operational_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    IF OLD.game_id IS NOT NULL
       AND rsctf_event_history_purge_authorized(OLD.game_id) THEN
      RETURN OLD;
    END IF;
    RAISE EXCEPTION 'suspicion evaluation jobs cannot be deleted';
  END IF;
  IF NEW.id IS DISTINCT FROM OLD.id
     OR NEW.job_kind IS DISTINCT FROM OLD.job_kind
     OR NEW.source_kind IS DISTINCT FROM OLD.source_kind
     OR NEW.source_id IS DISTINCT FROM OLD.source_id
     OR NEW.game_id IS DISTINCT FROM OLD.game_id
     OR NEW.participation_id IS DISTINCT FROM OLD.participation_id
     OR NEW.challenge_id IS DISTINCT FROM OLD.challenge_id
     OR NEW.rule_kind IS DISTINCT FROM OLD.rule_kind
     OR NEW.evidence_key IS DISTINCT FROM OLD.evidence_key
     OR NEW.observed_at_utc IS DISTINCT FROM OLD.observed_at_utc
     OR NEW.evidence_payload IS DISTINCT FROM OLD.evidence_payload
     OR NEW.evidence_version IS DISTINCT FROM OLD.evidence_version THEN
    RAISE EXCEPTION 'suspicion evaluation identity is immutable';
  END IF;
  RETURN NEW;
END
$$;
"#;

/// Restores the unconditional m0089 guard.
const DOWN_SQL: &str = r#"
CREATE OR REPLACE FUNCTION rsctf_guard_outbox_operational_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'suspicion evaluation jobs cannot be deleted';
  END IF;
  IF NEW.id IS DISTINCT FROM OLD.id
     OR NEW.job_kind IS DISTINCT FROM OLD.job_kind
     OR NEW.source_kind IS DISTINCT FROM OLD.source_kind
     OR NEW.source_id IS DISTINCT FROM OLD.source_id
     OR NEW.game_id IS DISTINCT FROM OLD.game_id
     OR NEW.participation_id IS DISTINCT FROM OLD.participation_id
     OR NEW.challenge_id IS DISTINCT FROM OLD.challenge_id
     OR NEW.rule_kind IS DISTINCT FROM OLD.rule_kind
     OR NEW.evidence_key IS DISTINCT FROM OLD.evidence_key
     OR NEW.observed_at_utc IS DISTINCT FROM OLD.observed_at_utc
     OR NEW.evidence_payload IS DISTINCT FROM OLD.evidence_payload
     OR NEW.evidence_version IS DISTINCT FROM OLD.evidence_version THEN
    RAISE EXCEPTION 'suspicion evaluation identity is immutable';
  END IF;
  RETURN NEW;
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
    fn outbox_delete_requires_the_transaction_scoped_purge_authorization() {
        assert!(UP_SQL.contains("rsctf_event_history_purge_authorized(OLD.game_id)"));
        assert!(UP_SQL.contains("OLD.game_id IS NOT NULL"));
        assert!(UP_SQL.contains("suspicion evaluation jobs cannot be deleted"));
        assert!(UP_SQL.contains("suspicion evaluation identity is immutable"));
        assert!(!DOWN_SQL.contains("rsctf_event_history_purge_authorized"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn only_an_authorized_purge_can_delete_evaluation_jobs() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "GamePurgeOperations" (
                operation_id UUID PRIMARY KEY,
                game_id INTEGER NOT NULL,
                actor_user_id UUID NOT NULL,
                request_digest TEXT NOT NULL,
                expected_configuration_revision BIGINT NOT NULL,
                confirmation_title TEXT NOT NULL,
                status SMALLINT NOT NULL DEFAULT 0,
                result JSONB,
                completed_at_utc TIMESTAMPTZ
            );
            "#,
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::raw_sql(crate::migrations::m0343_event_history_purge_trigger_authorization::UP_SQL)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&mut *tx).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE purge_outbox_fixture (
                id BIGINT PRIMARY KEY, job_kind SMALLINT, source_kind SMALLINT,
                source_id INTEGER, game_id INTEGER, participation_id INTEGER,
                challenge_id INTEGER, rule_kind SMALLINT, evidence_key TEXT,
                observed_at_utc TIMESTAMPTZ, evidence_payload JSONB,
                evidence_version SMALLINT, attempts INTEGER NOT NULL DEFAULT 0
            );
            CREATE TRIGGER purge_outbox_fixture_guard
            BEFORE UPDATE OR DELETE ON purge_outbox_fixture
            FOR EACH ROW EXECUTE FUNCTION rsctf_guard_outbox_operational_update();
            INSERT INTO purge_outbox_fixture (id, game_id, evidence_key)
            VALUES (1, 2000000000, 'a'), (2, 1999999999, 'b');
            "#,
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        let game_id = 2_000_000_000;
        sqlx::query("SAVEPOINT unauthorized")
            .execute(&mut *tx)
            .await
            .unwrap();
        let error = sqlx::query("DELETE FROM purge_outbox_fixture WHERE game_id = $1")
            .bind(game_id)
            .execute(&mut *tx)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("suspicion evaluation jobs cannot be deleted"));
        sqlx::query("ROLLBACK TO SAVEPOINT unauthorized")
            .execute(&mut *tx)
            .await
            .unwrap();

        let operation_id = uuid::Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "GamePurgeOperations"
                 (operation_id, game_id, actor_user_id, request_digest,
                  expected_configuration_revision, confirmation_title)
               VALUES ($1, $2, $3, $4, 0, 'migration-test')"#,
        )
        .bind(operation_id)
        .bind(game_id)
        .bind(uuid::Uuid::new_v4())
        .bind("0".repeat(64))
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query_scalar::<_, String>(
            "SELECT set_config('rsctf.event_history_purge_operation', $1::text, TRUE)",
        )
        .bind(operation_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        // Operational bookkeeping stays writable; identity stays immutable.
        sqlx::query("UPDATE purge_outbox_fixture SET attempts = attempts + 1 WHERE id = 1")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("SAVEPOINT immutable_identity")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(
            sqlx::query("UPDATE purge_outbox_fixture SET evidence_key = 'z' WHERE id = 1")
                .execute(&mut *tx)
                .await
                .unwrap_err()
                .to_string()
                .contains("identity is immutable")
        );
        sqlx::query("ROLLBACK TO SAVEPOINT immutable_identity")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query("DELETE FROM purge_outbox_fixture WHERE game_id = $1")
                .bind(game_id)
                .execute(&mut *tx)
                .await
                .unwrap()
                .rows_affected(),
            1,
            "the authorized game's jobs are deleted"
        );
        sqlx::query("SAVEPOINT other_game")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(
            sqlx::query("DELETE FROM purge_outbox_fixture WHERE game_id = 1999999999")
                .execute(&mut *tx)
                .await
                .unwrap_err()
                .to_string()
                .contains("cannot be deleted"),
            "another game's jobs stay protected"
        );
        sqlx::query("ROLLBACK TO SAVEPOINT other_game")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
    }
}
