use super::*;

const JOB_LEASE_SECONDS: i64 = 10 * 60;
const MAX_DUE_JOBS_PER_PASS: i64 = 8;
const MAX_RETAINED_ERROR_CHARS: usize = 2_048;

#[derive(sqlx::FromRow)]
struct ClaimedProvisionJob {
    game_id: i32,
    participation_id: i32,
    attempts: i32,
}

pub(crate) async fn enqueue_accepted_provisioning(
    transaction: &mut sqlx::PgConnection,
    game_id: i32,
    participation_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "ParticipationProvisionJobs"
              (participation_id, game_id, attempts, next_attempt_at,
               lease_owner, lease_until, last_error, updated_at_utc)
           VALUES ($1, $2, 0, clock_timestamp(), NULL, NULL, NULL, clock_timestamp())
           ON CONFLICT (participation_id) DO UPDATE
             SET game_id = EXCLUDED.game_id,
                 next_attempt_at = LEAST(
                     "ParticipationProvisionJobs".next_attempt_at,
                     EXCLUDED.next_attempt_at
                 ),
                 updated_at_utc = clock_timestamp()"#,
    )
    .bind(participation_id)
    .bind(game_id)
    .execute(transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(crate) async fn cancel_accepted_provisioning(
    transaction: &mut sqlx::PgConnection,
    participation_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "ParticipationProvisionJobs"
            WHERE participation_id = $1"#,
    )
    .bind(participation_id)
    .execute(transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

fn retry_delay_seconds(attempts: i32) -> i64 {
    let exponent = u32::try_from(attempts.max(0)).unwrap_or(u32::MAX).min(6);
    (30_i64.saturating_mul(2_i64.saturating_pow(exponent))).min(30 * 60)
}

fn retained_error(error: &AppError) -> String {
    error
        .to_string()
        .chars()
        .take(MAX_RETAINED_ERROR_CHARS)
        .collect()
}

async fn claim_job(
    pool: &sqlx::PgPool,
    participation_id: i32,
    owner: Uuid,
) -> AppResult<Option<ClaimedProvisionJob>> {
    sqlx::query_as::<_, ClaimedProvisionJob>(
        r#"UPDATE "ParticipationProvisionJobs"
              SET lease_owner = $2,
                  lease_until = clock_timestamp() + ($3 * interval '1 second'),
                  updated_at_utc = clock_timestamp()
            WHERE participation_id = $1
              AND next_attempt_at <= clock_timestamp()
              AND (lease_until IS NULL OR lease_until < clock_timestamp())
          RETURNING game_id, participation_id, attempts"#,
    )
    .bind(participation_id)
    .bind(owner)
    .bind(JOB_LEASE_SECONDS)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn finish_job(pool: &sqlx::PgPool, participation_id: i32, owner: Uuid) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "ParticipationProvisionJobs"
            WHERE participation_id = $1 AND lease_owner = $2"#,
    )
    .bind(participation_id)
    .bind(owner)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn reschedule_job(
    pool: &sqlx::PgPool,
    job: &ClaimedProvisionJob,
    owner: Uuid,
    error: &AppError,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "ParticipationProvisionJobs"
              SET attempts = attempts + 1,
                  next_attempt_at = clock_timestamp() + ($3 * interval '1 second'),
                  lease_owner = NULL,
                  lease_until = NULL,
                  last_error = $4,
                  updated_at_utc = clock_timestamp()
            WHERE participation_id = $1 AND lease_owner = $2"#,
    )
    .bind(job.participation_id)
    .bind(owner)
    .bind(retry_delay_seconds(job.attempts))
    .bind(retained_error(error))
    .execute(pool)
    .await
    .map_err(|update_error| AppError::internal(update_error.to_string()))?;
    Ok(())
}

/// Attempt one durable job. An external failure is persisted before it is returned.
pub(crate) async fn run_accepted_provisioning_job(
    st: &SharedState,
    participation_id: i32,
) -> AppResult<bool> {
    let owner = Uuid::new_v4();
    let Some(job) = claim_job(st.pg(), participation_id, owner).await? else {
        return Ok(false);
    };
    match super::provision_accepted_participation(st, job.game_id, job.participation_id).await {
        Ok(()) => {
            finish_job(st.pg(), job.participation_id, owner).await?;
            Ok(true)
        }
        Err(error) => {
            reschedule_job(st.pg(), &job, owner, &error).await?;
            Err(error)
        }
    }
}

/// Bounded maintenance pass. Jobs are leased before any container or VPN await.
pub(crate) async fn recover_accepted_provisioning(st: &SharedState) -> AppResult<u64> {
    let candidates = sqlx::query_scalar::<_, i32>(
        r#"SELECT participation_id
             FROM "ParticipationProvisionJobs"
            WHERE next_attempt_at <= clock_timestamp()
              AND (lease_until IS NULL OR lease_until < clock_timestamp())
            ORDER BY next_attempt_at, participation_id
            LIMIT $1"#,
    )
    .bind(MAX_DUE_JOBS_PER_PASS)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut completed = 0;
    for participation_id in candidates {
        match run_accepted_provisioning_job(st, participation_id).await {
            Ok(true) => completed += 1,
            Ok(false) => {}
            Err(error) => tracing::warn!(
                participation = participation_id,
                %error,
                "accepted-participation provisioning remains queued"
            ),
        }
    }
    Ok(completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn recovery_backoff_and_diagnostics_are_bounded() {
        assert_eq!(retry_delay_seconds(0), 30);
        assert_eq!(retry_delay_seconds(1), 60);
        assert_eq!(retry_delay_seconds(i32::MAX), 1_800);
        let message = retained_error(&AppError::bad_request("界".repeat(3_000)));
        assert_eq!(message.chars().count(), MAX_RETAINED_ERROR_CHARS);
        assert_eq!(MAX_DUE_JOBS_PER_PASS, 8);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn enqueue_claim_and_retry_are_atomic_and_idempotent() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("provision_jobs_{}", Uuid::new_v4().simple());
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
            r#"CREATE TABLE "Games" (
                 id INTEGER PRIMARY KEY,
                 deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
                 practice_mode BOOLEAN NOT NULL DEFAULT FALSE,
                 end_time_utc TIMESTAMPTZ NOT NULL
               );
               CREATE TABLE "Participations" (
                 id INTEGER PRIMARY KEY,
                 game_id INTEGER NOT NULL,
                 status SMALLINT NOT NULL
               );
               CREATE TABLE "GameChallenges" (
                 id INTEGER PRIMARY KEY,
                 game_id INTEGER NOT NULL,
                 is_enabled BOOLEAN NOT NULL,
                 review_status SMALLINT NOT NULL,
                 "Type" SMALLINT NOT NULL,
                 ad_self_hosted BOOLEAN NOT NULL,
                 deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
                 container_image TEXT
               );
               CREATE TABLE "AdTeamServices" (
                 participation_id INTEGER NOT NULL,
                 challenge_id INTEGER NOT NULL,
                 container_id TEXT,
                 host TEXT NOT NULL DEFAULT '',
                 port INTEGER NOT NULL DEFAULT 0
               );
               INSERT INTO "Games" VALUES
                 (7, FALSE, FALSE, clock_timestamp() + interval '1 day'),
                 (8, FALSE, FALSE, clock_timestamp() - interval '1 day');
               INSERT INTO "Participations" VALUES
                 (9, 7, 1),
                 (10, 7, 1),
                 (12, 8, 1);
               INSERT INTO "GameChallenges" VALUES
                 (11, 7, TRUE, 0, 4, FALSE, FALSE,
                  'registry.test/ad@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'),
                 (13, 8, TRUE, 0, 4, FALSE, FALSE,
                  'registry.test/ad@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb');
               INSERT INTO "AdTeamServices" VALUES
                 (10, 11, 'runtime-id', '10.13.0.10', 31337);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(crate::migrations::PARTICIPATION_PROVISION_JOBS_SQL)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(crate::migrations::PARTICIPATION_PROVISION_JOBS_SQL)
            .execute(&pool)
            .await
            .unwrap();
        let backfilled: Vec<(i32, i32)> = sqlx::query_as(
            r#"SELECT participation_id, game_id
                 FROM "ParticipationProvisionJobs"
                ORDER BY participation_id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            backfilled,
            vec![(9, 7)],
            "backfill included an ended game or a team with a live service"
        );

        let mut transaction = pool.begin().await.unwrap();
        enqueue_accepted_provisioning(&mut transaction, 7, 9)
            .await
            .unwrap();
        enqueue_accepted_provisioning(&mut transaction, 7, 9)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        let count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "ParticipationProvisionJobs""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            count, 1,
            "migration backfill and duplicate enqueue diverged"
        );

        let owner = Uuid::new_v4();
        let job = claim_job(&pool, 9, owner).await.unwrap().unwrap();
        assert!(claim_job(&pool, 9, Uuid::new_v4()).await.unwrap().is_none());
        reschedule_job(&pool, &job, owner, &AppError::unavailable("runtime down"))
            .await
            .unwrap();
        let (attempts, leased, retry_is_future): (i32, bool, bool) = sqlx::query_as(
            r#"SELECT attempts, lease_owner IS NOT NULL,
                      next_attempt_at > clock_timestamp()
                 FROM "ParticipationProvisionJobs" WHERE participation_id = 9"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((attempts, leased, retry_is_future), (1, false, true));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
