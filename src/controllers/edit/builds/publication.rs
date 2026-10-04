use super::*;

pub(super) const BUILD_FINGERPRINT_SQL: &str = r#"SELECT challenge.container_image,
              challenge.original_archive_blob_path,
              challenge.build_context_subdir
         FROM "GameChallenges" challenge
         JOIN "Games" game ON game.id = challenge.game_id
        WHERE challenge.id = $1
          AND challenge.deletion_pending = FALSE
          AND game.deletion_pending = FALSE"#;

pub(super) const PUBLISH_BUILD_OUTCOME_SQL: &str = r#"UPDATE "GameChallenges" challenge
      SET build_status = $2,
          last_build_log = $3,
          build_image_digest = $4
    WHERE challenge.id = $1
      AND challenge.deletion_pending = FALSE
      AND EXISTS (
            SELECT 1 FROM "Games" game
             WHERE game.id = challenge.game_id
               AND game.deletion_pending = FALSE
      )
      AND challenge.container_image IS NOT DISTINCT FROM $5
      AND challenge.original_archive_blob_path IS NOT DISTINCT FROM $6
      AND challenge.build_context_subdir IS NOT DISTINCT FROM $7"#;

pub(super) const UPSERT_IMAGE_OWNERSHIP_SQL: &str = r#"INSERT INTO "BuildImageOwnerships"
 (installation_scope, canonical_ref, image_id, updated_at_utc, last_used_at_utc)
 VALUES ($1, $2, $3, clock_timestamp(), NULL)
 ON CONFLICT (installation_scope, canonical_ref) DO UPDATE
 SET image_id=EXCLUDED.image_id,
     updated_at_utc=clock_timestamp(),
     cleanup_claim_token=NULL,
     cleanup_claim_until=NULL,
     cleanup_removal_started=FALSE,
     last_used_at_utc=CASE
       WHEN "BuildImageOwnerships".image_id=EXCLUDED.image_id
       THEN "BuildImageOwnerships".last_used_at_utc
       ELSE NULL
     END
 WHERE "BuildImageOwnerships".cleanup_removal_started = FALSE
    OR "BuildImageOwnerships".cleanup_claim_until <= clock_timestamp()"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BuildFingerprint {
    pub(super) container_image: Option<String>,
    pub(super) original_archive_blob_path: Option<String>,
    pub(super) build_context_subdir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BuildImageOwnership {
    pub(super) installation_scope: String,
    pub(super) canonical_ref: String,
    pub(super) image_id: String,
}

impl BuildFingerprint {
    pub(super) fn from_challenge(challenge: &game_challenge::Model) -> Self {
        Self {
            container_image: challenge.container_image.clone(),
            original_archive_blob_path: challenge.original_archive_blob_path.clone(),
            build_context_subdir: challenge.build_context_subdir.clone(),
        }
    }

    pub(super) fn identity(&self) -> String {
        crate::utils::codec::sha256_str(
            &serde_json::json!({
                "containerImage": self.container_image.as_deref(),
                "archive": self.original_archive_blob_path.as_deref(),
                "context": self.build_context_subdir.as_deref(),
            })
            .to_string(),
        )
    }
}

pub(super) fn superseded_build_outcome(message: &str) -> BuildOutcome {
    BuildOutcome {
        status: ChallengeBuildStatus::Failed,
        log: Some(message.to_string()),
        image_digest: None,
    }
}

/// Publish the result only while ordered against every runtime-definition
/// writer. The slow Docker/blob work has already completed before this lock is
/// acquired, so the advisory transaction remains short.
pub(super) async fn publish_build_outcome(
    st: &SharedState,
    challenge: &game_challenge::Model,
    requested: &BuildFingerprint,
    outcome: &BuildOutcome,
    ownership: Option<&BuildImageOwnership>,
) -> AppResult<u64> {
    let mut definition_lock = crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        challenge.game_id,
        challenge.id,
    )
    .await?;
    super::super::challenges::reject_pending_mutation(
        &mut **definition_lock.transaction_mut(),
        challenge.game_id,
        challenge.id,
    )
    .await?;
    let result = sqlx::query(PUBLISH_BUILD_OUTCOME_SQL)
        .bind(challenge.id)
        .bind(outcome.status as i16)
        .bind(outcome.log.clone())
        .bind(outcome.image_digest.clone())
        .bind(&requested.container_image)
        .bind(&requested.original_archive_blob_path)
        .bind(&requested.build_context_subdir)
        .execute(&mut **definition_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let rows_affected = result.rows_affected();
    if rows_affected == 1 {
        if let Some(ownership) = ownership {
            let upserted = sqlx::query(UPSERT_IMAGE_OWNERSHIP_SQL)
                .bind(&ownership.installation_scope)
                .bind(&ownership.canonical_ref)
                .bind(&ownership.image_id)
                .execute(&mut **definition_lock.transaction_mut())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            if upserted.rows_affected() != 1 {
                definition_lock.rollback().await?;
                return Err(AppError::overloaded(
                    "Image cleanup is finalizing this build image; retry publication shortly",
                    1,
                ));
            }
        }
    }
    definition_lock.release().await?;
    Ok(rows_affected)
}

