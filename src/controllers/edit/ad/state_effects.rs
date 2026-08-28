//! Durable post-commit effects for A&D/KotH challenge desired-state changes.
//!
//! The desired row and this ledger entry commit together. Runtime cleanup and
//! reconcile admission happen later under a short lease, so a process crash or
//! lost HTTP response cannot strand the database state without its effect.

use std::sync::{Arc, LazyLock};

use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};

const EFFECT_LEASE_SECONDS: i64 = 15 * 60;
const MAX_EFFECTS_PER_PASS: usize = 8;
const MAX_CONCURRENT_PASSES: usize = 2;
const MAX_RETAINED_ERROR_BYTES: usize = 2_048;
const TERMINAL_RETENTION_DAYS: i32 = 7;
const MAX_PURGE_ROWS: i64 = 128;

static EFFECT_PASSES: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_PASSES)));

#[derive(Debug, sqlx::FromRow)]
struct ClaimedEffect {
    game_id: i32,
    challenge_id: i32,
    revision: i64,
    desired_enabled: bool,
    operation_id: Uuid,
    attempts: i32,
    claim_id: Uuid,
}

pub(super) async fn enqueue_locked(
    transaction: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    revision: i64,
    desired_enabled: bool,
    operation_id: Uuid,
) -> AppResult<()> {
    let stored = sqlx::query_scalar::<_, i64>(
        r#"INSERT INTO "AdChallengeStateEffects"
              (game_id, challenge_id, revision, desired_enabled, operation_id)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (challenge_id, revision) DO UPDATE
             SET updated_at_utc = "AdChallengeStateEffects".updated_at_utc
           WHERE "AdChallengeStateEffects".game_id = EXCLUDED.game_id
             AND "AdChallengeStateEffects".desired_enabled = EXCLUDED.desired_enabled
             AND "AdChallengeStateEffects".operation_id = EXCLUDED.operation_id
           RETURNING revision"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(revision)
    .bind(desired_enabled)
    .bind(operation_id)
    .fetch_optional(transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if stored != Some(revision) {
        return Err(AppError::conflict(
            "A&D state-effect revision already belongs to a different intent",
        ));
    }
    Ok(())
}

async fn claim_one(pool: &sqlx::PgPool) -> AppResult<Option<ClaimedEffect>> {
    let claim_id = Uuid::new_v4();
    sqlx::query_as::<_, ClaimedEffect>(
        r#"WITH candidate AS (
               SELECT challenge_id, revision
                 FROM "AdChallengeStateEffects"
                WHERE completed_at_utc IS NULL
                  AND next_attempt_at_utc <= clock_timestamp()
                  AND (claim_expires_at_utc IS NULL OR claim_expires_at_utc < clock_timestamp())
                ORDER BY next_attempt_at_utc, challenge_id, revision
                FOR UPDATE SKIP LOCKED
                LIMIT 1
           )
           UPDATE "AdChallengeStateEffects" effect
              SET claim_id = $1,
                  claim_expires_at_utc = clock_timestamp() + ($2 * interval '1 second'),
                  attempts = LEAST(attempts + 1, 1000000),
                  updated_at_utc = clock_timestamp()
             FROM candidate
            WHERE effect.challenge_id = candidate.challenge_id
              AND effect.revision = candidate.revision
          RETURNING effect.game_id, effect.challenge_id, effect.revision,
                    effect.desired_enabled, effect.operation_id,
                    effect.attempts, effect.claim_id"#,
    )
    .bind(claim_id)
    .bind(EFFECT_LEASE_SECONDS)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn mark_complete(pool: &sqlx::PgPool, effect: &ClaimedEffect) -> AppResult<()> {
    let changed = sqlx::query(
        r#"UPDATE "AdChallengeStateEffects"
              SET completed_at_utc = clock_timestamp(),
                  claim_id = NULL, claim_expires_at_utc = NULL,
                  last_error = NULL, updated_at_utc = clock_timestamp()
            WHERE challenge_id = $1 AND revision = $2 AND claim_id = $3"#,
    )
    .bind(effect.challenge_id)
    .bind(effect.revision)
    .bind(effect.claim_id)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if changed != 1 {
        return Err(AppError::conflict("A&D state-effect lease was lost"));
    }
    Ok(())
}

fn retry_delay_seconds(attempts: i32) -> i64 {
    let exponent = u32::try_from(attempts.saturating_sub(1).max(0))
        .unwrap_or(u32::MAX)
        .min(6);
    (30_i64.saturating_mul(2_i64.saturating_pow(exponent))).min(30 * 60)
}

fn retained_error(error: &AppError) -> String {
    let mut message = error.to_string();
    if message.len() <= MAX_RETAINED_ERROR_BYTES {
        return message;
    }
    let mut boundary = MAX_RETAINED_ERROR_BYTES;
    while !message.is_char_boundary(boundary) {
        boundary -= 1;
    }
    message.truncate(boundary);
    message
}

async fn reschedule(
    pool: &sqlx::PgPool,
    effect: &ClaimedEffect,
    error: &AppError,
) -> AppResult<()> {
    let changed = sqlx::query(
        r#"UPDATE "AdChallengeStateEffects"
              SET next_attempt_at_utc = clock_timestamp() + ($4 * interval '1 second'),
                  claim_id = NULL, claim_expires_at_utc = NULL,
                  last_error = $5, updated_at_utc = clock_timestamp()
            WHERE challenge_id = $1 AND revision = $2 AND claim_id = $3"#,
    )
    .bind(effect.challenge_id)
    .bind(effect.revision)
    .bind(effect.claim_id)
    .bind(retry_delay_seconds(effect.attempts))
    .bind(retained_error(error))
    .execute(pool)
    .await
    .map_err(|update_error| AppError::internal(update_error.to_string()))?
    .rows_affected();
    if changed != 1 {
        return Err(AppError::conflict(
            "A&D state-effect lease was lost before retry persistence",
        ));
    }
    Ok(())
}

async fn apply_claimed(st: &SharedState, effect: &ClaimedEffect) -> AppResult<()> {
    let runtime = crate::services::challenge_workloads::acquire_runtime_transition_lock(
        st.pg(),
        effect.challenge_id,
    )
    .await?;
    let current = sqlx::query_as::<_, (i16, bool, i64, bool)>(
        r#"SELECT "Type", is_enabled, ad_control_revision, deletion_pending
             FROM "GameChallenges" WHERE game_id = $1 AND id = $2"#,
    )
    .bind(effect.game_id)
    .bind(effect.challenge_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()));
    let current = match current {
        Ok(current) => current,
        Err(error) => {
            if let Err(release_error) = runtime.release().await {
                tracing::warn!(%release_error, "runtime transition lock release failed after state-effect read error");
            }
            return Err(error);
        }
    };

    let result = match current {
        // A later desired-state revision supersedes this effect. Never let an
        // old disable destroy runtimes belonging to a newer enable.
        None | Some((_, _, _, true)) => Ok(()),
        Some((_, enabled, revision, _))
            if revision != effect.revision || enabled != effect.desired_enabled =>
        {
            Ok(())
        }
        Some((challenge_type, _, _, _))
            if challenge_type != ChallengeType::AttackDefense as i16
                && challenge_type != ChallengeType::KingOfTheHill as i16 =>
        {
            Ok(())
        }
        Some(_) if effect.desired_enabled => super::request_ad_reconcile_job_with_operation(
            st,
            effect.game_id,
            true,
            true,
            effect.operation_id,
        )
        .await
        .map(|_| ()),
        Some(_) => {
            async {
                st.byoc
                    .disconnect_challenge(&st.db, effect.challenge_id)
                    .await?;
                crate::controllers::edit::destroy_challenge_containers_by_id(
                    st,
                    effect.game_id,
                    effect.challenge_id,
                    true,
                    true,
                )
                .await?;
                crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await
            }
            .await
        }
    };
    let released = runtime
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()));
    result.and(released)
}

