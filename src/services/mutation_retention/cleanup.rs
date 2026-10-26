use std::time::{Duration, Instant};

use sqlx::{Postgres, Transaction};

const CLEANUP_BATCH: i64 = 64;
const MAX_PAGES_PER_TABLE: usize = 8;
const WORK_BUDGET: Duration = Duration::from_millis(200);

pub(super) const DELETES: [&str; 2] = [
    r#"DELETE FROM "MutationOperations" WHERE ctid IN (
           SELECT ctid FROM "MutationOperations"
            WHERE expires_at_utc <= CURRENT_TIMESTAMP
            ORDER BY expires_at_utc
            LIMIT $1 FOR UPDATE SKIP LOCKED
       )"#,
    r#"DELETE FROM "AccountLinkAttempts" WHERE token_digest IN (
           SELECT token_digest FROM "AccountLinkAttempts"
            WHERE expires_at_utc <= CURRENT_TIMESTAMP
            ORDER BY expires_at_utc
            LIMIT $1 FOR UPDATE SKIP LOCKED
       )"#,
];

const OLDEST_LAG: [&str; 2] = [
    r#"SELECT GREATEST(0, CEIL(EXTRACT(EPOCH FROM
            (clock_timestamp() - expires_at_utc))))::BIGINT
         FROM "MutationOperations" ORDER BY expires_at_utc LIMIT 1"#,
    r#"SELECT GREATEST(0, CEIL(EXTRACT(EPOCH FROM
            (clock_timestamp() - expires_at_utc))))::BIGINT
         FROM "AccountLinkAttempts" ORDER BY expires_at_utc LIMIT 1"#,
];

#[derive(Debug, Default)]
pub struct RetentionProgress {
    pub removed_mutations: u64,
    pub removed_links: u64,
    pub delete_statements: usize,
    pub oldest_expired_seconds: i64,
    pub elapsed_ms: u128,
}

/// Start at most eight fixed-size pages per table, alternating families so a
/// busy table cannot monopolize a pass. The time budget stops additional pages;
/// individual SQL statements also have server-enforced lock/execution limits.
/// Pool admission and COMMIT are outside this query-scheduling budget.
/// CURRENT_TIMESTAMP fixes the cutoff at transaction start and permits an
/// expiration-index range bound, including a final partial page before live rows.
pub(crate) async fn purge_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<RetentionProgress, sqlx::Error> {
    let started = Instant::now();
    sqlx::query("SET LOCAL lock_timeout = '10ms'")
        .execute(&mut **transaction)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '50ms'")
        .execute(&mut **transaction)
        .await?;
    let mut progress = RetentionProgress::default();
    let mut done = [false; 2];
    for _ in 0..MAX_PAGES_PER_TABLE {
        for family in 0..2 {
            if done[family] || started.elapsed() >= WORK_BUDGET {
                continue;
            }
            let removed = sqlx::query(DELETES[family])
                .bind(CLEANUP_BATCH)
                .execute(&mut **transaction)
                .await?
                .rows_affected();
            progress.delete_statements += 1;
            if family == 0 {
                progress.removed_mutations += removed;
            } else {
                progress.removed_links += removed;
            }
            done[family] = removed < CLEANUP_BATCH as u64;
        }
        if done.iter().all(|done| *done) || started.elapsed() >= WORK_BUDGET {
            break;
        }
    }
    // Read just the first entry in each existing expiration index. Include
    // locked rows in the lag gauge without counting/scanning the whole backlog.
    for query in OLDEST_LAG {
        let lag: Option<i64> = sqlx::query_scalar(query)
            .fetch_optional(&mut **transaction)
            .await?;
        progress.oldest_expired_seconds = progress.oldest_expired_seconds.max(lag.unwrap_or(0));
    }
    progress.elapsed_ms = started.elapsed().as_millis();
    Ok(progress)
}