#[cfg(test)]
mod cleanup_claim_tests {
    use std::time::Duration;

    use super::UPSERT_IMAGE_OWNERSHIP_SQL;

    #[test]
    fn publication_supersedes_only_a_preclaim_or_expired_fence() {
        assert!(UPSERT_IMAGE_OWNERSHIP_SQL.contains("cleanup_claim_token=NULL"));
        assert!(UPSERT_IMAGE_OWNERSHIP_SQL.contains("cleanup_claim_until=NULL"));
        assert!(UPSERT_IMAGE_OWNERSHIP_SQL.contains("cleanup_removal_started=FALSE"));
        assert!(UPSERT_IMAGE_OWNERSHIP_SQL.contains("cleanup_claim_until <= clock_timestamp()"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn publication_supersedes_a_concurrent_preclaim_but_refuses_finalizing() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("image_publish_claim_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(3)
            .connect_with(
                crate::migrations::test_pg_connect_options(&database_url)
                    .options([("search_path", schema.as_str())]),
            )
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "BuildImageOwnerships" (
                 installation_scope TEXT NOT NULL,
                 canonical_ref TEXT NOT NULL,
                 image_id TEXT NOT NULL,
                 updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 last_used_at_utc TIMESTAMPTZ NULL,
                 cleanup_claim_token UUID NULL,
                 cleanup_claim_until TIMESTAMPTZ NULL,
                 cleanup_removal_started BOOLEAN NOT NULL DEFAULT FALSE,
                 PRIMARY KEY (installation_scope, canonical_ref)
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let scope = "0123456789abcdef0123456789abcdef";
        let canonical = "docker.io/rsctf/game/app:latest";
        let old_id = format!("sha256:{}", "a".repeat(64));
        let new_id = format!("sha256:{}", "b".repeat(64));
        sqlx::query(
            r#"INSERT INTO "BuildImageOwnerships"
                 (installation_scope, canonical_ref, image_id)
               VALUES ($1, $2, $3)"#,
        )
        .bind(scope)
        .bind(canonical)
        .bind(&old_id)
        .execute(&pool)
        .await
        .unwrap();

        let token = uuid::Uuid::new_v4();
        let mut claimant = pool.begin().await.unwrap();
        sqlx::query(
            r#"UPDATE "BuildImageOwnerships"
                  SET cleanup_claim_token = $3,
                      cleanup_claim_until = clock_timestamp() + interval '2 minutes',
                      cleanup_removal_started = FALSE
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .bind(token)
        .execute(&mut *claimant)
        .await
        .unwrap();

        let mut publisher = tokio::spawn({
            let pool = pool.clone();
            let new_id = new_id.clone();
            async move {
                sqlx::query(UPSERT_IMAGE_OWNERSHIP_SQL)
                    .bind(scope)
                    .bind(canonical)
                    .bind(new_id)
                    .execute(&pool)
                    .await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut publisher)
                .await
                .is_err()
        );
        claimant.commit().await.unwrap();
        let preempted = tokio::time::timeout(Duration::from_secs(2), &mut publisher)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(preempted.rows_affected(), 1);

        let preempted_row = sqlx::query_as::<
            _,
            (
                String,
                Option<uuid::Uuid>,
                Option<chrono::DateTime<chrono::Utc>>,
                bool,
            ),
        >(
            r#"SELECT image_id, cleanup_claim_token, cleanup_claim_until,
                      cleanup_removal_started
                 FROM "BuildImageOwnerships"
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(preempted_row, (new_id.clone(), None, None, false));

        let finalizing_token = uuid::Uuid::new_v4();
        sqlx::query(
            r#"UPDATE "BuildImageOwnerships"
                  SET cleanup_claim_token = $3,
                      cleanup_claim_until = clock_timestamp() + interval '2 minutes',
                      cleanup_removal_started = TRUE
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .bind(finalizing_token)
        .execute(&pool)
        .await
        .unwrap();
        let blocked = sqlx::query(UPSERT_IMAGE_OWNERSHIP_SQL)
            .bind(scope)
            .bind(canonical)
            .bind(&old_id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(blocked.rows_affected(), 0);
        let finalizing: (String, Option<uuid::Uuid>, bool) = sqlx::query_as(
            r#"SELECT image_id, cleanup_claim_token, cleanup_removal_started
                 FROM "BuildImageOwnerships"
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(finalizing, (new_id, Some(finalizing_token), true));

        sqlx::query(
            r#"UPDATE "BuildImageOwnerships"
                  SET cleanup_claim_until = clock_timestamp() - interval '1 second'
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .execute(&pool)
        .await
        .unwrap();
        let retried = sqlx::query(UPSERT_IMAGE_OWNERSHIP_SQL)
            .bind(scope)
            .bind(canonical)
            .bind(&old_id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(retried.rows_affected(), 1);
        let row: (
            String,
            Option<uuid::Uuid>,
            Option<chrono::DateTime<chrono::Utc>>,
            bool,
        ) = sqlx::query_as(
            r#"SELECT image_id, cleanup_claim_token, cleanup_claim_until,
                      cleanup_removal_started
                     FROM "BuildImageOwnerships"
                    WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, (old_id, None, None, false));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