async fn purge_completed(pool: &sqlx::PgPool) -> AppResult<u64> {
    let removed = sqlx::query(
        r#"WITH expired AS (
               SELECT challenge_id, revision
                 FROM "AdChallengeStateEffects"
                WHERE completed_at_utc < clock_timestamp() - make_interval(days => $1)
                ORDER BY completed_at_utc, challenge_id, revision
                FOR UPDATE SKIP LOCKED LIMIT $2
           )
           DELETE FROM "AdChallengeStateEffects" effect
            USING expired
            WHERE effect.challenge_id = expired.challenge_id
              AND effect.revision = expired.revision"#,
    )
    .bind(TERMINAL_RETENTION_DAYS)
    .bind(MAX_PURGE_ROWS)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    Ok(removed)
}

async fn run_bounded(st: SharedState) {
    for _ in 0..MAX_EFFECTS_PER_PASS {
        let effect = match claim_one(st.pg()).await {
            Ok(Some(effect)) => effect,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, "A&D state-effect claim failed");
                break;
            }
        };
        match apply_claimed(&st, &effect).await {
            Ok(()) => {
                if let Err(error) = mark_complete(st.pg(), &effect).await {
                    tracing::warn!(challenge = effect.challenge_id, revision = effect.revision, %error, "A&D state-effect completion failed");
                }
            }
            Err(error) => {
                if let Err(update_error) = reschedule(st.pg(), &effect, &error).await {
                    tracing::warn!(challenge = effect.challenge_id, revision = effect.revision, %update_error, "A&D state-effect retry persistence failed");
                }
                tracing::warn!(challenge = effect.challenge_id, revision = effect.revision, %error, "A&D state effect remains queued");
            }
        }
    }
    if let Err(error) = purge_completed(st.pg()).await {
        tracing::warn!(%error, "A&D state-effect retention sweep failed");
    }
}

