//! Shared final lock hierarchy for irreversible game/challenge deletion.

use crate::utils::error::{AppError, AppResult};

/// A hard delete keeps one game-control connection while it performs nested
/// definition and runtime-owner cleanup. Admit before checking out that outer
/// connection so unrelated deletes cannot consume every pool slot while all
/// wait for one more connection to make progress.
const HARD_DELETION_CONCURRENCY: usize = 1;
static HARD_DELETION_GATE: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(HARD_DELETION_CONCURRENCY))
    });

/// Per-replica admission for the complete irreversible-delete lifecycle. The
/// permit is intentionally acquired before the first game transaction and is
/// moved into the final lock guard after slow external teardown.
pub(super) struct HardDeletionAdmission {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

pub(super) async fn acquire_hard_deletion_admission() -> AppResult<HardDeletionAdmission> {
    let permit = HARD_DELETION_GATE
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(HardDeletionAdmission { _permit: permit })
}

/// Canonical order: game control -> game test lifecycle. Challenge deletion
/// adds its definition lock only after this guard is acquired.
pub(super) struct GameTestDeletionLocks {
    game: crate::services::ad_engine::GameControlLock,
    _test_local: crate::utils::single_flight::CoalesceGuard,
    _admission: HardDeletionAdmission,
}

impl GameTestDeletionLocks {
    pub(super) fn game_transaction_mut(
        &mut self,
    ) -> &mut sqlx::Transaction<'static, sqlx::Postgres> {
        self.game.transaction_mut()
    }

    pub(super) async fn release(self) -> AppResult<()> {
        let Self {
            game,
            _test_local,
            _admission,
        } = self;
        game.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        drop(_test_local);
        drop(_admission);
        Ok(())
    }
}

pub(super) async fn acquire_game_test_deletion_locks(
    db: &sea_orm::DatabaseConnection,
    game_id: i32,
    admission: HardDeletionAdmission,
) -> AppResult<GameTestDeletionLocks> {
    let mut game = crate::services::ad_engine::acquire_ad_game_lock(db, game_id).await?;
    let key = format!("test-containers-game:{game_id}");
    let test_local = crate::utils::single_flight::coalesce(&key).await;
    crate::utils::single_flight::acquire_transaction_advisory_lock(game.transaction_mut(), &key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(GameTestDeletionLocks {
        game,
        _test_local: test_local,
        _admission: admission,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::PgPoolOptions;

    use super::{acquire_game_test_deletion_locks, acquire_hard_deletion_admission};

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn queued_hard_delete_waits_before_pool_checkout() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
        let seed = (uuid::Uuid::new_v4().as_u128() % 1_000_000_000) as i32;
        let first_admission = acquire_hard_deletion_admission().await.unwrap();
        let first = acquire_game_test_deletion_locks(&database, seed + 1, first_admission)
            .await
            .unwrap();

        let mut second = tokio::spawn({
            let database = database.clone();
            async move {
                let admission = acquire_hard_deletion_admission().await?;
                acquire_game_test_deletion_locks(&database, seed + 2, admission).await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "a second hard delete bypassed the one-operation admission gate"
        );

        let headroom = tokio::time::timeout(Duration::from_secs(1), pool.acquire())
            .await
            .expect("queued deletion consumed the remaining pool connection")
            .unwrap();
        drop(headroom);

        first.release().await.unwrap();
        let second = tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .expect("queued deletion did not enter after permit release")
            .unwrap()
            .unwrap();
        second.release().await.unwrap();
        pool.close().await;
    }
}
