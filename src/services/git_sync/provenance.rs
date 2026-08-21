//! Atomic event-start fence for repository-owned provenance policy.

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseTransaction, Statement};

use crate::utils::enums::{ChallengeBuildStatus, ChallengeVariantMode, SolveReceiptMode};
use crate::utils::error::{AppError, AppResult};

pub(super) struct ProvenanceIntent {
    pub variant_mode: ChallengeVariantMode,
    pub generator_image: Option<String>,
    pub generator_digest: Option<String>,
    pub generator_build_context_subdir: Option<String>,
    pub generator_build_status: ChallengeBuildStatus,
    pub generator_last_build_log: Option<String>,
    pub solve_receipt_mode: SolveReceiptMode,
    pub receipt_verifier_identity: Option<String>,
}

const PUBLISH_PROVENANCE_POLICY_SQL: &str = r#"
UPDATE "GameChallenges" challenge
   SET variant_mode = $3,
       variant_generator_image = $4,
       variant_generator_digest = $5,
       variant_generator_build_context_subdir = $6,
       variant_generator_build_status = $7,
       variant_generator_last_build_log = $8,
       solve_receipt_mode = $9,
       receipt_verifier_identity = $10
  FROM "Games" game
 WHERE challenge.id = $1
   AND challenge.game_id = $2
   AND game.id = challenge.game_id
   AND clock_timestamp() < game.start_time_utc
"#;

/// Keep the wall-clock predicate in the policy write itself. Prepared
/// artifacts cannot be published if the event starts while a scan is running.
pub(super) async fn publish_provenance_policy_locked(
    transaction: &DatabaseTransaction,
    challenge_id: i32,
    game_id: i32,
    intent: &ProvenanceIntent,
) -> AppResult<bool> {
    let result = transaction
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            PUBLISH_PROVENANCE_POLICY_SQL,
            [
                challenge_id.into(),
                game_id.into(),
                (intent.variant_mode as i16).into(),
                intent.generator_image.clone().into(),
                intent.generator_digest.clone().into(),
                intent.generator_build_context_subdir.clone().into(),
                (intent.generator_build_status as i16).into(),
                intent.generator_last_build_log.clone().into(),
                (intent.solve_receipt_mode as i16).into(),
                intent.receipt_verifier_identity.clone().into(),
            ],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(result.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::time::Duration;

    use sea_orm::{SqlxPostgresConnector, TransactionTrait};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn event_boundary_is_in_the_policy_update_predicate() {
        assert!(PUBLISH_PROVENANCE_POLICY_SQL.starts_with("\nUPDATE \"GameChallenges\""));
        assert!(PUBLISH_PROVENANCE_POLICY_SQL.contains("clock_timestamp() < game.start_time_utc"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn policy_write_fails_if_the_event_starts_before_publication() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("provenance_boundary_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
                id INTEGER PRIMARY KEY,
                start_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                variant_mode SMALLINT NOT NULL DEFAULT 0,
                variant_generator_image TEXT,
                variant_generator_digest TEXT,
                variant_generator_build_context_subdir TEXT,
                variant_generator_build_status SMALLINT NOT NULL DEFAULT 0,
                variant_generator_last_build_log TEXT,
                solve_receipt_mode SMALLINT NOT NULL DEFAULT 0,
                receipt_verifier_identity TEXT
            );
            INSERT INTO "Games" (id, start_time_utc)
            VALUES (1, clock_timestamp() + interval '200 milliseconds');
            INSERT INTO "GameChallenges" (id, game_id) VALUES (10, 1);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
        let transaction = database.begin().await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let published = publish_provenance_policy_locked(
            &transaction,
            10,
            1,
            &ProvenanceIntent {
                variant_mode: ChallengeVariantMode::PerParticipation,
                generator_image: Some("generator:test".to_string()),
                generator_digest: Some("sha256:test".to_string()),
                generator_build_context_subdir: Some("generator".to_string()),
                generator_build_status: ChallengeBuildStatus::Success,
                generator_last_build_log: None,
                solve_receipt_mode: SolveReceiptMode::Required,
                receipt_verifier_identity: Some("verifier:test".to_string()),
            },
        )
        .await
        .unwrap();
        assert!(!published);
        transaction.rollback().await.unwrap();

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
