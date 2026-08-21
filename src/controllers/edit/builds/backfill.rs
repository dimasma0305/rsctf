const BACKFILL_TERMINAL_BUILD_RECORDS_SQL: &str = r#"
WITH inserted AS (
    INSERT INTO "BuildRecords" (
        challenge_id, game_id, challenge_title, enqueued_at_utc,
        started_at_utc, finished_at_utc, trigger, kind, attempt,
        status, digest, image_ref, log_tail
    )
    SELECT challenge.id, challenge.game_id, challenge.title,
           clock_timestamp(), clock_timestamp(), clock_timestamp(),
           'Backfill', 'Challenge', 1, challenge.build_status,
           challenge.build_image_digest,
           CASE WHEN challenge.build_status = 1 THEN challenge.container_image END,
           right(challenge.last_build_log, 4096)
      FROM "GameChallenges" challenge
     WHERE challenge.build_status IN (1, 2, 6)
       AND NOT EXISTS (
            SELECT 1 FROM "BuildRecords" record
             WHERE record.challenge_id = challenge.id
       )
    RETURNING 1
)
SELECT COUNT(*)::BIGINT FROM inserted
"#;

/// One-time-per-boot, set-based backfill of terminal builds that have no audit
/// history. Queued/building rows are excluded because no worker owns them.
pub async fn backfill_build_records(db: &sea_orm::DatabaseConnection) -> u64 {
    backfill_terminal_build_records(db.get_postgres_connection_pool()).await
}

pub(super) async fn backfill_terminal_build_records(pool: &sqlx::PgPool) -> u64 {
    let mut transaction = match pool.begin().await {
        Ok(transaction) => transaction,
        Err(_) => return 0,
    };
    if sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('rsctf:build-record-backfill', 0))",
    )
    .execute(&mut *transaction)
    .await
    .is_err()
    {
        return 0;
    }
    let inserted = match sqlx::query_scalar::<_, i64>(BACKFILL_TERMINAL_BUILD_RECORDS_SQL)
        .fetch_one(&mut *transaction)
        .await
    {
        Ok(inserted) => inserted.max(0) as u64,
        Err(_) => return 0,
    };
    if transaction.commit().await.is_err() {
        return 0;
    }
    inserted
}
