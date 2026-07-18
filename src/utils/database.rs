use sea_orm::{DatabaseConnection, DatabaseTransaction, DbErr, TransactionTrait};
use sqlx::{PgPool, Postgres, Transaction};

const READ_ONLY_REPEATABLE_READ_BEGIN: &str =
    "BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY";

async fn begin_sqlx_transaction_with(
    pool: &PgPool,
    statement: Option<&'static str>,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let pool = pool.clone();
    tokio::spawn(async move {
        match statement {
            Some(statement) => pool.begin_with(statement).await,
            None => pool.begin().await,
        }
    })
    .await
    .map_err(|error| sqlx::Error::Protocol(format!("transaction start task failed: {error}")))?
}

/// Start a regular SQLx transaction in an owned task.
///
/// PostgreSQL can accept `BEGIN` before SQLx records the matching transaction
/// depth. Finishing that narrow handshake outside the request future means an
/// HTTP cancellation cannot return an untracked transaction to the pool.
pub(crate) async fn begin_sqlx_transaction(
    pool: &PgPool,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    begin_sqlx_transaction_with(pool, None).await
}

/// Start a SeaORM transaction with the same cancellation boundary as SQLx.
/// Existing ORM-backed write paths use this while the repository migrates to
/// raw SQLx; new query code should continue to prefer [`begin_sqlx_transaction`].
pub(crate) async fn begin_seaorm_transaction(
    database: &DatabaseConnection,
) -> Result<DatabaseTransaction, DbErr> {
    let database = database.clone();
    tokio::spawn(async move { database.begin().await })
        .await
        .map_err(|error| DbErr::Custom(format!("transaction start task failed: {error}")))?
}

/// Start a consistent read snapshot with its transaction characteristics set
/// atomically by PostgreSQL's `BEGIN` statement.
///
/// Keeping the modes in the startup statement is important on pooled
/// connections: a separate `SET TRANSACTION` can arrive after another command
/// and PostgreSQL will reject it instead of opening the requested snapshot.
pub(crate) async fn begin_read_only_repeatable_read(
    pool: &PgPool,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    // SQLx records PostgreSQL's transaction depth only after the BEGIN reply is
    // fully received. If an HTTP future is cancelled inside that narrow await,
    // the server can have an open transaction while the pooled client still
    // believes its depth is zero. Finish the handshake in an owned task; when
    // the caller disappears, dropping the completed tracked Transaction queues
    // the normal rollback instead of returning poisoned row locks to the pool.
    begin_sqlx_transaction_with(pool, Some(READ_ONLY_REPEATABLE_READ_BEGIN)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires RSCTF_TEST_DATABASE_URL"]
    async fn cancelled_caller_cannot_strand_transaction_or_advisory_lock() {
        const LOCK_KEY: i64 = 912_345_679;
        const DELAYED_BEGIN: &str =
            "BEGIN; SELECT pg_advisory_xact_lock(912345679); SELECT pg_sleep(0.2)";

        let url = std::env::var("RSCTF_TEST_DATABASE_URL").expect("test database URL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect test database");
        let mut observer = pool.acquire().await.expect("hold observer connection");
        let caller = tokio::spawn({
            let pool = pool.clone();
            async move { begin_sqlx_transaction_with(&pool, Some(DELAYED_BEGIN)).await }
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let held: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND objid = $1::oid AND granted)",
                )
                .bind(LOCK_KEY as i32)
                .fetch_one(&mut *observer)
                .await
                .expect("observe transaction advisory lock");
                if held {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("delayed BEGIN reached PostgreSQL");
        caller.abort();
        let _ = caller.await;

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let available: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(LOCK_KEY)
            .fetch_one(&mut *observer)
            .await
            .expect("probe released transaction lock");
        assert!(available);
        let released: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(LOCK_KEY)
            .fetch_one(&mut *observer)
            .await
            .expect("release probe lock");
        assert!(released);

        let mut reusable = pool
            .acquire()
            .await
            .expect("reacquire transaction connection");
        let value: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&mut *reusable)
            .await
            .expect("reuse connection after cancellation");
        assert_eq!(value, 1);
    }

    #[tokio::test]
    #[ignore = "requires RSCTF_TEST_DATABASE_URL"]
    async fn read_snapshot_modes_survive_pool_reuse_and_contention() {
        let url = std::env::var("RSCTF_TEST_DATABASE_URL").expect("test database URL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test database");

        // Return a transaction after it has executed a query. SQLx must finish
        // the queued rollback before the custom BEGIN can reuse the connection.
        let mut dropped = pool.begin().await.expect("begin transaction to drop");
        sqlx::query("SELECT 1")
            .execute(&mut *dropped)
            .await
            .expect("query transaction to drop");
        drop(dropped);

        let tasks = (0..32)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move {
                    let mut transaction = begin_read_only_repeatable_read(&pool)
                        .await
                        .expect("begin read snapshot");
                    let modes = sqlx::query_as::<_, (String, String)>(
                        r#"SELECT current_setting('transaction_isolation'),
                                  current_setting('transaction_read_only')"#,
                    )
                    .fetch_one(&mut *transaction)
                    .await
                    .expect("read transaction modes");
                    assert_eq!(modes, ("repeatable read".to_string(), "on".to_string()));
                    transaction.commit().await.expect("commit read snapshot");
                })
            })
            .collect::<Vec<_>>();

        for task in tasks {
            task.await.expect("read snapshot task");
        }
    }
}