/// Wake a bounded recovery owner. Calls are cheap and coalesce inside one
/// process; PostgreSQL row leases provide the cross-replica boundary.
pub(crate) fn kick(st: SharedState) {
    let Ok(permit) = EFFECT_PASSES.clone().try_acquire_owned() else {
        return;
    };
    tokio::spawn(async move {
        let _permit = permit;
        run_bounded(st).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn retry_and_retention_bounds_are_fixed() {
        assert_eq!(retry_delay_seconds(1), 30);
        assert_eq!(retry_delay_seconds(2), 60);
        assert_eq!(retry_delay_seconds(i32::MAX), 1_800);
        assert_eq!(MAX_EFFECTS_PER_PASS, 8);
        assert_eq!(MAX_CONCURRENT_PASSES, 2);
        assert_eq!(MAX_PURGE_ROWS, 128);
        let retained = retained_error(&AppError::internal("界".repeat(3_000)));
        assert!(retained.len() <= MAX_RETAINED_ERROR_BYTES);
        assert!(retained.is_char_boundary(retained.len()));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn effect_enqueue_and_claim_are_idempotent_across_connections() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("ad_state_effects_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "GameChallenges" (
                   game_id INTEGER NOT NULL,
                   id INTEGER NOT NULL,
                   PRIMARY KEY (game_id, id)
               );
               INSERT INTO "GameChallenges" VALUES (7, 11);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(crate::migrations::AD_CHALLENGE_STATE_EFFECTS_SQL)
            .execute(&pool)
            .await
            .unwrap();

        let operation_id = Uuid::new_v4();
        let mut transaction = pool.begin().await.unwrap();
        enqueue_locked(&mut transaction, 7, 11, 2, false, operation_id)
            .await
            .unwrap();
        enqueue_locked(&mut transaction, 7, 11, 2, false, operation_id)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let (first, second) = tokio::join!(claim_one(&pool), claim_one(&pool));
        let claimed = match (first.unwrap(), second.unwrap()) {
            (Some(effect), None) | (None, Some(effect)) => effect,
            other => panic!("expected exactly one leased effect, got {other:?}"),
        };
        assert_eq!(claimed.operation_id, operation_id);
        mark_complete(&pool, &claimed).await.unwrap();
        assert!(claim_one(&pool).await.unwrap().is_none());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
